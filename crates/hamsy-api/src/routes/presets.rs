//! `/api/presets/passthrough` route.

use axum::Json;
use hamsy_core::passthrough_presets::PRESETS;
use serde::Serialize;

/// Wire shape for one entry of `GET /api/presets/passthrough`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PassthroughPreset {
    name: &'static str,
    label: &'static str,
    description: &'static str,
    hosts: &'static [&'static str],
}

/// `GET /api/presets/passthrough` -- the built-in passthrough preset
/// catalog, for the Settings page to render as a toggle group. Static data
/// (no `ApiState` needed): the catalog is compiled into the binary, not
/// user-editable.
pub async fn list_passthrough() -> Json<Vec<PassthroughPreset>> {
    Json(
        PRESETS
            .iter()
            .map(|p| PassthroughPreset {
                name: p.name,
                label: p.label,
                description: p.description,
                hosts: p.hosts,
            })
            .collect(),
    )
}
