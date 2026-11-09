//! Read-only rule preview; the app evaluates its retained snapshot in place.
use super::*;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PreviewArgs {
    /// Live capture only; imported HAR sessions cannot be used for this preview.
    session_id: Option<String>,
    /// Existing retained live flow UUID.
    id: String,
    /// Proposed complete rule, evaluated alone without saving or enabling it.
    rule: hamsy_core::Rule,
}

impl Bridge {
    pub(super) async fn preview_rule(&self, arguments: Value) -> Result<Value> {
        let args: PreviewArgs = parse(arguments)?;
        ensure_argument!(
            matches!(args.session_id.as_deref(), None | Some("live" | "app:live")),
            "preview_rule requires live capture (app:live)."
        );
        let id = flow_id(&args.id)?;
        self.request(
            Method::POST,
            "rules/preview",
            vec![],
            Some(json!({"flowId":id,"rule":args.rule})),
        )
        .await
    }
}
