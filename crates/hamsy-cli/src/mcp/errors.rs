//! Stable recovery information shared by MCP and the JSON command interface.
use std::fmt;

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ErrorCode {
    InvalidArguments,
    PermissionDenied,
    UnknownTool,
    ConnectionFailed,
    Timeout,
    ResponseTooLarge,
    InvalidResponse,
    NotFound,
    SessionUnavailable,
    Busy,
    IncompatibleVersion,
    StaleCursor,
    ApiError,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AgentError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub outcome_unknown: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            outcome_unknown: false,
            details: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArguments, message)
    }

    /// Browser workers return only these read failure codes. Never forward
    /// browser prose, suggested retries, or write outcome claims.
    pub fn from_read_result(value: &Value) -> Option<Self> {
        let error = value.get("error")?;
        let (code, message, retryable) = match error.get("code").and_then(Value::as_str) {
            Some("busy") => (
                ErrorCode::Busy,
                "The HAR browser is busy; wait for the current read to finish.",
                true,
            ),
            Some("timeout") => (
                ErrorCode::Timeout,
                "The HAR read exceeded its time limit; simplify the query before trying again.",
                true,
            ),
            Some("session_unavailable") => (
                ErrorCode::SessionUnavailable,
                "The HAR browser worker is unavailable; wake or reload its window and rediscover sessions.",
                true,
            ),
            _ => (
                ErrorCode::InvalidArguments,
                "The read could not evaluate the selected data; check the query, field, source size and capture truncation.",
                false,
            ),
        };
        let mut result = Self::new(code, message);
        result.retryable = retryable;
        Some(result)
    }

    pub fn uncertain(mut self, write: bool) -> Self {
        self.outcome_unknown = write;
        self.retryable = !write && self.retryable;
        if write {
            self.message.push_str(" The operation may have executed; inspect state before acting. Never automatically retry replay.");
        }
        self
    }

    pub fn network(error: &reqwest::Error, write: bool) -> Self {
        let (code, message) = if error.is_timeout() {
            (ErrorCode::Timeout, "Hamsy did not respond in time.")
        } else {
            (
                ErrorCode::ConnectionFailed,
                "Cannot reach Hamsy; check the API address or reconnect MCP.",
            )
        };
        let mut result = Self::new(code, message);
        result.retryable = !write;
        // A refused connection cannot have executed a request; other transport
        // failures may occur after a server has accepted a mutating request.
        result.uncertain(write && !error.is_connect())
    }

    pub fn api(status: StatusCode, value: Option<&Value>, write: bool, session: bool) -> Self {
        let api_code = value
            .and_then(|v| v["code"].as_str())
            .or_else(|| value.and_then(|v| v["error"].as_str()));
        let (code, message, retryable) = match api_code {
            Some("stale_cursor") => (ErrorCode::StaleCursor, "The capture cursor is no longer valid; establish a new baseline.", false),
            Some("session_unavailable" | "session_timeout") => (ErrorCode::SessionUnavailable, "The HAR session is unavailable; wake or reopen its browser window and rediscover sessions.", true),
            Some("session_not_open") => (ErrorCode::SessionUnavailable, "The HAR session is no longer open; rediscover sessions.", false),
            Some("busy" | "session_busy") => (ErrorCode::Busy, "Hamsy is busy; wait for the current operation to finish.", true),
            Some("incompatible_version") => (ErrorCode::IncompatibleVersion, "The running app does not support this operation; update and restart it.", false),
            Some("bad_request" | "invalid_arguments" | "invalid_rule") => (ErrorCode::InvalidArguments, "Hamsy rejected the arguments; check the tool schema and rule syntax.", false),
            Some("payload_too_large" | "response_too_large") => (ErrorCode::ResponseTooLarge, "The operation exceeds a size limit; narrow the selection or omit bodies.", false),
            _ => match status.as_u16() {
                400 | 422 => (ErrorCode::InvalidArguments, "Hamsy rejected the arguments; check the tool schema.", false),
                401 | 403 => (ErrorCode::PermissionDenied, "Hamsy denied this operation.", false),
                404 if session => (ErrorCode::SessionUnavailable, "The HAR session or flow is no longer available; rediscover sessions.", false),
                404 => (ErrorCode::NotFound, "The requested resource or API route was not found; refresh state and check the app version.", false),
                405 | 501 => (ErrorCode::IncompatibleVersion, "The running app does not support this operation; update and restart it.", false),
                413 => (ErrorCode::ResponseTooLarge, "The operation exceeds a size limit; narrow the selection or omit bodies.", false),
                429 | 503 => (ErrorCode::Busy, "Hamsy is busy; wait before reading again.", true),
                502 | 504 if session => (ErrorCode::SessionUnavailable, "The HAR browser did not respond; wake or reopen its window.", true),
                _ => (ErrorCode::ApiError, "Hamsy returned an API error.", !write),
            },
        };
        let mut result = Self::new(code, message);
        result.retryable = retryable && !write;
        // Preserve protocol metadata, never arbitrary server prose or payloads.
        let mut details = json!({"httpStatus": status.as_u16()});
        if let Some(value) = value.and_then(|value| value.get("details")) {
            if let Some(reset) = value["resetRequired"].as_bool() {
                details["resetRequired"] = json!(reset);
            }
            if let Some(id) = value["captureId"]
                .as_str()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
            {
                details["captureId"] = json!(id);
            }
            if let Some(cursor) = value["cursor"]
                .as_str()
                .filter(|cursor| valid_cursor(cursor))
            {
                details["cursor"] = json!(cursor);
            }
            if let Some(reason) = value["reason"].as_str().filter(|reason| {
                matches!(
                    *reason,
                    "generation"
                        | "history_gap"
                        | "ahead"
                        | "cursor_ahead"
                        | "capture_changed"
                        | "history_lost"
                )
            }) {
                details["reason"] = json!(reason);
            }
        }
        result.details = Some(details);
        result.uncertain(write && status.is_server_error())
    }

    pub fn envelope(error: anyhow::Error) -> Value {
        let error = error
            .downcast::<Self>()
            .unwrap_or_else(|_| Self::invalid("Invalid tool arguments; consult the input schema."));
        json!({"error": error})
    }
}

fn valid_cursor(cursor: &str) -> bool {
    cursor.split_once(':').is_some_and(|(id, revision)| {
        uuid::Uuid::parse_str(id).is_ok() && revision.parse::<u64>().is_ok()
    })
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for AgentError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_read_errors_preserve_only_known_codes_and_safe_recovery() {
        for (code, expected, retryable) in [
            ("busy", "busy", true),
            ("timeout", "timeout", true),
            ("session_unavailable", "session_unavailable", true),
            ("invalid_arguments", "invalid_arguments", false),
            ("untrusted_code", "invalid_arguments", false),
        ] {
            let value = json!({"error":{"code":code,"message":"PRIVATE_BROWSER_PROSE",
                "retryable":!retryable,"outcomeUnknown":true}});
            let result =
                serde_json::to_value(AgentError::from_read_result(&value).unwrap()).unwrap();
            assert_eq!(result["code"], expected);
            assert_eq!(result["retryable"], retryable);
            assert_eq!(result["outcomeUnknown"], false);
            assert!(!result.to_string().contains("PRIVATE_BROWSER_PROSE"));
        }
        let legacy =
            AgentError::from_read_result(&json!({"error":"PRIVATE_LEGACY_PROSE"})).unwrap();
        assert_eq!(
            serde_json::to_value(&legacy).unwrap()["code"],
            "invalid_arguments"
        );
        assert!(!legacy.message.contains("PRIVATE_LEGACY_PROSE"));
        assert!(AgentError::from_read_result(&json!({"matches":[]})).is_none());
    }
}
