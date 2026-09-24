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
use futures_util::{SinkExt, StreamExt};
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
        let sender = self.clients.lock().values()
            .find(|client| client.sessions.iter().any(|s| s.id == session))
            .map(|client| client.sender.clone())
            .ok_or_else(|| ApiError::NotFound("HAR session is not open; keep its Hamsy browser window open and list sessions again".into()))?;
        let (reply, response) = oneshot::channel();
        sender.try_send(Job { request: json!({"type":"read", "requestId":Uuid::new_v4(), "sessionId":session,"operation":operation,"query":query}), reply })
            .map_err(|_| ApiError::BadGateway("HAR window is busy or disconnected; list sessions again".into()))?;
        match tokio::time::timeout(REQUEST_TIMEOUT, response).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(_))) => Err(ApiError::BadGateway("HAR session could not be read. Keep its browser window open; body responses over 8 MiB require includeBodies=false".into())),
            _ => Err(ApiError::BadGateway("HAR window did not respond; reopen or wake the window and list sessions again".into())),
        }
    }
}

pub fn router() -> Router<ApiState> {
    Router::new()
        .route("/sessions", get(list))
        .route("/sessions/ws", get(connect))
        .route("/sessions/{id}/flows", get(flows))
        .route("/sessions/{id}/flows/{flow_id}", get(flow))
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
