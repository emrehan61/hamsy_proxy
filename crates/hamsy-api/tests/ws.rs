//! A real WebSocket test: binds an ephemeral TCP port, serves the router on
//! it, and drives `/api/ws` with a `tokio-tungstenite` client to verify the
//! initial sync messages and the 50ms flow-update coalescing behavior.

mod common;

use std::time::Duration;

use hamsy_core::ServerEvent;
use futures_util::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as TMessage;

async fn spawn_server() -> (
    std::net::SocketAddr,
    hamsy_api::ApiState,
    tokio::task::JoinHandle<()>,
) {
    let state = common::make_state();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let app = hamsy_api::router(state.clone());
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    // Give the accept loop a moment to start.
    tokio::time::sleep(Duration::from_millis(30)).await;
    (addr, state, handle)
}

#[tokio::test]
async fn initial_sync_then_coalesced_flow_updates() {
    let (addr, state, handle) = spawn_server().await;
    let url = format!("ws://{addr}/api/ws");
    let (mut ws, _response) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");

    // First message on connect: a `state` snapshot.
    let msg = ws.next().await.expect("stream open").expect("ws message");
    let value: serde_json::Value =
        serde_json::from_str(&msg.into_text().expect("text frame")).expect("json");
    assert_eq!(value["type"], "state");
    assert!(value["state"].get("flowCount").is_some());

    // Second message: an initial `flows` batch (empty on a fresh store).
    let msg = ws.next().await.expect("stream open").expect("ws message");
    let value: serde_json::Value =
        serde_json::from_str(&msg.into_text().expect("text frame")).expect("json");
    assert_eq!(value["type"], "flows");
    assert_eq!(value["flows"].as_array().expect("flows array").len(), 0);

    // Push ~500 Flow events through the broadcast channel in a tight loop.
    for i in 1..=500u64 {
        let flow = common::sample_flow(i, "GET", "a.com", Some(200));
        state.broadcast(ServerEvent::Flow { flow: flow.summary });
    }

    // Collect whatever arrives over a window comfortably larger than the
    // 50ms coalescing interval, and count how many `flows` batch messages
    // came through.
    let mut flows_messages = 0usize;
    let mut total_flows_seen = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(TMessage::Text(text)))) => {
                let value: serde_json::Value =
                    serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                if value["type"] == "flows" {
                    flows_messages += 1;
                    total_flows_seen += value["flows"].as_array().map(|a| a.len()).unwrap_or(0);
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }

    assert!(
        flows_messages < 50,
        "expected heavy coalescing, got {flows_messages} separate flows messages"
    );
    assert_eq!(
        total_flows_seen, 500,
        "all 500 flow updates should still be delivered, just batched"
    );

    handle.abort();
}
