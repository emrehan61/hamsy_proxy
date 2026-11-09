//! Pure, isolated rule evaluation. Reports metadata only; never sleeps or sends traffic.
use crate::{
    Action, BodyKind, BodyPayload, Flow, FlowState, RequestCtx, ResponseCtx, Rule, RuleSet,
};
use serde_json::{json, Value};
use url::Url;

fn incomplete(body: &BodyPayload, known_size: u64) -> bool {
    let decoded = crate::from_payload(body);
    body.truncated
        || body.kind == BodyKind::Truncated
        || body.size > decoded.len() as u64
        || (decoded.is_empty() && known_size > 0)
        || (body.kind == BodyKind::Base64 && !body.data.is_empty() && decoded.is_empty())
}

fn skipped(reason: &str) -> Value {
    json!({"evaluated":false,"reason":reason})
}

pub fn preview_rule(flow: &Flow, mut rule: Rule) -> Result<Value, String> {
    if rule.name.trim().is_empty() {
        return Err("Rule name must not be empty.".into());
    }
    let serialized = serde_json::to_string(&rule).map_err(|_| "Invalid proposed rule")?;
    if serialized.len() > 128 * 1024 {
        return Err("Proposed rule exceeds 128 KiB.".into());
    }
    if serialized.contains("[REDACTED]") || serialized.contains("[rule payload omitted]") {
        return Err("Supply explicit proposed rule values, not masked values.".into());
    }
    let enabled = rule.enabled;
    rule.enabled = true;
    rule.id = "preview".into();
    let rules = RuleSet::new(vec![rule.clone()]);
    if !rules.errors().is_empty() {
        return Err("Rule has invalid regex/glob conditions or actions.".into());
    }
    let mut result = json!({
        "flowId":flow.summary.id,"sessionId":"app:live","mode":"isolated_rule",
        "ruleEnabled":enabled,"payloadsOmitted":true,"persisted":false,
        "input":{"request":if flow.original_request.is_some() {"original"} else {"captured"},
            "response":if flow.original_response.is_some() {"original"} else {"captured"}},
        "request":skipped("rule_disabled"),"response":skipped("rule_disabled"),
        "limitations":["Only the proposed rule is evaluated; interactions with other rules are excluded.",
            "Response evaluation reuses the retained response; a changed upstream request may produce a different real response.",
            "Delay and throttle effects are reported without waiting or sending traffic."]
    });
    if !enabled {
        return Ok(result);
    }
    let Some(request) = flow.original_request.as_ref().or(flow.request.as_ref()) else {
        result["request"] = skipped("request_unavailable");
        result["response"] = skipped("request_unavailable");
        return Ok(result);
    };
    let url = Url::parse(&request.url).map_err(|_| "Captured request has an invalid URL.")?;
    let request_body = crate::from_payload(&request.body);
    let resource_type = crate::ResourceType::infer(
        request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-type"))
            .map(|header| header.value.as_str()),
        url.path(),
    );
    let request_context = || RequestCtx {
        method: &request.method,
        url: &url,
        headers: &request.headers,
        body: Some(&request_body),
        resource_type,
    };
    let body_required = rules.needs_request_body(request_context());
    if body_required
        && (incomplete(
            &request.body,
            if flow.original_request.is_some() {
                request.body.size
            } else {
                flow.summary.request_size
            },
        ) || flow.summary.state != FlowState::Complete)
    {
        result["request"] = skipped("request_body_incomplete");
        result["response"] = skipped("request_phase_unevaluated");
        return Ok(result);
    }
    let outcome = rules.apply_request(request_context());
    let request_action = |action: &Action| {
        !matches!(
            action,
            Action::SetResponseHeader { .. }
                | Action::RemoveResponseHeader { .. }
                | Action::SetResponseBody { .. }
                | Action::ReplaceInResponseBody { .. }
                | Action::JsonPatchResponse { .. }
                | Action::SetStatus { .. }
        )
    };
    let response_action = |action: &Action| {
        matches!(
            action,
            Action::SetResponseHeader { .. }
                | Action::RemoveResponseHeader { .. }
                | Action::SetResponseBody { .. }
                | Action::ReplaceInResponseBody { .. }
                | Action::JsonPatchResponse { .. }
                | Action::SetStatus { .. }
                | Action::Delay { .. }
                | Action::Throttle { .. }
                | Action::Block { .. }
        )
    };
    result["request"] = json!({"evaluated":true,"matched":!outcome.matched.is_empty(),
        "modified":outcome.modified,"blocked":outcome.blocked.is_some(),
        "mocked":outcome.mocked.as_ref().map(|mock| json!({"status":mock.status,"bodyBytes":mock.body.len(),"delayMs":mock.delay_ms})),
        "delayMs":outcome.delay_ms,"throttleBytesPerSecond":outcome.throttle_bps,
        "changes":{"method":outcome.method != request.method,"url":outcome.url != request.url,
            "headers":outcome.headers != request.headers,"body":outcome.body.as_deref() != Some(request_body.as_slice())},
        "skippedActionIndices":rule.actions.iter().enumerate().filter_map(|(i,a)| (!request_action(a)).then_some(i)).collect::<Vec<_>>()});
    let skip = if outcome.blocked.is_some() {
        Some("request_blocked")
    } else if outcome.mocked.is_some() {
        Some("mocked_response")
    } else {
        None
    };
    if let Some(reason) = skip {
        result["response"] = skipped(reason);
        return Ok(result);
    }
    let Some(response) = flow.original_response.as_ref().or(flow.response.as_ref()) else {
        result["response"] = skipped("response_unavailable");
        return Ok(result);
    };
    let response_body = crate::from_payload(&response.body);
    let final_url =
        Url::parse(&outcome.url).map_err(|_| "Rule produced an invalid request URL.")?;
    // The HTTP proxy rechecks response rules with the original method/body and
    // request-phase rewritten URL/headers (http.rs apply_response contexts).
    let response_context = || ResponseCtx {
        method: &request.method,
        url: &final_url,
        headers: &outcome.headers,
        body: Some(&request_body),
        resource_type,
        status: response.status,
        resp_headers: &response.headers,
        resp_body: Some(&response_body),
    };
    if rules.needs_response_body(response_context())
        && (incomplete(
            &response.body,
            if flow.original_response.is_some() {
                response.body.size
            } else {
                flow.summary.response_size
            },
        ) || flow.summary.state != FlowState::Complete)
    {
        result["response"] = skipped("response_body_incomplete");
        return Ok(result);
    }
    let response_outcome = rules.apply_response(response_context());
    result["response"] = json!({"evaluated":true,"matched":!response_outcome.matched.is_empty(),
        "modified":response_outcome.modified,"statusBefore":response.status,"statusAfter":response_outcome.status,
        "delayMs":response_outcome.delay_ms,"throttleBytesPerSecond":response_outcome.throttle_bps,
        "changes":{"status":response_outcome.status != response.status,"headers":response_outcome.headers != response.headers,
            "body":response_outcome.body.as_deref() != Some(response_body.as_slice())},
        "skippedActionIndices":rule.actions.iter().enumerate().filter_map(|(i,a)| (!response_action(a)).then_some(i)).collect::<Vec<_>>()});
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> Flow {
        crate::import_har(&json!({"log":{"entries":[{
            "request":{"method":"GET","url":"https://example.test/api","headers":[],"bodySize":12},
            "response":{"status":200,"headers":[{"name":"Content-Type","value":"application/json"}],"content":{"mimeType":"application/json","text":"{}"}}
        }]}})).unwrap().remove(0)
    }

    #[test]
    fn request_resource_type_does_not_use_response_mime_classification() {
        let flow = flow();
        assert_eq!(flow.summary.resource_type, crate::ResourceType::Json);
        let rule = serde_json::from_value(
            json!({"name":"json only","match":{"resourceTypes":["json"]},
            "actions":[{"type":"setRequestHeader","name":"X-Test","value":"yes"}]}),
        )
        .unwrap();
        let preview = preview_rule(&flow, rule).unwrap();
        assert_eq!(preview["request"]["matched"], false);
        assert_eq!(preview["response"]["matched"], false);
    }

    #[test]
    fn missing_retained_body_is_not_an_empty_body_match() {
        let mut flow = flow();
        flow.request.as_mut().unwrap().body.size = 12;
        let rule = serde_json::from_value(
            json!({"name":"empty body","match":{"requestBody":{"op":"equals","value":""}},
            "actions":[{"type":"block"}]}),
        )
        .unwrap();
        let preview = preview_rule(&flow, rule).unwrap();
        assert_eq!(preview["request"]["reason"], "request_body_incomplete");
        assert_eq!(preview["response"]["reason"], "request_phase_unevaluated");
    }
}
