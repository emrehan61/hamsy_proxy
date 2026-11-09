//! Bounded observation of live capture mutations, independent of flow sequence.
use std::{collections::HashMap, time::Duration};

use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use hamsy_core::store::ChangeCursorError;
use serde_json::{json, Value};

use crate::{ApiError, ApiState};

fn number(
    query: &HashMap<String, String>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, ApiError> {
    let Some(raw) = query.get(key) else {
        return Ok(default);
    };
    raw.parse::<u64>()
        .ok()
        .filter(|value| (min..=max).contains(value))
        .ok_or_else(|| ApiError::BadRequest(format!("{key} must be between {min} and {max}")))
}

pub async fn get(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    if state.viewer_only() {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            "live_session_required",
            "Change observation is available only for live capture",
            false,
        ));
    }
    let limit = number(&query, "limit", 50, 1, 200)? as usize;
    let wait_ms = number(&query, "waitMs", 0, 0, 10_000)?;
    let mut cursor = query.get("cursor").cloned();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let page = state.flows().changes(cursor.as_deref(), limit).map_err(|error| match error {
            ChangeCursorError::Invalid => ApiError::BadRequest("Invalid change cursor; use a cursor returned by this capture".into()),
            ChangeCursorError::Stale { reason, capture_id, cursor } => ApiError::coded(
                StatusCode::CONFLICT, "stale_cursor", "Capture changed or observation history expired; establish a new baseline", false,
            ).with_details(json!({"resetRequired":true,"reason":reason,"captureId":capture_id,"cursor":cursor})),
        })?;
        let expired = tokio::time::Instant::now() >= deadline;
        if !page.changes.is_empty() || expired {
            let timed_out = wait_ms > 0 && page.changes.is_empty();
            let mut value = serde_json::to_value(page).expect("serializable change metadata");
            value["timedOut"] = json!(timed_out);
            return Ok(Json(value));
        }
        // The core stays runtime-independent. Short asynchronous sleeps bound
        // server work while one client request waits, and drop immediately on
        // request cancellation. Retain the first baseline to detect clears.
        cursor = Some(page.cursor);
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(100)),
        )
        .await;
    }
}
