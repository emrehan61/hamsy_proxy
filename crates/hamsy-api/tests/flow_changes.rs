mod common;

use std::time::Duration;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use hamsy_api::router;
use serde_json::Value;
use tower::ServiceExt;

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

async fn json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}

#[tokio::test]
async fn baseline_then_late_update_and_clear_are_observable() {
    let state = common::make_state();
    let flow = common::sample_flow(1, "GET", "example.test", None);
    let id = flow.summary.id;
    state.flows().insert(flow);
    let app = router(state.clone());
    let baseline = json(
        app.clone()
            .oneshot(get("/api/flows/changes"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(baseline["changes"], serde_json::json!([]));
    assert_eq!(baseline["timedOut"], false);
    let path = format!(
        "/api/flows/changes?cursor={}",
        baseline["cursor"].as_str().unwrap()
    );
    state
        .flows()
        .update(id, |flow| flow.summary.status = Some(200));
    let page = json(app.clone().oneshot(get(&path)).await.unwrap()).await;
    assert_eq!(page["changes"][0]["kind"], "updated");
    assert_eq!(page["changes"][0]["flowId"], id.to_string());
    state.flows().clear();
    let response = app.clone().oneshot(get(&path)).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = json(response).await;
    assert_eq!(error["code"], "stale_cursor");
    assert_eq!(error["retryable"], false);
    assert_eq!(error["details"]["resetRequired"], true);
    assert_eq!(error["details"]["reason"], "capture_changed");
    assert_ne!(error["details"]["captureId"], baseline["captureId"]);
    assert!(error["error"].is_string() && error["detail"].is_string());
    let legacy = app.oneshot(get("/api/flows")).await.unwrap();
    assert_eq!(legacy.status(), StatusCode::OK);
    assert_eq!(json(legacy).await["flows"], serde_json::json!([]));
}

#[tokio::test]
async fn waiting_returns_updates_timeout_and_can_be_cancelled() {
    let state = common::make_state();
    let app = router(state.clone());
    let cursor = state.flows().change_cursor().1;
    let waiting = tokio::spawn(app.clone().oneshot(get(&format!(
        "/api/flows/changes?cursor={cursor}&waitMs=1000"
    ))));
    tokio::time::sleep(Duration::from_millis(10)).await;
    state
        .flows()
        .insert(common::sample_flow(1, "GET", "example.test", None));
    let response = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let page = json(response).await;
    assert_eq!(page["changes"][0]["kind"], "inserted");
    assert_eq!(page["timedOut"], false);
    let cursor = page["cursor"].as_str().unwrap();
    let path = format!("/api/flows/changes?cursor={cursor}&waitMs=20");
    let start = tokio::time::Instant::now();
    let timeout = json(app.clone().oneshot(get(&path)).await.unwrap()).await;
    assert!(start.elapsed() >= Duration::from_millis(20));
    assert_eq!(timeout["timedOut"], true);
    assert_eq!(timeout["cursor"], cursor);
    let pending = tokio::spawn(app.clone().oneshot(get(&format!(
        "/api/flows/changes?cursor={cursor}&waitMs=10000"
    ))));
    tokio::task::yield_now().await;
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let reset_wait = tokio::spawn(app.clone().oneshot(get(&format!(
        "/api/flows/changes?cursor={cursor}&waitMs=1000"
    ))));
    tokio::time::sleep(Duration::from_millis(10)).await;
    state.flows().clear();
    let reset = tokio::time::timeout(Duration::from_secs(2), reset_wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reset.status(), StatusCode::CONFLICT);
    assert_eq!(json(reset).await["details"]["reason"], "capture_changed");
    assert_eq!(
        app.oneshot(get("/api/flows/changes"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn observer_validates_ranges_and_rejects_viewer_mode() {
    let state = common::make_state();
    let app = router(state.clone());
    for query in [
        "cursor=invalid",
        "limit=0",
        "limit=201",
        "waitMs=10001",
        "waitMs=-1",
    ] {
        let response = app
            .clone()
            .oneshot(get(&format!("/api/flows/changes?{query}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json(response).await["code"], "bad_request");
    }
    // Exercise the handler directly: the standalone HAR router deliberately
    // does not expose this live-only endpoint at all.
    let viewer = common::make_state().into_viewer();
    let response = hamsy_api::routes::changes::get(
        axum::extract::State(viewer),
        axum::extract::Query(Default::default()),
    )
    .await
    .unwrap_err();
    use axum::response::IntoResponse;
    assert_eq!(
        json(response.into_response()).await["code"],
        "live_session_required"
    );
}
