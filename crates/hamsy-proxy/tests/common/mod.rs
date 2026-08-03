//! Shared integration-test harness: throwaway HTTP/HTTPS origin servers and
//! a fully wired-up `ProxyServer` with an isolated, tempdir-backed context.
#![allow(dead_code)]

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use parking_lot::RwLock;
use rustls_pki_types::PrivatePkcs8KeyDer;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use flproxy_core::{Action, FlowStore, Matcher, Rule, RulesStore, Settings};
use flproxy_proxy::upstream::Connector;
use flproxy_proxy::{CertAuthority, ProxyContext, ProxyServer};

/// The body type origin-server test handlers return.
pub type OriginBody = BoxBody<Bytes, Infallible>;

/// Wraps `bytes` into an [`OriginBody`].
pub fn full(bytes: impl Into<Bytes>) -> OriginBody {
    Full::new(bytes.into())
        .map_err(|never: Infallible| match never {})
        .boxed()
}

/// Spawns a plain-HTTP origin server driven by `handler`, returning its
/// bound address. Runs for the remainder of the test process.
pub async fn spawn_http_origin<F, Fut>(handler: F) -> SocketAddr
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<OriginBody>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind origin");
    let addr = listener.local_addr().expect("origin addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| {
                    let handler = handler.clone();
                    async move { Ok::<_, Infallible>(handler(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });
    addr
}

/// Spawns a TLS origin server (self-signed cert for `localhost`), returning
/// its address plus the certificate's DER bytes - handed to a test client
/// that should trust the origin directly (used by the passthrough test).
pub async fn spawn_tls_origin<F, Fut>(handler: F) -> (SocketAddr, Vec<u8>)
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<OriginBody>> + Send + 'static,
{
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("self-signed cert");
    let cert_der = certified.cert.der().to_vec();
    let key_der: PrivatePkcs8KeyDer<'static> =
        PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key_der.into())
        .expect("tls server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tls origin");
    let addr = listener.local_addr().expect("tls origin addr");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let Ok(tls_stream) = acceptor.accept(stream).await else {
                    return;
                };
                let io = TokioIo::new(tls_stream);
                let service = service_fn(move |req| {
                    let handler = handler.clone();
                    async move { Ok::<_, Infallible>(handler(req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            });
        }
    });
    (addr, cert_der)
}

/// A running proxy instance plus its shared context, for tests to poke at
/// (insert rules, inspect recorded flows, ...). Aborts its server task on
/// drop so tests don't leak background tasks.
pub struct TestProxy {
    pub addr: SocketAddr,
    pub ctx: ProxyContext,
    join: JoinHandle<()>,
    _tempdir: tempfile::TempDir,
}

impl Drop for TestProxy {
    fn drop(&mut self) {
        self.join.abort();
    }
}

/// Spins up a full `ProxyServer` with a fresh, isolated `ProxyContext`
/// (tempdir-backed CA/rules, a fresh `FlowStore`) for one test.
pub async fn spawn_proxy(settings: Settings) -> TestProxy {
    spawn_proxy_trusting(settings, &[]).await
}

/// Like [`spawn_proxy`], but the proxy's *outbound* (upstream) TLS
/// connector additionally trusts `extra_roots` - needed when a test spins
/// up a throwaway TLS origin server signed by a one-off self-signed cert
/// that isn't in any real trust store.
pub async fn spawn_proxy_trusting(settings: Settings, extra_roots: &[Vec<u8>]) -> TestProxy {
    let dir = tempfile::tempdir().expect("tempdir");
    let ca = Arc::new(CertAuthority::load_or_generate(dir.path()).expect("ca"));
    let rules = Arc::new(RulesStore::load(&dir.path().join("rules.json")));
    let flows = Arc::new(FlowStore::new(1000));
    let (events, _rx) = tokio::sync::broadcast::channel(256);
    let roots: Vec<rustls_pki_types::CertificateDer<'static>> = extra_roots
        .iter()
        .map(|der| rustls_pki_types::CertificateDer::from(der.clone()))
        .collect();
    let upstream = Arc::new(Connector::with_extra_roots(&roots).expect("connector"));

    let ctx = ProxyContext {
        settings: Arc::new(RwLock::new(settings)),
        rules,
        flows,
        events,
        ca,
        upstream,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let server_ctx = ctx.clone();
    let join = tokio::spawn(async move {
        let server = ProxyServer::new(server_ctx);
        let _ = server.serve(listener, std::future::pending()).await;
    });

    TestProxy {
        addr,
        ctx,
        join,
        _tempdir: dir,
    }
}

/// A `reqwest::ClientBuilder` pre-configured to route all traffic through
/// `proxy` and trust `proxy`'s MITM root CA. Callers can chain further
/// options (e.g. `.no_gzip()`) before `.build()`.
pub fn client_builder(proxy: &TestProxy) -> reqwest::ClientBuilder {
    let ca_der = proxy.ctx.ca.der();
    reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr)).expect("proxy url"))
        .add_root_certificate(reqwest::Certificate::from_der(&ca_der).expect("ca cert"))
}

/// A ready-to-use client trusting `proxy`'s MITM root CA.
pub fn client_trusting_proxy_ca(proxy: &TestProxy) -> reqwest::Client {
    client_builder(proxy).build().expect("client")
}

/// Polls `proxy`'s flow store (up to ~2s) until a flow reaches a terminal
/// state (`Complete`/`Error`), since some flows (raw tunnels in particular)
/// only finalize once the underlying connection actually closes, which can
/// race a test's assertions immediately after its HTTP response completes.
pub async fn wait_for_terminal_flow(proxy: &TestProxy) -> flproxy_core::FlowSummary {
    for _ in 0..40 {
        let flows = proxy.ctx.flows.list(&Default::default());
        if let Some(flow) = flows.iter().find(|f| {
            matches!(
                f.state,
                flproxy_core::FlowState::Complete | flproxy_core::FlowState::Error
            )
        }) {
            return flow.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no flow reached a terminal state in time");
}

/// Builds a simple always-matching (or custom-matcher) [`Rule`] with the
/// given `actions`, for tests that just want "this action always applies".
pub fn simple_rule(id: &str, matcher: Matcher, actions: Vec<Action>) -> Rule {
    Rule {
        id: id.to_string(),
        name: id.to_string(),
        enabled: true,
        priority: 0,
        group: None,
        notes: None,
        matcher,
        actions,
    }
}
