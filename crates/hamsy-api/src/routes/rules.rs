//! `/api/rules*` routes: CRUD, toggle, reorder, import/export.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use flproxy_core::{Rule, ServerEvent};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::ApiState;

/// `GET /api/rules` — returns `{"rules": [Rule, ...]}` (persisted order).
///
/// Deliberately wrapped in an object (rather than a bare `Rule[]` array),
/// for symmetry with `GET /api/rules/export` and so the shape can grow
/// additional top-level fields later without a breaking change. The web
/// UI's `listRules()` (`ui/src/lib/api.ts`) expects exactly this envelope.
pub async fn list(State(state): State<ApiState>) -> Json<Value> {
    Json(json!({ "rules": state.rules().list() }))
}

/// `POST /api/rules` — creates a rule, server-assigning a UUID id when the
/// incoming `id` is empty. Returns 201 with the created rule.
pub async fn create(
    State(state): State<ApiState>,
    Json(mut rule): Json<Rule>,
) -> Result<impl IntoResponse, ApiError> {
    if rule.id.is_empty() {
        rule.id = uuid::Uuid::new_v4().to_string();
    }
    state.rules().create(rule.clone())?;
    state.broadcast(ServerEvent::RulesChanged);
    Ok((StatusCode::CREATED, Json(rule)))
}

/// `PUT /api/rules/:id` — replaces a rule wholesale. 404 if it doesn't
/// exist.
pub async fn update(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(rule): Json<Rule>,
) -> Result<Json<Rule>, ApiError> {
    state.rules().update(&id, rule.clone())?;
    state.broadcast(ServerEvent::RulesChanged);
    Ok(Json(rule))
}

/// `DELETE /api/rules/:id` — 404 if it doesn't exist.
pub async fn delete(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.rules().delete(&id)?;
    state.broadcast(ServerEvent::RulesChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/rules/:id/toggle` — flips `enabled` and returns the updated
/// rule. 404 if it doesn't exist.
pub async fn toggle(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<Rule>, ApiError> {
    state.rules().toggle(&id)?;
    let rule = state
        .rules()
        .get(&id)
        .ok_or_else(|| ApiError::NotFound(format!("rule {id} not found")))?;
    state.broadcast(ServerEvent::RulesChanged);
    Ok(Json(rule))
}

/// Body of `POST /api/rules/reorder`.
#[derive(Debug, Deserialize)]
pub struct ReorderBody {
    ids: Vec<String>,
}

/// `POST /api/rules/reorder`.
pub async fn reorder(
    State(state): State<ApiState>,
    Json(body): Json<ReorderBody>,
) -> Result<StatusCode, ApiError> {
    state.rules().reorder(&body.ids)?;
    state.broadcast(ServerEvent::RulesChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Body of `POST /api/rules/import`.
#[derive(Debug, Deserialize)]
pub struct ImportBody {
    rules: Vec<Rule>,
    #[serde(default)]
    replace: Option<bool>,
}

/// `POST /api/rules/import`.
///
/// `replace: true` (or omitted) replaces the entire rule set via
/// [`flproxy_core::RulesStore::import`]. `replace: false` merges: existing
/// rules are kept, incoming rules with a matching `id` overwrite them in
/// place, and new ids are appended (`RulesStore` has no native partial
/// import, so this is implemented here as read-merge-`import`).
///
/// Returns `{"imported": n}` (200), for symmetry with `POST /api/har/import`.
pub async fn import(
    State(state): State<ApiState>,
    Json(body): Json<ImportBody>,
) -> Result<Json<Value>, ApiError> {
    let imported_count = body.rules.len();
    if body.replace.unwrap_or(true) {
        state.rules().import(body.rules)?;
    } else {
        let mut current = state.rules().list();
        for incoming in body.rules {
            if let Some(pos) = current.iter().position(|r| r.id == incoming.id) {
                current[pos] = incoming;
            } else {
                current.push(incoming);
            }
        }
        state.rules().import(current)?;
    }
    state.broadcast(ServerEvent::RulesChanged);
    Ok(Json(json!({ "imported": imported_count })))
}

/// `GET /api/rules/export` — returns `{"rules": [Rule, ...]}`.
pub async fn export(State(state): State<ApiState>) -> Json<Value> {
    Json(json!({ "rules": state.rules().export() }))
}
