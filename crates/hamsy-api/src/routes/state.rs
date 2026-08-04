//! `GET /api/state`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::sysproxy;
use crate::ApiState;

/// Builds the `GET /api/state` response body. Shared with `routes::ws`,
/// which sends the same shape as the initial `state` event on connect.
///
/// `sysproxy::status()` shells out to a platform command
/// (`networksetup`/`reg`/`gsettings`) synchronously, so it runs via
/// `spawn_blocking` rather than stalling the calling async task -- this
/// function is on the hot path for every `GET /api/state` poll and every WS
/// connect.
pub async fn state_snapshot(state: &ApiState) -> Value {
    let settings = state.settings();
    let system_proxy_enabled = tokio::task::spawn_blocking(sysproxy::status)
        .await
        .unwrap_or(Ok(false))
        .unwrap_or(false);

    json!({
        "version": state.version(),
        "proxyPort": settings.proxy_port,
        "uiPort": settings.ui_port,
        "capturing": !settings.paused,
        "paused": settings.paused,
        "flowCount": state.flows().len(),
        "caFingerprint": state.cert_hook().fingerprint(),
        "uptimeSecs": state.uptime_secs(),
        "systemProxy": {
            "enabled": system_proxy_enabled,
            "platform": sysproxy::platform(),
            "supported": sysproxy::supported(),
        },
    })
}

/// Returns a snapshot of overall server state: version, ports, capture
/// status, flow count, CA fingerprint, uptime, and system-proxy status.
pub async fn get_state(State(state): State<ApiState>) -> Json<Value> {
    Json(state_snapshot(&state).await)
}
