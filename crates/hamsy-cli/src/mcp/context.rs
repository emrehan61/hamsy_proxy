//! Match evidence uses the selected source; raw large bodies never cross MCP.
use super::*;
use hamsy_core::agent_context::ContextQuery;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ContextArgs {
    /// Exact sessionId returned by list_sessions; default app:live.
    session_id: Option<String>,
    id: String,
    #[serde(flatten)]
    context: ContextQuery,
}
impl Bridge {
    pub(super) async fn match_context(&self, arguments: Value) -> Result<Value> {
        let args: ContextArgs = parse(arguments)?;
        args.context
            .validate()
            .map_err(super::errors::AgentError::invalid)?;
        let (target, session) = self.session_target(args.session_id.as_deref()).await?;
        let id = flow_id(&args.id)?;
        let path = if session == "live" {
            format!("flows/{id}/context")
        } else {
            format!("sessions/{session}/flows/{id}/context")
        };
        let mut result = target
            .request(
                Method::GET,
                &path,
                vec![("params", serde_json::to_string(&args.context)?)],
                None,
            )
            .await?;
        if let Some(error) = super::errors::AgentError::from_read_result(&result) {
            return Err(error.into());
        }
        result["sessionId"] = json!(args.session_id.unwrap_or_else(|| "app:live".into()));
        Ok(result)
    }
}
