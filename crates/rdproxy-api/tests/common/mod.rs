//! Shared test helpers: building an [`ApiState`] backed by fresh temp-file
//! stores, and constructing sample [`Flow`]s without a real proxy backend.

use std::path::PathBuf;
use std::sync::Arc;

use rdproxy_api::ApiState;
use rdproxy_core::{
    BodyPayload, Flow, FlowStore, HeaderPair, RequestRecord, ResponseRecord, RulesStore, Settings,
};

/// Returns a fresh temp-file path (never actually written unless the test
/// exercises persistence), namespaced by `prefix`.
pub fn temp_path(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{prefix}-{}.json", uuid::Uuid::new_v4()))
}

/// Builds a standalone [`ApiState`] (no-op replay/cert hooks) with a
/// generously-sized flow store and default settings.
pub fn make_state() -> ApiState {
    make_state_with_capacity(1000)
}

/// Same as [`make_state`], with an explicit flow store capacity.
pub fn make_state_with_capacity(capacity: usize) -> ApiState {
    let flows = Arc::new(FlowStore::new(capacity));
    let rules = Arc::new(RulesStore::load(&temp_path("rdproxy-api-test-rules")));
    ApiState::new_standalone(
        flows,
        rules,
        Settings::default(),
        temp_path("rdproxy-api-test-settings"),
        "test",
    )
}

/// Builds a simple complete (or still-pending, if `status` is `None`) flow
/// for store-population tests.
pub fn sample_flow(seq: u64, method: &str, host: &str, status: Option<u16>) -> Flow {
    let id = uuid::Uuid::new_v4();
    let request = RequestRecord {
        method: method.to_string(),
        url: format!("http://{host}/path"),
        http_version: "HTTP/1.1".to_string(),
        headers: vec![HeaderPair::new("X-Test", "hello")],
        body: BodyPayload::default(),
        query: vec![],
    };
    let mut flow = Flow::new_request(
        id,
        seq,
        0,
        method,
        "http",
        host,
        80,
        "/path",
        format!("http://{host}/path"),
        "HTTP/1.1",
        "127.0.0.1:1234",
        request,
    );
    flow.summary.seq = seq;
    if let Some(status) = status {
        let response = ResponseRecord {
            status,
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: BodyPayload::default(),
        };
        flow.mark_complete(response, 100);
    }
    flow
}
