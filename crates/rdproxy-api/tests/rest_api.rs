//! In-process REST API tests, driven via `tower::ServiceExt::oneshot`
//! against `rdproxy_api::router` (no real sockets involved).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rdproxy_api::router;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("valid json body")
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn state_shape() {
    let state = common::make_state();
    let app = router(state);

    let resp = app.oneshot(get("/api/state")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    for key in [
        "version",
        "proxyPort",
        "uiPort",
        "capturing",
        "paused",
        "flowCount",
        "caFingerprint",
        "uptimeSecs",
        "systemProxy",
    ] {
        assert!(body.get(key).is_some(), "missing key {key} in {body}");
    }
    assert_eq!(body["paused"], false);
    assert_eq!(body["capturing"], true);
    assert_eq!(body["flowCount"], 0);
    let sys = &body["systemProxy"];
    assert!(sys.get("enabled").is_some());
    assert!(sys.get("platform").is_some());
    assert!(sys.get("supported").is_some());
}

#[tokio::test]
async fn flows_list_filters_and_after_seq() {
    let state = common::make_state();
    state
        .flows()
        .insert(common::sample_flow(1, "GET", "a.com", Some(200)));
    state
        .flows()
        .insert(common::sample_flow(2, "POST", "b.com", Some(404)));
    state
        .flows()
        .insert(common::sample_flow(3, "GET", "a.com", Some(500)));
    let app = router(state);

    // No filters: all three.
    let resp = app.clone().oneshot(get("/api/flows")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let flows = body["flows"].as_array().expect("flows array");
    assert_eq!(flows.len(), 3);

    // Method filter.
    let resp = app
        .clone()
        .oneshot(get("/api/flows?methods=POST"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["flows"].as_array().unwrap().len(), 1);

    // Host filter.
    let resp = app
        .clone()
        .oneshot(get("/api/flows?host=a.com"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["flows"].as_array().unwrap().len(), 2);

    // statusClass filter.
    let resp = app
        .clone()
        .oneshot(get("/api/flows?statusClass=4"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["flows"].as_array().unwrap().len(), 1);

    // afterSeq + limit.
    let resp = app
        .clone()
        .oneshot(get("/api/flows?afterSeq=1&limit=1"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let flows = body["flows"].as_array().unwrap();
    assert_eq!(flows.len(), 1);
    assert!(flows[0]["seq"].as_u64().unwrap() > 1);

    // resourceTypes: none of these flows resolve to "script".
    let resp = app
        .clone()
        .oneshot(get("/api/flows?resourceTypes=script"))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["flows"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn flow_get_by_id_404_for_unknown_uuid() {
    let state = common::make_state();
    let app = router(state);
    let random_id = uuid::Uuid::new_v4();

    let resp = app
        .oneshot(get(&format!("/api/flows/{random_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert!(body.get("error").is_some());
    assert!(body.get("detail").is_some());
}

#[tokio::test]
async fn flow_get_by_id_returns_full_flow() {
    let state = common::make_state();
    let flow = common::sample_flow(1, "GET", "a.com", Some(200));
    let id = flow.summary.id;
    state.flows().insert(flow);
    let app = router(state);

    let resp = app.oneshot(get(&format!("/api/flows/{id}"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["id"], id.to_string());
    assert!(body.get("request").is_some());
}

#[tokio::test]
async fn delete_flows_clears_store() {
    let state = common::make_state();
    state
        .flows()
        .insert(common::sample_flow(1, "GET", "a.com", Some(200)));
    assert_eq!(state.flows().len(), 1);
    let app = router(state.clone());

    let resp = app.oneshot(delete("/api/flows")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(state.flows().len(), 0);
}

#[tokio::test]
async fn replay_404_for_unknown_flow() {
    let state = common::make_state();
    let app = router(state);
    let random_id = uuid::Uuid::new_v4();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/flows/{random_id}/replay"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn replay_501_when_no_backend_attached() {
    let state = common::make_state();
    let flow = common::sample_flow(1, "GET", "a.com", Some(200));
    let id = flow.summary.id;
    state.flows().insert(flow);
    let app = router(state);

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/flows/{id}/replay"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
}

fn sample_rule(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "enabled": true,
        "priority": 0,
        "group": null,
        "notes": null,
        "match": {},
        "actions": [],
    })
}

#[tokio::test]
async fn rules_crud_toggle_reorder_round_trip() {
    let state = common::make_state();
    let app = router(state);

    // Create with server-assigned id (empty id in body).
    let resp = app
        .clone()
        .oneshot(json_req("POST", "/api/rules", sample_rule("", "first")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert!(!id.is_empty());

    // List contains it, wrapped in {"rules": [...]}.
    let resp = app.clone().oneshot(get("/api/rules")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp).await;
    assert_eq!(list["rules"].as_array().unwrap().len(), 1);

    // Create a second rule with an explicit id.
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/rules",
            sample_rule("second-id", "second"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Toggle flips enabled.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/rules/{id}/toggle"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let toggled = body_json(resp).await;
    assert_eq!(toggled["enabled"], false);

    // Update replaces it.
    let mut updated_body = sample_rule(&id, "renamed");
    updated_body["enabled"] = json!(true);
    let resp = app
        .clone()
        .oneshot(json_req("PUT", &format!("/api/rules/{id}"), updated_body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated = body_json(resp).await;
    assert_eq!(updated["name"], "renamed");

    // Update on unknown id is 404.
    let resp = app
        .clone()
        .oneshot(json_req("PUT", "/api/rules/nope", sample_rule("nope", "x")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Reorder.
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/rules/reorder",
            json!({"ids": ["second-id", id.clone()]}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app.clone().oneshot(get("/api/rules")).await.unwrap();
    let list = body_json(resp).await;
    assert_eq!(list["rules"][0]["id"], "second-id");

    // Export.
    let resp = app.clone().oneshot(get("/api/rules/export")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let exported = body_json(resp).await;
    assert_eq!(exported["rules"].as_array().unwrap().len(), 2);

    // Delete.
    let resp = app
        .clone()
        .oneshot(delete(&format!("/api/rules/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app
        .clone()
        .oneshot(delete(&format!("/api/rules/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Import (replace).
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/rules/import",
            json!({"rules": [sample_rule("imported-1", "imp1")], "replace": true}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["imported"], 1);
    let resp = app.clone().oneshot(get("/api/rules")).await.unwrap();
    let list = body_json(resp).await;
    assert_eq!(list["rules"].as_array().unwrap().len(), 1);

    // Import (merge, replace: false) appends/overwrites by id.
    let resp = app
        .clone()
        .oneshot(json_req(
            "POST",
            "/api/rules/import",
            json!({"rules": [sample_rule("imported-2", "imp2")], "replace": false}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app.clone().oneshot(get("/api/rules")).await.unwrap();
    let list = body_json(resp).await;
    assert_eq!(list["rules"].as_array().unwrap().len(), 2);
}

/// Contract test for the confirmed bug: `GET /api/rules` must return
/// `{"rules": [...]}`, not a bare `Rule[]` array — `ui/src/lib/api.ts`'s
/// `listRules()` reads `data.rules`, which would be `undefined` (and the
/// Rules page would silently list nothing) against a bare-array response.
#[tokio::test]
async fn rules_list_returns_rules_wrapper_object() {
    let state = common::make_state();
    let app = router(state);

    let create_resp = app
        .clone()
        .oneshot(json_req("POST", "/api/rules", sample_rule("r1", "one")))
        .await
        .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);

    let resp = app.oneshot(get("/api/rules")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let obj = body
        .as_object()
        .expect("must be a JSON object, not a bare array");
    assert_eq!(
        obj.keys().collect::<Vec<_>>(),
        vec!["rules"],
        "unexpected top-level keys: {body}"
    );
    let rules = body["rules"].as_array().expect("`rules` must be an array");
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], "r1");
}

/// `POST /api/rules` must accept a body that omits the `id` field entirely
/// (not just one where `id` is present-but-empty) and server-assign a UUID.
/// `ui/src/pages/rules/templates.ts`'s "new rule from template" flow — the
/// only way the web UI creates rules — builds exactly this kind of payload
/// (`{name, enabled, priority, group, notes, match, actions}`, no `id` key),
/// so a `Rule` deserializer that requires `id` would 400/422 on every
/// "New rule" click.
#[tokio::test]
async fn rules_create_without_id_field_gets_server_assigned_id() {
    let state = common::make_state();
    let app = router(state);

    let body_without_id = json!({
        "name": "from template",
        "enabled": true,
        "priority": 0,
        "group": null,
        "notes": null,
        "match": {},
        "actions": [],
    });
    assert!(
        body_without_id.get("id").is_none(),
        "test payload must omit `id` to exercise the bug"
    );

    let resp = app
        .clone()
        .oneshot(json_req("POST", "/api/rules", body_without_id))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp).await;
    assert!(created["id"].as_str().is_some_and(|id| !id.is_empty()));

    let resp = app.oneshot(get("/api/rules")).await.unwrap();
    let list = body_json(resp).await;
    assert_eq!(list["rules"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn settings_partial_merge_and_restart_required() {
    let state = common::make_state();
    let app = router(state);

    // Change only theme: unrelated fields preserved, no restart required.
    let resp = app
        .clone()
        .oneshot(json_req("PUT", "/api/settings", json!({"theme": "light"})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["theme"], "light");
    assert_eq!(body["maxFlows"], 10_000);
    assert_eq!(body["restartRequired"], false);

    // Change proxyPort: restart required, theme change from before persists.
    let resp = app
        .clone()
        .oneshot(json_req(
            "PUT",
            "/api/settings",
            json!({"proxyPort": 12345}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["proxyPort"], 12345);
    assert_eq!(body["theme"], "light");
    assert_eq!(body["restartRequired"], true);

    let resp = app.clone().oneshot(get("/api/settings")).await.unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["proxyPort"], 12345);
    assert_eq!(body["theme"], "light");
}

#[tokio::test]
async fn har_export_has_correct_headers_and_shape() {
    let state = common::make_state();
    state
        .flows()
        .insert(common::sample_flow(1, "GET", "a.com", Some(200)));
    let app = router(state);

    let resp = app.oneshot(get("/api/har")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(content_type, "application/json");
    let disposition = resp
        .headers()
        .get("content-disposition")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(disposition.starts_with("attachment; filename=\"rdproxy-"));
    assert!(disposition.ends_with(".har\""));

    let body = body_json(resp).await;
    assert_eq!(body["log"]["version"], "1.2");
    assert_eq!(body["log"]["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn har_import_increases_flow_count() {
    let state = common::make_state();
    assert_eq!(state.flows().len(), 0);
    let app = router(state.clone());

    let har_doc = json!({
        "log": {
            "version": "1.2",
            "creator": {"name": "test", "version": "0"},
            "entries": [
                {
                    "startedDateTime": "2024-01-01T00:00:00.000Z",
                    "time": 10,
                    "request": {
                        "method": "GET",
                        "url": "http://example.com/x",
                        "httpVersion": "HTTP/1.1",
                        "headers": [],
                        "queryString": []
                    },
                    "response": {
                        "status": 200,
                        "statusText": "OK",
                        "httpVersion": "HTTP/1.1",
                        "headers": [],
                        "content": {"size": 0, "mimeType": "text/plain"}
                    },
                    "cache": {},
                    "timings": {"blocked": 0, "dns": -1, "connect": -1, "ssl": -1, "send": 0, "wait": 0, "receive": 0}
                }
            ]
        }
    });

    let resp = app
        .oneshot(json_req("POST", "/api/har/import", har_doc))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["imported"], 1);
    assert_eq!(state.flows().len(), 1);
}

#[tokio::test]
async fn cert_pem_has_correct_content_type() {
    let state = common::make_state();
    let app = router(state);

    let resp = app.oneshot(get("/cert/rdproxy-ca.pem")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(content_type, "application/x-pem-file");
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("CERTIFICATE"));
}

#[tokio::test]
async fn cert_crt_is_x509_mime_type() {
    let state = common::make_state();
    let app = router(state);

    let resp = app.oneshot(get("/cert/rdproxy-ca.crt")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(content_type, "application/x-x509-ca-cert");
}

#[tokio::test]
async fn setup_payload_has_expected_shape() {
    let state = common::make_state();
    let app = router(state);

    let resp = app.oneshot(get("/api/setup")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    for key in [
        "proxyHost",
        "proxyPort",
        "lanAddresses",
        "certUrl",
        "caFingerprint",
        "qrSvg",
    ] {
        assert!(body.get(key).is_some(), "missing key {key}");
    }
    assert!(body["lanAddresses"].is_array());
    assert!(body["qrSvg"].as_str().unwrap().starts_with("<svg"));
}

#[tokio::test]
async fn spa_fallback_vs_api_404() {
    let state = common::make_state();
    let app = router(state);

    // A non-API, non-cert path falls back to the UI (200, HTML).
    let resp = app.clone().oneshot(get("/rules")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("text/html"));

    // An unknown /api/* path is a JSON 404 ApiError, not the SPA fallback.
    let resp = app.clone().oneshot(get("/api/nope")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("application/json"));
    let body = body_json(resp).await;
    assert!(body.get("error").is_some());

    // Same for an unknown /cert/* path.
    let resp = app.clone().oneshot(get("/cert/nope")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let content_type = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(content_type.starts_with("application/json"));
}
