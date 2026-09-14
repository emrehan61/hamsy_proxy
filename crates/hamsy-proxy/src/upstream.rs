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
//! HTTP/1.1 senders are checked out exclusively until the response drains.
//! HTTP/2 senders remain shared in the pool; each checkout owns a stream
//! permit until its response completes or is dropped. ALPN preferences and
//! chained-proxy routes are part of the key, including for HTTP/1.1 fallbacks.
//!
//! Each [`Sender`] carries the peer address it was dialed to (see
//! [`Sender::addr`]), resolved once at dial time and never recomputed. That
//! address rides along through checkin and checkout, so a pooled reuse
//! reports the same [`Obtained::server_addr`] the original dial did, instead
//! of going blank on every request after the first.

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
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
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
/// Maximum pooled connections retained per destination and ALPN/route policy.
/// Bounds one very chatty host's bucket from growing without limit between
/// sweeps; excess connections are dropped oldest-first.
const MAX_IDLE_PER_KEY: usize = 8;
/// Local upper bound; hyper also honors the peer SETTINGS limit.
const MAX_H2_STREAMS: usize = 100;

/// Which HTTP version a [`Sender`] (and thus a pooled connection) speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    /// HTTP/1.1.
    Http1,
    /// HTTP/2 (cleartext is never used here; only ever negotiated over TLS ALPN).
    Http2,
}

/// The underlying HTTP/1.1 or HTTP/2 client handle wrapped by [`Sender`].
enum SenderKind {
    /// An HTTP/1.1 connection.
    Http1(hyper::client::conn::http1::SendRequest<BoxBody>),
    /// An HTTP/2 connection.
    Http2(hyper::client::conn::http2::SendRequest<BoxBody>),
}

/// A live sender for an upstream connection, either HTTP/1.1 or HTTP/2,
/// paired with the peer address it was dialed to.
///
/// The address is resolved once, at dial time (see [`Connector::obtain`]),
/// and is carried on the `Sender` itself rather than threaded separately
/// through the pool's checkin/checkout calls: a `Sender` travels from a
/// fresh dial, through a caller-owned request/response cycle, into
/// [`ReleaseOnComplete`] and back into the pool without passing through any
/// call site this module controls end-to-end, so this is the only way a
/// pooled reuse can still report the same `server_addr` the original dial
/// did (see `Obtained::server_addr`).
pub struct Sender {
    kind: SenderKind,
    addr: String,
    pool_key: Option<PoolKey>,
    h2_slots: Option<Arc<Semaphore>>,
    stream_permit: Option<OwnedSemaphorePermit>,
}

impl Sender {
    fn is_closed(&self) -> bool {
        match &self.kind {
            SenderKind::Http1(s) => s.is_closed(),
            SenderKind::Http2(s) => s.is_closed(),
        }
    }

    /// Which [`HttpVersion`] this sender speaks.
    pub fn version(&self) -> HttpVersion {
        match &self.kind {
            SenderKind::Http1(_) => HttpVersion::Http1,
            SenderKind::Http2(_) => HttpVersion::Http2,
        }
    }

    /// The resolved peer address this connection was dialed to (or, for a
    /// connection made through a chained upstream proxy, the proxy's
    /// address).
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Sends `req` on this connection and awaits the response.
    pub async fn send_request(
        &mut self,
        req: Request<BoxBody>,
    ) -> hyper::Result<Response<Incoming>> {
        match &mut self.kind {
            SenderKind::Http1(s) => s.send_request(req).await,
            SenderKind::Http2(s) => s.send_request(req).await,
        }
    }
}

/// Identifies a class of interchangeable pooled connections.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    https: bool,
    host: String,
    port: u16,
    mirror_h2: bool,
    upstream_proxy: Option<String>,
}

struct PooledConn {
    sender: Sender,
    idle_since: Instant,
}

/// Idle H1 and shared H2 connections keyed by destination and ALPN/route policy.
struct Pool {
    conns: Mutex<HashMap<PoolKey, Vec<PooledConn>>>,
}

impl Pool {
    fn new() -> Self {
        Pool {
            conns: Mutex::new(HashMap::new()),
        }
    }

    /// H1 is removed exclusively; H2 is cloned while its pool handle stays
    /// available. Waiting for capacity never holds the pool lock.
    async fn checkout(&self, key: &PoolKey) -> Option<Sender> {
        let mut sender = {
            let mut conns = self.conns.lock();
            let bucket = conns.get_mut(key)?;
            let now = Instant::now();
            bucket.retain(|p| {
                !p.sender.is_closed()
                    && (p
                        .sender
                        .h2_slots
                        .as_ref()
                        .is_some_and(|s| s.available_permits() < MAX_H2_STREAMS)
                        || now.saturating_duration_since(p.idle_since) <= IDLE_TIMEOUT)
            });
            // Prefer capacity already available on any connection, rather than
            // queueing behind a saturated last-inserted H2 connection.
            let index = bucket
                .iter()
                .rposition(|p| {
                    p.sender
                        .h2_slots
                        .as_ref()
                        .is_none_or(|slots| slots.available_permits() > 0)
                })
                .or_else(|| bucket.len().checked_sub(1))?;
            let pooled = &mut bucket[index];
            if let SenderKind::Http2(handle) = &pooled.sender.kind {
                pooled.idle_since = now;
                Sender {
                    kind: SenderKind::Http2(handle.clone()),
                    addr: pooled.sender.addr.clone(),
                    pool_key: pooled.sender.pool_key.clone(),
                    h2_slots: pooled.sender.h2_slots.clone(),
                    stream_permit: None,
                }
            } else {
                bucket.swap_remove(index).sender
            }
        };
        if let Some(slots) = &sender.h2_slots {
            sender.stream_permit = Some(slots.clone().acquire_owned().await.ok()?);
            if sender.is_closed() {
                return None;
            }
        }
        Some(sender)
    }

    /// Publish a fresh H2 connection before its first response finishes.
    fn share_h2(&self, key: PoolKey, sender: &Sender) {
        if let SenderKind::Http2(handle) = &sender.kind {
            self.checkin(
                key,
                Sender {
                    kind: SenderKind::Http2(handle.clone()),
                    addr: sender.addr.clone(),
                    pool_key: sender.pool_key.clone(),
                    h2_slots: sender.h2_slots.clone(),
                    stream_permit: None,
                },
            );
        }
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
    fn checkin(&self, key: PoolKey, mut sender: Sender) {
        sender.pool_key = Some(key.clone());
        if sender.is_closed() {
            return;
        }
        let mut conns = self.conns.lock();
        let now = Instant::now();
        conns.retain(|_, bucket| {
            bucket.retain(|pooled| {
                !pooled.sender.is_closed()
                    && (pooled
                        .sender
                        .h2_slots
                        .as_ref()
                        .is_some_and(|s| s.available_permits() < MAX_H2_STREAMS)
                        || now.saturating_duration_since(pooled.idle_since) <= IDLE_TIMEOUT)
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
    /// The peer address this connection was dialed to. Populated both for a
    /// freshly dialed connection and for a pooled reuse - a pooled hit
    /// reports the same address captured at the connection's original dial
    /// (see [`Sender::addr`]).
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
            mirror_h2: is_https && mirror_h2,
            upstream_proxy: upstream_proxy.map(str::to_owned),
        };

        if let Some(sender) = self.pool.checkout(&key).await {
            let version = sender.version();
            let server_addr = sender.addr().to_string();
            return Ok(Obtained {
                sender,
                version,
                connect_ms: 0.0,
                ssl_ms: -1.0,
                server_addr: Some(server_addr),
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
            let (mut sender, version) = handshake(io, negotiated_h2, server_addr.clone()).await?;
            sender.pool_key = Some(key.clone());
            self.pool.share_h2(key, &sender);
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
            let (mut sender, version) = handshake(io, false, server_addr.clone()).await?;
            sender.pool_key = Some(key);
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
    pub fn release(&self, _scheme: &str, _host: &str, _port: u16, mut sender: Sender) {
        if sender.version() == HttpVersion::Http2 {
            // The shared pool handle already exists. Dropping the lease releases
            // stream capacity, including when the response was cancelled.
            if let Some(key) = &sender.pool_key {
                if let Some(bucket) = self.pool.conns.lock().get_mut(key) {
                    for pooled in bucket {
                        if pooled
                            .sender
                            .h2_slots
                            .as_ref()
                            .zip(sender.h2_slots.as_ref())
                            .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
                        {
                            pooled.idle_since = Instant::now();
                        }
                    }
                }
            }
            return;
        }
        if let Some(key) = sender.pool_key.take() {
            self.pool.checkin(key, sender);
        }
    }
}

/// Performs the HTTP/1.1 or HTTP/2 client handshake over an already
/// connected (and, for HTTPS, already TLS-terminated) stream, spawning a
/// background task to drive the connection.
async fn handshake<T>(io: T, want_h2: bool, addr: String) -> Result<(Sender, HttpVersion)>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    if want_h2 {
        let slots = Arc::new(Semaphore::new(MAX_H2_STREAMS));
        let permit = slots
            .clone()
            .try_acquire_owned()
            .expect("fresh stream budget");
        let (sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
            .await
            .map_err(|e| ProxyError::UpstreamConnect(format!("http2 handshake failed: {e}")))?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(%err, "upstream h2 connection task ended");
            }
        });
        Ok((
            Sender {
                kind: SenderKind::Http2(sender),
                addr,
                pool_key: None,
                h2_slots: Some(slots),
                stream_permit: Some(permit),
            },
            HttpVersion::Http2,
        ))
    } else {
        let (sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| ProxyError::UpstreamConnect(format!("http1 handshake failed: {e}")))?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                tracing::debug!(%err, "upstream h1 connection task ended");
            }
        });
        Ok((
            Sender {
                kind: SenderKind::Http1(sender),
                addr,
                pool_key: None,
                h2_slots: None,
                stream_permit: None,
            },
            HttpVersion::Http1,
        ))
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
/// is simply dropped instead of being pooled. For HTTP/2 this releases only
/// the stream permit; the shared connection remains reusable.
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
    ) -> Self
    where
        B: Body,
    {
        let complete = inner.is_end_stream();
        let mut wrapped = ReleaseOnComplete {
            inner,
            release: Some((connector, https, host, port, sender)),
        };
        // HEAD/204/empty bodies can be discarded without ever being polled.
        if complete {
            wrapped.finish();
        }
        wrapped
    }

    fn finish(&mut self) {
        if let Some((connector, https, host, port, sender)) = self.release.take() {
            let scheme = if https { "https" } else { "http" };
            connector.release(scheme, &host, port, sender);
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
        if matches!(&poll, Poll::Ready(None))
            || (matches!(&poll, Poll::Ready(Some(Ok(_)))) && this.inner.is_end_stream())
        {
            this.finish();
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

    use http_body_util::{BodyExt, Full};
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn origin(
        h2: bool,
    ) -> (
        Connector,
        u16,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = cert.cert.der().clone();
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![der.clone()],
                rustls_pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
            )
            .unwrap();
        config.alpn_protocols = vec![if h2 {
            b"h2".to_vec()
        } else {
            b"http/1.1".to_vec()
        }];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let count = connections.clone();
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let tls = acceptor.accept(tcp).await.unwrap();
                    let service = hyper::service::service_fn(|_: Request<Incoming>| async {
                        Ok::<_, std::convert::Infallible>(Response::new(Full::new(
                            Bytes::from_static(b"hello"),
                        )))
                    });
                    if h2 {
                        let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                            .serve_connection(TokioIo::new(tls), service)
                            .await;
                    } else {
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tls), service)
                            .await;
                    }
                });
            }
        });
        (
            Connector::with_extra_roots(&[der]).unwrap(),
            port,
            connections,
            task,
        )
    }

    fn request(port: u16) -> Request<BoxBody> {
        Request::builder()
            .uri(format!("https://localhost:{port}/"))
            .body(
                Full::new(Bytes::new())
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .unwrap()
    }

    #[tokio::test]
    async fn alpn_fallback_reuses_h1_for_h2_preference() {
        let (connector, port, count, task) = origin(false).await;
        for _ in 0..3 {
            let mut obtained = connector
                .obtain("https", "localhost", port, true, None)
                .await
                .unwrap();
            assert_eq!(obtained.version, HttpVersion::Http1);
            obtained
                .sender
                .send_request(request(port))
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap();
            connector.release("https", "localhost", port, obtained.sender);
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // A different route must not silently reuse the direct connection.
        assert!(connector
            .obtain("https", "localhost", port, true, Some("invalid proxy URL"))
            .await
            .is_err());
        task.abort();
    }

    #[tokio::test]
    async fn h2_shares_connection_before_previous_response_is_released() {
        let (connector, port, count, task) = origin(true).await;
        let mut first = connector
            .obtain("https", "localhost", port, true, None)
            .await
            .unwrap();
        let response = first.sender.send_request(request(port)).await.unwrap();
        // Keep both first response and first lease alive while a second stream runs.
        let mut second = connector
            .obtain("https", "localhost", port, true, None)
            .await
            .unwrap();
        assert_eq!(second.version, HttpVersion::Http2);
        let body = second
            .sender
            .send_request(request(port))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap();
        assert_eq!(body.to_bytes(), Bytes::from_static(b"hello"));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(response);
        drop(first);
        connector.release("https", "localhost", port, second.sender);
        task.abort();
    }

    #[tokio::test]
    async fn h2_stream_budget_waits_and_recovers_after_cancellation() {
        let (connector, port, count, task) = origin(true).await;
        let mut leases = Vec::new();
        for _ in 0..MAX_H2_STREAMS {
            leases.push(
                connector
                    .obtain("https", "localhost", port, true, None)
                    .await
                    .unwrap(),
            );
        }
        assert!(tokio::time::timeout(
            Duration::from_millis(30),
            connector.obtain("https", "localhost", port, true, None)
        )
        .await
        .is_err());
        leases.pop();
        let recovered = tokio::time::timeout(
            Duration::from_secs(1),
            connector.obtain("https", "localhost", port, true, None),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(recovered.connect_ms, 0.0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }

    #[tokio::test]
    async fn h2_selects_available_connection_before_saturated_one() {
        let (connector, port, _, task) = origin(true).await;
        let first = connector
            .obtain("https", "localhost", port, true, None)
            .await
            .unwrap();
        let key = first.sender.pool_key.clone().unwrap();
        // Temporarily remove the first handle to create a second real connection.
        let first_bucket = connector.pool.conns.lock().remove(&key).unwrap();
        let mut leases = Vec::new();
        for _ in 0..MAX_H2_STREAMS {
            leases.push(
                connector
                    .obtain("https", "localhost", port, true, None)
                    .await
                    .unwrap(),
            );
        }
        {
            let mut conns = connector.pool.conns.lock();
            let saturated = conns.remove(&key).unwrap();
            let mut combined = first_bucket;
            combined.extend(saturated);
            conns.insert(key, combined);
        }
        let next = tokio::time::timeout(
            Duration::from_secs(1),
            connector.obtain("https", "localhost", port, true, None),
        )
        .await
        .expect("must use available older connection")
        .unwrap();
        assert!(Arc::ptr_eq(
            next.sender.h2_slots.as_ref().unwrap(),
            first.sender.h2_slots.as_ref().unwrap()
        ));
        task.abort();
    }

    #[tokio::test]
    async fn empty_response_returns_h1_without_body_poll() {
        let (connector, port, count, task) = origin(false).await;
        let connector = Arc::new(connector);
        let mut first = connector
            .obtain("https", "localhost", port, false, None)
            .await
            .unwrap();
        let mut head = request(port);
        *head.method_mut() = hyper::Method::HEAD;
        let response = first.sender.send_request(head).await.unwrap();
        assert!(response.body().is_end_stream());
        let wrapped = ReleaseOnComplete::new(
            response.into_body(),
            connector.clone(),
            true,
            "localhost".into(),
            port,
            first.sender,
        );
        drop(wrapped); // hyper need not poll an already-ended body.
        let mut second = connector
            .obtain("https", "localhost", port, false, None)
            .await
            .unwrap();
        second
            .sender
            .send_request(request(port))
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        task.abort();
    }

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
