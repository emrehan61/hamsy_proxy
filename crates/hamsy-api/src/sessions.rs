//! Read-only access to open browser HAR tabs. Browsers advertise metadata, then
//! hydrate only the requested session from IndexedDB. Payloads are never copied
//! into live capture or persisted in a second server-side archive.
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{ApiError, ApiState};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{stream::FuturesUnordered, SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const MAX_MESSAGE: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Session {
    id: Uuid,
    name: String,
    flow_count: usize,
    imported_at: u64,
    #[serde(default)]
    active: bool,
}
struct Client {
    sessions: Vec<Session>,
    sender: mpsc::Sender<Job>,
}
struct Job {
    request: Value,
    reply: oneshot::Sender<Result<Value, String>>,
}
#[derive(Default)]
pub struct BrowserSessions {
    clients: Mutex<HashMap<Uuid, Client>>,
}
impl BrowserSessions {
    fn list(&self) -> Vec<Value> {
        let mut sessions: BTreeMap<Uuid, Value> = BTreeMap::new();
        for client in self.clients.lock().values() {
            for session in &client.sessions {
                let entry = sessions.entry(session.id).or_insert_with(|| json!({
                    "id":session.id,"name":session.name,"kind":"har", "flowCount":session.flow_count,
                    "importedAt":session.imported_at,"active":false,"readOnly":true,"openWindows":0
                }));
                entry["active"] =
                    json!(entry["active"].as_bool().unwrap_or(false) || session.active);
                entry["openWindows"] = json!(entry["openWindows"].as_u64().unwrap_or(0) + 1);
            }
        }
        sessions.into_values().collect()
    }
    async fn read(&self, session: Uuid, operation: &str, query: Value) -> Result<Value, ApiError> {
        let mut owners: Vec<_> = self
            .clients
            .lock()
            .values()
            .filter_map(|client| {
                client
                    .sessions
                    .iter()
                    .find(|s| s.id == session)
                    .map(|s| (s.active, client.sender.clone()))
            })
            .collect();
        if owners.is_empty() {
            return Err(ApiError::coded(StatusCode::NOT_FOUND, "session_not_open",
                "HAR session is not open; keep its Hamsy browser window open and list sessions again", false));
        }
        owners.sort_by_key(|(active, _)| !active);
        let mut responses = FuturesUnordered::new();
        // Every operation is read-only. Race a bounded set of owners so a
        // suspended original window cannot hide a responsive pop-out window.
        for (_, sender) in owners.into_iter().take(8) {
            let (reply, response) = oneshot::channel();
            let request = json!({"type":"read", "requestId":Uuid::new_v4(), "sessionId":session,"operation":operation,"query":query});
            if sender.try_send(Job { request, reply }).is_ok() {
                responses.push(response);
            }
        }
        if responses.is_empty() {
            return Err(ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "session_busy",
                "HAR windows are busy or disconnected; list sessions again",
                true,
            ));
        }
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            while let Some(response) = responses.next().await {
                if let Ok(Ok(value)) = response {
                    return Some(value);
                }
            }
            None
        })
        .await;
        match result {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(ApiError::coded(StatusCode::BAD_GATEWAY, "session_unavailable",
                "HAR session could not be read; keep its browser window open and reduce the requested response", true)),
            Err(_) => Err(ApiError::coded(StatusCode::GATEWAY_TIMEOUT, "session_timeout",
                "HAR windows did not respond; reopen or wake a window and list sessions again", true)),
        }
    }
}

pub fn router() -> Router<ApiState> {
    Router::new()
        .route("/sessions", get(list))
        .route("/sessions/ws", get(connect))
        .route("/sessions/{id}/flows", get(flows))
        .route("/sessions/{id}/flows/{flow_id}", get(flow))
        .route("/sessions/{id}/flows/{flow_id}/context", get(context))
        .route("/sessions/{id}/har", get(har))
        .route("/sessions/{id}/search", get(search))
}
async fn list(State(state): State<ApiState>) -> Json<Value> {
    let mut sessions = state.browser_sessions().list();
    if !state.viewer_only() {
        sessions.insert(0, json!({"id":"live","name":"Live capture","kind":"live", "flowCount":state.flows().len(), "readOnly":false}));
    }
    Json(json!({"sessions":sessions}))
}
async fn flows(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        state
            .browser_sessions()
            .read(id, "list_flows", json!(query))
            .await?,
    ))
}
async fn flow(
    State(state): State<ApiState>,
    Path((id, flow_id)): Path<(Uuid, Uuid)>,
    Query(mut query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    query.insert("id".into(), flow_id.to_string());
    Ok(Json(
        state
            .browser_sessions()
            .read(id, "get_flow", json!(query))
            .await?,
    ))
}
async fn har(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        state
            .browser_sessions()
            .read(id, "export_har", json!(query))
            .await?,
    ))
}

async fn context(
    State(state): State<ApiState>,
    Path((id, flow_id)): Path<(Uuid, Uuid)>,
    Query(mut query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    query.insert("id".into(), flow_id.to_string());
    Ok(Json(
        state
            .browser_sessions()
            .read(id, "get_match_context", json!(query))
            .await?,
    ))
}

async fn search(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        state
            .browser_sessions()
            .read(id, "search_flows", json!(query))
            .await?,
    ))
}

// A browser must be the Hamsy page itself (or the configured Vite dev origin).
// Cross-origin pages must not register themselves as archive owners.
fn allowed_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    origin == format!("http://{host}")
        || origin == format!("https://{host}")
        || matches!(origin, "http://localhost:5173" | "http://127.0.0.1:5173")
}
async fn connect(
    ws: WebSocketUpgrade,
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Response {
    if !allowed_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| serve(socket, state.browser_sessions().clone()))
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum BrowserMessage {
    Sessions {
        sessions: Vec<Session>,
    },
    Reply {
        #[serde(rename = "requestId")]
        request_id: Uuid,
        result: Option<Value>,
        error: Option<String>,
    },
}
async fn serve(socket: WebSocket, registry: Arc<BrowserSessions>) {
    let client_id = Uuid::new_v4();
    let (sender, mut jobs) = mpsc::channel::<Job>(8);
    registry.clients.lock().insert(
        client_id,
        Client {
            sessions: vec![],
            sender,
        },
    );
    let (mut outbound, mut inbound) = socket.split();
    let mut pending: HashMap<Uuid, (Instant, oneshot::Sender<Result<Value, String>>)> =
        HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(15));
    let mut last_seen = Instant::now();
    loop {
        let message = tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { break; };
                if job.reply.is_closed() { continue; }
                if pending.len() >= 8 { let _ = job.reply.send(Err("busy".into())); continue; }
                let id = serde_json::from_value(job.request["requestId"].clone()).expect("generated UUID");
                pending.insert(id, (Instant::now(), job.reply));
                Some(Message::Text(job.request.to_string().into()))
            }
            incoming = inbound.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        match serde_json::from_str::<BrowserMessage>(&text) {
                            Ok(BrowserMessage::Sessions { sessions }) => {
                                if sessions.len() > 1000 || sessions.iter().any(|s| s.name.len() > 1024) { break; }
                                if let Some(client) = registry.clients.lock().get_mut(&client_id) { client.sessions = sessions; }
                            }
                            Ok(BrowserMessage::Reply { request_id, result, error }) => {
                                if let Some((_, reply)) = pending.remove(&request_id) {
                                    let _ = reply.send(if error.is_some() { Err("browser read failed".into()) } else { result.ok_or_else(|| "missing result".into()) });
                                }
                            }
                            Err(_) => break,
                        }
                        None
                    }
                    Some(Ok(Message::Pong(_))) => { last_seen = Instant::now(); None }
                    Some(Ok(Message::Ping(data))) => Some(Message::Pong(data)),
                    _ => break,
                }
            }
            _ = tick.tick() => {
                pending.retain(|_, (created, reply)| !reply.is_closed() && created.elapsed() < REQUEST_TIMEOUT);
                if last_seen.elapsed() > Duration::from_secs(45) { break; }
                Some(Message::Ping(Default::default()))
            }
        };
        if let Some(message) = message {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(3), outbound.send(message)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    }
    registry.clients.lock().remove(&client_id);
    // Dropping pending replies wakes in-flight HTTP readers immediately.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: Uuid, active: bool) -> Session {
        Session {
            id,
            name: "archive".into(),
            flow_count: 1,
            imported_at: 1,
            active,
        }
    }

    #[tokio::test]
    async fn reads_a_responsive_owner_when_another_window_is_asleep() {
        let registry = BrowserSessions::default();
        let id = Uuid::new_v4();
        let (sleeping, _sleeping_jobs) = mpsc::channel(8);
        let (awake, mut awake_jobs) = mpsc::channel::<Job>(8);
        registry.clients.lock().insert(
            Uuid::new_v4(),
            Client {
                sessions: vec![session(id, true)],
                sender: sleeping,
            },
        );
        registry.clients.lock().insert(
            Uuid::new_v4(),
            Client {
                sessions: vec![session(id, false)],
                sender: awake,
            },
        );
        let responder = tokio::spawn(async move {
            let job = awake_jobs.recv().await.unwrap();
            assert_eq!(job.request["operation"], "get_match_context");
            assert_eq!(job.request["query"]["params"], "{}");
            job.reply.send(Ok(json!({"matches":[]}))).unwrap();
        });
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            registry.read(id, "get_match_context", json!({"params":"{}"})),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response["matches"], json!([]));
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn disconnected_sessions_return_stable_recovery_code() {
        let registry = BrowserSessions::default();
        let response = registry
            .read(Uuid::new_v4(), "list_flows", json!({}))
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["code"], "session_not_open");
        assert_eq!(value["retryable"], false);
        assert!(value["error"].is_string() && value["detail"].is_string());
    }
}
