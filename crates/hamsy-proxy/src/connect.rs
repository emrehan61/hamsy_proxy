//! `CONNECT` handling: replying immediately, peeking the tunneled bytes to
//! decide tunnel-vs-MITM, and (for MITM) hand-parsing the TLS ClientHello to
//! extract SNI/ALPN before terminating TLS ourselves.

use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use uuid::Uuid;

use hamsy_core::{BodyPayload, Flow, FlowState, RequestRecord, ResponseRecord, ServerEvent};

use crate::config::ProxyContext;
use crate::error::{ProxyError, Result};
use crate::http::{self, now_ms, ConnInfo};
use crate::server;
use crate::BoxBody;

/// Max bytes we'll read while peeking for a TLS ClientHello before giving up
/// and treating whatever we have as final (still replayed via [`Rewind`]).
const MAX_PEEK_BYTES: usize = 16 * 1024;
/// Size of each individual peek read.
const PEEK_CHUNK: usize = 4096;

/// Handles a `CONNECT host:port` request: replies `200` immediately (without
/// dialing upstream), then completes the hyper upgrade and hands the raw
/// duplex stream to [`handle_tunnel`] in the background.
///
/// Returns an explicitly boxed future (rather than a plain `async fn`,
/// which would desugar to an opaque `impl Future` type) because this
/// function is mutually recursive with `server::serve_h1`/`serve_h2`
/// through `server::route` (a MITM'd/tunneled connection can itself carry
/// further requests, including in principle another `CONNECT`) - Rust's
/// opaque-type inference can't resolve an auto-trait (`Send`) cycle across
/// that recursion, so one leg of it needs a concrete, spelled-out type.
pub fn handle_connect(
    ctx: ProxyContext,
    mut req: Request<Incoming>,
    client_addr: SocketAddr,
    app: Option<String>,
) -> Pin<Box<dyn std::future::Future<Output = Response<BoxBody>> + Send>> {
    Box::pin(async move {
        let (host, port) = match parse_authority(&req) {
            Ok(v) => v,
            Err(e) => return http::error_response(StatusCode::BAD_REQUEST, &e.to_string()),
        };

        let upgrade_fut = hyper::upgrade::on(&mut req);
        tokio::spawn(async move {
            match upgrade_fut.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    handle_tunnel(ctx, io, host, port, client_addr, app).await;
                }
                Err(err) => {
                    tracing::debug!(%err, "CONNECT upgrade failed");
                }
            }
        });

        Response::builder()
            .status(StatusCode::OK)
            .body(crate::empty_body())
            .unwrap_or_else(|_| {
                let mut resp = Response::new(crate::empty_body());
                *resp.status_mut() = StatusCode::OK;
                resp
            })
    })
}

/// Extracts `(host, port)` from a `CONNECT` request's authority-form target.
fn parse_authority(req: &Request<Incoming>) -> Result<(String, u16)> {
    let authority = req.uri().authority().ok_or_else(|| {
        ProxyError::InvalidTarget("CONNECT request missing target authority".to_string())
    })?;
    let host = authority.host().to_string();
    let port = authority
        .port_u16()
        .ok_or_else(|| ProxyError::InvalidTarget("CONNECT target missing a port".to_string()))?;
    Ok((host, port))
}

/// After the `CONNECT` upgrade completes, decides what the tunneled bytes
/// are (TLS, plaintext HTTP, or opaque) and routes accordingly.
async fn handle_tunnel<IO>(
    ctx: ProxyContext,
    io: TokioIo<IO>,
    host: String,
    port: u16,
    client_addr: SocketAddr,
    app: Option<String>,
) where
    IO: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let mut io = io;
    let peeked = match peek_bytes(&mut io).await {
        Ok(p) => p,
        Err(err) => {
            tracing::debug!(%err, "failed to peek CONNECT tunnel bytes");
            return;
        }
    };
    if peeked.is_empty() {
        return; // client closed immediately after CONNECT; nothing to do.
    }

    let rewind = Rewind::new(io, Bytes::from(peeked.clone()));

    if peeked[0] != 0x16 {
        if looks_like_plaintext_http(&peeked) {
            let conn_info = ConnInfo {
                client_addr,
                scheme: "http",
                authority: Some(format!("{host}:{port}")),
                tls: None,
                mirror_h2: false,
                app,
            };
            server::serve_h1(ctx, TokioIo::new(rewind), conn_info).await;
        } else if let Err(err) = raw_tunnel(rewind, &host, port).await {
            tracing::debug!(%err, host = %host, port, "opaque tunnel failed");
        }
        return;
    }

    // TLS ClientHello. Passthrough (blind tunnel) vs MITM.
    if !ctx.should_intercept(&host) {
        run_passthrough_flow(&ctx, rewind, &host, port, client_addr, app).await;
        return;
    }

    let info = parse_client_hello(&peeked);
    let sni = info
        .as_ref()
        .and_then(|i| i.sni.clone())
        .unwrap_or_else(|| host.clone());
    let offered_alpn = match &info {
        Some(i) if !i.alpn.is_empty() => i.alpn.clone(),
        _ => vec![b"http/1.1".to_vec()],
    };

    let server_config = match ctx.ca.server_config(&sni, &offered_alpn) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::debug!(%err, host = %sni, "failed to build TLS server config for MITM");
            return;
        }
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);
    let tls_stream = match acceptor.accept(rewind).await {
        Ok(s) => s,
        Err(err) => {
            tracing::debug!(%err, host = %sni, "TLS handshake with client failed");
            return;
        }
    };

    let (_, conn) = tls_stream.get_ref();
    let negotiated_h2 = conn.alpn_protocol() == Some(b"h2");
    let tls_info = build_tls_info(conn, &sni);
    let tokio_io = TokioIo::new(tls_stream);

    let conn_info = ConnInfo {
        client_addr,
        scheme: "https",
        authority: Some(format!("{host}:{port}")),
        tls: Some(tls_info),
        mirror_h2: negotiated_h2,
        app,
    };

    if negotiated_h2 {
        server::serve_h2(ctx, tokio_io, conn_info).await;
    } else {
        server::serve_h1(ctx, tokio_io, conn_info).await;
    }
}

/// Records a minimal `CONNECT` flow and blind-tunnels bytes to the origin,
/// so the UI can show that a passthrough occurred even though no HTTP
/// content was inspected.
async fn run_passthrough_flow<S>(
    ctx: &ProxyContext,
    client: S,
    host: &str,
    port: u16,
    client_addr: SocketAddr,
    app: Option<String>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !ctx.should_capture(host) {
        if let Err(err) = raw_tunnel(client, host, port).await {
            tracing::debug!(%err, host, port, "passthrough tunnel failed");
        }
        return;
    }

    let flow_id = Uuid::new_v4();
    let seq = ctx.flows.next_seq();
    let started_at = now_ms();
    let url = format!("https://{host}:{port}");
    let req_record = RequestRecord {
        method: "CONNECT".to_string(),
        url: url.clone(),
        http_version: "HTTP/1.1".to_string(),
        headers: vec![],
        body: BodyPayload::default(),
        query: vec![],
    };
    let mut flow = Flow::new_request(
        flow_id,
        seq,
        started_at,
        "CONNECT",
        "https",
        host,
        port,
        "",
        url,
        "HTTP/1.1",
        client_addr.to_string(),
        req_record,
    );
    flow.summary.state = FlowState::Requesting;
    flow.summary.app = app;
    ctx.flows.insert(flow.clone());
    let _ = ctx.events.send(ServerEvent::Flow {
        flow: flow.summary(),
    });

    let result = raw_tunnel(client, host, port).await;
    let finished_at = now_ms();
    match result {
        Ok(()) => {
            let resp_record = ResponseRecord {
                status: 200,
                status_text: "Connection Closed".to_string(),
                http_version: "HTTP/1.1".to_string(),
                headers: vec![],
                body: BodyPayload::default(),
            };
            if let Some(summary) = ctx
                .flows
                .update(flow_id, |f| f.mark_complete(resp_record, finished_at))
            {
                let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
            }
        }
        Err(err) => {
            if let Some(summary) = ctx
                .flows
                .update(flow_id, |f| f.mark_error(err.to_string(), finished_at))
            {
                let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
            }
        }
    }
}

/// Best-effort raw byte tunnel to `host:port`.
async fn raw_tunnel<S>(mut client: S, host: &str, port: u16) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut upstream = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .map_err(|_| ProxyError::Timeout(format!("connecting to {host}:{port}")))?
    .map_err(|e| ProxyError::UpstreamConnect(format!("{host}:{port}: {e}")))?;
    let _ = upstream.set_nodelay(true);
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

fn looks_like_plaintext_http(data: &[u8]) -> bool {
    const METHODS: &[&[u8]] = &[
        b"GET ",
        b"POST ",
        b"PUT ",
        b"HEAD ",
        b"DELETE ",
        b"OPTIONS ",
        b"PATCH ",
        b"CONNECT ",
        b"TRACE ",
    ];
    METHODS.iter().any(|m| data.starts_with(m))
}

fn build_tls_info(conn: &rustls::CommonState, sni: &str) -> hamsy_core::TlsInfo {
    hamsy_core::TlsInfo {
        version: conn.protocol_version().map(format_tls_version),
        cipher_suite: conn
            .negotiated_cipher_suite()
            .map(|cs| format!("{:?}", cs.suite())),
        alpn: conn
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).to_string()),
        sni: Some(sni.to_string()),
        peer_cert_subject: None,
        peer_cert_issuer: None,
        not_before: None,
        not_after: None,
    }
}

fn format_tls_version(v: rustls::ProtocolVersion) -> String {
    match v {
        rustls::ProtocolVersion::TLSv1_3 => "TLSv1.3".to_string(),
        rustls::ProtocolVersion::TLSv1_2 => "TLSv1.2".to_string(),
        rustls::ProtocolVersion::TLSv1_1 => "TLSv1.1".to_string(),
        rustls::ProtocolVersion::TLSv1_0 => "TLSv1.0".to_string(),
        other => format!("{other:?}"),
    }
}

/// Reads an initial chunk of bytes from `io`, and - if it looks like the
/// start of a TLS record - keeps reading until the declared record length is
/// satisfied (or a safety cap is hit), so [`parse_client_hello`] has the best
/// chance of seeing a complete ClientHello in one shot.
async fn peek_bytes<IO: AsyncRead + Unpin>(io: &mut IO) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; PEEK_CHUNK];
    let n = io.read(&mut buf).await?;
    buf.truncate(n);
    if n == 0 || buf[0] != 0x16 || buf.len() < 5 {
        return Ok(buf);
    }
    let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    let target = (5 + record_len).min(MAX_PEEK_BYTES);
    while buf.len() < target {
        let want = (target - buf.len()).min(PEEK_CHUNK);
        let mut extra = vec![0u8; want];
        let n2 = io.read(&mut extra).await?;
        if n2 == 0 {
            break;
        }
        extra.truncate(n2);
        buf.extend_from_slice(&extra);
    }
    Ok(buf)
}

// ===== Hand-rolled TLS ClientHello parser =====
//
// Deliberately dependency-free: we only need SNI + ALPN, and pulling in a
// full TLS message parser for that would be overkill. Every access below is
// bounds-checked (`Cursor::take*` returns `Option`, never panics/indexes
// out of bounds), so adversarial or truncated input just yields `None`
// rather than a panic - callers fall back to the CONNECT authority host and
// `["http/1.1"]` in that case.

/// SNI hostname and offered ALPN protocols extracted from a ClientHello.
#[derive(Debug, Default, Clone)]
pub struct ClientHelloInfo {
    /// The `server_name` extension's first hostname entry, if present.
    pub sni: Option<String>,
    /// The `application_layer_protocol_negotiation` extension's protocol
    /// list, in client preference order (empty if absent).
    pub alpn: Vec<Vec<u8>>,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn take_u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }

    fn take_u16(&mut self) -> Option<u16> {
        let s = self.take(2)?;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

/// Parses a TLS record containing a ClientHello, extracting SNI and ALPN.
/// Returns `None` if `data` doesn't look like a (sufficiently complete)
/// TLS 1.x ClientHello handshake record.
pub fn parse_client_hello(data: &[u8]) -> Option<ClientHelloInfo> {
    let mut c = Cursor::new(data);
    if c.take_u8()? != 0x16 {
        return None; // not a handshake record
    }
    let _legacy_version = c.take_u16()?;
    let _record_len = c.take_u16()?;

    if c.take_u8()? != 0x01 {
        return None; // not a ClientHello
    }
    let _handshake_len = c.take(3)?;
    let _client_version = c.take_u16()?;
    let _random = c.take(32)?;

    let session_id_len = c.take_u8()? as usize;
    c.take(session_id_len)?;

    let cipher_suites_len = c.take_u16()? as usize;
    c.take(cipher_suites_len)?;

    let compression_len = c.take_u8()? as usize;
    c.take(compression_len)?;

    let mut sni = None;
    let mut alpn = Vec::new();

    if c.remaining() >= 2 {
        let ext_total_len = c.take_u16()? as usize;
        let ext_bytes = c.take(ext_total_len)?;
        let mut ec = Cursor::new(ext_bytes);
        while ec.remaining() >= 4 {
            let Some(ext_type) = ec.take_u16() else { break };
            let Some(ext_len) = ec.take_u16() else { break };
            let Some(ext_data) = ec.take(ext_len as usize) else {
                break;
            };
            match ext_type {
                0x0000 => sni = parse_sni(ext_data),
                0x0010 => alpn = parse_alpn(ext_data),
                _ => {}
            }
        }
    }

    Some(ClientHelloInfo { sni, alpn })
}

/// Parses a `server_name` extension body, returning the first `host_name`
/// entry (name_type `0`), which is what virtually every TLS client sends.
fn parse_sni(data: &[u8]) -> Option<String> {
    let mut c = Cursor::new(data);
    let _list_len = c.take_u16()?;
    let name_type = c.take_u8()?;
    let name_len = c.take_u16()? as usize;
    let name = c.take(name_len)?;
    if name_type != 0 {
        return None;
    }
    std::str::from_utf8(name).ok().map(str::to_string)
}

/// Parses an `application_layer_protocol_negotiation` extension body into
/// its list of protocol name byte strings.
fn parse_alpn(data: &[u8]) -> Vec<Vec<u8>> {
    let mut c = Cursor::new(data);
    let mut out = Vec::new();
    if c.take_u16().is_none() {
        return out;
    }
    while c.remaining() > 0 {
        let Some(len) = c.take_u8() else { break };
        let Some(proto) = c.take(len as usize) else {
            break;
        };
        out.push(proto.to_vec());
    }
    out
}

// ===== Rewind adapter =====

/// Wraps an `AsyncRead + AsyncWrite` stream plus a leftover byte buffer that
/// is drained (replayed) before any further reads reach the underlying
/// stream. Used to "un-peek" bytes consumed while sniffing for a TLS
/// ClientHello.
pub struct Rewind<S> {
    io: S,
    leftover: Option<Bytes>,
}

impl<S> Rewind<S> {
    /// Wraps `io`, replaying `leftover` before any of `io`'s own bytes.
    pub fn new(io: S, leftover: Bytes) -> Self {
        Rewind {
            io,
            leftover: if leftover.is_empty() {
                None
            } else {
                Some(leftover)
            },
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Rewind<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if let Some(mut data) = self.leftover.take() {
            if !data.is_empty() {
                let n = std::cmp::min(data.len(), buf.remaining());
                buf.put_slice(&data[..n]);
                data.advance(n);
                if !data.is_empty() {
                    self.leftover = Some(data);
                }
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Rewind<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Hand-encodes a minimal ClientHello record containing SNI + ALPN, for
    /// parser testing without a real TLS stack.
    fn build_client_hello(sni: &str, alpn: &[&str]) -> Vec<u8> {
        let mut sni_ext = Vec::new();
        let name = sni.as_bytes();
        let entry_len = 1 + 2 + name.len();
        sni_ext.extend_from_slice(&(entry_len as u16).to_be_bytes());
        sni_ext.push(0); // host_name
        sni_ext.extend_from_slice(&(name.len() as u16).to_be_bytes());
        sni_ext.extend_from_slice(name);

        let mut alpn_ext = Vec::new();
        let mut proto_list = Vec::new();
        for p in alpn {
            proto_list.push(p.len() as u8);
            proto_list.extend_from_slice(p.as_bytes());
        }
        alpn_ext.extend_from_slice(&(proto_list.len() as u16).to_be_bytes());
        alpn_ext.extend_from_slice(&proto_list);

        let mut extensions = Vec::new();
        extensions.extend_from_slice(&0x0000u16.to_be_bytes());
        extensions.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&sni_ext);
        extensions.extend_from_slice(&0x0010u16.to_be_bytes());
        extensions.extend_from_slice(&(alpn_ext.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&alpn_ext);

        let mut body = Vec::new();
        body.extend_from_slice(&0x0303u16.to_be_bytes()); // client_version
        body.extend_from_slice(&[0u8; 32]); // random
        body.push(0); // session_id_len
        body.extend_from_slice(&2u16.to_be_bytes()); // cipher_suites_len
        body.extend_from_slice(&[0x13, 0x01]); // one cipher suite
        body.push(1); // compression_methods_len
        body.push(0);
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);

        let mut handshake = Vec::new();
        handshake.push(0x01); // ClientHello
        let body_len = body.len() as u32;
        handshake.extend_from_slice(&body_len.to_be_bytes()[1..4]);
        handshake.extend_from_slice(&body);

        let mut record = Vec::new();
        record.push(0x16);
        record.extend_from_slice(&0x0301u16.to_be_bytes());
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    #[test]
    fn parses_sni_and_alpn() {
        let record = build_client_hello("example.com", &["h2", "http/1.1"]);
        let info = parse_client_hello(&record).expect("should parse");
        assert_eq!(info.sni.as_deref(), Some("example.com"));
        assert_eq!(info.alpn, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    }

    #[test]
    fn non_tls_first_byte_returns_none() {
        assert!(parse_client_hello(b"GET / HTTP/1.1\r\n").is_none());
    }

    #[test]
    fn truncated_input_returns_none_not_panic() {
        let record = build_client_hello("example.com", &["h2"]);
        for cut in 0..record.len() {
            // Must never panic regardless of where we truncate.
            let _ = parse_client_hello(&record[..cut]);
        }
    }

    #[test]
    fn empty_and_garbage_input_does_not_panic() {
        assert!(parse_client_hello(&[]).is_none());
        assert!(parse_client_hello(&[0x16]).is_none());
        assert!(parse_client_hello(&[0xff; 200]).is_none());
    }

    #[test]
    fn looks_like_plaintext_http_detects_methods() {
        assert!(looks_like_plaintext_http(b"GET / HTTP/1.1\r\n"));
        assert!(looks_like_plaintext_http(b"POST /x HTTP/1.1\r\n"));
        assert!(!looks_like_plaintext_http(&[0x16, 0x03, 0x01]));
        assert!(!looks_like_plaintext_http(b"garbage"));
    }

    #[tokio::test]
    async fn rewind_replays_leftover_before_live_data() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut rewind = Rewind::new(server, Bytes::from_static(b"peeked-"));
        client.write_all(b"live").await.unwrap();
        drop(client);

        let mut out = Vec::new();
        rewind.read_to_end(&mut out).await.unwrap();
        assert_eq!(out, b"peeked-live");
    }
}
