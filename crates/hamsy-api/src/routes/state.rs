//! `GET /api/state`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::sysproxy;
use crate::ApiState;

/// Builds the `GET /api/state` response body. Shared with `routes::ws`,
/// which sends the same shape as the initial `state` event on connect.
///
/// `sysproxy::status_for` shells out to a platform command
/// (`networksetup`/`reg`/`gsettings`) synchronously, so it runs via
/// `spawn_blocking` rather than stalling the calling async task -- this
/// function is on the hot path for every `GET /api/state` poll and every WS
/// connect.
///
/// `status_for` (not `status`) is used deliberately: `systemProxy.enabled`
/// must answer "is the OS proxy pointed at *this* hamsy instance", not
/// "is some OS proxy on at all". Another application (or the user, by
/// hand) can hold the OS proxy just as well, and reporting that as `true`
/// here would both mislabel someone else's proxy as hamsy's in the UI and
/// make the toggle-off control disable it out from under them.
pub async fn state_snapshot(state: &ApiState) -> Value {
    let settings = state.settings();
    let proxy_port = settings.proxy_port;
    let system_proxy_enabled = if state.viewer_only() {
        false
    } else {
        tokio::task::spawn_blocking(move || sysproxy::status_for("127.0.0.1", proxy_port))
            .await
            .unwrap_or(Ok(false))
            .unwrap_or(false)
    };

    json!({
        "version": state.version(),
        "viewerOnly": state.viewer_only(),
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
            "supported": !state.viewer_only() && sysproxy::supported(),
        },
    })
}

/// Returns a snapshot of overall server state: version, ports, capture
/// status, flow count, CA fingerprint, uptime, and system-proxy status.
pub async fn get_state(State(state): State<ApiState>) -> Json<Value> {
    Json(state_snapshot(&state).await)
}
