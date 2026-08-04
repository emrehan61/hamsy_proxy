//! The top-level listener: accepts client connections and serves each one,
//! routing individual requests to `CONNECT` handling, WebSocket upgrade
//! handling, or the plain HTTP pipeline as appropriate.

use std::future::Future;
use std::time::Duration;

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::config::ProxyContext;
use crate::error::Result;
use crate::http::{self, ConnInfo};
use crate::{connect, websocket, BoxBody};

/// Bounded time given to already-accepted connections to finish naturally
/// once shutdown begins, before they're forcibly aborted.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Backoff applied after an `accept()` failure that looks like file
/// descriptor exhaustion (`EMFILE`/`ENFILE`), so a hot loop doesn't spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The top-level proxy listener.
pub struct ProxyServer {
    ctx: ProxyContext,
}

impl ProxyServer {
    /// Creates a new server bound to the given shared [`ProxyContext`].
    pub fn new(ctx: ProxyContext) -> Self {
        ProxyServer { ctx }
    }

    /// Runs the accept loop on `listener` until `shutdown` resolves, then
    /// stops accepting new connections and waits (with a bounded timeout)
    /// for already-accepted connections to finish on their own.
    pub async fn serve(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()>,
    ) -> Result<()> {
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    tracing::debug!("proxy listener stopping: shutdown requested");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer_addr)) => {
                            let _ = stream.set_nodelay(true);
                            let ctx = self.ctx.clone();
                            tasks.spawn(async move {
                                // Resolved once per accepted connection, here
                                // in the per-connection task rather than the
                                // accept loop, so a slow/rate-limited scan
                                // (see `crate::appid`) never blocks accepting
                                // the next connection.
                                let app = crate::appid::resolve_client_app(peer_addr).await;
                                let io = TokioIo::new(stream);
                                let conn_info = ConnInfo {
                                    client_addr: peer_addr,
                                    scheme: "http",
                                    authority: None,
                                    tls: None,
                                    mirror_h2: false,
                                    app,
                                };
                                serve_h1(ctx, io, conn_info).await;
                            });
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "accept() failed");
                            if is_resource_exhausted(&err) {
                                tokio::time::sleep(ACCEPT_BACKOFF).await;
                            }
                        }
                    }
                }
            }
        }

        let drain = async { while tasks.join_next().await.is_some() {} };
        if tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, drain)
            .await
            .is_err()
        {
            tracing::warn!("shutdown drain timed out; aborting remaining connections");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        Ok(())
    }
}

/// True if `err` looks like `EMFILE`/`ENFILE` (file descriptor exhaustion),
/// in which case the accept loop should briefly back off rather than
/// spin-loop retrying `accept()` immediately.
fn is_resource_exhausted(err: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(err.raw_os_error(), Some(23) | Some(24)) // ENFILE, EMFILE
    }
    #[cfg(not(unix))]
    {
        let _ = err;
        false
    }
}

/// Routes one request within a connection: `CONNECT`, WebSocket upgrade, or
/// the plain HTTP pipeline.
async fn route(
    ctx: ProxyContext,
    req: Request<Incoming>,
    conn_info: ConnInfo,
) -> Response<BoxBody> {
    if req.method() == Method::CONNECT {
        return connect::handle_connect(ctx, req, conn_info.client_addr, conn_info.app.clone())
            .await;
    }
    if websocket::is_websocket_upgrade(&req) {
        return websocket::handle_upgrade(ctx, req, conn_info).await;
    }
    match http::handle_proxy_request(ctx, req, conn_info).await {
        Ok(resp) => resp,
        Err(err) => http::error_response(StatusCode::BAD_GATEWAY, &err.to_string()),
    }
}

/// Builds a fresh per-connection `hyper` [`Service`](hyper::service::Service)
/// closure over `ctx`/`conn_info`.
fn build_service(
    ctx: ProxyContext,
    conn_info: ConnInfo,
) -> impl hyper::service::Service<
    Request<Incoming>,
    Response = Response<BoxBody>,
    Error = std::convert::Infallible,
    Future = impl Future<Output = std::result::Result<Response<BoxBody>, std::convert::Infallible>>,
> {
    hyper::service::service_fn(move |req: Request<Incoming>| {
        let ctx = ctx.clone();
        let conn_info = conn_info.clone();
        async move { Ok(route(ctx, req, conn_info).await) }
    })
}

/// Serves one HTTP/1.1 connection (with upgrade support, for `CONNECT` and
/// WebSocket), dispatching each request via [`route`].
pub(crate) async fn serve_h1<IO>(ctx: ProxyContext, io: TokioIo<IO>, conn_info: ConnInfo)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = build_service(ctx, conn_info);
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.preserve_header_case(true);
    builder.title_case_headers(false);
    if let Err(err) = builder.serve_connection(io, service).with_upgrades().await {
        tracing::debug!(%err, "http1 connection ended");
    }
}

/// Serves one HTTP/2 connection (always over an already TLS-terminated,
/// ALPN-negotiated stream; hamsy-proxy never speaks cleartext h2c).
pub(crate) async fn serve_h2<IO>(ctx: ProxyContext, io: TokioIo<IO>, conn_info: ConnInfo)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = build_service(ctx, conn_info);
    let builder = hyper::server::conn::http2::Builder::new(TokioExecutor::new());
    if let Err(err) = builder.serve_connection(io, service).await {
        tracing::debug!(%err, "http2 connection ended");
    }
}
