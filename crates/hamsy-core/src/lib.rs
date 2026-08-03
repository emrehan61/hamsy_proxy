//! `hamsy-core`: pure logic for the hamsy-proxy HTTP(S) debugging proxy.
//!
//! This crate has no networking or server dependencies. It defines the
//! shared data model (flows, rules, settings), the rule matching/mutation
//! engine, body codecs, an in-memory flow store, and HAR import/export.
//! Other crates (a proxy server, a REST/WebSocket API, a CLI) build on top
//! of this foundation.

pub mod body;
pub mod error;
pub mod event;
pub mod flow;
pub mod har;
pub mod rule;
pub mod rules_store;
pub mod settings;
pub mod store;

pub use body::{decode_body, encode_body, from_payload, is_textual_mime, pretty_json, to_payload};
pub use error::{CoreError, Result};
pub use event::{ClientCommand, ServerEvent};
pub use flow::{
    BodyKind, BodyPayload, Flow, FlowId, FlowState, FlowSummary, HeaderPair, RequestRecord,
    ResourceType, ResponseRecord, Timings, TlsInfo, WsDirection, WsMessage,
};
pub use har::{export_har, import_har};
pub use rule::{
    Action, BodyCond, BodyCondOp, HeaderCond, HeaderOp, JsonOp, JsonOpKind, Matcher,
    MockedResponse, PayloadEncoding, RequestCtx, RequestOutcome, ResponseCtx, ResponseOutcome,
    Rule, RuleError, RuleSet, UrlOp,
};
pub use rules_store::RulesStore;
pub use settings::{data_dir, Settings};
pub use store::{FlowQuery, FlowStore};
