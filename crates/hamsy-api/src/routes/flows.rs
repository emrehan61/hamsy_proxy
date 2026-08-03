//! `/api/flows*` routes: list, get, clear, replay.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use bytes::Bytes;
use hamsy_core::{FlowQuery, RequestRecord, ResourceType, ServerEvent};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::ApiState;

/// Default `limit` applied to `GET /api/flows` when the caller omits it.
const DEFAULT_LIMIT: usize = 2000;
/// Hard cap on `limit`, regardless of what the caller requests.
const MAX_LIMIT: usize = 50_000;

/// Raw (string-typed) query parameters accepted by `GET /api/flows`, parsed
/// into a [`FlowQuery`] by [`list`].
#[derive(Debug, Deserialize)]
pub struct FlowsQueryParams {
    limit: Option<usize>,
    #[serde(rename = "afterSeq")]
    after_seq: Option<u64>,
    q: Option<String>,
    methods: Option<String>,
    #[serde(rename = "statusClass")]
    status_class: Option<u16>,
    #[serde(rename = "resourceTypes")]
    resource_types: Option<String>,
    host: Option<String>,
    #[serde(rename = "onlyModified")]
    only_modified: Option<String>,
}

fn parse_resource_type(token: &str) -> Option<ResourceType> {
    match token.trim().to_ascii_lowercase().as_str() {
        "document" => Some(ResourceType::Document),
        "stylesheet" => Some(ResourceType::Stylesheet),
        "script" => Some(ResourceType::Script),
        "image" => Some(ResourceType::Image),
        "font" => Some(ResourceType::Font),
        "xhr" => Some(ResourceType::Xhr),
        "json" => Some(ResourceType::Json),
        "media" => Some(ResourceType::Media),
        "websocket" => Some(ResourceType::WebSocket),
        "other" => Some(ResourceType::Other),
        _ => None,
    }
}

fn parse_bool_flag(s: &str) -> bool {
    matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Builds a [`FlowQuery`] from raw query-string parameters.
fn build_query(params: FlowsQueryParams) -> FlowQuery {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let resource_types = params
        .resource_types
        .map(|raw| {
            split_csv(&raw)
                .iter()
                .filter_map(|t| parse_resource_type(t))
                .collect()
        })
        .unwrap_or_default();
    let methods = params
        .methods
        .map(|raw| split_csv(&raw))
        .unwrap_or_default();

    FlowQuery {
        after_seq: params.after_seq,
        limit: Some(limit),
        search: params.q.filter(|s| !s.is_empty()),
        methods,
        status_class: params.status_class,
        resource_types,
        host: params.host.filter(|s| !s.is_empty()),
        only_modified: params
            .only_modified
            .as_deref()
            .map(parse_bool_flag)
            .unwrap_or(false),
    }
}

/// `GET /api/flows` — returns `{ "flows": [FlowSummary, ...] }` matching
/// the filters encoded in the query string.
pub async fn list(
    State(state): State<ApiState>,
    Query(params): Query<FlowsQueryParams>,
) -> Json<Value> {
    let query = build_query(params);
    let flows = state.flows().list(&query);
    Json(json!({ "flows": flows }))
}

/// Parses a flow id path segment, returning a 404 (with a message
/// distinguishing "malformed id" from "not found") rather than panicking on
/// invalid UUID input.
fn parse_flow_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::NotFound(format!("'{raw}' is not a valid flow id")))
}

/// `GET /api/flows/:id` — returns the full [`hamsy_core::Flow`] detail.
pub async fn get(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<hamsy_core::Flow>, ApiError> {
    let flow_id = parse_flow_id(&id)?;
    state
        .flows()
        .get(flow_id)
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("flow {flow_id} not found")))
}

/// `DELETE /api/flows` — clears the flow store and broadcasts
/// [`ServerEvent::Cleared`].
pub async fn clear(State(state): State<ApiState>) -> impl IntoResponse {
    state.flows().clear();
    state.broadcast(ServerEvent::Cleared);
    StatusCode::NO_CONTENT
}

/// `POST /api/flows/:id/replay` — replays a captured request (optionally
/// with an edited [`RequestRecord`] body) through the configured
/// [`crate::hooks::ReplayHook`]. 404s early if the source flow doesn't
/// exist; any hook failure (in practice, "no proxy backend attached") is
/// reported as 501 Not Implemented.
pub async fn replay(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let flow_id = parse_flow_id(&id)?;
    if state.flows().get(flow_id).is_none() {
        return Err(ApiError::NotFound(format!("flow {flow_id} not found")));
    }
    let edited: Option<RequestRecord> = if body.is_empty() {
        None
    } else {
        Some(serde_json::from_slice(&body)?)
    };

    match state.replay_hook().replay(flow_id, edited).await {
        Ok(new_id) => Ok(Json(json!({ "id": new_id }))),
        Err(message) => Err(ApiError::NotImplemented(message)),
    }
}
