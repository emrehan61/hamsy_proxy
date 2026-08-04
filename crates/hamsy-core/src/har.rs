//! HAR 1.2 export/import.
//!
//! See <http://www.softwareishard.com/blog/har-12-spec/> for the format.
//! Exported entries carry an `_hamsy` extension object
//! (`{matchedRules, modified, flowId, resourceType}`) so hamsy-proxy-exported
//! HAR files round-trip losslessly through [`import_har`]; HAR files from
//! other tools (which lack that extension) still import, with hamsy-proxy's
//! metadata falling back to sensible defaults.

use serde_json::Value;
use time::format_description::well_known::Rfc3339;

use crate::error::{CoreError, Result};
use crate::flow::{
    BodyKind, BodyPayload, Flow, HeaderPair, RequestRecord, ResourceType, ResponseRecord,
};

/// Exports `flows` as a HAR 1.2 document.
pub fn export_har(flows: &[Flow], creator_version: &str) -> Value {
    let entries: Vec<Value> = flows.iter().map(export_entry).collect();
    serde_json::json!({
        "log": {
            "version": "1.2",
            "creator": {"name": "hamsy-proxy", "version": creator_version},
            "browser": {"name": "hamsy-proxy", "version": creator_version},
            "pages": [],
            "entries": entries,
        }
    })
}

fn header_to_json(h: &HeaderPair) -> Value {
    serde_json::json!({"name": h.name, "value": h.value})
}

/// Parses a `Cookie` request header into a list of HAR cookie objects.
fn parse_cookie_header(value: &str) -> Vec<Value> {
    value
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            if pair.is_empty() {
                return None;
            }
            let (name, val) = pair.split_once('=')?;
            Some(serde_json::json!({"name": name.trim(), "value": val.trim()}))
        })
        .collect()
}

/// Parses a single `Set-Cookie` response header into a HAR cookie object.
fn parse_set_cookie_header(value: &str) -> Value {
    let mut parts = value.split(';');
    let first = parts.next().unwrap_or("").trim();
    let (name, val) = first.split_once('=').unwrap_or((first, ""));

    let mut path: Option<String> = None;
    let mut domain: Option<String> = None;
    let mut expires: Option<String> = None;
    let mut http_only = false;
    let mut secure = false;

    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            match k.trim().to_ascii_lowercase().as_str() {
                "path" => path = Some(v.trim().to_string()),
                "domain" => domain = Some(v.trim().to_string()),
                "expires" => expires = Some(v.trim().to_string()),
                _ => {}
            }
        } else {
            match part.to_ascii_lowercase().as_str() {
                "httponly" => http_only = true,
                "secure" => secure = true,
                _ => {}
            }
        }
    }

    serde_json::json!({
        "name": name.trim(),
        "value": val.trim(),
        "path": path,
        "domain": domain,
        "expires": expires,
        "httpOnly": http_only,
        "secure": secure,
    })
}

fn format_start_time(ms: i64) -> String {
    let nanos = i128::from(ms) * 1_000_000;
    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn parse_started_date_time(s: &str) -> Option<i64> {
    let dt = time::OffsetDateTime::parse(s, &Rfc3339).ok()?;
    Some(dt.unix_timestamp() * 1000 + i64::from(dt.millisecond()))
}

fn split_server_addr(addr: Option<&str>) -> (Value, Value) {
    match addr {
        Some(a) => match a.rsplit_once(':') {
            Some((ip, port)) => (
                Value::String(ip.to_string()),
                Value::String(port.to_string()),
            ),
            None => (Value::String(a.to_string()), Value::Null),
        },
        None => (Value::Null, Value::Null),
    }
}

fn export_entry(flow: &Flow) -> Value {
    let started = format_start_time(flow.summary.started_at);
    let time_ms = flow.summary.duration_ms.unwrap_or(0) as f64;

    let request = flow.request.as_ref();
    let response = flow.response.as_ref();

    let req_headers: Vec<Value> = request
        .map(|r| r.headers.iter().map(header_to_json).collect())
        .unwrap_or_default();
    let req_cookies: Vec<Value> = request
        .and_then(|r| {
            r.headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("cookie"))
        })
        .map(|h| parse_cookie_header(&h.value))
        .unwrap_or_default();
    let query_string: Vec<Value> = request
        .map(|r| r.query.iter().map(header_to_json).collect())
        .unwrap_or_default();

    let mut request_json = serde_json::json!({
        "method": flow.summary.method,
        "url": flow.summary.url,
        "httpVersion": flow.summary.http_version,
        "cookies": req_cookies,
        "headers": req_headers,
        "queryString": query_string,
        "headersSize": -1,
        "bodySize": request.map(|r| r.body.size).unwrap_or(0),
    });
    if let (Some(r), Some(obj)) = (request, request_json.as_object_mut()) {
        if r.body.kind != BodyKind::None {
            let mime = r
                .headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.clone())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            obj.insert(
                "postData".to_string(),
                serde_json::json!({"mimeType": mime, "text": r.body.data}),
            );
        }
    }

    let resp_headers: Vec<Value> = response
        .map(|r| r.headers.iter().map(header_to_json).collect())
        .unwrap_or_default();
    let resp_cookies: Vec<Value> = response
        .map(|r| {
            r.headers
                .iter()
                .filter(|h| h.name.eq_ignore_ascii_case("set-cookie"))
                .map(|h| parse_set_cookie_header(&h.value))
                .collect()
        })
        .unwrap_or_default();
    let redirect_url = response
        .and_then(|r| {
            r.headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("location"))
        })
        .map(|h| h.value.clone())
        .unwrap_or_default();

    let mut content = serde_json::json!({
        "size": response.map(|r| r.body.size).unwrap_or(0),
        "mimeType": flow.summary.mime_type.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
        // We only track the decoded (logical) body size, not the original
        // wire-compressed size, so we cannot compute real bytes-saved here.
        "compression": 0,
    });
    if let (Some(r), Some(obj)) = (response, content.as_object_mut()) {
        if r.body.kind != BodyKind::None {
            obj.insert("text".to_string(), Value::String(r.body.data.clone()));
            if r.body.kind == BodyKind::Base64 {
                obj.insert("encoding".to_string(), Value::String("base64".to_string()));
            }
        }
    }

    let response_json = serde_json::json!({
        "status": response.map(|r| r.status).unwrap_or(0),
        "statusText": response.map(|r| r.status_text.clone()).unwrap_or_default(),
        "httpVersion": response.map(|r| r.http_version.clone()).unwrap_or_else(|| flow.summary.http_version.clone()),
        "cookies": resp_cookies,
        "headers": resp_headers,
        "content": content,
        "redirectURL": redirect_url,
        "headersSize": -1,
        "bodySize": response.map(|r| r.body.size).unwrap_or(0),
    });

    let (server_ip, connection) = split_server_addr(flow.server_addr.as_deref());

    serde_json::json!({
        "startedDateTime": started,
        "time": time_ms,
        "request": request_json,
        "response": response_json,
        "cache": {},
        "timings": {
            "blocked": flow.timings.blocked,
            "dns": flow.timings.dns,
            "connect": flow.timings.connect,
            "ssl": flow.timings.ssl,
            "send": flow.timings.send,
            "wait": flow.timings.wait,
            "receive": flow.timings.receive,
        },
        "serverIPAddress": server_ip,
        "connection": connection,
        // Custom (non-HAR-spec) extension: the resolved originating app, if
        // any. Kept as a top-level `_app` field (rather than nested inside
        // `_hamsy`) per the leading-underscore convention HAR tools use for
        // their own extension fields.
        "_app": flow.summary.app,
        "_hamsy": {
            "matchedRules": flow.summary.matched_rules,
            "modified": flow.summary.modified,
            "flowId": flow.summary.id.to_string(),
            "resourceType": serde_json::to_value(flow.summary.resource_type).unwrap_or(Value::Null),
        },
    })
}

fn parse_har_headers(value: Option<&Value>) -> Vec<HeaderPair> {
    value
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let name = item.get("name")?.as_str()?.to_string();
                    let val = item.get("value")?.as_str()?.to_string();
                    Some(HeaderPair::new(name, val))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Builds a [`BodyPayload`] from a HAR `request.postData` object. HAR does
/// not define a binary encoding for request bodies, so `text` is always
/// treated as literal content (re-classified as text/base64 via
/// [`crate::body::to_payload`] based on its own bytes and `mimeType`).
fn parse_post_data(value: Option<&Value>) -> Option<BodyPayload> {
    let obj = value?;
    let mime = obj
        .get("mimeType")
        .and_then(|v| v.as_str())
        .map(String::from);
    let text = obj.get("text").and_then(|v| v.as_str())?;
    Some(crate::body::to_payload(
        text.as_bytes(),
        mime.as_deref(),
        None,
        usize::MAX,
    ))
}

/// Builds a [`BodyPayload`] from a HAR `response.content` object, honoring
/// the optional `encoding: "base64"` field per the HAR 1.2 spec.
fn parse_content(value: Option<&Value>) -> Option<BodyPayload> {
    let obj = value?;
    let mime = obj
        .get("mimeType")
        .and_then(|v| v.as_str())
        .map(String::from);
    let text = obj.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        return Some(BodyPayload::default());
    }
    let encoding = obj.get("encoding").and_then(|v| v.as_str());
    if encoding == Some("base64") {
        let size = obj.get("size").and_then(Value::as_u64).unwrap_or(0);
        Some(BodyPayload {
            kind: BodyKind::Base64,
            data: text.to_string(),
            size,
            truncated: false,
            encoding: None,
        })
    } else {
        Some(crate::body::to_payload(
            text.as_bytes(),
            mime.as_deref(),
            None,
            usize::MAX,
        ))
    }
}

fn parse_entry(entry: &Value, seq: u64) -> Option<Flow> {
    let request = entry.get("request")?;
    let response = entry.get("response")?;

    let method = request.get("method")?.as_str()?.to_string();
    let url_str = request.get("url")?.as_str()?.to_string();
    let http_version = request
        .get("httpVersion")
        .and_then(|v| v.as_str())
        .unwrap_or("HTTP/1.1")
        .to_string();
    let req_headers = parse_har_headers(request.get("headers"));
    let query = parse_har_headers(request.get("queryString"));
    let req_body = parse_post_data(request.get("postData")).unwrap_or_default();

    let status = response.get("status").and_then(Value::as_u64).unwrap_or(0) as u16;
    let status_text = response
        .get("statusText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let resp_http_version = response
        .get("httpVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(&http_version)
        .to_string();
    let resp_headers = parse_har_headers(response.get("headers"));
    let resp_body = parse_content(response.get("content")).unwrap_or_default();

    let parsed_url = url::Url::parse(&url_str).ok();
    let scheme = parsed_url
        .as_ref()
        .map(|u| u.scheme().to_string())
        .unwrap_or_else(|| "http".to_string());
    let host = parsed_url
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or("")
        .to_string();
    let port = parsed_url
        .as_ref()
        .and_then(|u| u.port_or_known_default())
        .unwrap_or(0);
    let path = parsed_url
        .as_ref()
        .map(|u| {
            let mut p = u.path().to_string();
            if let Some(q) = u.query() {
                p.push('?');
                p.push_str(q);
            }
            p
        })
        .unwrap_or_default();

    let ext = entry.get("_hamsy");
    let flow_id = ext
        .and_then(|e| e.get("flowId"))
        .and_then(|v| v.as_str())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);
    let matched_rules = ext
        .and_then(|e| e.get("matchedRules"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let modified = ext
        .and_then(|e| e.get("modified"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let resource_type: Option<ResourceType> = ext
        .and_then(|e| e.get("resourceType"))
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let started_at = entry
        .get("startedDateTime")
        .and_then(|v| v.as_str())
        .and_then(parse_started_date_time)
        .unwrap_or(0);
    let duration_ms = entry
        .get("time")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .max(0.0) as i64;

    let request_record = RequestRecord {
        method: method.clone(),
        url: url_str.clone(),
        http_version: http_version.clone(),
        headers: req_headers,
        body: req_body,
        query,
    };
    let response_record = ResponseRecord {
        status,
        status_text,
        http_version: resp_http_version,
        headers: resp_headers,
        body: resp_body,
    };

    let mut flow = Flow::new_request(
        flow_id,
        seq,
        started_at,
        method,
        scheme,
        host,
        port,
        path,
        url_str,
        http_version,
        String::new(),
        request_record,
    );
    flow.mark_complete(response_record, started_at + duration_ms);
    flow.summary.matched_rules = matched_rules;
    flow.summary.modified = modified;
    if let Some(rt) = resource_type {
        flow.summary.resource_type = rt;
    }
    flow.summary.app = entry
        .get("_app")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(flow)
}

/// Imports flows from a HAR 1.2 document previously produced by
/// [`export_har`] (or, on a best-effort basis, any spec-conformant HAR
/// file).
pub fn import_har(v: &Value) -> Result<Vec<Flow>> {
    let entries = v
        .get("log")
        .and_then(|l| l.get("entries"))
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::Codec("HAR document missing log.entries array".to_string()))?;

    Ok(entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| parse_entry(entry, i as u64))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{BodyKind, HeaderPair};

    fn sample_flow() -> Flow {
        let request = RequestRecord {
            method: "POST".to_string(),
            url: "http://example.com/api/items?x=1".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![HeaderPair::new("Content-Type", "text/plain")],
            body: BodyPayload {
                kind: BodyKind::Text,
                data: "hello".to_string(),
                size: 5,
                truncated: false,
                encoding: None,
            },
            query: vec![HeaderPair::new("x", "1")],
        };
        let mut flow = Flow::new_request(
            uuid::Uuid::new_v4(),
            1,
            1_700_000_000_000,
            "POST",
            "http",
            "example.com",
            80,
            "/api/items?x=1",
            "http://example.com/api/items?x=1",
            "HTTP/1.1",
            "127.0.0.1:5555",
            request,
        );
        let response = ResponseRecord {
            status: 200,
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            headers: vec![HeaderPair::new("Content-Type", "application/json")],
            body: BodyPayload {
                kind: BodyKind::Text,
                data: "{\"ok\":true}".to_string(),
                size: 12,
                truncated: false,
                encoding: None,
            },
        };
        flow.mark_complete(response, 1_700_000_000_500);
        flow.summary.matched_rules = vec!["rule-1".to_string()];
        flow.summary.modified = true;
        flow.summary.app = Some("curl".to_string());
        flow
    }

    #[test]
    fn export_produces_valid_har_shape() {
        let flow = sample_flow();
        let har = export_har(std::slice::from_ref(&flow), "0.1.0");
        assert_eq!(har["log"]["version"], "1.2");
        assert_eq!(har["log"]["creator"]["name"], "hamsy-proxy");
        let entries = har["log"]["entries"].as_array().expect("entries array");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["request"]["method"], "POST");
        assert_eq!(entries[0]["response"]["status"], 200);
    }

    #[test]
    fn round_trip_preserves_key_fields() {
        let flow = sample_flow();
        let har = export_har(std::slice::from_ref(&flow), "0.1.0");
        let imported = import_har(&har).unwrap();
        assert_eq!(imported.len(), 1);
        let round_tripped = &imported[0];

        assert_eq!(round_tripped.summary.id, flow.summary.id);
        assert_eq!(round_tripped.summary.method, "POST");
        assert_eq!(round_tripped.summary.url, flow.summary.url);
        assert_eq!(round_tripped.summary.status, Some(200));
        assert!(round_tripped.summary.modified);
        assert_eq!(
            round_tripped.summary.matched_rules,
            vec!["rule-1".to_string()]
        );
        assert_eq!(round_tripped.summary.app.as_deref(), Some("curl"));

        let req = round_tripped.request.as_ref().unwrap();
        assert_eq!(req.body.data, "hello");
        let resp = round_tripped.response.as_ref().unwrap();
        assert_eq!(resp.body.data, "{\"ok\":true}");
    }

    #[test]
    fn round_trip_with_no_resolved_app_stays_none() {
        let mut flow = sample_flow();
        flow.summary.app = None;
        let har = export_har(std::slice::from_ref(&flow), "0.1.0");
        assert!(har["log"]["entries"][0]["_app"].is_null());
        let imported = import_har(&har).unwrap();
        assert_eq!(imported[0].summary.app, None);
    }

    #[test]
    fn import_rejects_missing_entries() {
        let bad = serde_json::json!({"log": {}});
        assert!(import_har(&bad).is_err());
    }
}
