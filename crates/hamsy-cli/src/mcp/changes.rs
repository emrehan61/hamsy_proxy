use super::{errors::AgentError, parse, Bridge};
use anyhow::Result;
use reqwest::Method;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ChangesArgs {
    /// Live capture only: app:live (default) or live. HAR sessions are immutable.
    session_id: Option<String>,
    /// Omit to establish a baseline. Reuse the returned cursor on later calls.
    /// Clear, restart, or expired change history returns a stale_cursor error.
    cursor: Option<String>,
    /// Maximum metadata events per page, 1–200 (default 50).
    limit: Option<usize>,
    /// Wait up to 0–10000 ms for a change (default 0). Timeout is a successful
    /// empty page; a stale cursor always requires explicitly resetting baseline.
    wait_ms: Option<u64>,
}

impl Bridge {
    pub(super) async fn changes(&self, arguments: Value) -> Result<Value> {
        let args: ChangesArgs = parse(arguments)?;
        if !matches!(args.session_id.as_deref(), None | Some("live" | "app:live")) {
            return Err(AgentError::invalid(
                "wait_for_changes requires the live app:live session; HAR sessions do not change.",
            )
            .into());
        }
        let limit = args.limit.unwrap_or(50);
        let wait_ms = args.wait_ms.unwrap_or(0);
        if !(1..=200).contains(&limit) || wait_ms > 10_000 {
            return Err(
                AgentError::invalid("limit must be 1–200 and waitMs must be 0–10000.").into(),
            );
        }
        if args.cursor.as_ref().is_some_and(|cursor| cursor.len() > 64) {
            return Err(AgentError::invalid(
                "Invalid change cursor; use the cursor returned by this capture.",
            )
            .into());
        }
        let mut query = vec![
            ("limit", limit.to_string()),
            ("waitMs", wait_ms.to_string()),
        ];
        if let Some(cursor) = args.cursor {
            query.push(("cursor", cursor));
        }
        let mut result = self
            .request(Method::GET, "flows/changes", query, None)
            .await?;
        result["sessionId"] = json!("app:live");
        result["contentsOmitted"] = json!(true);
        Ok(result)
    }
}
