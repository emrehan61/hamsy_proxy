//! Bounded content search over immutable flow snapshots. Results carry locations,
//! never snippets that could bypass the agent's credential/body redaction.
use crate::{BodyKind, BodyPayload, Flow};
use base64::{engine::general_purpose::STANDARD, Engine};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SearchQuery {
    /// Text or regex pattern (1–1024 UTF-8 bytes). Searches retained content.
    pub query: String,
    /// Interpret query as a regex. Default false (literal text).
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    /// Maximum matching requests per page, default 50, maximum 200.
    pub limit: Option<usize>,
    /// Continue with nextAfterSeq while hasMore=true, including pages with no matches.
    pub after_seq: Option<u64>,
    /// Exact host, without scheme or port.
    pub host: Option<String>,
    /// Exact hosts to exclude from this search (maximum 100).
    #[serde(default)]
    pub excluded_hosts: Vec<String>,
    /// Comma-separated methods, e.g. GET,POST.
    pub methods: Option<String>,
    pub status_class: Option<u16>,
}
impl SearchQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.query.is_empty() || self.query.len() > 1024 {
            return Err("query must contain 1–1024 UTF-8 bytes".into());
        }
        if !(1..=200).contains(&self.limit.unwrap_or(50)) {
            return Err("limit must be 1–200".into());
        }
        if self.status_class.is_some_and(|c| !(1..=5).contains(&c)) {
            return Err("statusClass must be 1–5".into());
        }
        if self.excluded_hosts.len() > 100
            || self.excluded_hosts.iter().any(|h| h.len() > 255)
            || self.host.as_ref().is_some_and(|h| h.len() > 255)
        {
            return Err("use at most 100 excluded hosts, each up to 255 bytes".into());
        }
        Ok(())
    }
    pub fn pattern(&self) -> Result<Regex, String> {
        self.validate()?;
        RegexBuilder::new(&if self.regex { self.query.clone() } else { regex::escape(&self.query) })
            .case_insensitive(!self.case_sensitive).multi_line(true).size_limit(1024 * 1024)
            .build().map_err(|_| "Invalid or overly complex regex. Live search uses Rust regex syntax; lookaround and backreferences are unsupported".into())
    }
}
fn text_body(body: &BodyPayload) -> Cow<'_, str> {
    match body.kind {
        BodyKind::None => Cow::Borrowed(""),
        BodyKind::Text | BodyKind::Truncated => Cow::Borrowed(&body.data),
        BodyKind::Base64 => STANDARD
            .decode(&body.data)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| !s.chars().any(|c| matches!(c as u32, 0..=8 | 14..=31)))
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed("")),
    }
}
fn fields(flow: &Flow, pattern: &Regex) -> Vec<&'static str> {
    let mut found = vec![];
    let mut check = |label, text: &str| {
        if !text.is_empty() && pattern.is_match(text) && !found.contains(&label) {
            found.push(label);
        }
    };
    check("URL", &flow.summary.url);
    check("Method", &flow.summary.method);
    check(
        "Status",
        &format!(
            "{} {}",
            flow.summary
                .status
                .map(|s| s.to_string())
                .unwrap_or_default(),
            flow.summary.status_text.as_deref().unwrap_or_default()
        ),
    );
    if let Some(error) = &flow.summary.error {
        check("Error", error);
    }
    if let Some(mime) = &flow.summary.mime_type {
        check("Content type", mime);
    }
    if let Some(request) = &flow.request {
        for h in &request.headers {
            check("Request header", &format!("{}: {}", h.name, h.value));
        }
        for q in &request.query {
            check("Query parameter", &format!("{}: {}", q.name, q.value));
        }
        check("Request body", &text_body(&request.body));
    }
    if let Some(response) = &flow.response {
        for h in &response.headers {
            check("Response header", &format!("{}: {}", h.name, h.value));
        }
        check("Response body", &text_body(&response.body));
    }
    for message in &flow.ws_messages {
        if message.opcode == "text" {
            check("WebSocket message", &message.data);
        }
    }
    found
}

/// Call on a blocking worker, never while holding the capture store's lock.
/// Sorting ensures afterSeq pagination also works with out-of-order insertions.
pub fn search_flows(
    mut flows: Vec<Arc<Flow>>,
    query: &SearchQuery,
) -> Result<serde_json::Value, String> {
    let pattern = query.pattern()?;
    flows.sort_by_key(|f| f.summary.seq);
    let mut matches = vec![];
    let mut scanned = 0;
    let mut next_after_seq = query.after_seq;
    let mut has_more = false;
    let started = Instant::now();
    let methods: Vec<_> = query
        .methods
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .collect();
    for flow in flows {
        let s = &flow.summary;
        if query.after_seq.is_some_and(|after| s.seq <= after) {
            continue;
        }
        if matches.len() >= query.limit.unwrap_or(50)
            || scanned >= 2000
            || started.elapsed() >= Duration::from_secs(5)
        {
            has_more = true;
            break;
        }
        scanned += 1;
        next_after_seq = Some(s.seq);
        if query
            .host
            .as_ref()
            .is_some_and(|h| !h.eq_ignore_ascii_case(&s.host))
            || query
                .excluded_hosts
                .iter()
                .any(|h| h.eq_ignore_ascii_case(&s.host))
            || (!methods.is_empty() && !methods.iter().any(|m| m.eq_ignore_ascii_case(&s.method)))
            || query
                .status_class
                .is_some_and(|c| s.status.is_none_or(|status| status / 100 != c))
        {
            continue;
        }
        let fields = fields(&flow, &pattern);
        if !fields.is_empty() {
            matches.push(serde_json::json!({"flowId":s.id,"seq":s.seq,"fields":fields}));
        }
    }
    Ok(
        serde_json::json!({"matches":matches,"scanned":scanned,"nextAfterSeq":next_after_seq,"hasMore":has_more,"contentsOmitted":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WsDirection, WsMessage};
    fn flow(seq: u64, host: &str) -> Arc<Flow> {
        let mut flow = crate::import_har(&serde_json::json!({"log":{"entries":[{
            "request":{"method":"POST","url":format!("https://{host}/needle"),"headers":[{"name":"X-Test","value":"needle header"}],"queryString":[{"name":"item","value":"needle query"}],"postData":{"mimeType":"text/plain","text":"needle request [a+b]"}},
            "response":{"status":500,"statusText":"Error","headers":[],"content":{"mimeType":"text/plain","text":"needle response"}}
        }]}})).unwrap().remove(0);
        flow.summary.seq = seq;
        Arc::new(flow)
    }
    fn query(text: &str) -> SearchQuery {
        serde_json::from_value(serde_json::json!({"query":text})).unwrap()
    }
    #[test]
    fn searches_full_content_without_returning_it_and_decodes_text() {
        let mut f = (*flow(0, "example.test")).clone();
        f.response.as_mut().unwrap().body = BodyPayload {
            kind: BodyKind::Base64,
            data: STANDARD.encode("needle café"),
            ..Default::default()
        };
        f.ws_messages.push(WsMessage {
            direction: WsDirection::Recv,
            opcode: "text".into(),
            timestamp: 0,
            data: "needle socket".into(),
            size: 13,
        });
        let result = search_flows(vec![Arc::new(f)], &query("needle")).unwrap();
        let fields = result["matches"][0]["fields"].as_array().unwrap();
        for name in [
            "URL",
            "Request header",
            "Query parameter",
            "Request body",
            "Response body",
            "WebSocket message",
        ] {
            assert!(fields.contains(&serde_json::json!(name)), "{result}");
        }
        assert!(!result.to_string().contains("needle"));
        assert_eq!(result["matches"][0]["seq"], 0);
    }
    #[test]
    fn regex_literal_case_and_invalid_patterns() {
        let f = flow(0, "example.test");
        let mut q = query("NEEDLE (request|response)");
        q.regex = true;
        assert_eq!(
            search_flows(vec![f.clone()], &q).unwrap()["matches"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        q.case_sensitive = true;
        assert_eq!(
            search_flows(vec![f.clone()], &q).unwrap()["matches"],
            serde_json::json!([])
        );
        assert_eq!(
            search_flows(vec![f.clone()], &query("[a+b]")).unwrap()["matches"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        q.query = "[".into();
        assert!(search_flows(vec![f], &q).is_err());
        assert!(query("").validate().is_err());
    }
    #[test]
    fn paginates_in_order_with_exclusions_and_empty_scan_pages() {
        let mut q = query("needle");
        q.limit = Some(1);
        q.excluded_hosts = vec!["ADS.TEST".into()];
        let data = vec![
            flow(2, "example.test"),
            flow(0, "example.test"),
            flow(1, "ads.test"),
        ];
        let first = search_flows(data.clone(), &q).unwrap();
        assert_eq!(first["matches"][0]["seq"], 0);
        assert_eq!(first["hasMore"], true);
        q.after_seq = first["nextAfterSeq"].as_u64();
        let second = search_flows(data, &q).unwrap();
        assert_eq!(second["matches"][0]["seq"], 2);
        assert_eq!(second["hasMore"], false);
        let data: Vec<_> = (0..2001).map(|i| flow(i, "ads.test")).collect();
        q.after_seq = None;
        let first = search_flows(data.clone(), &q).unwrap();
        assert_eq!(first["matches"], serde_json::json!([]));
        assert_eq!(first["nextAfterSeq"], 1999);
        assert_eq!(first["hasMore"], true);
        q.after_seq = Some(1999);
        assert_eq!(search_flows(data, &q).unwrap()["hasMore"], false);
    }
    #[test]
    fn ignores_binary_and_missing_bodies() {
        let mut f = (*flow(0, "example.test")).clone();
        f.request = None;
        f.response.as_mut().unwrap().body = BodyPayload {
            kind: BodyKind::Base64,
            data: STANDARD.encode(b"\x00binary needle"),
            ..Default::default()
        };
        f.ws_messages.push(WsMessage {
            direction: WsDirection::Recv,
            opcode: "binary".into(),
            timestamp: 0,
            data: "needle".into(),
            size: 6,
        });
        let mut q = query("needle");
        q.regex = true;
        assert_eq!(
            search_flows(vec![Arc::new(f)], &q).unwrap()["matches"][0]["fields"],
            serde_json::json!(["URL"])
        );
    }
}
