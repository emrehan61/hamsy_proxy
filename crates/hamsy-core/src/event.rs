//! WebSocket message shapes exchanged between the future `hamsy-api`
//! server and UI clients.

use serde::{Deserialize, Serialize};

use crate::flow::{Flow, FlowId, FlowSummary, WsMessage};
use crate::settings::Settings;

/// A message pushed from the server to subscribed WebSocket clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerEvent {
    /// A single flow was created or updated.
    Flow {
        /// The updated flow summary.
        flow: FlowSummary,
    },
    /// A batch of flow summaries, e.g. sent on initial subscription.
    Flows {
        /// The flow summaries.
        flows: Vec<FlowSummary>,
    },
    /// The full detail of one flow, e.g. sent in response to a selection.
    FlowDetail {
        /// The full flow record.
        flow: Box<Flow>,
    },
    /// A new WebSocket frame was captured on an existing flow.
    WsMessage {
        /// The flow the message belongs to.
        flow_id: FlowId,
        /// The captured message.
        message: WsMessage,
    },
    /// All flows were cleared.
    Cleared,
    /// Arbitrary server state snapshot (paused flag, counts, etc).
    State {
        /// Opaque state payload.
        state: serde_json::Value,
    },
    /// The rule set changed (created/updated/deleted/reordered/imported).
    RulesChanged,
    /// Settings were updated.
    SettingsChanged {
        /// The new settings.
        settings: Settings,
    },
    /// A human-readable notice for the UI to display.
    Notice {
        /// Severity level, e.g. `"info"`, `"warning"`, `"error"`.
        level: String,
        /// Message text.
        message: String,
    },
}

/// A message sent from a WebSocket client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientCommand {
    /// Pauses or resumes capture.
    Pause {
        /// Whether capture should be paused.
        paused: bool,
    },
    /// Clears all stored flows.
    Clear,
    /// Subscribes to flow updates, optionally restricted by an
    /// implementation-defined filter expression.
    Subscribe {
        /// Optional filter expression.
        filter: Option<String>,
    },
    /// A liveness check; the server should reply in an implementation
    /// defined way (e.g. no-op or a `Notice`).
    Ping,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_event_cleared_serializes_with_type_tag() {
        let json = serde_json::to_value(ServerEvent::Cleared).unwrap();
        assert_eq!(json["type"], "cleared");
    }

    #[test]
    fn client_command_pause_round_trips() {
        let json = serde_json::json!({"type": "pause", "paused": true});
        let cmd: ClientCommand = serde_json::from_value(json).unwrap();
        assert!(matches!(cmd, ClientCommand::Pause { paused: true }));
    }
}

/// Regression guard for the "`#[serde(tag = \"type\")]` on an enum renames
/// variant names, not struct-variant field names" bug: every [`ServerEvent`]
/// and [`ClientCommand`] variant must serialize with exactly the camelCase
/// keys the UI's `ui/src/lib/types.ts` (`WsServerMessage`/`WsClientMessage`)
/// declares, and must round-trip through serialize/deserialize.
#[cfg(test)]
mod wire_format_tests {
    use super::*;
    use crate::flow::{BodyPayload, Flow, FlowState, RequestRecord, ResourceType, WsDirection};
    use crate::settings::Settings;
    use std::collections::BTreeSet;

    fn keys(v: &serde_json::Value) -> BTreeSet<String> {
        v.as_object()
            .expect("must serialize to a JSON object")
            .keys()
            .cloned()
            .collect()
    }

    fn key_set(extra: &[&str]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = extra.iter().map(|s| s.to_string()).collect();
        set.insert("type".to_string());
        set
    }

    fn sample_flow_summary() -> FlowSummary {
        FlowSummary {
            id: FlowId::nil(),
            seq: 1,
            state: FlowState::Complete,
            started_at: 0,
            duration_ms: Some(5),
            method: "GET".to_string(),
            scheme: "http".to_string(),
            host: "example.com".to_string(),
            port: 80,
            path: "/".to_string(),
            url: "http://example.com/".to_string(),
            http_version: "HTTP/1.1".to_string(),
            status: Some(200),
            status_text: Some("OK".to_string()),
            mime_type: Some("text/plain".to_string()),
            resource_type: ResourceType::Other,
            request_size: 0,
            response_size: 0,
            client_addr: "127.0.0.1:1".to_string(),
            matched_rules: vec![],
            modified: false,
            error: None,
            websocket: false,
            from_cache: false,
        }
    }

    fn sample_flow() -> Flow {
        let request = RequestRecord {
            method: "GET".to_string(),
            url: "http://example.com/".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: BodyPayload::default(),
            query: vec![],
        };
        Flow::new_request(
            FlowId::nil(),
            1,
            0,
            "GET",
            "http",
            "example.com",
            80,
            "/",
            "http://example.com/",
            "HTTP/1.1",
            "127.0.0.1:1",
            request,
        )
    }

    fn sample_ws_message() -> WsMessage {
        WsMessage {
            direction: WsDirection::Send,
            opcode: "text".to_string(),
            timestamp: 0,
            data: "hi".to_string(),
            size: 2,
        }
    }

    /// Serializes `event`, asserts its JSON keys are exactly `"type"` +
    /// `expected_keys`, then round-trips it through deserialize and a
    /// second serialize, checking the resulting JSON is identical (avoids
    /// requiring `PartialEq` on the domain types nested inside events).
    fn check_server_event(event: ServerEvent, expected_keys: &[&str]) {
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            keys(&value),
            key_set(expected_keys),
            "unexpected wire keys for {value}"
        );

        let round_tripped: ServerEvent = serde_json::from_value(value.clone()).unwrap();
        let re_serialized = serde_json::to_value(&round_tripped).unwrap();
        assert_eq!(value, re_serialized);
    }

    /// Serializes `cmd`, asserts its JSON keys are exactly `"type"` +
    /// `expected_keys`, then round-trips it the same way as
    /// [`check_server_event`].
    fn check_client_command(cmd: ClientCommand, expected_keys: &[&str]) {
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            keys(&value),
            key_set(expected_keys),
            "unexpected wire keys for {value}"
        );

        let round_tripped: ClientCommand = serde_json::from_value(value.clone()).unwrap();
        let re_serialized = serde_json::to_value(&round_tripped).unwrap();
        assert_eq!(value, re_serialized);
    }

    #[test]
    fn server_event_flow_wire_format() {
        check_server_event(
            ServerEvent::Flow {
                flow: sample_flow_summary(),
            },
            &["flow"],
        );
    }

    #[test]
    fn server_event_flows_wire_format() {
        check_server_event(
            ServerEvent::Flows {
                flows: vec![sample_flow_summary()],
            },
            &["flows"],
        );
    }

    #[test]
    fn server_event_flow_detail_wire_format() {
        check_server_event(
            ServerEvent::FlowDetail {
                flow: Box::new(sample_flow()),
            },
            &["flow"],
        );
    }

    #[test]
    fn server_event_ws_message_wire_format() {
        check_server_event(
            ServerEvent::WsMessage {
                flow_id: FlowId::nil(),
                message: sample_ws_message(),
            },
            &["flowId", "message"],
        );
    }

    #[test]
    fn server_event_ws_message_serializes_flow_id_as_camel_case() {
        let value = serde_json::to_value(ServerEvent::WsMessage {
            flow_id: FlowId::nil(),
            message: sample_ws_message(),
        })
        .unwrap();
        assert!(value.get("flowId").is_some());
        assert!(value.get("flow_id").is_none());
    }

    #[test]
    fn server_event_cleared_wire_format() {
        check_server_event(ServerEvent::Cleared, &[]);
    }

    #[test]
    fn server_event_state_wire_format() {
        check_server_event(
            ServerEvent::State {
                state: serde_json::json!({"paused": false}),
            },
            &["state"],
        );
    }

    #[test]
    fn server_event_rules_changed_wire_format() {
        check_server_event(ServerEvent::RulesChanged, &[]);
    }

    #[test]
    fn server_event_settings_changed_wire_format() {
        check_server_event(
            ServerEvent::SettingsChanged {
                settings: Settings::default(),
            },
            &["settings"],
        );
    }

    #[test]
    fn server_event_notice_wire_format() {
        check_server_event(
            ServerEvent::Notice {
                level: "info".to_string(),
                message: "hi".to_string(),
            },
            &["level", "message"],
        );
    }

    #[test]
    fn client_command_pause_wire_format() {
        check_client_command(ClientCommand::Pause { paused: true }, &["paused"]);
    }

    #[test]
    fn client_command_clear_wire_format() {
        check_client_command(ClientCommand::Clear, &[]);
    }

    #[test]
    fn client_command_subscribe_wire_format() {
        check_client_command(
            ClientCommand::Subscribe {
                filter: Some("host:example.com".to_string()),
            },
            &["filter"],
        );
    }

    #[test]
    fn client_command_ping_wire_format() {
        check_client_command(ClientCommand::Ping, &[]);
    }

    #[test]
    fn client_command_subscribe_accepts_ui_payload() {
        // Exactly the shape the UI sends per `ui/src/lib/types.ts`
        // (`WsClientMessage`), with an optional `filter`.
        let json = serde_json::json!({"type": "subscribe"});
        let cmd: ClientCommand = serde_json::from_value(json).unwrap();
        assert!(matches!(cmd, ClientCommand::Subscribe { filter: None }));
    }
}
