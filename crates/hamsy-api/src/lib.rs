//! REST + WebSocket API and web UI asset server for hamsy-proxy.
//!
//! This crate is self-contained: it depends only on `hamsy-core` for the
//! shared data model, and defines its own small [`hooks::ReplayHook`] /
//! [`hooks::CertHook`] traits as extension points for a proxy backend and
//! certificate authority, rather than depending on those crates directly.
//! Build an [`ApiState`], call [`router`] (or [`serve`]) to get a runnable
//! `axum` application.

pub mod assets;
pub mod error;
pub mod hooks;
pub mod qr;
pub mod routes;
pub mod state;
pub mod sysproxy;
pub mod sysproxy_state;

use std::net::SocketAddr;

use axum::extract::DefaultBodyLimit;
use axum::http::HeaderValue;
use axum::routing::{get, post, put};
use axum::Router;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

pub use crate::error::ApiError;
pub use crate::hooks::{CertHook, NoopReplay, ReplayHook, StubCert};
pub use crate::routes::setup::lan_addresses;
pub use crate::state::ApiState;

/// Request bodies larger than this are rejected. Applied globally rather
/// than per-route for simplicity; `POST /api/har/import` is the only
/// endpoint expected to receive large bodies.
const MAX_BODY_BYTES: usize = 512 * 1024 * 1024;

/// Builds the full `axum` application: every REST/WebSocket route under
/// `/api`, the certificate routes under `/cert`, and a `GET /*` fallback
/// serving the web UI (see [`assets`]).
pub fn router(state: ApiState) -> Router {
    let api_routes = Router::new()
        .route("/state", get(routes::state::get_state))
        .route(
            "/flows",
            get(routes::flows::list).delete(routes::flows::clear),
        )
        .route("/flows/{id}", get(routes::flows::get))
        .route("/flows/{id}/replay", post(routes::flows::replay))
        .route(
            "/rules",
            get(routes::rules::list).post(routes::rules::create),
        )
        .route(
            "/rules/{id}",
            put(routes::rules::update).delete(routes::rules::delete),
        )
        .route("/rules/{id}/toggle", post(routes::rules::toggle))
        .route("/rules/reorder", post(routes::rules::reorder))
        .route("/rules/import", post(routes::rules::import))
        .route("/rules/export", get(routes::rules::export))
        .route(
            "/settings",
            get(routes::settings::get_settings).put(routes::settings::put_settings),
        )
        .route("/system-proxy", post(routes::settings::system_proxy))
        .route("/setup", get(routes::setup::setup))
        .route("/har", get(routes::har::export))
        .route("/har/import", post(routes::har::import))
        .route("/ws", get(routes::ws::ws_handler))
        .fallback(api_not_found);

    let cert_routes = Router::new()
        .route("/hamsy-ca.pem", get(routes::setup::cert_pem))
        .route("/hamsy-ca.crt", get(routes::setup::cert_crt))
        .route("/hamsy-ca.der", get(routes::setup::cert_der))
        .fallback(api_not_found);

    // Two explicit dev-server origins (Vite's default port), plus a
    // permissive `Any` for methods/headers. Not combined with
    // `allow_credentials(true)`: this API uses no cookies/auth, so a
    // wide-open CORS policy carries no meaningful risk, and mixing `Any`
    // origins with credentials is rejected by both the CORS spec and
    // `tower-http` at runtime.
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            HeaderValue::from_static("http://localhost:5173"),
            HeaderValue::from_static("http://127.0.0.1:5173"),
        ]))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    Router::new()
        .nest("/api", api_routes)
        .nest("/cert", cert_routes)
        .fallback(assets::asset_handler)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(CompressionLayer::new().gzip(true))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Fallback for unmatched `/api/*` and `/cert/*` paths: a JSON 404
/// [`ApiError`], rather than falling through to the UI's SPA fallback.
async fn api_not_found() -> ApiError {
    ApiError::NotFound("no such API route".to_string())
}

/// Binds `addr` and serves [`router`] until `shutdown` resolves.
pub async fn serve(
    state: ApiState,
    addr: SocketAddr,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}
