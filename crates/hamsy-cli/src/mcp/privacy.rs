//! Best-effort output redaction, not a guarantee that arbitrary traffic is safe.
use reqwest::Url;
use serde_json::{json, Value};

const MASK: &str = "[REDACTED]";

fn sensitive(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "authorization",
        "cookie",
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "credential",
        "sessionid",
    ]
    .iter()
    .any(|part| key.contains(part))
}

fn redact_url(text: &str) -> String {
    let relative = text.starts_with('/');
    let parsed = if relative {
        Url::parse(&format!("http://hamsy.invalid{text}"))
    } else {
        Url::parse(text)
    };
    let Ok(mut url) = parsed else {
        return text.to_string();
    };
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return text.to_string();
    }
    if !url.username().is_empty() {
        let _ = url.set_username(MASK);
    }
    if url.password().is_some() {
        let _ = url.set_password(Some(MASK));
    }
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let val = if sensitive(&k) {
                    MASK.to_string()
                } else {
                    v.into_owned()
                };
                (k.into_owned(), val)
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    // Fragments can carry OAuth credentials; they are not sent to the server.
    if url.fragment().is_some() {
        url.set_fragment(Some(MASK));
    }
    if relative {
        url[url::Position::BeforePath..].to_string()
    } else {
        url.to_string()
    }
}

fn preview(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut boundary = limit;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…[truncated]", &text[..boundary])
}

pub(super) fn sanitize(value: &mut Value, include_bodies: bool, limit: usize) {
    match value {
        Value::Array(items) => {
            for item in items {
                sanitize(item, include_bodies, limit);
            }
        }
        Value::Object(obj) => {
            // Header/query pairs, including rule actions and HAR cookies.
            if obj
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(sensitive)
                && obj.contains_key("value")
            {
                obj.insert("value".into(), json!(MASK));
            }
            // Captured bodies (Flow's BodyData).
            if obj.contains_key("kind") && obj.contains_key("data") && obj.contains_key("size") {
                let is_text = obj.get("kind").and_then(Value::as_str) == Some("text");
                if !include_bodies || !is_text {
                    obj.insert("data".into(), json!("[body omitted]"));
                    obj.insert("agentOmitted".into(), json!(true));
                } else if let Some(text) = obj.get("data").and_then(Value::as_str) {
                    let redacted = redact_body(text, limit);
                    let truncated = redacted.len() > limit;
                    obj.insert("data".into(), json!(preview(&redacted, limit)));
                    if truncated {
                        obj.insert("agentTruncated".into(), json!(true));
                    }
                }
            }
            // HAR payloads are always omitted; these exports are inspection copies.
            if obj.contains_key("mimeType") && obj.contains_key("text") {
                obj.remove("text");
                obj.remove("encoding");
            }
            if obj.contains_key("mimeType") && obj.contains_key("params") {
                obj.remove("params");
            }
            for key in ["requestBody", "responseBody"] {
                if let Some(value) = obj
                    .get_mut(key)
                    .and_then(|condition| condition.get_mut("value"))
                {
                    *value = json!("[rule payload omitted]");
                }
            }
            let replaces_body = matches!(
                obj.get("type").and_then(Value::as_str),
                Some("replaceInRequestBody" | "replaceInResponseBody")
            );
            if matches!(
                obj.get("type").and_then(Value::as_str),
                Some("jsonPatchRequest" | "jsonPatchResponse")
            ) {
                if let Some(ops) = obj.get_mut("ops").and_then(Value::as_array_mut) {
                    for op in ops {
                        if let Some(value) = op.get_mut("value").filter(|value| !value.is_null()) {
                            *value = json!("[rule payload omitted]");
                        }
                    }
                }
            }
            for (key, item) in obj.iter_mut() {
                if sensitive(key) {
                    *item = if item.is_array() {
                        json!([])
                    } else {
                        json!(MASK)
                    };
                    continue;
                }
                if matches!(key.as_str(), "wsMessages" | "_webSocketMessages") {
                    // WS messages may be binary or arbitrarily large; omit them in beta.
                    *item = json!([]);
                    continue;
                }
                if (matches!(key.as_str(), "body" | "replacement")
                    || (replaces_body && matches!(key.as_str(), "find" | "replace")))
                    && item.is_string()
                {
                    *item = json!("[rule payload omitted]");
                    continue;
                }
                if let Value::String(text) = item {
                    if matches!(
                        key.as_str(),
                        "url"
                            | "path"
                            | "upstreamProxy"
                            | "value"
                            | "urlValue"
                            | "redirectURL"
                            | "to"
                            | "find"
                            | "replace"
                    ) {
                        *text = redact_url(text);
                    }
                }
                sanitize(item, include_bodies, limit);
            }
        }
        _ => {}
    }
}

fn redact_body(text: &str, limit: usize) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        sanitize(&mut value, false, limit);
        value.to_string()
    } else {
        // Form bodies can carry passwords/tokens even when not JSON.
        let pairs: Vec<_> = url::form_urlencoded::parse(text.as_bytes()).collect();
        if pairs.iter().any(|(key, _)| sensitive(key)) {
            let mut output = url::form_urlencoded::Serializer::new(String::new());
            for (key, value) in pairs {
                output.append_pair(&key, if sensitive(&key) { MASK } else { &value });
            }
            output.finish()
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_serialized_rule_body_replacements_and_url_actions() {
        use hamsy_core::{Action, JsonOp, JsonOpKind};

        let mut value = serde_json::to_value(vec![
            Action::ReplaceInRequestBody {
                find: "PRIVATE_REQUEST_MATCH".into(),
                replace: "PRIVATE_REQUEST_PAYLOAD".into(),
                regex: false,
            },
            Action::ReplaceInResponseBody {
                find: "PRIVATE_RESPONSE_MATCH".into(),
                replace: "PRIVATE_RESPONSE_PAYLOAD".into(),
                regex: true,
            },
            Action::Redirect {
                to: "https://user:PRIVATE_PASSWORD@example.test/?token=PRIVATE_TOKEN&item=4".into(),
            },
            Action::RewriteUrl {
                find: "https://old.test/?token=PRIVATE_OLD_TOKEN&item=4".into(),
                replace: "https://example.test/?api_key=PRIVATE_KEY&item=4".into(),
                regex: false,
            },
            Action::JsonPatchRequest {
                ops: vec![JsonOp {
                    op: JsonOpKind::Set,
                    path: "password".into(),
                    value: Some(json!("PRIVATE_PATCH_REQUEST")),
                }],
            },
            Action::JsonPatchResponse {
                ops: vec![JsonOp {
                    op: JsonOpKind::Merge,
                    path: "data".into(),
                    value: Some(json!({"unrecognized": "PRIVATE_PATCH_RESPONSE"})),
                }],
            },
        ])
        .unwrap();

        sanitize(&mut value, true, 4096);

        assert!(!value.to_string().contains("PRIVATE_"), "{value}");
        for action in value.as_array().unwrap().iter().take(2) {
            assert_eq!(action["find"], "[rule payload omitted]");
            assert_eq!(action["replace"], "[rule payload omitted]");
        }
        assert_eq!(value[0]["regex"], false);
        assert_eq!(value[1]["regex"], true);
        assert!(value[2]["to"].as_str().unwrap().contains("item=4"));
        assert!(value[3]["find"].as_str().unwrap().contains("item=4"));
        assert!(value[3]["replace"].as_str().unwrap().contains("item=4"));
        assert_eq!(value[4]["ops"][0]["path"], "password");
        assert_eq!(value[5]["ops"][0]["op"], "merge");
        assert_eq!(value[4]["ops"][0]["value"], "[rule payload omitted]");
        assert_eq!(value[5]["ops"][0]["value"], "[rule payload omitted]");
    }

    #[test]
    fn omits_serialized_body_conditions_but_preserves_operators() {
        use hamsy_core::{BodyCond, BodyCondOp, Matcher};

        let mut value = serde_json::to_value(Matcher {
            request_body: Some(BodyCond {
                op: BodyCondOp::Contains,
                value: "PRIVATE_REQUEST_MATCH".into(),
            }),
            response_body: Some(BodyCond {
                op: BodyCondOp::Regex,
                value: "PRIVATE_RESPONSE_MATCH".into(),
            }),
            ..Default::default()
        })
        .unwrap();

        sanitize(&mut value, true, 4096);

        assert!(!value.to_string().contains("PRIVATE_"), "{value}");
        assert_eq!(value["requestBody"]["op"], "contains");
        assert_eq!(value["responseBody"]["op"], "regex");
        assert_eq!(value["requestBody"]["value"], "[rule payload omitted]");
        assert_eq!(value["responseBody"]["value"], "[rule payload omitted]");
    }

    #[test]
    fn redacts_nested_credentials_originals_and_har_without_touching_unrelated_values() {
        let mut value = json!({
            "url":"https://user:PASS@example.test/?access_token=TOKEN&item=4#FRAGMENT",
            "path":"/path?api_key=KEY&item=4",
            "originalRequest":{"headers":[{"name":"pRoXy-AuThOrIzAtIoN","value":"AUTH"}],
                "body":{"kind":"text","size":40,"data":"{\"nested\":{\"clientSecret\":\"SECRET\"},\"ok\":true}"}},
            "response":{"body":{"kind":"text","size":40,"data":"password=PASSWORD&message=hello"}},
            "wsMessages":[{"data":"WS_SECRET"}],
            "cookies":[{"name":"custom","value":"COOKIE"}],
            "content":{"mimeType":"text/plain","text":"HAR_SECRET"},
            "redirectURL":"https://example.test/?token=REDIRECT_SECRET"
        });
        sanitize(&mut value, true, 4096);
        let serialized = value.to_string();
        for secret in [
            "PASS", "TOKEN", "FRAGMENT", "KEY", "AUTH", "SECRET", "PASSWORD", "COOKIE",
        ] {
            assert!(
                !serialized.contains(secret),
                "leaked {secret}: {serialized}"
            );
        }
        assert!(serialized.contains("item=4"));
        assert!(serialized.contains("hello"));
        assert!(value["cookies"].is_array());
        assert_eq!(value["wsMessages"], json!([]));
    }
    #[test]
    fn previews_respect_utf8_and_omit_binary_by_default() {
        let mut value = json!({"body":{"kind":"text","data":"🍋🍋🍋","size":12}});
        sanitize(&mut value, true, 5);
        assert_eq!(value["body"]["data"], "🍋…[truncated]");
        assert_eq!(value["body"]["agentTruncated"], true);
        let mut value = json!({"body":{"kind":"base64","data":"SECRET","size":6}});
        sanitize(&mut value, true, 4096);
        assert_eq!(value["body"]["agentOmitted"], true);
        assert!(!value.to_string().contains("SECRET"));
    }
}
