//! `/api/har*` routes: HAR export and import.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hamsy_core::{export_har, import_har, ServerEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::ApiState;

/// Query parameters accepted by `GET /api/har`.
#[derive(Debug, Deserialize)]
pub struct HarQueryParams {
    ids: Option<String>,
}

/// `GET /api/har?ids=a,b` — exports the whole session (or just the given
/// flow ids) as a HAR 1.2 document, served as a file download.
pub async fn export(
    State(state): State<ApiState>,
    Query(params): Query<HarQueryParams>,
) -> Result<Response, ApiError> {
    // `flows().all()`/`.ids()` already take the store's read lock only long
    // enough to clone the matching flows, releasing it before returning --
    // so by the time `flows` lands here, the lock is already gone; this is
    // just the owned snapshot.
    let flows = match params.ids.filter(|s| !s.is_empty()) {
        Some(ids_raw) => {
            let ids: Vec<Uuid> = ids_raw
                .split(',')
                .filter_map(|s| Uuid::parse_str(s.trim()).ok())
                .collect();
            state.flows().ids(&ids)
        }
        None => state.flows().all(),
    };

    // Building the HAR document and serializing it is CPU-bound and can be
    // sizeable for a large session; run it on the blocking pool so it
    // doesn't stall this async worker thread.
    let version = state.version().to_string();
    let body = tokio::task::spawn_blocking(move || {
        let har = export_har(&flows, &version);
        serde_json::to_vec(&har)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("HAR export task panicked: {e}")))??;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"hamsy-{ts}.har\""),
            ),
        ],
        body,
    )
        .into_response())
}

/// `POST /api/har/import` — imports flows from a HAR 1.2 document. Each
/// imported flow is assigned a fresh `seq` from the store (rather than
/// trusting the HAR's own entry ordering) so insertion order and sequence
/// numbers stay consistent with live-captured flows.
pub async fn import(
    State(state): State<ApiState>,
    Json(doc): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let flows = import_har(&doc).map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let mut summaries = Vec::with_capacity(flows.len());
    for mut flow in flows {
        flow.summary.seq = state.flows().next_seq();
        summaries.push(flow.summary());
        state.flows().insert(flow);
    }
    let imported = summaries.len();
    state.broadcast(ServerEvent::Flows { flows: summaries });

    Ok(Json(json!({ "imported": imported })))
}
