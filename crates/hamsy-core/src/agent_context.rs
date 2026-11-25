//! Redact complete selected fields before locating and cropping agent evidence.
use crate::{agent_privacy, BodyKind, BodyPayload, Flow};
use base64::{engine::general_purpose::STANDARD, Engine};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SOURCES: usize = 2000;
const MAX_MATCHES: usize = 3;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ContextField {
    #[serde(rename = "URL")]
    Url,
    Method,
    Status,
    Error,
    #[serde(rename = "Content type")]
    ContentType,
    #[serde(rename = "Request header")]
    RequestHeader,
    #[serde(rename = "Query parameter")]
    QueryParameter,
    #[serde(rename = "Request body")]
    RequestBody,
    #[serde(rename = "Response header")]
    ResponseHeader,
    #[serde(rename = "Response body")]
    ResponseBody,
    #[serde(rename = "WebSocket message")]
    WebSocketMessage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextQuery {
    /// Same query and field label returned by search_flows. Matching is applied after redaction.
    pub query: String,
    pub field: ContextField,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    /// Zero-based first WebSocket message to scan; continue using nextMessageIndex.
    pub message_index: Option<usize>,
    /// UTF-8 bytes per excerpt, default 512, maximum 4096.
    pub max_context_bytes: Option<usize>,
}
impl ContextQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.query.is_empty() || self.query.len() > 1024 {
            return Err("query must contain 1–1024 UTF-8 bytes".into());
        }
        if !(1..=4096).contains(&self.max_context_bytes.unwrap_or(512)) {
            return Err("maxContextBytes must be 1–4096".into());
        }
        if self.message_index.is_some() && self.field != ContextField::WebSocketMessage {
            return Err("messageIndex is only valid for WebSocket message context".into());
        }
        Ok(())
    }
    fn pattern(&self) -> Result<Regex, String> {
        self.validate()?;
        RegexBuilder::new(&if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        })
        .case_insensitive(!self.case_sensitive)
        .multi_line(true)
        .size_limit(1024 * 1024)
        .build()
        .map_err(|_| "Invalid or overly complex regex; live context uses Rust regex syntax".into())
    }
}

fn bounded(text: &str) -> Result<&str, String> {
    if text.len() > MAX_SOURCE_BYTES {
        return Err("Selected field exceeds the 16 MiB context scan limit".into());
    }
    Ok(text)
}
fn body_text(body: &BodyPayload) -> Result<String, String> {
    bounded(&body.data)?;
    if body.truncated || body.kind == BodyKind::Truncated {
        return Err("Context is unavailable for truncated bodies; complete structured data is required for redaction".into());
    }
    match body.kind {
        BodyKind::Text => Ok(body.data.clone()),
        BodyKind::Base64 => Ok(STANDARD
            .decode(&body.data)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|text| !text.chars().any(|c| matches!(c as u32, 0..=8 | 14..=31)))
            .unwrap_or_default()),
        _ => Ok(String::new()),
    }
}
fn pair_text(pair: &crate::HeaderPair) -> String {
    let mut value = json!(pair);
    agent_privacy::sanitize(&mut value, false, 0);
    format!(
        "{}: {}",
        value["name"].as_str().unwrap_or_default(),
        value["value"].as_str().unwrap_or_default()
    )
}
fn excerpt(text: &str, start: usize, end: usize, limit: usize) -> Value {
    let mut match_end = end.min(start + limit);
    while !text.is_char_boundary(match_end) {
        match_end -= 1;
    }
    let room = limit - (match_end - start);
    let mut before = start.saturating_sub(room / 2);
    while !text.is_char_boundary(before) {
        before += 1;
    }
    let mut after = text.len().min(match_end + room - (start - before));
    while !text.is_char_boundary(after) {
        after -= 1;
    }
    json!({"before": &text[before..start], "match": &text[start..match_end], "after": &text[match_end..after],
        "truncatedBefore": before > 0, "truncatedAfter": after < text.len(), "matchTruncated": match_end < end})
}

/// At most three excerpts and 16 MiB of selected source data. Binary and
/// truncated structured bodies are never used as a redaction fallback.
pub fn match_context(flow: &Flow, query: &ContextQuery) -> Result<Value, String> {
    let pattern = query.pattern()?;
    let mut matches = vec![];
    let mut scanned = 0;
    let mut bytes = 0;
    let mut has_more = false;
    let mut next_message_index = None;
    let mut add = |text: String,
                   message_index: Option<usize>,
                   structured: bool|
     -> Result<bool, String> {
        bounded(&text)?;
        if matches.len() >= MAX_MATCHES
            || scanned >= MAX_SOURCES
            || bytes + text.len() > MAX_SOURCE_BYTES
        {
            has_more = true;
            next_message_index = message_index;
            return Ok(false);
        }
        bytes += text.len();
        scanned += 1;
        if structured
            && matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'['))
            && serde_json::from_str::<Value>(&text).is_err()
        {
            return Err("Complete valid structured data is required for context redaction".into());
        }
        let text = if structured {
            agent_privacy::redact_body(&text, usize::MAX)
        } else {
            text
        };
        if let Some(found) = pattern.find(&text) {
            let mut context = excerpt(
                &text,
                found.start(),
                found.end(),
                query.max_context_bytes.unwrap_or(512),
            );
            context["field"] = json!(query.field);
            if let Some(index) = message_index {
                context["messageIndex"] = json!(index);
            }
            matches.push(context);
        }
        Ok(true)
    };
    match query.field {
        ContextField::Url => {
            add(
                agent_privacy::redact_url(bounded(&flow.summary.url)?),
                None,
                false,
            )?;
        }
        ContextField::Method => {
            add(bounded(&flow.summary.method)?.to_string(), None, false)?;
        }
        ContextField::Status => {
            add(
                format!(
                    "{} {}",
                    flow.summary
                        .status
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    bounded(flow.summary.status_text.as_deref().unwrap_or_default())?
                ),
                None,
                false,
            )?;
        }
        ContextField::Error => {
            add(
                bounded(flow.summary.error.as_deref().unwrap_or_default())?.to_string(),
                None,
                true,
            )?;
        }
        ContextField::ContentType => {
            add(
                bounded(flow.summary.mime_type.as_deref().unwrap_or_default())?.to_string(),
                None,
                false,
            )?;
        }
        ContextField::RequestBody | ContextField::ResponseBody => {
            let body = if query.field == ContextField::RequestBody {
                flow.request.as_ref().map(|r| &r.body)
            } else {
                flow.response.as_ref().map(|r| &r.body)
            };
            if let Some(body) = body {
                add(body_text(body)?, None, true)?;
            }
        }
        ContextField::RequestHeader
        | ContextField::ResponseHeader
        | ContextField::QueryParameter => {
            let pairs = match query.field {
                ContextField::RequestHeader => flow.request.as_ref().map(|r| &r.headers),
                ContextField::ResponseHeader => flow.response.as_ref().map(|r| &r.headers),
                _ => flow.request.as_ref().map(|r| &r.query),
            };
            if let Some(pairs) = pairs {
                let mut pair_bytes = 0;
                for pair in pairs {
                    let size = bounded(&pair.name)?.len() + bounded(&pair.value)?.len() + 2;
                    if size > MAX_SOURCE_BYTES {
                        return Err("Selected field exceeds the 16 MiB context scan limit".into());
                    }
                    if pair_bytes + size > MAX_SOURCE_BYTES {
                        has_more = true;
                        break;
                    }
                    pair_bytes += size;
                    if !add(pair_text(pair), None, false)? {
                        break;
                    }
                }
            }
        }
        ContextField::WebSocketMessage => {
            let start = query.message_index.unwrap_or(0);
            if query.message_index.is_some() && start >= flow.ws_messages.len() {
                return Err("messageIndex is outside the retained WebSocket messages".into());
            }
            for (index, message) in flow.ws_messages.iter().enumerate().skip(start) {
                if index - start >= MAX_SOURCES {
                    has_more = true;
                    next_message_index = Some(index);
                    break;
                }
                if message.opcode != "text" {
                    continue;
                }
                if message.data.len() > MAX_SOURCE_BYTES {
                    return Err("Selected field exceeds the 16 MiB context scan limit".into());
                }
                if message.size > message.data.len() as u64 {
                    return Err("Context is unavailable for truncated WebSocket messages".into());
                }
                if !add(message.data.clone(), Some(index), true)? {
                    break;
                }
            }
        }
    }
    Ok(
        json!({"flowId":flow.summary.id,"field":query.field,"matches":matches,"scanned":scanned,
        "hasMore":next_message_index.is_some(),"truncated":has_more,"nextMessageIndex":next_message_index,"redacted":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn flow() -> Flow {
        crate::import_har(&json!({"log":{"entries":[{
            "request":{"method":"POST","url":"https://user:PRIVATE@example.test/?token=PRIVATE&public=needle","headers":[{"name":"Authorization","value":"PRIVATE"}]},
            "response":{"status":200,"headers":[],"content":{"mimeType":"application/json","text":json!({"password":"PRIVATE","padding":"x".repeat(20000),"result":"café 🍋 needle result"}).to_string()}}
        }]}})).unwrap().remove(0)
    }
    fn query(field: ContextField, text: &str) -> ContextQuery {
        ContextQuery {
            field,
            query: text.into(),
            regex: false,
            case_sensitive: false,
            message_index: None,
            max_context_bytes: Some(31),
        }
    }
    #[test]
    fn late_context_is_bounded_utf8_and_redacted_before_matching() {
        let flow = flow();
        let result = match_context(&flow, &query(ContextField::ResponseBody, "needle")).unwrap();
        let context = &result["matches"][0];
        assert_eq!(context["match"], "needle");
        let text = format!(
            "{}{}{}",
            context["before"].as_str().unwrap(),
            context["match"].as_str().unwrap(),
            context["after"].as_str().unwrap()
        );
        assert!(text.len() <= 31);
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(context["truncatedBefore"], true);
        assert_eq!(
            match_context(&flow, &query(ContextField::ResponseBody, "PRIVATE")).unwrap()["matches"],
            json!([])
        );
        assert_eq!(
            match_context(&flow, &query(ContextField::RequestHeader, "PRIVATE")).unwrap()
                ["matches"],
            json!([])
        );
        assert_eq!(
            match_context(&flow, &query(ContextField::Url, "PRIVATE")).unwrap()["matches"],
            json!([])
        );
    }
    #[test]
    fn websocket_batches_continue_and_mask_structured_credentials() {
        let mut flow = flow();
        flow.ws_messages = (0..5).map(|index| crate::WsMessage { direction:crate::WsDirection::Recv,opcode:"text".into(),timestamp:0,
            data:json!({"token":"PRIVATE","padding":"x".repeat(18000),"event":format!("needle {index}")}).to_string(),size:0 }).collect();
        let mut q = query(ContextField::WebSocketMessage, "needle");
        let first = match_context(&flow, &q).unwrap();
        assert_eq!(first["matches"].as_array().unwrap().len(), 3);
        assert_eq!(first["nextMessageIndex"], 3);
        q.message_index = Some(3);
        let last = match_context(&flow, &q).unwrap();
        assert_eq!(last["matches"].as_array().unwrap().len(), 2);
        assert_eq!(last["matches"][0]["messageIndex"], 3);
        assert_eq!(last["hasMore"], false);
        q.query = "PRIVATE".into();
        q.message_index = None;
        assert_eq!(match_context(&flow, &q).unwrap()["matches"], json!([]));
    }
    #[test]
    fn oversized_metadata_is_rejected_before_redaction() {
        let mut flow = flow();
        flow.summary.url = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert!(match_context(&flow, &query(ContextField::Url, "x")).is_err());
        flow.request.as_mut().unwrap().headers[0].value = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert!(match_context(&flow, &query(ContextField::RequestHeader, "x")).is_err());
    }
    #[test]
    fn encoded_text_forms_binary_truncation_and_validation() {
        let mut flow = flow();
        let q = query(ContextField::ResponseBody, "needle");
        let body = &mut flow.response.as_mut().unwrap().body;
        body.kind = BodyKind::Base64;
        body.data = STANDARD.encode("token=PRIVATE&public=needle");
        let value = match_context(&flow, &q).unwrap();
        assert_eq!(value["matches"][0]["match"], "needle");
        assert!(!value.to_string().contains("PRIVATE"));
        flow.response.as_mut().unwrap().body.data = STANDARD.encode(b"\x00needle");
        assert_eq!(match_context(&flow, &q).unwrap()["matches"], json!([]));
        flow.response.as_mut().unwrap().body.truncated = true;
        assert!(match_context(&flow, &q).is_err());
        flow.response.as_mut().unwrap().body.truncated = false;
        flow.response.as_mut().unwrap().body.kind = BodyKind::Text;
        flow.response.as_mut().unwrap().body.data =
            r#"{"password":"PRIVATE","result":"needle"#.into();
        assert!(match_context(&flow, &q).is_err());
        let mut q = q;
        q.regex = true;
        q.query = "[".into();
        assert!(match_context(&flow, &q).is_err());
        q.regex = false;
        q.query = "x".into();
        q.max_context_bytes = Some(4097);
        assert!(q.validate().is_err());
    }
}
