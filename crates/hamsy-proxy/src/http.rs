//! The request/response pipeline: the heart of the proxy.
//!
//! [`handle_proxy_request`] is called for every non-`CONNECT` request, both
//! for plain absolute-form HTTP proxying (`server.rs`) and for MITM'd
//! relative-form requests arriving over a terminated TLS connection
//! (`connect.rs`). It decides whether to capture/record the request at all,
//! runs `hamsy-core` rules against it, dispatches to the upstream server,
//! and records the resulting [`Flow`].
//!
//! # The body-buffering decision
//!
//! Bodies stream to their destination while being captured for recording,
//! capped at `Settings::max_body_bytes`, UNLESS some rule might need to
//! mutate the body - in which case it must be fully buffered first so the
//! mutation can be applied before anything is sent.
//!
//! Knowing "does any rule need this body" *exactly* would require
//! duplicating `rule.rs`'s full (non-body) matching logic here, just to
//! evaluate whether a specific request's body condition is even reachable.
//! Instead, [`ruleset_needs_request_body`]/[`ruleset_needs_response_body`]
//! use a conservative, always-correct approximation: if *any* enabled rule
//! anywhere has a body condition or a body-mutating action, we buffer -
//! regardless of whether that particular rule would even match this
//! request. This trades a bit of unnecessary buffering (when some unrelated
//! rule elsewhere has a body condition) for the guarantee that a mutation is
//! never silently skipped, without needing to leave `hamsy-core` or
//! duplicate its matching semantics.
//!
//! When not buffering, the body is streamed through a [`crate::tee::TeeBody`]
//! (capped copy for recording, full stream to the destination) wrapped in a
//! [`crate::tee::FinalizeBody`] that updates the flow once the body finishes
//! draining - which is also how a flow still gets finalized (with whatever
//! was captured) if the client disconnects mid-response rather than the
//! stream ending cleanly.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::{HeaderMap, Method, Request, Response, StatusCode};
use hyper::body::Incoming;
use parking_lot::Mutex;
use tokio::time::Duration;
use uuid::Uuid;

use hamsy_core::{
    Action, BodyKind, BodyPayload, Flow, FlowId, FlowState, HeaderPair, MockedResponse, RequestCtx,
    RequestRecord, ResourceType, ResponseCtx, ResponseOutcome, ResponseRecord, Rule, RuleSet,
    TlsInfo,
};

use crate::config::ProxyContext;
use crate::error::{ProxyError, Result};
use crate::tee::{collect_capped, FinalizeBody, TeeBody, TeeState, Throttled};
use crate::upstream::ReleaseOnComplete;
use crate::BoxBody;

/// Hard ceiling on how much of a request/response body we'll fully buffer
/// in memory to let a rule mutate it. Beyond this, mutation is skipped for
/// that particular body (logged at debug level) rather than risking
/// unbounded memory use.
const HARD_BUFFER_CAP: usize = 64 * 1024 * 1024;

/// Per-connection context `server.rs`/`connect.rs` provide so a single
/// [`handle_proxy_request`] implementation can serve both plain absolute-form
/// proxy requests and MITM'd relative-form requests.
#[derive(Clone)]
pub struct ConnInfo {
    /// The client's socket address.
    pub client_addr: SocketAddr,
    /// `"http"` or `"https"` (or `"ws"`/`"wss"`, though those are handled by
    /// `websocket.rs` before reaching here).
    pub scheme: &'static str,
    /// For MITM'd/relative-form requests, the `host:port` to resolve the
    /// request's path against. `None` for plain absolute-form requests
    /// (which already carry a full URL).
    pub authority: Option<String>,
    /// TLS session info, for MITM'd HTTPS connections.
    pub tls: Option<TlsInfo>,
    /// Whether the client negotiated HTTP/2 with the proxy on this
    /// connection (see `upstream.rs`'s module docs for why this is mirrored
    /// upstream rather than negotiated independently).
    pub mirror_h2: bool,
    /// Display name of the local application that owns `client_addr`,
    /// resolved once per accepted connection (see `crate::appid`). `None`
    /// when unresolved (remote/LAN client, non-macOS, or lookup failure).
    pub app: Option<String>,
}

/// Handles one proxied request: the full capture/rule/dispatch/record
/// pipeline. Used for both plain HTTP proxying and MITM'd HTTPS traffic.
pub async fn handle_proxy_request(
    ctx: ProxyContext,
    req: Request<Incoming>,
    conn: ConnInfo,
) -> Result<Response<BoxBody>> {
    let (parts, body) = req.into_parts();

    let url = match build_target_url(&parts, &conn) {
        Ok(u) => u,
        Err(e) => return Ok(error_response(StatusCode::BAD_REQUEST, &e.to_string())),
    };
    let host = url.host_str().unwrap_or("").to_string();

    let mut headers = header_pairs_from(&parts.headers);
    strip_hop_by_hop(&mut headers);

    if ctx.is_paused() || !ctx.should_capture(&host) {
        return Ok(forward_untouched(&ctx, parts, body, &conn, url, headers).await);
    }

    Ok(handle_captured_request(ctx, parts, body, conn, url, headers).await)
}

// ===== URL / header helpers =====

/// Builds the absolute [`url::Url`] a request targets: taken as-is for
/// absolute-form requests (plain HTTP proxying), or combined from
/// `conn`'s scheme/authority with the request's path for relative-form
/// requests (MITM'd HTTPS, or plaintext HTTP tunneled through `CONNECT`).
pub(crate) fn build_target_url(parts: &http::request::Parts, conn: &ConnInfo) -> Result<url::Url> {
    if parts.uri.scheme_str().is_some() {
        return url::Url::parse(&parts.uri.to_string())
            .map_err(|e| ProxyError::InvalidTarget(format!("invalid request target: {e}")));
    }
    let authority = conn
        .authority
        .clone()
        .or_else(|| {
            parts
                .headers
                .get(http::header::HOST)
                .and_then(|h| h.to_str().ok())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            ProxyError::InvalidTarget(
                "no authority to resolve relative request against".to_string(),
            )
        })?;
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let full = format!("{}://{authority}{path_and_query}", conn.scheme);
    url::Url::parse(&full)
        .map_err(|e| ProxyError::InvalidTarget(format!("invalid request target '{full}': {e}")))
}

/// Header names dropped before forwarding a request/response, per-hop only.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
];

fn strip_hop_by_hop(headers: &mut Vec<HeaderPair>) {
    headers.retain(|h| {
        !HOP_BY_HOP
            .iter()
            .any(|hop| h.name.eq_ignore_ascii_case(hop))
    });
}

pub(crate) fn header_pairs_from(map: &HeaderMap) -> Vec<HeaderPair> {
    // Non-UTF8 header values (unusual, but a hostile/broken client could
    // send them) are dropped rather than causing a panic or a lossy-but-odd
    // recorded value.
    map.iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|val| HeaderPair::new(k.as_str(), val)))
        .collect()
}

fn header_value<'a>(headers: &'a [HeaderPair], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn set_header(headers: &mut Vec<HeaderPair>, name: &str, value: &str) {
    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
    headers.push(HeaderPair::new(name, value));
}

fn remove_header(headers: &mut Vec<HeaderPair>, name: &str) {
    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
}

/// Rebuilds `map` from `pairs`, preserving repeated header names.
fn apply_headers_to_map(map: &mut HeaderMap, pairs: &[HeaderPair]) -> Result<()> {
    map.clear();
    for h in pairs {
        let name = http::header::HeaderName::from_bytes(h.name.as_bytes())
            .map_err(|e| ProxyError::Other(format!("invalid header name '{}': {e}", h.name)))?;
        let value = http::header::HeaderValue::from_str(&h.value).map_err(|e| {
            ProxyError::Other(format!("invalid header value for '{}': {e}", h.name))
        })?;
        map.append(name, value);
    }
    Ok(())
}

fn query_pairs(url: &url::Url) -> Vec<HeaderPair> {
    url.query_pairs()
        .map(|(k, v)| HeaderPair::new(k.into_owned(), v.into_owned()))
        .collect()
}

fn path_with_query(url: &url::Url) -> String {
    match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    }
}

fn resource_type_for_request(headers: &[HeaderPair], path: &str) -> ResourceType {
    ResourceType::infer(header_value(headers, "content-type"), path)
}

pub(crate) fn set_host_header(headers: &mut Vec<HeaderPair>, host: &str, port: u16, scheme: &str) {
    let default_port = if scheme.eq_ignore_ascii_case("https") {
        443
    } else {
        80
    };
    let value = if port == default_port {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    set_header(headers, "Host", &value);
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Clamps an arbitrary (possibly rule-supplied) status code to a valid
/// [`StatusCode`], defensively - a malformed rule must not be able to panic
/// the response-building path.
pub(crate) fn safe_status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// Parses an HTTP version string like `"HTTP/1.1"` (as recorded on a
/// [`RequestRecord`]) back into an [`http::Version`], defaulting to
/// HTTP/1.1 for anything unrecognized.
pub(crate) fn version_from_str(s: &str) -> http::Version {
    match s {
        "HTTP/0.9" => http::Version::HTTP_09,
        "HTTP/1.0" => http::Version::HTTP_10,
        "HTTP/2.0" => http::Version::HTTP_2,
        "HTTP/3.0" => http::Version::HTTP_3,
        _ => http::Version::HTTP_11,
    }
}

fn union_matched(mut a: Vec<String>, b: Vec<String>) -> Vec<String> {
    for id in b {
        if !a.contains(&id) {
            a.push(id);
        }
    }
    a
}

/// Builds a small JSON error response, used for 400/403/502 pages.
pub(crate) fn error_response(status: StatusCode, message: &str) -> Response<BoxBody> {
    let payload =
        serde_json::json!({ "error": error_label(status), "reason": message }).to_string();
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(crate::full_body(payload))
        .unwrap_or_else(|_| {
            let mut resp = Response::new(crate::empty_body());
            *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            resp
        })
}

fn error_label(status: StatusCode) -> &'static str {
    match status {
        StatusCode::FORBIDDEN => "blocked",
        StatusCode::BAD_GATEWAY => "upstream_error",
        StatusCode::BAD_REQUEST => "bad_request",
        _ => "error",
    }
}

// ===== Buffering decision =====

/// Conservative, ruleset-wide check for whether *any* enabled rule could
/// need the request body (see the module-level doc for why this is
/// intentionally coarser than per-request matching).
fn ruleset_needs_request_body(ruleset: &RuleSet) -> bool {
    ruleset.rules().iter().any(rule_touches_request_body)
}

fn rule_touches_request_body(rule: &Rule) -> bool {
    rule.matcher.request_body.is_some()
        || rule.actions.iter().any(|a| {
            matches!(
                a,
                Action::SetRequestBody { .. }
                    | Action::ReplaceInRequestBody { .. }
                    | Action::JsonPatchRequest { .. }
            )
        })
}

/// Same as [`ruleset_needs_request_body`], for the response phase.
fn ruleset_needs_response_body(ruleset: &RuleSet) -> bool {
    ruleset.rules().iter().any(rule_touches_response_body)
}

fn rule_touches_response_body(rule: &Rule) -> bool {
    rule.matcher.response_body.is_some()
        || rule.actions.iter().any(|a| {
            matches!(
                a,
                Action::SetResponseBody { .. }
                    | Action::ReplaceInResponseBody { .. }
                    | Action::JsonPatchResponse { .. }
            )
        })
}

/// Builds a [`BodyPayload`] from a (possibly tee-capped) capture, correcting
/// `size`/`truncated` when the *capture itself* (not `to_payload`'s own
/// truncation) already lost bytes - which can otherwise under-report the
/// true original size when a rule-free capture caps out.
fn payload_from_capture(
    captured: Bytes,
    total: u64,
    capture_truncated: bool,
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    max_bytes: usize,
) -> BodyPayload {
    let mut payload =
        hamsy_core::to_payload(&captured, content_type, content_encoding, max_bytes);
    if capture_truncated {
        payload.truncated = true;
        payload.size = payload.size.max(total);
        if payload.kind != BodyKind::None {
            payload.kind = BodyKind::Truncated;
        }
    }
    payload
}

// ===== Untouched (uncaptured) forwarding =====

async fn forward_untouched(
    ctx: &ProxyContext,
    parts: http::request::Parts,
    body: Incoming,
    conn: &ConnInfo,
    url: url::Url,
    mut headers: Vec<HeaderPair>,
) -> Response<BoxBody> {
    let host = url.host_str().unwrap_or("").to_string();
    let port = url
        .port_or_known_default()
        .unwrap_or(default_port(url.scheme()));
    set_host_header(&mut headers, &host, port, url.scheme());

    let upstream_proxy = ctx.settings.read().upstream_proxy.clone();
    let via_proxy = upstream_proxy.is_some();
    let outbound_body = crate::box_body(body);
    let outbound = match build_outbound_request(
        &parts.method,
        &url,
        &headers,
        via_proxy,
        parts.version,
        outbound_body,
    ) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, &e.to_string()),
    };

    match dispatch(
        ctx,
        &url,
        conn.mirror_h2,
        upstream_proxy.as_deref(),
        outbound,
    )
    .await
    {
        Ok((resp, _timings, _addr)) => resp.map(crate::box_body),
        Err(e) => error_response(
            StatusCode::BAD_GATEWAY,
            &format!("could not connect to {host}:{port}: {e}"),
        ),
    }
}

pub(crate) fn default_port(scheme: &str) -> u16 {
    match scheme {
        "https" | "wss" => 443,
        _ => 80,
    }
}

// ===== Shared upstream dispatch =====

/// Obtains a connection and sends `outbound`, wrapping the response body so
/// the connection is returned to the pool once fully, cleanly drained.
pub(crate) async fn dispatch(
    ctx: &ProxyContext,
    url: &url::Url,
    mirror_h2: bool,
    upstream_proxy: Option<&str>,
    outbound: Request<BoxBody>,
) -> Result<(
    Response<ReleaseOnComplete<Incoming>>,
    hamsy_core::Timings,
    Option<String>,
)> {
    let host = url
        .host_str()
        .ok_or_else(|| ProxyError::InvalidTarget("missing host".to_string()))?
        .to_string();
    let port = url
        .port_or_known_default()
        .unwrap_or(default_port(url.scheme()));
    let scheme = url.scheme().to_string();
    let https = scheme.eq_ignore_ascii_case("https");

    let obtained = ctx
        .upstream
        .obtain(&scheme, &host, port, mirror_h2, upstream_proxy)
        .await?;
    let crate::upstream::Obtained {
        mut sender,
        connect_ms,
        ssl_ms,
        server_addr,
        ..
    } = obtained;

    let wait_start = tokio::time::Instant::now();
    let resp = sender
        .send_request(outbound)
        .await
        .map_err(ProxyError::from)?;
    let wait_ms = wait_start.elapsed().as_secs_f64() * 1000.0;

    let (resp_parts, resp_body) = resp.into_parts();
    let released =
        ReleaseOnComplete::new(resp_body, ctx.upstream.clone(), https, host, port, sender);
    let resp = Response::from_parts(resp_parts, released);

    // NOTE: hyper's high-level client API doesn't expose a hook between
    // "request fully sent" and "response headers received", so `send`
    // (time spent uploading the request) can't be measured separately here;
    // the whole span is attributed to `wait`. Documented simplification.
    let timings = hamsy_core::Timings {
        blocked: 0.0,
        dns: -1.0,
        connect: connect_ms,
        ssl: ssl_ms,
        send: 0.0,
        wait: wait_ms,
        receive: 0.0,
    };
    Ok((resp, timings, server_addr))
}

/// Builds the outbound request sent upstream, choosing absolute-form
/// (`scheme://host/path`) vs. origin-form (`/path`) for the request target.
///
/// Absolute-form is required when chaining through an upstream proxy
/// (`via_proxy`), and *also* whenever `version` is HTTP/2: h2 derives its
/// `:authority`/`:scheme` pseudo-headers from the request URI itself, not
/// from the `Host` header, and has no fallback for a relative URI - `h2`
/// rejects it locally with `UserError::MissingUriSchemeAndAuthority` before
/// anything reaches the wire. HTTP/1.x has no such requirement, so it keeps
/// using origin-form (relying on the `Host` header set by callers) unless
/// chained through a proxy.
pub(crate) fn build_outbound_request(
    method: &Method,
    url: &url::Url,
    headers: &[HeaderPair],
    via_proxy: bool,
    version: http::Version,
    body: BoxBody,
) -> Result<Request<BoxBody>> {
    let needs_absolute_form = via_proxy || version == http::Version::HTTP_2;
    let target = if needs_absolute_form {
        url.as_str().to_string()
    } else {
        path_with_query(url)
    };
    let req = Request::builder()
        .method(method.clone())
        .uri(target)
        .version(version)
        .body(body)
        .map_err(|e| ProxyError::Other(format!("failed to build outbound request: {e}")))?;
    let (mut parts, body) = req.into_parts();
    apply_headers_to_map(&mut parts.headers, headers)?;
    Ok(Request::from_parts(parts, body))
}

// ===== The captured (recorded + rule-applied) pipeline =====

#[allow(clippy::too_many_lines)]
async fn handle_captured_request(
    ctx: ProxyContext,
    parts: http::request::Parts,
    body: Incoming,
    conn: ConnInfo,
    mut url: url::Url,
    mut req_headers: Vec<HeaderPair>,
) -> Response<BoxBody> {
    let started_at = now_ms();
    let flow_id = Uuid::new_v4();
    let seq = ctx.flows.next_seq();
    let method_str = parts.method.to_string();
    let http_version_str = format!("{:?}", parts.version);
    let mut host = url.host_str().unwrap_or("").to_string();
    let mut port = url
        .port_or_known_default()
        .unwrap_or(default_port(url.scheme()));
    let scheme = url.scheme().to_string();

    let ruleset = ctx.ruleset();
    let need_req_body = ruleset_needs_request_body(&ruleset);
    let max_body_bytes = ctx.max_body_bytes();

    let content_type = header_value(&req_headers, "content-type").map(str::to_string);
    let content_encoding = header_value(&req_headers, "content-encoding").map(str::to_string);
    let path = path_with_query(&url);
    let resource_type = resource_type_for_request(&req_headers, url.path());
    let query = query_pairs(&url);

    // ----- Step 1: obtain (or defer) the request body -----
    // `Buffered` holds a `Bytes` (not `Vec<u8>`): it's cloned below both for
    // rule matching and for capture, and `Bytes::clone` is a cheap refcount
    // bump rather than a byte-for-byte copy, unlike `Vec<u8>::clone`.
    enum ReqBody {
        Buffered(Bytes),
        Streamed(Arc<Mutex<TeeState>>),
    }
    let (req_body_plan, outbound_body_base): (ReqBody, BoxBody) = if need_req_body {
        match collect_capped(body, HARD_BUFFER_CAP).await {
            Ok((bytes, _total, hit_hard_cap)) => {
                if hit_hard_cap {
                    tracing::debug!(host = %host, "request body exceeded 64MiB hard cap; skipping rule body matching/mutation for this request");
                }
                (ReqBody::Buffered(bytes.clone()), crate::full_body(bytes))
            }
            Err(e) => {
                tracing::debug!(error = %e, "failed to buffer request body; forwarding will likely fail");
                (ReqBody::Buffered(Bytes::new()), crate::empty_body())
            }
        }
    } else {
        let (tee, state) = TeeBody::new(body, max_body_bytes);
        (ReqBody::Streamed(state), crate::box_body(tee))
    };

    let req_body_for_rules: Option<Bytes> = match &req_body_plan {
        ReqBody::Buffered(b) => Some(b.clone()),
        ReqBody::Streamed(_) => None,
    };

    let req_record = RequestRecord {
        method: method_str.clone(),
        url: url.to_string(),
        http_version: http_version_str.clone(),
        headers: req_headers.clone(),
        body: match &req_body_plan {
            ReqBody::Buffered(b) => hamsy_core::to_payload(
                b,
                content_type.as_deref(),
                content_encoding.as_deref(),
                max_body_bytes,
            ),
            ReqBody::Streamed(_) => BodyPayload::default(),
        },
        query: query.clone(),
    };

    let mut flow = Flow::new_request(
        flow_id,
        seq,
        started_at,
        method_str.clone(),
        scheme.clone(),
        host.clone(),
        port,
        path.clone(),
        url.to_string(),
        http_version_str.clone(),
        conn.client_addr.to_string(),
        req_record.clone(),
    );
    flow.summary.state = FlowState::Requesting;
    flow.summary.resource_type = resource_type;
    flow.summary.app = conn.app.clone();
    flow.tls = conn.tls.clone();
    ctx.flows.insert(flow.clone());
    let _ = ctx.events.send(hamsy_core::ServerEvent::Flow {
        flow: flow.summary(),
    });

    // ----- Step 2: request-phase rules -----
    let outcome = ruleset.apply_request(RequestCtx {
        method: &method_str,
        url: &url,
        headers: &req_headers,
        body: req_body_for_rules.as_deref(),
        resource_type,
    });

    if let Some(reason) = outcome.blocked.clone() {
        let original = if outcome.modified {
            Some(req_record.clone())
        } else {
            None
        };
        let body_bytes = serde_json::json!({ "error": "blocked", "reason": reason }).to_string();
        let resp_record = ResponseRecord {
            status: 403,
            status_text: "Forbidden".to_string(),
            http_version: http_version_str.clone(),
            headers: vec![HeaderPair::new("Content-Type", "application/json")],
            body: hamsy_core::to_payload(
                body_bytes.as_bytes(),
                Some("application/json"),
                None,
                max_body_bytes,
            ),
        };
        let finished_at = now_ms();
        let summary = ctx.flows.update(flow_id, |f| {
            f.original_request = original;
            f.mark_complete(resp_record, finished_at);
            f.summary.matched_rules = outcome.matched.clone();
            f.summary.modified = true;
        });
        if let Some(summary) = summary {
            let _ = ctx
                .events
                .send(hamsy_core::ServerEvent::Flow { flow: summary });
        }
        return error_response(StatusCode::FORBIDDEN, &reason);
    }

    if let Some(mocked) = outcome.mocked.clone() {
        if mocked.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(mocked.delay_ms)).await;
        }
        let original = if outcome.modified {
            Some(req_record.clone())
        } else {
            None
        };
        return respond_mocked(
            &ctx,
            flow_id,
            http_version_str.clone(),
            original,
            outcome.matched.clone(),
            max_body_bytes,
            mocked,
        )
        .await;
    }

    // ----- Step 3: apply request mutations, recompute target -----
    let original_request = if outcome.modified {
        Some(req_record.clone())
    } else {
        None
    };

    if outcome.url != url.to_string() {
        if let Ok(parsed) = url::Url::parse(&outcome.url) {
            url = parsed;
            host = url.host_str().unwrap_or(&host).to_string();
            port = url.port_or_known_default().unwrap_or(port);
            set_host_header(&mut req_headers, &host, port, url.scheme());
        }
    }
    req_headers = outcome.headers.clone();
    strip_hop_by_hop(&mut req_headers);
    if !outcome.method.eq_ignore_ascii_case(&method_str) {
        // method changed via SetMethod; parsed below when building the request.
    }
    let final_method =
        http::Method::from_bytes(outcome.method.as_bytes()).unwrap_or(parts.method.clone());

    let outbound_body = if let Some(new_body) = &outcome.body {
        remove_header(&mut req_headers, "content-encoding");
        set_header(
            &mut req_headers,
            "Content-Length",
            &new_body.len().to_string(),
        );
        crate::full_body(new_body.clone())
    } else if let ReqBody::Buffered(b) = &req_body_plan {
        // Buffered but not mutated: still forward the exact bytes we read.
        crate::full_body(b.clone())
    } else {
        outbound_body_base
    };

    // Wire up deferred finalization for the streamed request body case: once
    // the body finishes draining to upstream, backfill the flow's recorded
    // request body (and its original-request snapshot, since body content
    // itself can't have been mutated on this path - see module doc).
    let outbound_body = match &req_body_plan {
        ReqBody::Streamed(state) if outcome.body.is_none() => {
            let state = state.clone();
            let flows = ctx.flows.clone();
            let events = ctx.events.clone();
            let content_type = content_type.clone();
            let content_encoding = content_encoding.clone();
            let modified = outcome.modified;
            let finalize = move || {
                let (captured, total, truncated) = state.lock().snapshot();
                let payload = payload_from_capture(
                    captured,
                    total,
                    truncated,
                    content_type.as_deref(),
                    content_encoding.as_deref(),
                    max_body_bytes,
                );
                if let Some(summary) = flows.update(flow_id, |f| {
                    if let Some(r) = f.request.as_mut() {
                        r.body = payload.clone();
                    }
                    if modified {
                        if let Some(r) = f.original_request.as_mut() {
                            r.body = payload;
                        }
                    }
                }) {
                    let _ = events.send(hamsy_core::ServerEvent::Flow { flow: summary });
                }
            };
            crate::box_body(FinalizeBody::new(outbound_body, finalize))
        }
        _ => outbound_body,
    };

    if outcome.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(outcome.delay_ms)).await;
    }
    let outbound_body = match outcome.throttle_bps {
        Some(bps) => crate::box_body(Throttled::new(outbound_body, bps)),
        None => outbound_body,
    };

    let upstream_proxy = ctx.settings.read().upstream_proxy.clone();
    let via_proxy = upstream_proxy.is_some();
    let outbound = match build_outbound_request(
        &final_method,
        &url,
        &req_headers,
        via_proxy,
        parts.version,
        outbound_body,
    ) {
        Ok(r) => r,
        Err(e) => {
            return finalize_error(
                &ctx,
                flow_id,
                original_request,
                outcome.matched.clone(),
                outcome.modified,
                &e,
            )
            .await;
        }
    };

    // ----- Step 4: dispatch upstream -----
    let dispatch_result = dispatch(
        &ctx,
        &url,
        conn.mirror_h2,
        upstream_proxy.as_deref(),
        outbound,
    )
    .await;
    let (resp, timings, server_addr) = match dispatch_result {
        Ok(v) => v,
        Err(e) => {
            return finalize_error(
                &ctx,
                flow_id,
                original_request,
                outcome.matched.clone(),
                outcome.modified,
                &e,
            )
            .await;
        }
    };

    if let Some(summary) = ctx.flows.update(flow_id, |f| {
        f.summary.state = FlowState::Responding;
        f.server_addr = server_addr.clone();
        f.timings = timings.clone();
    }) {
        let _ = ctx
            .events
            .send(hamsy_core::ServerEvent::Flow { flow: summary });
    }

    // ----- Step 5: response phase -----
    let (resp_parts, resp_body) = resp.into_parts();
    let resp_headers = header_pairs_from(&resp_parts.headers);
    let resp_content_type = header_value(&resp_headers, "content-type").map(str::to_string);
    let resp_content_encoding = header_value(&resp_headers, "content-encoding").map(str::to_string);
    let need_resp_body = ruleset_needs_response_body(&ruleset);
    let status_text = resp_parts
        .status
        .canonical_reason()
        .unwrap_or("")
        .to_string();
    let resp_http_version = format!("{:?}", resp_parts.version);

    if need_resp_body {
        let (bytes, _total, hit_hard_cap) = match collect_capped(resp_body, HARD_BUFFER_CAP).await {
            Ok(v) => v,
            Err(e) => {
                return finalize_error(
                    &ctx,
                    flow_id,
                    original_request,
                    outcome.matched.clone(),
                    outcome.modified,
                    &e,
                )
                .await;
            }
        };
        if hit_hard_cap {
            tracing::debug!(host = %host, "response body exceeded 64MiB hard cap; skipping rule body matching/mutation for this response");
        }
        let resp_record = ResponseRecord {
            status: resp_parts.status.as_u16(),
            status_text: status_text.clone(),
            http_version: resp_http_version.clone(),
            headers: resp_headers.clone(),
            body: hamsy_core::to_payload(
                &bytes,
                resp_content_type.as_deref(),
                resp_content_encoding.as_deref(),
                max_body_bytes,
            ),
        };

        let resp_outcome: ResponseOutcome = ruleset.apply_response(ResponseCtx {
            method: &method_str,
            url: &url,
            headers: &req_headers,
            body: req_body_for_rules.as_deref(),
            resource_type,
            status: resp_parts.status.as_u16(),
            resp_headers: &resp_headers,
            resp_body: Some(&bytes),
        });

        let original_response = if resp_outcome.modified {
            Some(resp_record.clone())
        } else {
            None
        };
        let mut final_headers = resp_outcome.headers.clone();
        // `Bytes` rather than `Vec<u8>`: the unmodified path below just
        // reuses the already-materialized `bytes` via a cheap refcount
        // clone instead of copying the whole body again.
        let final_body_bytes: Bytes = match &resp_outcome.body {
            Some(b) => {
                remove_header(&mut final_headers, "content-encoding");
                set_header(&mut final_headers, "Content-Length", &b.len().to_string());
                Bytes::from(b.clone())
            }
            None => bytes.clone(),
        };
        strip_hop_by_hop(&mut final_headers);

        if resp_outcome.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(resp_outcome.delay_ms)).await;
        }

        let final_record = ResponseRecord {
            status: resp_outcome.status,
            status_text: StatusCode::from_u16(resp_outcome.status)
                .ok()
                .and_then(|s| s.canonical_reason())
                .unwrap_or(&status_text)
                .to_string(),
            http_version: resp_http_version,
            headers: final_headers.clone(),
            body: hamsy_core::to_payload(
                &final_body_bytes,
                resp_content_type.as_deref(),
                None,
                max_body_bytes,
            ),
        };

        let matched_rules = union_matched(outcome.matched.clone(), resp_outcome.matched.clone());
        let modified = outcome.modified || resp_outcome.modified;
        let finished_at = now_ms();
        if let Some(summary) = ctx.flows.update(flow_id, |f| {
            f.original_request = original_request.clone();
            f.original_response = original_response;
            f.mark_complete(final_record, finished_at);
            f.summary.matched_rules = matched_rules;
            f.summary.modified = modified;
        }) {
            let _ = ctx
                .events
                .send(hamsy_core::ServerEvent::Flow { flow: summary });
        }

        let mut response_body: BoxBody = crate::full_body(final_body_bytes);
        if let Some(bps) = resp_outcome.throttle_bps {
            response_body = crate::box_body(Throttled::new(response_body, bps));
        }
        return build_client_response(resp_outcome.status, &final_headers, response_body);
    }

    // Streamed response path: rules can still touch status/headers (never
    // body, per the buffering decision), the body streams straight through.
    let resp_outcome: ResponseOutcome = ruleset.apply_response(ResponseCtx {
        method: &method_str,
        url: &url,
        headers: &req_headers,
        body: req_body_for_rules.as_deref(),
        resource_type,
        status: resp_parts.status.as_u16(),
        resp_headers: &resp_headers,
        resp_body: None,
    });
    let mut final_headers = resp_outcome.headers.clone();
    strip_hop_by_hop(&mut final_headers);
    remove_header(&mut final_headers, "content-length"); // length unknown ahead of stream completion changes

    if resp_outcome.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(resp_outcome.delay_ms)).await;
    }

    let (tee_body, tee_state) = TeeBody::new(resp_body, max_body_bytes);
    let matched_rules = union_matched(outcome.matched.clone(), resp_outcome.matched.clone());
    let modified = outcome.modified || resp_outcome.modified;
    let final_status = resp_outcome.status;
    let final_status_text = StatusCode::from_u16(final_status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or(&status_text)
        .to_string();
    let flows_for_finalize = ctx.flows.clone();
    let events_for_finalize = ctx.events.clone();
    let headers_for_finalize = final_headers.clone();
    let finalize = move || {
        let (captured, total, truncated) = tee_state.lock().snapshot();
        let payload = payload_from_capture(
            captured,
            total,
            truncated,
            resp_content_type.as_deref(),
            resp_content_encoding.as_deref(),
            max_body_bytes,
        );
        let record = ResponseRecord {
            status: final_status,
            status_text: final_status_text,
            http_version: resp_http_version,
            headers: headers_for_finalize,
            body: payload.clone(),
        };
        let finished_at = now_ms();
        if let Some(summary) = flows_for_finalize.update(flow_id, |f| {
            f.original_request = original_request.clone();
            if modified {
                f.original_response = f.response.clone();
            }
            f.mark_complete(record, finished_at);
            f.summary.matched_rules = matched_rules.clone();
            f.summary.modified = modified;
        }) {
            let _ = events_for_finalize.send(hamsy_core::ServerEvent::Flow { flow: summary });
        }
    };
    let final_body = crate::box_body(FinalizeBody::new(tee_body, finalize));
    let final_body = match resp_outcome.throttle_bps {
        Some(bps) => crate::box_body(Throttled::new(final_body, bps)),
        None => final_body,
    };

    build_client_response(final_status, &final_headers, final_body)
}

async fn finalize_error(
    ctx: &ProxyContext,
    flow_id: FlowId,
    original_request: Option<RequestRecord>,
    matched: Vec<String>,
    modified: bool,
    err: &ProxyError,
) -> Response<BoxBody> {
    let message = err.to_string();
    let finished_at = now_ms();
    if let Some(summary) = ctx.flows.update(flow_id, |f| {
        f.original_request = original_request.clone();
        f.summary.matched_rules = matched.clone();
        f.summary.modified = modified;
        f.mark_error(message.clone(), finished_at);
    }) {
        let _ = ctx
            .events
            .send(hamsy_core::ServerEvent::Flow { flow: summary });
    }
    error_response(StatusCode::BAD_GATEWAY, &message)
}

async fn respond_mocked(
    ctx: &ProxyContext,
    flow_id: FlowId,
    http_version: String,
    original_request: Option<RequestRecord>,
    matched: Vec<String>,
    max_body_bytes: usize,
    mocked: MockedResponse,
) -> Response<BoxBody> {
    let status_text = safe_status(mocked.status)
        .canonical_reason()
        .unwrap_or("")
        .to_string();
    let content_type = mocked
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.clone());
    let resp_record = ResponseRecord {
        status: mocked.status,
        status_text,
        http_version,
        headers: mocked.headers.clone(),
        body: hamsy_core::to_payload(&mocked.body, content_type.as_deref(), None, max_body_bytes),
    };
    let finished_at = now_ms();
    if let Some(summary) = ctx.flows.update(flow_id, |f| {
        f.original_request = original_request.clone();
        f.mark_complete(resp_record, finished_at);
        f.summary.matched_rules = matched.clone();
        f.summary.modified = true;
        f.summary.from_cache = true;
    }) {
        let _ = ctx
            .events
            .send(hamsy_core::ServerEvent::Flow { flow: summary });
    }
    build_client_response(
        mocked.status,
        &mocked.headers,
        crate::full_body(mocked.body),
    )
}

pub(crate) fn build_client_response(
    status: u16,
    headers: &[HeaderPair],
    body: BoxBody,
) -> Response<BoxBody> {
    let mut builder = Response::builder().status(safe_status(status));
    let map = builder.headers_mut();
    if let Some(map) = map {
        if apply_headers_to_map(map, headers).is_err() {
            // Fall back to no custom headers rather than failing the response.
            map.clear();
        }
    }
    builder.body(body).unwrap_or_else(|_| {
        let mut resp = Response::new(crate::empty_body());
        *resp.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        resp
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hamsy_core::{BodyCond, BodyCondOp, Matcher};

    fn sample_rule(actions: Vec<Action>, matcher: Matcher) -> Rule {
        Rule {
            id: "r1".to_string(),
            name: "r1".to_string(),
            enabled: true,
            priority: 0,
            group: None,
            notes: None,
            matcher,
            actions,
        }
    }

    #[test]
    fn strip_hop_by_hop_removes_known_headers() {
        let mut headers = vec![
            HeaderPair::new("Connection", "keep-alive"),
            HeaderPair::new("X-Custom", "1"),
            HeaderPair::new("Transfer-Encoding", "chunked"),
        ];
        strip_hop_by_hop(&mut headers);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].name, "X-Custom");
    }

    #[test]
    fn ruleset_needs_request_body_detects_matcher_and_actions() {
        let with_cond = RuleSet::new(vec![sample_rule(
            vec![],
            Matcher {
                request_body: Some(BodyCond {
                    op: BodyCondOp::Contains,
                    value: "x".to_string(),
                }),
                ..Matcher::default()
            },
        )]);
        assert!(ruleset_needs_request_body(&with_cond));

        let with_action = RuleSet::new(vec![sample_rule(
            vec![Action::ReplaceInRequestBody {
                find: "a".to_string(),
                replace: "b".to_string(),
                regex: false,
            }],
            Matcher::default(),
        )]);
        assert!(ruleset_needs_request_body(&with_action));

        let without = RuleSet::new(vec![sample_rule(
            vec![Action::SetRequestHeader {
                name: "X".to_string(),
                value: "1".to_string(),
            }],
            Matcher::default(),
        )]);
        assert!(!ruleset_needs_request_body(&without));
    }

    #[test]
    fn build_target_url_absolute_form_passthrough() {
        let req = Request::builder()
            .uri("http://example.com/path?x=1")
            .body(())
            .unwrap();
        let (parts, _) = req.into_parts();
        let conn = ConnInfo {
            client_addr: "127.0.0.1:1".parse().unwrap(),
            scheme: "http",
            authority: None,
            tls: None,
            mirror_h2: false,
            app: None,
        };
        let url = build_target_url(&parts, &conn).unwrap();
        assert_eq!(url.as_str(), "http://example.com/path?x=1");
    }

    #[test]
    fn build_target_url_relative_form_uses_authority() {
        let req = Request::builder().uri("/path?x=1").body(()).unwrap();
        let (parts, _) = req.into_parts();
        let conn = ConnInfo {
            client_addr: "127.0.0.1:1".parse().unwrap(),
            scheme: "https",
            authority: Some("example.com:8443".to_string()),
            tls: None,
            mirror_h2: false,
            app: None,
        };
        let url = build_target_url(&parts, &conn).unwrap();
        assert_eq!(url.as_str(), "https://example.com:8443/path?x=1");
    }

    #[test]
    fn safe_status_falls_back_on_invalid_code() {
        assert_eq!(safe_status(200), StatusCode::OK);
        assert_eq!(safe_status(9999), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn payload_from_capture_overrides_size_and_kind_when_truncated() {
        let payload = payload_from_capture(
            Bytes::from_static(b"partial"),
            1_000_000,
            true,
            Some("text/plain"),
            None,
            1024,
        );
        assert_eq!(payload.kind, BodyKind::Truncated);
        assert!(payload.truncated);
        assert_eq!(payload.size, 1_000_000);
    }

    #[test]
    fn build_outbound_request_http2_uses_absolute_form() {
        let url = url::Url::parse("https://example.com/foo?bar=1").unwrap();
        let req = build_outbound_request(
            &Method::GET,
            &url,
            &[],
            false,
            http::Version::HTTP_2,
            crate::empty_body(),
        )
        .unwrap();
        assert!(req.uri().authority().is_some());
        assert!(req.uri().scheme().is_some());
        assert_eq!(
            req.uri(),
            &http::Uri::try_from("https://example.com/foo?bar=1").unwrap()
        );
    }

    #[test]
    fn build_outbound_request_http11_direct_uses_origin_form() {
        let url = url::Url::parse("https://example.com/foo?bar=1").unwrap();
        let req = build_outbound_request(
            &Method::GET,
            &url,
            &[],
            false,
            http::Version::HTTP_11,
            crate::empty_body(),
        )
        .unwrap();
        assert!(req.uri().authority().is_none());
        assert_eq!(req.uri().path_and_query().unwrap(), "/foo?bar=1");
    }

    #[test]
    fn union_matched_dedupes_preserving_order() {
        let a = vec!["r1".to_string(), "r2".to_string()];
        let b = vec!["r2".to_string(), "r3".to_string()];
        assert_eq!(
            union_matched(a, b),
            vec!["r1".to_string(), "r2".to_string(), "r3".to_string()]
        );
    }
}
