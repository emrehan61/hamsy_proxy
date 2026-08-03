//! `GET /api/ws` — the live event feed.
//!
//! On connect, a client receives a `state` snapshot and a `flows` batch of
//! the most recent flows, then a live stream of [`ServerEvent`]s.
//! [`ServerEvent::Flow`]/[`ServerEvent::Flows`] events are coalesced into
//! at-most-one `flows` message per 50ms window (deduped by flow id,
//! last-write-wins, ordered by `seq`) so a fast-capturing proxy can't flood
//! a slow client; every other event type is flushed immediately (after
//! first flushing any buffered flow updates, to keep relative ordering
//! sane). See [`handle_socket`] for the full event loop.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use bytes::Bytes;
use flproxy_core::{ClientCommand, FlowId, FlowQuery, FlowSummary, ServerEvent};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio::time::interval;

use crate::routes::state::state_snapshot;
use crate::ApiState;

/// How often buffered [`ServerEvent::Flow`]/[`ServerEvent::Flows`] updates
/// are coalesced and flushed as a single `flows` message.
const COALESCE_INTERVAL: Duration = Duration::from_millis(50);
/// How often a liveness ping is sent to the client.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// Consecutive missed pongs after which the connection is dropped.
const MAX_MISSED_PONGS: u32 = 2;
/// Number of recent flows sent on initial connect / lag resync.
const RESYNC_LIMIT: usize = 2000;

type WsSender = SplitSink<WebSocket, Message>;

/// `GET /api/ws` — upgrades the connection and hands off to
/// [`handle_socket`].
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<ApiState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Builds a [`ServerEvent::Flows`] carrying the most recent flows, used for
/// both the initial sync and lag resync.
fn recent_flows_event(state: &ApiState) -> ServerEvent {
    let flows = state.flows().list(&FlowQuery {
        limit: Some(RESYNC_LIMIT),
        ..Default::default()
    });
    ServerEvent::Flows { flows }
}

fn notice(level: &str, message: impl Into<String>) -> ServerEvent {
    ServerEvent::Notice {
        level: level.to_string(),
        message: message.into(),
    }
}

/// Serializes and sends a single [`ServerEvent`] as a WS text frame.
async fn send_event(sender: &mut WsSender, event: &ServerEvent) -> Result<(), axum::Error> {
    let text = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    sender.send(Message::Text(text.into())).await
}

/// Flushes any buffered flow updates as a single `flows` message, sorted by
/// `seq`. A no-op (returns `true`) if the buffer is empty. Returns `false`
/// if the send failed, signaling the caller to close the connection.
async fn flush_coalesced(sender: &mut WsSender, buffer: &mut HashMap<FlowId, FlowSummary>) -> bool {
    if buffer.is_empty() {
        return true;
    }
    let mut flows: Vec<FlowSummary> = buffer.drain().map(|(_, summary)| summary).collect();
    flows.sort_by_key(|f| f.seq);
    send_event(sender, &ServerEvent::Flows { flows })
        .await
        .is_ok()
}

/// Applies a single inbound [`ClientCommand`], replying directly on this
/// socket where the command calls for it (`Ping`).
async fn process_command(cmd: ClientCommand, state: &ApiState, sender: &mut WsSender) {
    match cmd {
        ClientCommand::Pause { paused } => {
            let mut settings = state.settings();
            settings.paused = paused;
            match state.save_settings(settings.clone()) {
                Ok(()) => state.broadcast(ServerEvent::SettingsChanged { settings }),
                Err(e) => tracing::warn!("failed to persist paused setting: {e}"),
            }
        }
        ClientCommand::Clear => {
            state.flows().clear();
            state.broadcast(ServerEvent::Cleared);
        }
        ClientCommand::Ping => {
            let _ = send_event(sender, &notice("info", "pong")).await;
        }
        ClientCommand::Subscribe { .. } => {
            // No server-side filtering implemented yet; accepted as a no-op.
        }
    }
}

/// Handles one inbound WS frame. Returns [`ControlFlow::Break`] when the
/// connection should close.
async fn handle_inbound(msg: Message, state: &ApiState, sender: &mut WsSender) -> ControlFlow<()> {
    match msg {
        Message::Text(text) => {
            match serde_json::from_str::<ClientCommand>(text.as_str()) {
                Ok(cmd) => process_command(cmd, state, sender).await,
                Err(e) => tracing::debug!("ignoring malformed ws client command: {e}"),
            }
            ControlFlow::Continue(())
        }
        Message::Binary(bytes) => {
            match serde_json::from_slice::<ClientCommand>(&bytes) {
                Ok(cmd) => process_command(cmd, state, sender).await,
                Err(e) => tracing::debug!("ignoring malformed ws client command: {e}"),
            }
            ControlFlow::Continue(())
        }
        Message::Close(_) => ControlFlow::Break(()),
        Message::Ping(_) | Message::Pong(_) => ControlFlow::Continue(()),
    }
}

/// The main per-connection event loop: fans out broadcast [`ServerEvent`]s
/// (coalescing flow updates), handles inbound [`ClientCommand`]s, and runs
/// a ping/pong liveness check.
async fn handle_socket(socket: WebSocket, state: ApiState) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = state.subscribe();

    if send_event(
        &mut sender,
        &ServerEvent::State {
            state: state_snapshot(&state),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    if send_event(&mut sender, &recent_flows_event(&state))
        .await
        .is_err()
    {
        return;
    }

    let mut coalesced: HashMap<FlowId, FlowSummary> = HashMap::new();
    let mut flush_tick = interval(COALESCE_INTERVAL);
    let mut ping_tick = interval(PING_INTERVAL);
    let mut awaiting_pong = false;
    let mut missed_pongs: u32 = 0;

    loop {
        tokio::select! {
            recv = events.recv() => {
                match recv {
                    Ok(ServerEvent::Flow { flow }) => {
                        coalesced.insert(flow.id, flow);
                    }
                    Ok(ServerEvent::Flows { flows }) => {
                        for flow in flows {
                            coalesced.insert(flow.id, flow);
                        }
                    }
                    Ok(other) => {
                        if !flush_coalesced(&mut sender, &mut coalesced).await {
                            break;
                        }
                        if send_event(&mut sender, &other).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        if send_event(&mut sender, &notice("warning", format!("dropped {n} events"))).await.is_err() {
                            break;
                        }
                        coalesced.clear();
                        if send_event(&mut sender, &recent_flows_event(&state)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = flush_tick.tick() => {
                if !flush_coalesced(&mut sender, &mut coalesced).await {
                    break;
                }
            }
            _ = ping_tick.tick() => {
                if awaiting_pong {
                    missed_pongs += 1;
                    if missed_pongs >= MAX_MISSED_PONGS {
                        break;
                    }
                }
                if sender.send(Message::Ping(Bytes::new())).await.is_err() {
                    break;
                }
                awaiting_pong = true;
            }
            inbound = receiver.next() => {
                match inbound {
                    Some(Ok(msg)) => {
                        awaiting_pong = false;
                        missed_pongs = 0;
                        if handle_inbound(msg, &state, &mut sender).await.is_break() {
                            break;
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }
        }
    }
}
