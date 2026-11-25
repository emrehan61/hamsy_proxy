//! `/api/settings` and `/api/system-proxy` routes.

use axum::extract::State;
use axum::Json;
use hamsy_core::{ServerEvent, Settings};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::sysproxy_state;
use crate::ApiState;

/// `GET /api/settings`.
pub async fn get_settings(State(state): State<ApiState>) -> Json<Settings> {
    Json(state.settings())
}

/// `PUT /api/settings` — accepts a *partial* JSON object, merges it
/// field-by-field over the current [`Settings`], persists, applies
/// `maxFlows` to the flow store immediately, and broadcasts
/// [`ServerEvent::SettingsChanged`].
///
/// The response is always the merged `Settings` plus a `restartRequired`
/// boolean (true iff `proxyPort`, `uiPort`, or `bindAddr` changed), so the
/// response shape is stable regardless of whether a restart is actually
/// needed.
pub async fn put_settings(
    State(state): State<ApiState>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Value::Object(patch_obj) = patch else {
        return Err(ApiError::BadRequest(
            "settings body must be a JSON object".to_string(),
        ));
    };

    let previous = state.settings();
    let mut merged_value = serde_json::to_value(&previous)?;
    if let Value::Object(base) = &mut merged_value {
        for (key, value) in patch_obj {
            base.insert(key, value);
        }
    }
    let merged: Settings = serde_json::from_value(merged_value)?;

    state.save_settings(merged.clone())?;

    let restart_required = merged.proxy_port != previous.proxy_port
        || merged.ui_port != previous.ui_port
        || merged.bind_addr != previous.bind_addr;

    state.broadcast(ServerEvent::SettingsChanged {
        settings: merged.clone(),
    });

    let mut response = serde_json::to_value(&merged)?;
    if let Value::Object(obj) = &mut response {
        obj.insert("restartRequired".to_string(), Value::Bool(restart_required));
    }
    Ok(Json(response))
}

/// Body of `POST /api/system-proxy`.
#[derive(Debug, Deserialize)]
pub struct SystemProxyBody {
    enabled: bool,
}

/// Builds the `{"enabled": ...}` response body for `POST /api/system-proxy`,
/// echoing back the value that was just (successfully) applied.
fn system_proxy_response(enabled: bool) -> Value {
    json!({ "enabled": enabled })
}

/// `POST /api/system-proxy` — enables or disables the OS system proxy,
/// pointing it at this instance's proxy port on `127.0.0.1`. Responds with
/// `200 {"enabled": <bool>}` on success (matching `ui/src/lib/api.ts`'s
/// `setSystemProxy` return type), not a bare `204` — the caller needs the
/// applied value back to update its own UI state.
pub async fn system_proxy(
    State(state): State<ApiState>,
    Json(body): Json<SystemProxyBody>,
) -> Result<Json<Value>, ApiError> {
    if state.viewer_only() {
        return Err(ApiError::NotImplemented(
            "HAR viewer cannot control the system proxy".into(),
        ));
    }
    if body.enabled {
        let settings = state.settings();
        sysproxy_state::acquire(
            state.data_dir(),
            "127.0.0.1",
            settings.proxy_port,
            &settings.system_proxy_bypass,
        )
        .map_err(ApiError::BadGateway)?;
    } else {
        sysproxy_state::release(state.data_dir()).map_err(ApiError::BadGateway)?;
    }
    Ok(Json(system_proxy_response(body.enabled)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract check for `POST /api/system-proxy`'s response shape. This is
    /// a narrow unit test on the pure body-building function rather than a
    /// full router/integration test that calls the real handler, because
    /// `system_proxy` shells out to genuine OS commands (`networksetup` /
    /// `reg` / `gsettings`) that would mutate whatever machine runs the test
    /// suite — not something `cargo test` should ever do as a side effect.
    #[test]
    fn system_proxy_response_shape() {
        let enabled_body = system_proxy_response(true);
        let obj = enabled_body
            .as_object()
            .expect("must serialize to a JSON object");
        assert_eq!(obj.keys().collect::<Vec<_>>(), vec!["enabled"]);
        assert_eq!(enabled_body["enabled"], true);

        let disabled_body = system_proxy_response(false);
        assert_eq!(disabled_body["enabled"], false);
    }
}
