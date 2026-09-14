//! In-memory flow store: a capacity-bounded ring buffer of [`Flow`]s.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::flow::{Flow, FlowId, FlowSummary, ResourceType};

/// Filter/pagination parameters for [`FlowStore::list`].
#[derive(Debug, Clone, Default)]
pub struct FlowQuery {
    /// Only include flows with `seq` strictly greater than this value.
    pub after_seq: Option<u64>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Case-insensitive free-text search across URL, host, method, status,
    /// and header values.
    pub search: Option<String>,
    /// Restrict to these HTTP methods (case-insensitive). Empty = any.
    pub methods: Vec<String>,
    /// Restrict to a status class, e.g. `4` for 4xx. `None` = any.
    pub status_class: Option<u16>,
    /// Restrict to these resource types. Empty = any.
    pub resource_types: Vec<ResourceType>,
    /// Restrict to flows whose host equals this value (case-insensitive).
    pub host: Option<String>,
    /// Restrict to flows whose originating app equals this value
    /// (case-insensitive). Flows with no resolved app never match.
    pub app: Option<String>,
    /// Restrict to flows that were modified by a rule.
    pub only_modified: bool,
}

struct Inner {
    flows: HashMap<FlowId, Arc<Flow>>,
    order: VecDeque<FlowId>,
    capacity: usize,
    next_seq: u64,
    total_bytes: u64,
    /// Byte budget enforced alongside `capacity`; see [`FlowStore::set_max_total_bytes`].
    max_total_bytes: u64,
}

impl Inner {
    /// Evicts oldest-first until BOTH the flow count is within `capacity`
    /// and `total_bytes` is within `max_total_bytes`, including eviction
    /// of a single flow that exceeds the entire budget.
    fn evict_if_needed(&mut self) {
        while self.order.len() > self.capacity || self.total_bytes > self.max_total_bytes {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(flow) = self.flows.remove(&oldest) {
                    self.total_bytes = self.total_bytes.saturating_sub(stored_payload_bytes(&flow));
                }
            } else {
                break;
            }
        }
    }
}

/// A thread-safe, capacity-bounded store of captured [`Flow`]s.
///
/// Internally backed by a `HashMap` (for O(1) lookup/update) plus a
/// `VecDeque` tracking insertion order (for O(1) oldest-eviction once the
/// configured capacity is exceeded).
pub struct FlowStore {
    inner: RwLock<Inner>,
}

impl FlowStore {
    /// Creates an empty store that holds at most `capacity` flows,
    /// evicting the oldest flow once a new insert would exceed it.
    pub fn new(capacity: usize) -> Self {
        FlowStore {
            inner: RwLock::new(Inner {
                flows: HashMap::new(),
                order: VecDeque::new(),
                capacity: capacity.max(1),
                next_seq: 1,
                total_bytes: 0,
                max_total_bytes: u64::MAX,
            }),
        }
    }

    /// Atomically allocates the next monotonically increasing sequence
    /// number, for use as [`FlowSummary::seq`].
    pub fn next_seq(&self) -> u64 {
        let mut inner = self.inner.write();
        let seq = inner.next_seq;
        inner.next_seq += 1;
        seq
    }

    /// Inserts a new flow, evicting the oldest flow if the store is at
    /// capacity. If a flow with the same id already exists, it is replaced
    /// (its old byte accounting is removed first).
    pub fn insert(&self, flow: Flow) {
        let mut inner = self.inner.write();
        let id = flow.summary.id;
        if let Some(old) = inner.flows.remove(&id) {
            inner.total_bytes = inner.total_bytes.saturating_sub(stored_payload_bytes(&old));
            inner.order.retain(|existing| *existing != id);
        }
        inner.total_bytes += stored_payload_bytes(&flow);
        inner.order.push_back(id);
        inner.flows.insert(id, Arc::new(flow));
        inner.evict_if_needed();
    }

    /// Applies `f` to the flow with the given `id`, updating byte
    /// accounting, and returns its new summary. Returns `None` if no flow
    /// with that id exists or the updated flow exceeds the retention budget.
    pub fn update<F: FnOnce(&mut Flow)>(&self, id: FlowId, f: F) -> Option<FlowSummary> {
        // Export snapshots may share this flow. Detach outside the global
        // lock so a large active flow cannot pause unrelated capture updates.
        // Recheck identity before applying the FnOnce callback: another writer
        // may have updated/replaced/evicted it while the copy was made.
        let (mut inner, before) = loop {
            let inner = self.inner.write();
            let stored = inner.flows.get(&id)?;
            let before = stored_payload_bytes(stored);
            if Arc::strong_count(stored) == 1 && Arc::weak_count(stored) == 0 {
                break (inner, before);
            }
            let snapshot = Arc::clone(stored);
            drop(inner);
            let detached = Arc::new((*snapshot).clone());
            let mut inner = self.inner.write();
            if inner
                .flows
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &snapshot))
            {
                inner.flows.insert(id, detached);
                break (inner, before);
            }
        };
        let flow =
            Arc::get_mut(inner.flows.get_mut(&id)?).expect("exclusive flow under write lock");
        f(flow);
        let after = stored_payload_bytes(flow);
        let summary = flow.summary();
        inner.total_bytes = inner.total_bytes.saturating_sub(before) + after;
        inner.evict_if_needed();
        inner.flows.contains_key(&id).then_some(summary)
    }

    /// Returns a clone of the flow with the given `id`, if present.
    pub fn get(&self, id: FlowId) -> Option<Flow> {
        let snapshot = self.inner.read().flows.get(&id).cloned();
        snapshot.map(|flow| (*flow).clone())
    }

    /// Lists flow summaries matching `query`, newest-insertion-order last
    /// (i.e. in the same order flows were inserted), most-recently-inserted
    /// last within that constraint, honoring `after_seq`/`limit`/filters.
    pub fn list(&self, query: &FlowQuery) -> Vec<FlowSummary> {
        let inner = self.inner.read();
        let search = query.search.as_ref().map(|s| s.to_ascii_lowercase());
        let host_filter = query.host.as_ref().map(|h| h.to_ascii_lowercase());
        let app_filter = query.app.as_ref().map(|a| a.to_ascii_lowercase());

        let mut results: Vec<FlowSummary> = inner
            .order
            .iter()
            .filter_map(|id| inner.flows.get(id))
            .filter(|flow| query.after_seq.is_none_or(|after| flow.summary.seq > after))
            .filter(|flow| {
                query.methods.is_empty()
                    || query
                        .methods
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(&flow.summary.method))
            })
            .filter(|flow| {
                query.status_class.is_none_or(|class| {
                    flow.summary
                        .status
                        .map(|s| s / 100 == class)
                        .unwrap_or(false)
                })
            })
            .filter(|flow| {
                query.resource_types.is_empty()
                    || query.resource_types.contains(&flow.summary.resource_type)
            })
            .filter(|flow| {
                host_filter
                    .as_ref()
                    .is_none_or(|h| flow.summary.host.to_ascii_lowercase() == *h)
            })
            .filter(|flow| {
                app_filter.as_ref().is_none_or(|a| {
                    flow.summary
                        .app
                        .as_ref()
                        .is_some_and(|flow_app| flow_app.to_ascii_lowercase() == *a)
                })
            })
            .filter(|flow| !query.only_modified || flow.summary.modified)
            .filter(|flow| {
                search
                    .as_ref()
                    .is_none_or(|needle| flow_matches_search(flow, needle))
            })
            .map(|flow| flow.summary())
            .collect();

        if let Some(limit) = query.limit {
            if results.len() > limit {
                let start = results.len() - limit;
                results = results.split_off(start);
            }
        }
        results
    }

    /// Shares immutable snapshots in insertion order. Only Arc handles are
    /// cloned under the store lock; payloads remain shared with the store.
    /// A later update uses copy-on-write to preserve the snapshot.
    pub fn snapshots(&self, ids: Option<&[FlowId]>) -> Vec<Arc<Flow>> {
        let inner = self.inner.read();
        match ids {
            Some(ids) => ids
                .iter()
                .filter_map(|id| inner.flows.get(id).cloned())
                .collect(),
            None => inner
                .order
                .iter()
                .filter_map(|id| inner.flows.get(id).cloned())
                .collect(),
        }
    }

    /// Returns clones of every stored flow, copying outside the store lock.
    pub fn all(&self) -> Vec<Flow> {
        self.snapshots(None)
            .iter()
            .map(|flow| (**flow).clone())
            .collect()
    }

    /// Returns clones in requested order, copying outside the store lock.
    pub fn ids(&self, ids: &[FlowId]) -> Vec<Flow> {
        self.snapshots(Some(ids))
            .iter()
            .map(|flow| (**flow).clone())
            .collect()
    }

    /// Removes all flows and resets byte accounting (sequence numbers keep
    /// incrementing).
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.flows.clear();
        inner.order.clear();
        inner.total_bytes = 0;
    }

    /// Returns the number of flows currently stored.
    pub fn len(&self) -> usize {
        self.inner.read().order.len()
    }

    /// Returns true if the store currently holds no flows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Changes the maximum capacity, evicting the oldest flows immediately
    /// if the new capacity is smaller than the current size.
    pub fn set_capacity(&self, cap: usize) {
        let mut inner = self.inner.write();
        inner.capacity = cap.max(1);
        inner.evict_if_needed();
    }

    /// Changes the maximum retained payload allocation (including original
    /// bodies and WebSocket messages) the store may hold, evicting the oldest
    /// flows immediately if the store is currently over the new budget. See
    /// [`Settings::max_total_bytes`](crate::Settings::max_total_bytes).
    pub fn set_max_total_bytes(&self, max: u64) {
        let mut inner = self.inner.write();
        inner.max_total_bytes = max;
        inner.evict_if_needed();
    }

    /// Returns retained payload allocation bytes; excludes metadata and external snapshots.
    pub fn total_bytes(&self) -> u64 {
        self.inner.read().total_bytes
    }
}

// Count allocated storage, not wire sizes: base64 grows, decoded bodies can
// expand, and truncated captures may be much smaller than their source.
fn stored_payload_bytes(flow: &Flow) -> u64 {
    let requests = [&flow.request, &flow.original_request];
    let responses = [&flow.response, &flow.original_response];
    requests
        .into_iter()
        .flatten()
        .map(|r| r.body.data.capacity() as u64)
        .chain(
            responses
                .into_iter()
                .flatten()
                .map(|r| r.body.data.capacity() as u64),
        )
        .chain(flow.ws_messages.iter().map(|m| m.data.capacity() as u64))
        .sum()
}

fn flow_matches_search(flow: &Flow, needle: &str) -> bool {
    if flow.summary.url.to_ascii_lowercase().contains(needle) {
        return true;
    }
    if flow.summary.host.to_ascii_lowercase().contains(needle) {
        return true;
    }
    if flow.summary.method.to_ascii_lowercase().contains(needle) {
        return true;
    }
    if let Some(status) = flow.summary.status {
        if status.to_string().contains(needle) {
            return true;
        }
    }
    if let Some(req) = &flow.request {
        if req
            .headers
            .iter()
            .any(|h| h.value.to_ascii_lowercase().contains(needle))
        {
            return true;
        }
    }
    if let Some(resp) = &flow.response {
        if resp
            .headers
            .iter()
            .any(|h| h.value.to_ascii_lowercase().contains(needle))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{BodyPayload, HeaderPair, RequestRecord};

    fn make_flow(seq: u64, method: &str, host: &str, status: Option<u16>, modified: bool) -> Flow {
        let id = uuid::Uuid::new_v4();
        let request = RequestRecord {
            method: method.to_string(),
            url: format!("http://{host}/path"),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![HeaderPair::new("X-Test", "hello")],
            body: BodyPayload {
                data: "x".repeat(30),
                ..Default::default()
            },
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
        flow.summary.status = status;
        flow.summary.modified = modified;
        flow.summary.request_size = 10;
        flow.summary.response_size = 20;
        flow
    }

    #[test]
    fn insert_and_get() {
        let store = FlowStore::new(10);
        let flow = make_flow(1, "GET", "example.com", Some(200), false);
        let id = flow.summary.id;
        store.insert(flow);
        assert_eq!(store.len(), 1);
        let fetched = store.get(id).expect("flow present");
        assert_eq!(fetched.summary.host, "example.com");
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let store = FlowStore::new(2);
        let f1 = make_flow(1, "GET", "a.com", None, false);
        let f2 = make_flow(2, "GET", "b.com", None, false);
        let f3 = make_flow(3, "GET", "c.com", None, false);
        let id1 = f1.summary.id;
        store.insert(f1);
        store.insert(f2);
        store.insert(f3);
        assert_eq!(store.len(), 2);
        assert!(store.get(id1).is_none());
    }

    #[test]
    fn list_filters_by_method_status_host_modified() {
        let store = FlowStore::new(10);
        store.insert(make_flow(1, "GET", "a.com", Some(200), false));
        store.insert(make_flow(2, "POST", "b.com", Some(404), true));
        store.insert(make_flow(3, "GET", "a.com", Some(500), false));

        let by_method = store.list(&FlowQuery {
            methods: vec!["POST".to_string()],
            ..Default::default()
        });
        assert_eq!(by_method.len(), 1);

        let by_status_class = store.list(&FlowQuery {
            status_class: Some(4),
            ..Default::default()
        });
        assert_eq!(by_status_class.len(), 1);

        let by_host = store.list(&FlowQuery {
            host: Some("a.com".to_string()),
            ..Default::default()
        });
        assert_eq!(by_host.len(), 2);

        let only_modified = store.list(&FlowQuery {
            only_modified: true,
            ..Default::default()
        });
        assert_eq!(only_modified.len(), 1);
    }

    #[test]
    fn list_filters_by_app_case_insensitively() {
        let store = FlowStore::new(10);
        let mut curl_flow = make_flow(1, "GET", "a.com", Some(200), false);
        curl_flow.summary.app = Some("curl".to_string());
        let mut chrome_flow = make_flow(2, "GET", "b.com", Some(200), false);
        chrome_flow.summary.app = Some("Google Chrome".to_string());
        // Simulates an unresolved app (e.g. a remote client, or a replay).
        let unresolved_flow = make_flow(3, "GET", "c.com", Some(200), false);
        store.insert(curl_flow);
        store.insert(chrome_flow);
        store.insert(unresolved_flow);

        let by_app = store.list(&FlowQuery {
            app: Some("google chrome".to_string()),
            ..Default::default()
        });
        assert_eq!(by_app.len(), 1);
        assert_eq!(by_app[0].host, "b.com");

        // A flow with no resolved app never matches an app filter, even one
        // that (oddly) filters for an empty string.
        let empty_filter = store.list(&FlowQuery {
            app: Some(String::new()),
            ..Default::default()
        });
        assert!(empty_filter.iter().all(|f| f.host != "c.com"));

        let no_filter = store.list(&FlowQuery::default());
        assert_eq!(no_filter.len(), 3);
    }

    #[test]
    fn list_search_matches_header_and_url() {
        let store = FlowStore::new(10);
        store.insert(make_flow(1, "GET", "example.com", Some(200), false));

        let hit = store.list(&FlowQuery {
            search: Some("hello".to_string()),
            ..Default::default()
        });
        assert_eq!(hit.len(), 1);

        let hit_url = store.list(&FlowQuery {
            search: Some("example".to_string()),
            ..Default::default()
        });
        assert_eq!(hit_url.len(), 1);

        let miss = store.list(&FlowQuery {
            search: Some("nope".to_string()),
            ..Default::default()
        });
        assert!(miss.is_empty());
    }

    #[test]
    fn list_respects_after_seq_and_limit() {
        let store = FlowStore::new(10);
        for i in 1..=5u64 {
            store.insert(make_flow(i, "GET", "a.com", Some(200), false));
        }
        let page = store.list(&FlowQuery {
            after_seq: Some(2),
            limit: Some(2),
            ..Default::default()
        });
        assert_eq!(page.len(), 2);
        assert!(page.iter().all(|f| f.seq > 2));
    }

    #[test]
    fn clear_resets_store() {
        let store = FlowStore::new(10);
        store.insert(make_flow(1, "GET", "a.com", Some(200), false));
        store.clear();
        assert_eq!(store.len(), 0);
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn total_bytes_accounting() {
        let store = FlowStore::new(10);
        let flow = make_flow(1, "GET", "a.com", Some(200), false);
        let id = flow.summary.id;
        store.insert(flow);
        assert_eq!(store.total_bytes(), 30);
        store.update(id, |f| {
            f.request.as_mut().unwrap().body.data = "x".repeat(110);
        });
        assert_eq!(store.total_bytes(), 110);
    }

    #[test]
    fn update_counts_originals_and_ws_and_evicts() {
        let store = FlowStore::new(10);
        store.set_max_total_bytes(100);
        let first = make_flow(1, "GET", "a.com", None, false);
        let first_id = first.summary.id;
        let second = make_flow(2, "GET", "b.com", None, false);
        let id = second.summary.id;
        store.insert(first);
        store.insert(second);
        store.update(id, |flow| {
            flow.original_request = flow.request.clone();
            flow.ws_messages.push(crate::flow::WsMessage {
                direction: crate::flow::WsDirection::Recv,
                opcode: "text".into(),
                timestamp: 0,
                data: "w".repeat(20),
                size: 9999,
            });
        });
        assert!(store.get(first_id).is_none());
        assert_eq!(store.total_bytes(), 80);
        // Wire/decoded sizes never determine the retained allocation.
        store.update(id, |flow| flow.summary.response_size = u64::MAX);
        assert_eq!(store.total_bytes(), 80);
        assert!(store
            .update(id, |flow| {
                flow.request.as_mut().unwrap().body.data = "x".repeat(101);
            })
            .is_none());
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
    }

    #[test]
    fn snapshots_share_payloads_and_preserve_export_view() {
        let store = FlowStore::new(10);
        let flow = make_flow(1, "GET", "a.com", None, false);
        let id = flow.summary.id;
        store.insert(flow);
        let snapshot = store.snapshots(None);
        let second = store.snapshots(Some(&[id]));
        assert!(Arc::ptr_eq(&snapshot[0], &second[0]));
        store.update(id, |f| f.request.as_mut().unwrap().body.data = "new".into());
        assert_eq!(
            snapshot[0].request.as_ref().unwrap().body.data,
            "x".repeat(30)
        );
        assert_eq!(store.get(id).unwrap().request.unwrap().body.data, "new");
        store.clear();
        let har = crate::export_har_refs(snapshot.iter().map(AsRef::as_ref), "test");
        assert_eq!(har["log"]["entries"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_snapshot_updates_do_not_lose_writes() {
        let store = FlowStore::new(10);
        let flow = make_flow(1, "GET", "a.com", None, false);
        let id = flow.summary.id;
        store.insert(flow);
        let initial = store.snapshots(None);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..100 {
                        let snapshot = store.snapshots(None);
                        store
                            .update(id, |flow| flow.summary.response_size += 1)
                            .unwrap();
                        assert_eq!(snapshot.len(), 1);
                    }
                });
            }
        });
        assert_eq!(initial[0].summary.response_size, 20);
        assert_eq!(store.get(id).unwrap().summary.response_size, 420);
        assert_eq!(store.total_bytes(), 30);
    }

    #[test]
    fn set_capacity_evicts_immediately() {
        let store = FlowStore::new(5);
        for i in 1..=5u64 {
            store.insert(make_flow(i, "GET", "a.com", Some(200), false));
        }
        store.set_capacity(2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn set_max_total_bytes_evicts_immediately() {
        // Each flow retains a 30-byte body, independent of wire size.
        let store = FlowStore::new(10);
        for i in 1..=5u64 {
            store.insert(make_flow(i, "GET", "a.com", Some(200), false));
        }
        assert_eq!(store.len(), 5);
        assert_eq!(store.total_bytes(), 150);

        // A budget of 100 bytes only leaves room for the newest 3 flows
        // (3 * 30 = 90 <= 100; a 4th would push it to 120).
        store.set_max_total_bytes(100);
        assert_eq!(store.len(), 3);
        assert!(store.total_bytes() <= 100);
    }

    #[test]
    fn byte_budget_evicts_oldest_on_insert() {
        let store = FlowStore::new(10);
        store.set_max_total_bytes(65);
        for i in 1..=5u64 {
            store.insert(make_flow(i, "GET", "a.com", Some(200), false));
        }
        // 65 / 30 = 2 flows fit; capacity (10) never binds here.
        assert_eq!(store.len(), 2);
        assert!(store.total_bytes() <= 65);
    }
}
