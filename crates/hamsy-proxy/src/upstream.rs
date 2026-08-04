//! The outbound connector: dials upstream servers (optionally through a
//! chained proxy), negotiates HTTP/1.1 or HTTP/2, and pools idle
//! connections for reuse.
//!
//! # Deliberate simplification: mirrored ALPN, not independent negotiation
//!
//! The ALPN protocol offered to the upstream server mirrors whatever the
//! *client* negotiated with the proxy (see [`Connector::obtain`]'s
//! `mirror_h2` parameter) rather than negotiating independently with each
//! side. If the client speaks HTTP/1.1 to us, we speak HTTP/1.1 upstream
//! even if the origin would have preferred h2; if the client negotiated h2
//! with us, we offer `["h2", "http/1.1"]` upstream and use whichever the
//! origin picks. This avoids a large class of protocol-translation edge
//! cases a byte-perfect two-sided proxy would otherwise have to handle
//! per-hop (HTTP/2 has no chunked encoding, header casing differs, trailer
//! semantics differ between versions, ...).
//!
//! # Connection pooling
//!
//! A real (if simple) idle-connection pool is implemented below: senders
//! are checked out of a `parking_lot::Mutex<HashMap<PoolKey, Vec<_>>>`,
//! health-checked (`is_closed()`) on checkout, and expired after a 90s idle
//! timeout. Both HTTP/1.1 and HTTP/2 senders use the same
//! checkout-use-checkin cycle (exclusive use per checkout); this means an
//! HTTP/2 connection is not multiplexed across concurrent requests the way
//! a maximally-efficient h2 client would, but it is correct, simple, and
//! still allows concurrency via multiple pooled connections per key.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use parking_lot::Mutex;
use rustls_pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::error::{ProxyError, Result};
use crate::BoxBody;

/// Per-address-attempt connect timeout.
const PER_ADDR_TIMEOUT: Duration = Duration::from_secs(5);
/// Overall budget for DNS resolution plus trying every resolved address.
const OVERALL_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long an idle pooled connection is kept before it's discarded instead
/// of being reused.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Maximum idle connections retained per pool key (destination + protocol).
/// Bounds one very chatty host's bucket from growing without limit between
/// sweeps; excess connections are dropped oldest-first.
const MAX_IDLE_PER_KEY: usize = 8;

/// Which HTTP version a [`Sender`] (and thus a pooled connection) speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    /// HTTP/1.1.
    Http1,
    /// HTTP/2 (cleartext is never used here; only ever negotiated over TLS ALPN).
    Http2,
}

/// A live sender for an upstream connection, either HTTP/1.1 or HTTP/2.
pub enum Sender {
    /// An HTTP/1.1 connection.
    Http1(hyper::client::conn::http1::SendRequest<BoxBody>),
    /// An HTTP/2 connection.
    Http2(hyper::client::conn::http2::SendRequest<BoxBody>),
}

impl Sender {
    fn is_closed(&self) -> bool {
        match self {
            Sender::Http1(s) => s.is_closed(),
            Sender::Http2(s) => s.is_closed(),
        }
    }

    /// Which [`HttpVersion`] this sender speaks.
    pub fn version(&self) -> HttpVersion {
        match self {
            Sender::Http1(_) => HttpVersion::Http1,
            Sender::Http2(_) => HttpVersion::Http2,
        }
    }

    /// Sends `req` on this connection and awaits the response.
    pub async fn send_request(
        &mut self,
        req: Request<BoxBody>,
    ) -> hyper::Result<Response<Incoming>> {
        match self {
            Sender::Http1(s) => s.send_request(req).await,
            Sender::Http2(s) => s.send_request(req).await,
        }
    }
}

/// Identifies a class of interchangeable pooled connections.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    https: bool,
    host: String,
    port: u16,
    version: HttpVersion,
}

struct PooledConn {
    sender: Sender,
    idle_since: Instant,
}

/// A simple idle-connection pool keyed by destination + protocol.
struct Pool {
    conns: Mutex<HashMap<PoolKey, Vec<PooledConn>>>,
}

impl Pool {
    fn new() -> Self {
        Pool {
            conns: Mutex::new(HashMap::new()),
        }
    }

    /// Pops a healthy, non-expired connection for `key`, if any. Expired or
    /// closed connections encountered along the way are dropped.
    fn checkout(&self, key: &PoolKey) -> Option<Sender> {
        let mut conns = self.conns.lock();
        let bucket = conns.get_mut(key)?;
        let now = Instant::now();
        while let Some(pooled) = bucket.pop() {
            if pooled.sender.is_closed() {
                continue;
            }
            if now.saturating_duration_since(pooled.idle_since) > IDLE_TIMEOUT {
                continue;
            }
            return Some(pooled.sender);
        }
        None
    }

    /// Returns `sender` to the pool under `key`, unless it's already closed.
    ///
    /// Before inserting, sweeps *every* bucket in the pool (not just `key`'s)
    /// for connections idle past `IDLE_TIMEOUT`, dropping them. This is what
    /// reclaims connections to a host that's stopped being revisited: without
    /// it, `checkout`'s lazy expiry (which only ever looks at the bucket it's
    /// asked for) never runs for a key nothing checks out again, leaking that
    /// socket, its background driver task (see `handshake`), and the memory
    /// both hold for as long as the process lives. Dropping a `PooledConn`
    /// drops its `Sender`, which is what lets the driver task see the
    /// connection is done and exit -- no separate shutdown call needed.
    /// Also caps each bucket at `MAX_IDLE_PER_KEY`, oldest-first, so one
    /// frequently-revisited host can't grow its bucket without bound between
    /// sweeps.
    fn checkin(&self, key: PoolKey, sender: Sender) {
        if sender.is_closed() {
            return;
        }
        let mut conns = self.conns.lock();
        let now = Instant::now();
        conns.retain(|_, bucket| {
            bucket.retain(|pooled| {
                !pooled.sender.is_closed()
                    && now.saturating_duration_since(pooled.idle_since) <= IDLE_TIMEOUT
            });
            !bucket.is_empty()
        });

        let bucket = conns.entry(key).or_default();
        bucket.push(PooledConn {
            sender,
            idle_since: now,
        });
        if bucket.len() > MAX_IDLE_PER_KEY {
            let excess = bucket.len() - MAX_IDLE_PER_KEY;
            // `checkout` pops from the back (most recently returned first),
            // so the front of the vec is the oldest-inserted; drop those.
            bucket.drain(0..excess);
        }
    }
}

/// The result of [`Connector::obtain`]: a ready-to-use sender plus enough
/// per-phase timing/addressing info for the caller to populate
/// [`hamsy_core::Timings`] and [`hamsy_core::FlowSummary`].
pub struct Obtained {
    /// The sender, ready for `send_request`.
    pub sender: Sender,
    /// The HTTP version actually in use on this connection.
    pub version: HttpVersion,
    /// Milliseconds spent establishing the TCP connection (0.0 if this was
    /// a pooled, already-connected sender).
    pub connect_ms: f64,
    /// Milliseconds spent on the TLS handshake, or `-1.0` if this wasn't a
    /// TLS connection (or was pooled).
    pub ssl_ms: f64,
    /// The dialed peer address, if a fresh connection was made.
    pub server_addr: Option<String>,
    /// Whether this connection was made through a chained upstream proxy
    /// (see `Settings::upstream_proxy`); if true, the caller must send
    /// requests in absolute-form rather than origin-form.
    pub via_proxy: bool,
}

/// The outbound connector: TCP/TLS dialing, protocol handshake, and pooling.
pub struct Connector {
    pool: Pool,
    /// A `TlsConnector` offering only `["http/1.1"]` via ALPN.
    tls_h1: tokio_rustls::TlsConnector,
    /// A `TlsConnector` offering `["h2", "http/1.1"]` via ALPN.
    tls_h2: tokio_rustls::TlsConnector,
}

impl Connector {
    /// Builds a new connector, loading the platform's native root
    /// certificates (falling back to the compiled-in Mozilla root set if
    /// native loading fails or returns nothing usable, e.g. in minimal
    /// containers without a system trust store).
    pub fn new() -> Result<Self> {
        Self::with_extra_roots(&[])
    }

    /// Like [`Connector::new`], but additionally trusts `extra_roots` -
    /// intended for tests that stand up a throwaway TLS origin server (a
    /// real deployment has no such certs to add, so this is exercised only
    /// via [`Connector::new`] in production, which passes an empty slice).
    pub fn with_extra_roots(
        extra_roots: &[rustls_pki_types::CertificateDer<'static>],
    ) -> Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            // Skip individually malformed certs rather than failing outright.
            let _ = roots.add(cert);
        }
        if roots.is_empty() {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        for cert in extra_roots {
            let _ = roots.add(cert.clone());
        }

        let base = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let mut h1_config = base.clone();
        h1_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let mut h2_config = base;
        h2_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        Ok(Connector {
            pool: Pool::new(),
            tls_h1: tokio_rustls::TlsConnector::from(Arc::new(h1_config)),
            tls_h2: tokio_rustls::TlsConnector::from(Arc::new(h2_config)),
        })
    }

    /// Obtains a sender for `scheme://host:port`, reusing a pooled
    /// connection if one is available, otherwise dialing a fresh one
    /// (optionally through `upstream_proxy`, e.g. `"http://proxy:8080"`).
    ///
    /// `mirror_h2` should be true iff the client negotiated HTTP/2 with the
    /// proxy on the connection this request arrived on; see the module-level
    /// doc for why upstream ALPN mirrors the client's negotiated protocol.
    pub async fn obtain(
        &self,
        scheme: &str,
        host: &str,
        port: u16,
        mirror_h2: bool,
        upstream_proxy: Option<&str>,
    ) -> Result<Obtained> {
        let is_https = scheme.eq_ignore_ascii_case("https");
        let via_proxy = upstream_proxy.is_some();
        let key = PoolKey {
            https: is_https,
            host: host.to_string(),
            port,
            version: if mirror_h2 {
                HttpVersion::Http2
            } else {
                HttpVersion::Http1
            },
        };

        if let Some(sender) = self.pool.checkout(&key) {
            let version = sender.version();
            return Ok(Obtained {
                sender,
                version,
                connect_ms: 0.0,
                ssl_ms: -1.0,
                server_addr: None,
                via_proxy,
            });
        }

        let (dial_host, dial_port) = match upstream_proxy {
            Some(p) => parse_proxy_url(p)?,
            None => (host.to_string(), port),
        };

        let (mut tcp, addr, connect_ms) = dial_tcp(&dial_host, dial_port).await?;

        if via_proxy && is_https {
            tunnel_connect_through_proxy(&mut tcp, host, port).await?;
        }

        let server_addr = addr.to_string();

        if is_https {
            let ssl_start = Instant::now();
            let server_name = ServerName::try_from(host.to_string()).map_err(|_| {
                ProxyError::InvalidTarget(format!("invalid TLS server name: {host}"))
            })?;
            let connector = if mirror_h2 {
                &self.tls_h2
            } else {
                &self.tls_h1
            };
            let tls_stream = connector.connect(server_name, tcp).await.map_err(|e| {
                ProxyError::UpstreamConnect(format!("TLS handshake with {host}:{port} failed: {e}"))
            })?;
            let ssl_ms = ssl_start.elapsed().as_secs_f64() * 1000.0;
            let negotiated_h2 = tls_stream.get_ref().1.alpn_protocol() == Some(b"h2");
            let io = TokioIo::new(tls_stream);
            let (sender, version) = handshake(io, negotiated_h2).await?;
            Ok(Obtained {
                sender,
                version,
                connect_ms,
                ssl_ms,
                server_addr: Some(server_addr),
                via_proxy,
            })
        } else {
            let io = TokioIo::new(tcp);
            let (sender, version) = handshake(io, false).await?;
            Ok(Obtained {
                sender,
                version,
                connect_ms,
                ssl_ms: -1.0,
                server_addr: Some(server_addr),
                via_proxy,
            })
        }
    }

    /// Returns `sender` to the pool for future reuse, unless it's already
    /// closed. Keyed identically to [`Connector::obtain`]'s lookup.
    pub fn release(&self, scheme: &str, host: &str, port: u16, sender: Sender) {
        let key = PoolKey {
            https: scheme.eq_ignore_ascii_case("https"),
            host: host.to_string(),
            port,
            version: sender.version(),
        };
        self.pool.checkin(key, sender);
    }
}

/// Performs the HTTP/1.1 or HTTP/2 client handshake over an already
/// connected (and, for HTTPS, already TLS-terminated) stream, spawning a
/// background task to drive the connection.
async fn handshake<T>(io: T, want_h2: bool) -> Result<(Sender, HttpVersion)>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    if want_h2 {
        let (sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
            .await
            .map_err(|e| ProxyError::UpstreamConnect(format!("http2 handshake failed: {e}")))?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(%err, "upstream h2 connection task ended");
            }
        });
        Ok((Sender::Http2(sender), HttpVersion::Http2))
    } else {
        let (sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| ProxyError::UpstreamConnect(format!("http1 handshake failed: {e}")))?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(%err, "upstream h1 connection task ended");
            }
        });
        Ok((Sender::Http1(sender), HttpVersion::Http1))
    }
}

/// Resolves `host:port` and attempts each returned address in turn (~5s per
/// address, ~15s overall), returning the first successful connection.
/// Sequential rather than a parallel happy-eyeballs race, per spec.
async fn dial_tcp(host: &str, port: u16) -> Result<(TcpStream, std::net::SocketAddr, f64)> {
    let start = Instant::now();
    let lookup = tokio::time::timeout(
        OVERALL_CONNECT_TIMEOUT,
        tokio::net::lookup_host((host, port)),
    )
    .await
    .map_err(|_| ProxyError::Timeout(format!("DNS lookup timed out for {host}:{port}")))?
    .map_err(|e| ProxyError::UpstreamConnect(format!("{host}:{port}: DNS lookup failed: {e}")))?;
    let addrs: Vec<std::net::SocketAddr> = lookup.collect();
    if addrs.is_empty() {
        return Err(ProxyError::UpstreamConnect(format!(
            "{host}:{port}: no addresses found"
        )));
    }

    let deadline = start + OVERALL_CONNECT_TIMEOUT;
    let mut last_err: Option<String> = None;
    for addr in addrs {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let attempt_timeout = remaining.min(PER_ADDR_TIMEOUT);
        match tokio::time::timeout(attempt_timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                return Ok((stream, addr, elapsed_ms));
            }
            Ok(Err(e)) => last_err = Some(e.to_string()),
            Err(_) => last_err = Some("connection attempt timed out".to_string()),
        }
    }
    Err(ProxyError::UpstreamConnect(format!(
        "{host}:{port}: {}",
        last_err.unwrap_or_else(|| "unreachable".to_string())
    )))
}

/// Issues a `CONNECT host:port` request over an already-established TCP
/// connection to a chained upstream proxy, and waits for its `200`
/// response, so a TLS handshake can then proceed through the tunnel.
async fn tunnel_connect_through_proxy(tcp: &mut TcpStream, host: &str, port: u16) -> Result<()> {
    let req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n");
    tcp.write_all(req.as_bytes()).await?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 {
            return Err(ProxyError::UpstreamConnect(
                "chained proxy closed the connection during CONNECT".to_string(),
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 8192 {
            return Err(ProxyError::UpstreamConnect(
                "chained proxy CONNECT response too large".to_string(),
            ));
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("");
    if !status_line.contains(" 200") {
        return Err(ProxyError::UpstreamConnect(format!(
            "chained proxy refused CONNECT {host}:{port}: {status_line}"
        )));
    }
    Ok(())
}

/// A body wrapper that returns its associated [`Sender`] to the connection
/// pool once the body is fully, cleanly drained (`poll_frame` yields
/// `None`) - never on early abort (a client disconnecting mid-response, an
/// upstream error, ...), since a partially-drained HTTP/1.1 connection
/// can't safely be reused for another request. On early drop, the `Sender`
/// (and the connection it owns) is simply dropped instead of being pooled.
pub struct ReleaseOnComplete<B> {
    inner: B,
    release: Option<(Arc<Connector>, bool, String, u16, Sender)>,
}

impl<B> ReleaseOnComplete<B> {
    /// Wraps `inner`, arranging for `sender` to be returned to `connector`'s
    /// pool (under `(https, host, port)`) once `inner` cleanly completes.
    pub fn new(
        inner: B,
        connector: Arc<Connector>,
        https: bool,
        host: String,
        port: u16,
        sender: Sender,
    ) -> Self {
        ReleaseOnComplete {
            inner,
            release: Some((connector, https, host, port, sender)),
        }
    }
}

impl<B> Body for ReleaseOnComplete<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_frame(cx);
        if let Poll::Ready(None) = &poll {
            if let Some((connector, https, host, port, sender)) = this.release.take() {
                let scheme = if https { "https" } else { "http" };
                connector.release(scheme, &host, port, sender);
            }
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Parses an upstream proxy URL like `"http://host:port"` into
/// `(host, port)`, defaulting the port to `80` if absent.
fn parse_proxy_url(raw: &str) -> Result<(String, u16)> {
    let url = url::Url::parse(raw).map_err(|e| {
        ProxyError::InvalidTarget(format!("invalid upstream proxy url '{raw}': {e}"))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| {
            ProxyError::InvalidTarget(format!("upstream proxy url '{raw}' has no host"))
        })?
        .to_string();
    let port = url.port().unwrap_or(80);
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proxy_url_defaults_port() {
        let (host, port) = parse_proxy_url("http://proxy.example.com").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 80);
    }

    #[test]
    fn parse_proxy_url_explicit_port() {
        let (host, port) = parse_proxy_url("http://proxy.example.com:3128").unwrap();
        assert_eq!(host, "proxy.example.com");
        assert_eq!(port, 3128);
    }

    #[test]
    fn parse_proxy_url_rejects_garbage() {
        assert!(parse_proxy_url("not a url").is_err());
    }
}
