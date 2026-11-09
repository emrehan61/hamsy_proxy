//! Source-side context extraction keeps large payloads out of agent transport.
use crate::{ApiError, ApiState};
use axum::http::StatusCode;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use hamsy_core::agent_context::{match_context, ContextQuery};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ContextParams {
    params: String,
}

pub async fn get(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(params): Query<ContextParams>,
) -> Result<Json<Value>, ApiError> {
    if params.params.len() > 32768 {
        return Err(ApiError::BadRequest(
            "Context parameters exceed 32 KiB".into(),
        ));
    }
    let query: ContextQuery = serde_json::from_str(&params.params)?;
    query.validate().map_err(ApiError::BadRequest)?;
    static SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let slot = SLOTS.try_acquire().map_err(|_| {
        ApiError::coded(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Context worker is busy; retry after the current read finishes",
            true,
        )
    })?;
    let flow = state
        .flows()
        .snapshots(Some(&[id]))
        .pop()
        .ok_or_else(|| ApiError::NotFound("Flow is not retained; list flows again".into()))?;
    let value = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        match_context(&flow, &query)
    })
    .await
    .map_err(|_| ApiError::Internal("Context worker failed".into()))?
    .map_err(ApiError::BadRequest)?;
    Ok(Json(value))
}
