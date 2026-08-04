//! `hamsy-proxy`: a from-scratch MITM HTTP(S) debugging proxy engine.
//!
//! Built directly on `tokio`/`hyper`/`rustls` (not on an all-in-one proxy
//! framework), this crate implements the network side of hamsy-proxy: accepting
//! client connections, terminating and re-originating TLS for MITM'd HTTPS
//! traffic, applying `hamsy-core` rules to requests/responses, recording
//! [`hamsy_core::Flow`]s, and relaying WebSocket traffic.
//!
//! Module map:
//! - [`ca`]: certificate authority (root CA load/generate, leaf minting).
//! - [`config`]: [`ProxyContext`], the shared handle passed through the
//!   whole pipeline.
//! - [`server`]: the top-level listener/accept loop.
//! - [`connect`]: `CONNECT` handling (tunnel vs. MITM, TLS ClientHello peek).
//! - [`http`]: the request/response pipeline (rule application, flow
//!   recording, upstream dispatch).
//! - [`upstream`]: the outbound connector and connection pool.
//! - [`tee`]: capped body-capture body wrappers and a token-bucket throttle.
//! - [`websocket`]: WebSocket interception and message recording.
//! - [`replay`]: replaying a captured/edited request through the pipeline.
//! - [`appid`]: resolving a client connection to its originating macOS app.
//! - [`error`]: [`ProxyError`], this crate's error type.

pub mod appid;
pub mod ca;
pub mod config;
pub mod connect;
pub mod error;
pub mod http;
pub mod replay;
pub mod server;
pub mod tee;
pub mod upstream;
pub mod websocket;

pub use ca::CertAuthority;
pub use config::ProxyContext;
pub use error::{ProxyError, Result};
pub use server::ProxyServer;

/// A type-erased HTTP body used throughout the pipeline, since a single
/// logical body may pass through several concrete wrapper types over its
/// lifetime (hyper's [`hyper::body::Incoming`], [`http_body_util::Full`],
/// [`tee::TeeBody`], [`tee::Throttled`], ...).
pub type BoxBody = http_body_util::combinators::BoxBody<bytes::Bytes, ProxyError>;

/// Boxes any compatible body into a [`BoxBody`], converting its error type
/// via [`ProxyError`]'s `From` impls.
pub fn box_body<B>(body: B) -> BoxBody
where
    B: http_body::Body<Data = bytes::Bytes> + Send + Sync + 'static,
    ProxyError: From<B::Error>,
{
    use http_body_util::BodyExt;
    body.map_err(ProxyError::from).boxed()
}

/// Returns an empty [`BoxBody`], for responses/requests with no body.
pub fn empty_body() -> BoxBody {
    use http_body_util::BodyExt;
    http_body_util::Empty::new()
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}

/// Returns a [`BoxBody`] containing exactly `bytes`.
pub fn full_body(bytes: impl Into<bytes::Bytes>) -> BoxBody {
    use http_body_util::BodyExt;
    http_body_util::Full::new(bytes.into())
        .map_err(|never: std::convert::Infallible| match never {})
        .boxed()
}
