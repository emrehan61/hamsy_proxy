//! In-memory flow store: a capacity-bounded ring buffer of [`Flow`]s.

use std::collections::{HashMap, VecDeque};

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
    flows: HashMap<FlowId, Flow>,
    order: VecDeque<FlowId>,
    capacity: usize,
    next_seq: u64,
    total_bytes: u64,
}

impl Inner {
    fn evict_if_needed(&mut self) {
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                if let Some(flow) = self.flows.remove(&oldest) {
                    self.total_bytes = self
                        .total_bytes
                        .saturating_sub(flow.summary.request_size + flow.summary.response_size);
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
            inner.total_bytes = inner
                .total_bytes
                .saturating_sub(old.summary.request_size + old.summary.response_size);
            inner.order.retain(|existing| *existing != id);
        }
        inner.total_bytes += flow.summary.request_size + flow.summary.response_size;
        inner.order.push_back(id);
        inner.flows.insert(id, flow);
        inner.evict_if_needed();
    }

    /// Applies `f` to the flow with the given `id`, updating byte
    /// accounting, and returns its new summary. Returns `None` if no flow
    /// with that id exists.
    pub fn update<F: FnOnce(&mut Flow)>(&self, id: FlowId, f: F) -> Option<FlowSummary> {
        let mut inner = self.inner.write();
        let flow = inner.flows.get_mut(&id)?;
        let before = flow.summary.request_size + flow.summary.response_size;
        f(flow);
        let after = flow.summary.request_size + flow.summary.response_size;
        let summary = flow.summary();
        inner.total_bytes = inner.total_bytes.saturating_sub(before) + after;
        Some(summary)
    }

    /// Returns a clone of the flow with the given `id`, if present.
    pub fn get(&self, id: FlowId) -> Option<Flow> {
        self.inner.read().flows.get(&id).cloned()
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

    /// Returns clones of every stored flow, in insertion order.
    pub fn all(&self) -> Vec<Flow> {
        let inner = self.inner.read();
        inner
            .order
            .iter()
            .filter_map(|id| inner.flows.get(id))
            .cloned()
            .collect()
    }

    /// Returns clones of the flows matching `ids`, skipping any that are
    /// not present, in the order requested.
    pub fn ids(&self, ids: &[FlowId]) -> Vec<Flow> {
        let inner = self.inner.read();
        ids.iter()
            .filter_map(|id| inner.flows.get(id).cloned())
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

    /// Returns the sum of `requestSize + responseSize` across all stored flows.
    pub fn total_bytes(&self) -> u64 {
        self.inner.read().total_bytes
    }
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
            f.summary.response_size = 100;
        });
        assert_eq!(store.total_bytes(), 110);
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
}
