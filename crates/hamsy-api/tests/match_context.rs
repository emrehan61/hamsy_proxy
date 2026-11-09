mod common;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use hamsy_api::router;
use hamsy_core::{BodyKind, BodyPayload};
use serde_json::{json, Value};
use tower::ServiceExt;

#[tokio::test]
async fn context_reads_late_fields_without_transferring_payloads_or_masked_values() {
    let state = common::make_state();
    let mut flow = common::sample_flow(1, "GET", "example.test", Some(200));
    let id = flow.summary.id;
    flow.response.as_mut().unwrap().body = BodyPayload {
        kind: BodyKind::Text,
        data: json!({"password":"PRIVATE","padding":"x".repeat(25000),"result":"needle café"})
            .to_string(),
        ..Default::default()
    };
    state.flows().insert(flow);
    let app = router(state.clone());
    for (query, expected) in [("needle", 1), ("PRIVATE", 0)] {
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair(
                "params",
                &json!({"query":query,"field":"Response body","maxContextBytes":31}).to_string(),
            )
            .finish();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/flows/{id}/context?{params}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["matches"].as_array().unwrap().len(), expected);
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE"));
    }
    assert!(state
        .flows()
        .get(id)
        .unwrap()
        .response
        .unwrap()
        .body
        .data
        .contains("PRIVATE"));
}
