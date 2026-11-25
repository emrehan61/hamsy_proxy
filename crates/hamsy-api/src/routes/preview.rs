//! Evaluate a proposed rule against retained traffic without applying it.
use axum::{extract::State, http::StatusCode, Json};
use hamsy_core::Rule;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{ApiError, ApiState};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewRequest {
    flow_id: Uuid,
    rule: Rule,
}

pub async fn preview(
    State(state): State<ApiState>,
    Json(request): Json<PreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    if state.viewer_only() {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "live_session_required",
            "Rule preview requires live capture",
            false,
        ));
    }
    let slot = SLOTS.try_acquire().map_err(|_| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Rule preview is busy; wait for the current preview to finish",
            true,
        )
    })?;
    let flow = state
        .flows()
        .snapshots(Some(&[request.flow_id]))
        .pop()
        .ok_or_else(|| ApiError::NotFound("Flow is no longer retained".into()))?;
    let value = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        hamsy_core::rule_preview::preview_rule(&flow, request.rule)
    })
    .await
    .map_err(|_| ApiError::Internal("Rule preview failed".into()))?
    .map_err(|message| ApiError::coded(StatusCode::BAD_REQUEST, "invalid_rule", message, false))?;
    Ok(Json(value))
}
