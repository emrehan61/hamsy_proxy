//! WebSocket interception and message recording.
//!
//! Known limitation: WebSocket frames are recorded but never mutated by
//! rules in this version - this module is record-only. Rule-driven WS frame
//! editing is out of scope here (see the crate-level task notes).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures_util::{SinkExt, StreamExt};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;

use rdproxy_core::{BodyPayload, Flow, FlowId, FlowState, RequestRecord, ResponseRecord, ServerEvent, WsDirection, WsMessage};

use crate::config::ProxyContext;
use crate::http::{self, ConnInfo};
use crate::BoxBody;

/// Returns true if `req` is an HTTP/1.1 WebSocket upgrade request
/// (`Connection: upgrade` + `Upgrade: websocket` + a `Sec-WebSocket-Key`).
pub fn is_websocket_upgrade(req: &Request<Incoming>) -> bool {
    let headers = req.headers();
    let has_connection_upgrade = headers
        .get(::http::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|tok| tok.trim().eq_ignore_ascii_case("upgrade")))
        .unwrap_or(false);
    let has_upgrade_websocket = headers
        .get(::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    let has_key = headers.contains_key("sec-websocket-key");
    has_connection_upgrade && has_upgrade_websocket && has_key
}

/// Handles a WebSocket upgrade request: completes the handshake with the
/// origin ourselves, then either transparently relays raw bytes
/// (`capture_websockets` off) or relays frame-by-frame while recording each
/// message onto a [`Flow`] (`capture_websockets` on).
pub async fn handle_upgrade(ctx: ProxyContext, mut req: Request<Incoming>, conn: ConnInfo) -> Response<BoxBody> {
    let client_upgrade = hyper::upgrade::on(&mut req);
    let (parts, _body) = req.into_parts();

    let dial_scheme: &'static str = if conn.scheme.eq_ignore_ascii_case("https") { "https" } else { "http" };
    let ws_scheme: &'static str = if dial_scheme == "https" { "wss" } else { "ws" };

    let url = match http::build_target_url(&parts, &conn) {
        Ok(u) => u,
        Err(e) => return http::error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let host = url.host_str().unwrap_or("").to_string();
    let port = url.port_or_known_default().unwrap_or(http::default_port(dial_scheme));

    // Unlike the normal proxy pipeline, we must NOT strip Connection/Upgrade
    // /Sec-WebSocket-* headers - those are exactly what makes this an
    // upgrade handshake. Only proxy-specific headers are removed.
    let mut headers = http::header_pairs_from(&parts.headers);
    headers.retain(|h| !h.name.eq_ignore_ascii_case("proxy-connection") && !h.name.eq_ignore_ascii_case("proxy-authorization"));
    http::set_host_header(&mut headers, &host, port, dial_scheme);

    let outbound = match http::build_outbound_request(&parts.method, &url, &headers, false, parts.version, crate::empty_body()) {
        Ok(r) => r,
        Err(e) => return http::error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    let upstream_proxy = ctx.settings.read().upstream_proxy.clone();
    // Deliberately bypass the pooled connector's release-on-complete
    // wrapper: once upgraded, this connection is repurposed for raw
    // WebSocket bytes and must never be returned to the HTTP pool.
    let obtained = match ctx.upstream.obtain(dial_scheme, &host, port, false, upstream_proxy.as_deref()).await {
        Ok(o) => o,
        Err(e) => return http::error_response(StatusCode::BAD_GATEWAY, &format!("could not connect to {host}:{port}: {e}")),
    };
    let mut sender = obtained.sender;

    let mut origin_resp = match sender.send_request(outbound).await {
        Ok(r) => r,
        Err(e) => return http::error_response(StatusCode::BAD_GATEWAY, &format!("upstream websocket handshake failed: {e}")),
    };

    if origin_resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        // Origin declined the upgrade; relay its response as-is.
        let status = origin_resp.status().as_u16();
        let resp_headers = http::header_pairs_from(origin_resp.headers());
        let body = origin_resp.into_body();
        return http::build_client_response(status, &resp_headers, crate::box_body(body));
    }

    let origin_upgrade = hyper::upgrade::on(&mut origin_resp);
    let resp_headers = http::header_pairs_from(origin_resp.headers());
    let response = http::build_client_response(101, &resp_headers, crate::empty_body());

    let capture = ctx.settings.read().capture_websockets;
    let flow_meta = FlowMeta {
        host,
        port,
        url: url.to_string(),
        scheme: ws_scheme.to_string(),
        client_addr: conn.client_addr.to_string(),
    };

    tokio::spawn(async move {
        let client_upgraded = match client_upgrade.await {
            Ok(u) => u,
            Err(err) => {
                tracing::debug!(%err, "client websocket upgrade failed");
                return;
            }
        };
        let origin_upgraded = match origin_upgrade.await {
            Ok(u) => u,
            Err(err) => {
                tracing::debug!(%err, "origin websocket upgrade failed");
                return;
            }
        };
        relay(ctx, TokioIo::new(client_upgraded), TokioIo::new(origin_upgraded), flow_meta, capture).await;
    });

    response
}

struct FlowMeta {
    host: String,
    port: u16,
    url: String,
    scheme: String,
    client_addr: String,
}

/// Relays traffic between the client and origin WebSocket connections,
/// either as an opaque byte tunnel (`capture = false`) or frame-by-frame
/// with per-message recording (`capture = true`).
async fn relay<C, O>(ctx: ProxyContext, mut client_io: C, mut origin_io: O, meta: FlowMeta, capture: bool)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    O: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if !capture {
        if let Err(err) = tokio::io::copy_bidirectional(&mut client_io, &mut origin_io).await {
            tracing::debug!(%err, "websocket byte tunnel ended");
        }
        return;
    }

    let flow_id = Uuid::new_v4();
    let seq = ctx.flows.next_seq();
    let started_at = http::now_ms();
    let req_record = RequestRecord {
        method: "GET".to_string(),
        url: meta.url.clone(),
        http_version: "HTTP/1.1".to_string(),
        headers: vec![],
        body: BodyPayload::default(),
        query: vec![],
    };
    let mut flow = Flow::new_request(
        flow_id,
        seq,
        started_at,
        "GET",
        meta.scheme.clone(),
        meta.host.clone(),
        meta.port,
        "",
        meta.url.clone(),
        "HTTP/1.1",
        meta.client_addr.clone(),
        req_record,
    );
    flow.summary.state = FlowState::Responding;
    ctx.flows.insert(flow.clone());
    let _ = ctx.events.send(ServerEvent::Flow { flow: flow.summary() });

    let max_bytes = ctx.max_body_bytes();
    let client_ws = WebSocketStream::from_raw_socket(client_io, Role::Server, None).await;
    let origin_ws = WebSocketStream::from_raw_socket(origin_io, Role::Client, None).await;
    let (mut client_write, mut client_read) = client_ws.split();
    let (mut origin_write, mut origin_read) = origin_ws.split();

    loop {
        tokio::select! {
            msg = client_read.next() => {
                match msg {
                    Some(Ok(m)) => {
                        let closing = m.is_close();
                        record_message(&ctx, flow_id, WsDirection::Send, &m, max_bytes);
                        if origin_write.send(m).await.is_err() || closing {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            msg = origin_read.next() => {
                match msg {
                    Some(Ok(m)) => {
                        let closing = m.is_close();
                        record_message(&ctx, flow_id, WsDirection::Recv, &m, max_bytes);
                        if client_write.send(m).await.is_err() || closing {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
    }

    let finished_at = http::now_ms();
    let resp_record = ResponseRecord {
        status: 101,
        status_text: "Switching Protocols".to_string(),
        http_version: "HTTP/1.1".to_string(),
        headers: vec![],
        body: BodyPayload::default(),
    };
    if let Some(summary) = ctx.flows.update(flow_id, |f| f.mark_complete(resp_record, finished_at)) {
        let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
    }
}

fn record_message(ctx: &ProxyContext, flow_id: FlowId, direction: WsDirection, msg: &Message, max_bytes: usize) {
    let (opcode, data, size): (&str, String, u64) = match msg {
        Message::Text(t) => {
            let bytes = t.as_bytes();
            let size = bytes.len() as u64;
            let capped = cap_str(t.as_str(), max_bytes);
            ("text", capped, size)
        }
        Message::Binary(b) => {
            let size = b.len() as u64;
            let capped = cap_bytes(b, max_bytes);
            ("binary", BASE64.encode(capped), size)
        }
        Message::Ping(_) => ("ping", String::new(), 0),
        Message::Pong(_) => ("pong", String::new(), 0),
        Message::Close(_) => ("close", String::new(), 0),
        Message::Frame(_) => return, // raw frames aren't produced when reading
    };
    let ws_msg = WsMessage { direction, opcode: opcode.to_string(), timestamp: http::now_ms(), data, size };
    let broadcast_msg = ws_msg.clone();
    if ctx.flows.update(flow_id, |f| f.ws_messages.push(ws_msg)).is_some() {
        let _ = ctx.events.send(ServerEvent::WsMessage { flow_id, message: broadcast_msg });
    }
}

fn cap_bytes(bytes: &[u8], max_bytes: usize) -> &[u8] {
    if bytes.len() > max_bytes {
        &bytes[..max_bytes]
    } else {
        bytes
    }
}

fn cap_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_str_truncates_at_char_boundary() {
        let s = "héllo"; // é is 2 bytes in UTF-8
        let capped = cap_str(s, 2);
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
        assert!(capped.len() <= 2);
    }

    #[test]
    fn cap_str_no_truncation_when_within_limit() {
        assert_eq!(cap_str("hello", 100), "hello");
    }

    #[test]
    fn cap_bytes_truncates() {
        let b: Vec<u8> = b"0123456789".to_vec();
        assert_eq!(cap_bytes(&b, 4), b"0123");
        assert_eq!(cap_bytes(&b, 100), b"0123456789");
    }
}
