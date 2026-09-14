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
//! The core rule engine probes metadata mutations in execution order, stopping
//! at the first reachable body condition/action. Unrelated requests keep streaming.
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
    BodyKind, BodyPayload, Flow, FlowId, FlowState, HeaderPair, MockedResponse, RequestCtx,
    RequestRecord, ResourceType, ResponseCtx, ResponseOutcome, ResponseRecord, TlsInfo,
};

#[cfg(test)]
use hamsy_core::{Action, Rule, RuleSet};

use crate::config::{OwnListener, ProxyContext};
use crate::error::{ProxyError, Result};
use crate::tee::{body_cpu, collect_for_rules, CaptureBody, TeeBody, TeeState, Throttled};
use crate::upstream::{HttpVersion, ReleaseOnComplete};
use crate::BoxBody;

/// Hard ceiling on how much of a request/response body we'll fully buffer
/// in memory to let a rule mutate it. Exceeding the limit returns an explicit
/// error rather than silently forwarding a truncated upload or download.
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
    /// Best-effort attribution resolved independently of network traffic.
    pub app_resolution: Option<crate::appid::AppResolution>,
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
    let port = url
        .port_or_known_default()
        .unwrap_or(default_port(url.scheme()));

    let mut headers = header_pairs_from(&parts.headers);
    strip_hop_by_hop(&mut headers);

    // Loopback can now be excluded from the OS system-proxy bypass list
    // (`Settings::system_proxy_bypass`), so a request whose target is one of
    // hamsy's own listeners can genuinely reach this point. This check runs
    // *before* the pause/capture gate below because both of its outcomes
    // must apply unconditionally - even while paused, or while the host is
    // excluded from capture. See `OwnListener`'s doc for why the two cases
    // are handled oppositely (refuse vs. forward-but-never-capture).
    match ctx.own_listener(&host, port) {
        OwnListener::ProxyPort => return Ok(self_loop_response(&host, port)),
        OwnListener::UiPort => {
            return Ok(forward_untouched(&ctx, parts, body, &conn, url, headers).await)
        }
        OwnListener::None => {}
    }

    if ctx.is_paused() || !ctx.should_capture(&host) {
        return Ok(forward_untouched(&ctx, parts, body, &conn, url, headers).await);
    }

    Ok(handle_captured_request(ctx, parts, body, conn, url, headers).await)
}

/// Builds the refusal response for a request whose target is
/// [`OwnListener::ProxyPort`] - hamsy's own MITM proxy listener.
///
/// Forwarding such a request would make the proxy dial itself, and the
/// dialed request would in turn be accepted, handled, and dialed again by
/// this very code path: unbounded recursion that exhausts sockets/file
/// descriptors well before anything else notices. `508 Loop Detected`
/// (WebDAV, RFC 5842) is the closest standard status for "this would
/// recurse forever". Refusing fast here rather than forwarding is a hard
/// prerequisite for `Settings::system_proxy_bypass` ever excluding
/// loopback: without it, a client dialing hamsy's own proxy port *through*
/// hamsy (unavoidable once loopback isn't OS-bypassed) would spin the
/// process rather than getting a clear, immediate error.
///
/// Shared by the plain-HTTP path (here), the `CONNECT` path
/// (`connect::handle_connect`), and the WebSocket-upgrade path
/// (`websocket::handle_upgrade`) so the refusal is surfaced identically
/// regardless of which one caught it.
pub(crate) fn self_loop_response(host: &str, port: u16) -> Response<BoxBody> {
    error_response(
        StatusCode::LOOP_DETECTED,
        &format!(
            "refusing to forward to hamsy's own proxy port ({host}:{port}); this would loop back into hamsy itself"
        ),
    )
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

/// Applies a request-phase rule outcome's final header list and (possibly)
/// rewritten URL to the in-flight request state.
///
/// `outcome`'s headers are adopted as-is, and a URL rewrite deliberately
/// does *not* overwrite `Host` with the rewrite target's host. This follows
/// the "Map Remote" convention used by Charles/Proxyman and by
/// `proxy_set_header Host $host` in nginx: redirecting a request to a
/// different upstream changes *where* it's sent, not what origin it claims
/// to be. The target's own host is very often the wrong `Host` value - e.g.
/// a rule rewriting `https://dcs4s-live.mp.lura.live` to a local stitcher at
/// `http://0.0.0.0:8082` still needs the stitcher to see
/// `Host: dcs4s-live.mp.lura.live`, because the stitcher mints signed CDN
/// URLs derived from the incoming Host; handing it `Host: 0.0.0.0:8082`
/// makes it sign for the wrong origin and the CDN 403s the segments. A rule
/// that genuinely wants a different `Host` can still set one explicitly via
/// a `setRequestHeader` action - since that header arrives as part of
/// `outcome_headers`, it's adopted like any other header and is never
/// clobbered here.
///
/// The one case this function *does* synthesize a `Host` is when none is
/// present at all after adopting `outcome_headers` and the URL was
/// rewritten: an HTTP/2 client's request carries no `Host` header to begin
/// with (h2 sends the `:authority` pseudo-header instead), so forwarding it
/// as-is after a cross-host rewrite to an HTTP/1.1 upstream would produce a
/// Host-less request that most origins reject. In that case a `Host` is
/// computed from the *original* (pre-rewrite) URL, not the rewrite target,
/// so it still reflects the request's real origin rather than the plumbing
/// detail of where it now happens to be sent. The non-rewrite case is left
/// to [`ensure_host_header`]'s last-resort fallback further down the
/// pipeline.
///
/// A retained (or synthesized) `Host` that now names a different host than
/// `url` is exactly what it looks like to a downstream h2 leg -
/// [`align_h2_authority_with_host`] is what keeps that legal there by moving
/// the identity from the header into `:authority`, since h2 (unlike h1)
/// requires the two to agree when both are present.
pub(crate) fn apply_outcome_url_and_headers(
    url: &mut url::Url,
    host: &mut String,
    port: &mut u16,
    req_headers: &mut Vec<HeaderPair>,
    outcome_url: &str,
    outcome_headers: &[HeaderPair],
) {
    let original_url = url.clone();
    let url_changed = outcome_url != url.to_string();
    *req_headers = outcome_headers.to_vec();
    strip_hop_by_hop(req_headers);
    if url_changed {
        if let Ok(parsed) = url::Url::parse(outcome_url) {
            *url = parsed;
            *host = url.host_str().unwrap_or(host.as_str()).to_string();
            *port = url.port_or_known_default().unwrap_or(*port);
            if header_value(req_headers, "host").is_none() {
                let orig_host = original_url.host_str().unwrap_or(host.as_str());
                let orig_port = original_url
                    .port_or_known_default()
                    .unwrap_or(default_port(original_url.scheme()));
                set_host_header(req_headers, orig_host, orig_port, original_url.scheme());
            }
        }
    }
}

/// Builds the [`RequestRecord`] for a request as it is actually being sent
/// upstream, once request-phase rule mutations have been applied.
///
/// This is what [`Flow::request`] must hold after a rule rewrite - as
/// opposed to [`Flow::original_request`], the pre-rule client snapshot -
/// so a rewrite to e.g. a dead local server is still debuggable from the
/// captured data. `url`/`http_version`/`req_headers` are the post-mutation
/// values the caller already computed (`req_headers` is expected to be
/// `outcome.headers` post-[`strip_hop_by_hop`], with any body-driven
/// `Content-Length`/`Content-Encoding` adjustments already applied), and
/// `query` is recomputed from the rewritten `url` rather than copied from
/// the pre-rule record.
///
/// `outcome_body` is `RequestOutcome::body`: when a rule replaced the body,
/// it's re-encoded via [`hamsy_core::to_payload`] with the encoding forced
/// to `None`, mirroring that the caller already strips `Content-Encoding`
/// before forwarding a replaced body. When `None`, `fallback_body` is
/// reused as-is - callers must apply the resulting record to the flow
/// *before* the request is dispatched upstream, so a streamed body's later
/// `FinalizeBody` backfill (which only ever assigns `.body`, see
/// `handle_captured_request`) lands after this and is never clobbered by
/// it.
#[allow(clippy::too_many_arguments)]
fn effective_request_record(
    outcome_method: &str,
    url: &url::Url,
    http_version: &str,
    req_headers: &[HeaderPair],
    outcome_body: Option<&[u8]>,
    fallback_body: &BodyPayload,
    content_type: Option<&str>,
    max_body_bytes: usize,
) -> RequestRecord {
    RequestRecord {
        method: outcome_method.to_string(),
        url: url.to_string(),
        http_version: http_version.to_string(),
        headers: req_headers.to_vec(),
        body: match outcome_body {
            Some(bytes) => hamsy_core::to_payload(bytes, content_type, None, max_body_bytes),
            None => fallback_body.clone(),
        },
        query: query_pairs(url),
    }
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
        StatusCode::LOOP_DETECTED => "loop_detected",
        _ => "error",
    }
}

// ===== Body preparation =====

/// Rules consume decoded content. Unknown/corrupt/oversized codings produce an
/// explicit error rather than feeding compressed binary to text replacements.
fn decode_for_rules(bytes: &[u8], encoding: Option<&str>) -> Result<Bytes> {
    if encoding.is_some_and(|value| {
        value.split(',').any(|token| {
            !matches!(
                token.trim().to_ascii_lowercase().as_str(),
                "" | "identity" | "gzip" | "x-gzip" | "deflate" | "br" | "zstd"
            )
        })
    }) {
        return Err(ProxyError::Other(
            "unsupported Content-Encoding for body rule".into(),
        ));
    }
    let (decoded, truncated) = hamsy_core::decode_body(bytes, encoding, HARD_BUFFER_CAP)
        .map_err(|e| ProxyError::Other(format!("cannot decode body for rule: {e}")))?;
    if truncated {
        return Err(ProxyError::Other(
            "decoded body exceeds rule buffer limit".into(),
        ));
    }
    Ok(Bytes::from(decoded))
}

fn capture_payload(
    state: &Mutex<TeeState>,
    capture: bool,
    content_type: Option<&str>,
    encoding: Option<&str>,
    cap: usize,
) -> BodyPayload {
    if !capture {
        let size = state.lock().total();
        return BodyPayload {
            kind: if size == 0 {
                BodyKind::None
            } else {
                BodyKind::Truncated
            },
            data: String::new(),
            size,
            truncated: size != 0,
            encoding: encoding.map(str::to_string),
        };
    }
    let (bytes, total, truncated) = state.lock().snapshot();
    payload_from_capture(bytes, total, truncated, content_type, encoding, cap)
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
    let mut payload = hamsy_core::to_payload(&captured, content_type, content_encoding, max_bytes);
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
        version,
        connect_ms,
        ssl_ms,
        server_addr,
        via_proxy,
    } = obtained;

    // The sender obtained above speaks whatever version was actually
    // negotiated on the *upstream* connection, which can differ from
    // `outbound`'s version - e.g. a rule can redirect an h2 MITM client's
    // request to a plain-HTTP target, and plain-HTTP upstreams are always
    // HTTP/1.1 (see `upstream::Connector::obtain`'s non-`https` branch).
    // Sending an HTTP/2-labeled, absolute-form, Host-less request over an
    // HTTP/1.1 connection gets rejected by most origins ("HOST header is
    // missing"), so adapt the request to match the sender before sending.
    let (mut outbound_parts, outbound_body) = outbound.into_parts();
    adapt_request_to_sender(&mut outbound_parts, version, via_proxy, url);
    let outbound = Request::from_parts(outbound_parts, outbound_body);

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
    if version == http::Version::HTTP_2 {
        align_h2_authority_with_host(&mut parts);
    }
    Ok(Request::from_parts(parts, body))
}

/// Reconciles a `Host` header with an HTTP/2 request's `:authority` by
/// moving it there, then dropping the header.
///
/// In h2, origin identity is carried by the `:authority` pseudo-header
/// (which hyper derives from the request URI), not by a `host` header - h2
/// requests don't normally carry one at all. RFC 9113 §8.3.1 requires that
/// *if* a `host` header is present, it must match `:authority` exactly; a
/// mismatch is a protocol error the origin is required to reject, typically
/// by resetting the stream with `RST_STREAM(PROTOCOL_ERROR)`.
///
/// That mismatch is exactly what [`apply_outcome_url_and_headers`] can
/// produce: on a cross-host `rewriteUrl` rule, it deliberately *keeps* the
/// original `Host` header rather than adopting the rewrite target's host
/// (Charles "Map Remote" convention - see its doc for why). Over HTTP/1.1
/// that's fine, since `Host` is just a header there. But
/// [`build_outbound_request`] and [`adapt_request_to_sender`] both set an h2
/// request's URI to the (rewritten) `url`, so hyper would derive
/// `:authority` from the rewrite target while the stale `Host` header still
/// names the original origin - two different values, which h2 (correctly)
/// treats as a protocol violation rather than silently preferring one.
///
/// The fix is to honor the *intent* behind keeping `Host` - the upstream
/// should see the original origin identity - in the way h2 actually allows:
/// by putting that identity in `:authority` instead of a header. This is
/// purely a request-framing concern and does not change *where* the request
/// is sent: [`dispatch`] independently derives the connection target (and
/// TLS SNI) from `url`'s host, before the request this function edits is
/// ever handed to it. So the net effect matches the HTTP/1.1 path exactly:
/// connect to the rewrite target, but claim the original origin - just
/// expressed via `:authority` instead of `Host`.
///
/// Infallible, as required for a step in the request-forwarding path that
/// must never panic on attacker- or rule-controlled input:
/// - No `Host` header: nothing to do: `:authority` is already whatever the
///   URI produced.
/// - `Host` present and a valid [`http::uri::Authority`]: the URI is rebuilt
///   with that authority (keeping the existing scheme and path-and-query),
///   and the header is removed. If `Host` already equals the URI's
///   authority this is a no-op beyond dropping the now-redundant header.
/// - `Host` present but invalid, or URI reconstruction otherwise fails: the
///   authority is left as derived from the URI, but the header is still
///   removed - a leftover conflicting `host` header is the actual protocol
///   violation this function exists to prevent, so on any failure to honor
///   it, removing it is the safe default rather than leaving it in place.
fn align_h2_authority_with_host(parts: &mut http::request::Parts) {
    let Some(host_value) = parts.headers.remove(http::header::HOST) else {
        return;
    };
    let Some(authority) = host_value
        .to_str()
        .ok()
        .and_then(|s| s.parse::<http::uri::Authority>().ok())
    else {
        return;
    };
    let mut uri_parts = parts.uri.clone().into_parts();
    uri_parts.authority = Some(authority);
    if let Ok(uri) = http::Uri::from_parts(uri_parts) {
        parts.uri = uri;
    }
}

/// Adapts an already-built outbound request's version, request-target form,
/// and `Host` header to match `sender_version` - the version actually
/// negotiated on the upstream connection [`dispatch`] obtained, which can
/// differ from the version the request was originally built with (see
/// [`dispatch`]'s call site for why: a rule can retarget a request to an
/// upstream whose negotiated protocol doesn't match the client's).
///
/// - HTTP/2 request onto an HTTP/1 sender: downgrades to HTTP/1.1 and,
///   unless `via_proxy` (absolute-form is required when chaining through an
///   upstream proxy, regardless of version), rewrites the URI to
///   origin-form (path + query only). Either way, ensures a `Host` header
///   is present - h2 requests never carry one, relying on the `:authority`
///   pseudo-header instead, which HTTP/1.1 origins don't understand. The
///   `:authority` present on entry (captured before any origin-form rewrite
///   discards it) is preferred over deriving one from `url` - see
///   [`ensure_host_header`]'s doc for why this matters for a rewrite rule
///   that changed the request's host.
/// - Non-HTTP/2 request onto an HTTP/2 sender: upgrades to HTTP/2 and
///   rewrites the URI to absolute-form, since h2 derives its
///   `:authority`/`:scheme` pseudo-headers from the URI itself (see
///   [`build_outbound_request`]'s doc). Then reconciles any surviving `Host`
///   header with the new `:authority` via
///   [`align_h2_authority_with_host`] - an h1-shaped request being upgraded
///   here (e.g. a rule retargeted an h1 client's request to an upstream
///   that negotiated h2) still has whatever `Host` it started with, which
///   can now disagree with the `:authority` just derived from `url`.
/// - Otherwise (versions already compatible): left untouched.
pub(crate) fn adapt_request_to_sender(
    parts: &mut http::request::Parts,
    sender_version: HttpVersion,
    via_proxy: bool,
    url: &url::Url,
) {
    match sender_version {
        HttpVersion::Http1 if parts.version == http::Version::HTTP_2 => {
            parts.version = http::Version::HTTP_11;
            // Captured before the origin-form rewrite below discards it.
            // See `ensure_host_header`'s doc for why this - not `url` - is
            // the right source for the `Host` this downgrade needs.
            let preserved_authority = parts.uri.authority().map(|a| a.as_str().to_string());
            if !via_proxy {
                if let Ok(uri) = path_with_query(url).parse::<http::Uri>() {
                    parts.uri = uri;
                }
            }
            ensure_host_header(&mut parts.headers, url, preserved_authority.as_deref());
        }
        HttpVersion::Http2 if parts.version != http::Version::HTTP_2 => {
            parts.version = http::Version::HTTP_2;
            if let Ok(uri) = url.as_str().parse::<http::Uri>() {
                parts.uri = uri;
            }
            align_h2_authority_with_host(parts);
        }
        _ => {}
    }
}

/// Inserts a `Host` header, but only if one isn't already present.
///
/// `preferred_authority`, when `Some`, is used verbatim; otherwise a value
/// is computed from `url` the same way [`set_host_header`] does.
///
/// The h2-onto-h1 downgrade in [`adapt_request_to_sender`] always passes
/// the pre-downgrade `:authority` as `preferred_authority`, and needs to:
/// an h2 request built via [`build_outbound_request`] has already had any
/// `Host` header folded into `:authority` and removed by
/// [`align_h2_authority_with_host`] - including a `Host`
/// [`apply_outcome_url_and_headers`] preserved from *before* a cross-host
/// `rewriteUrl` rule, which is the entire point of that preservation. By
/// the time a downgrade to HTTP/1.1 happens here, that header is gone, so
/// falling back to `url` (the rewrite *target*) would silently reintroduce
/// the bug both of those functions exist to prevent - just one hop later,
/// and only when the origin happens to decline h2. `:authority` is the only
/// place that original identity still lives, so it must win over `url`.
///
/// Every other caller (there are none today besides that one) can safely
/// pass `None` to get the old `url`-derived behavior.
fn ensure_host_header(headers: &mut HeaderMap, url: &url::Url, preferred_authority: Option<&str>) {
    if headers.contains_key(http::header::HOST) {
        return;
    }
    if let Some(authority) = preferred_authority {
        if let Ok(hv) = http::HeaderValue::from_str(authority) {
            headers.insert(http::header::HOST, hv);
            return;
        }
    }
    let host = url.host_str().unwrap_or("");
    let default_port = default_port(url.scheme());
    let port = url.port_or_known_default().unwrap_or(default_port);
    let value = if port == default_port {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    if let Ok(hv) = http::HeaderValue::from_str(&value) {
        headers.insert(http::header::HOST, hv);
    }
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
    let max_body_bytes = ctx.max_body_bytes();

    let content_type = header_value(&req_headers, "content-type").map(str::to_string);
    let content_encoding = header_value(&req_headers, "content-encoding").map(str::to_string);
    let path = path_with_query(&url);
    let resource_type = resource_type_for_request(&req_headers, url.path());
    let need_req_body = ruleset.needs_request_body(RequestCtx {
        method: &method_str,
        url: &url,
        headers: &req_headers,
        body: None,
        resource_type,
    });
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
        match collect_for_rules(body, HARD_BUFFER_CAP).await {
            Ok(bytes) => (ReqBody::Buffered(bytes.clone()), crate::full_body(bytes)),
            Err(e) => {
                return error_response(StatusCode::PAYLOAD_TOO_LARGE, &e.to_string());
            }
        }
    } else {
        let (tee, state) = TeeBody::new(body, max_body_bytes);
        (ReqBody::Streamed(state), crate::box_body(tee))
    };

    let req_body_for_rules: Option<Bytes> = match &req_body_plan {
        ReqBody::Buffered(b) => {
            let bytes = b.clone();
            let encoding = content_encoding.clone();
            match body_cpu(move || decode_for_rules(&bytes, encoding.as_deref())).await {
                Ok(decoded) => Some(decoded),
                Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
            }
        }
        ReqBody::Streamed(_) => None,
    };

    let req_record = RequestRecord {
        method: method_str.clone(),
        url: url.to_string(),
        http_version: http_version_str.clone(),
        headers: req_headers.clone(),
        body: match &req_body_plan {
            ReqBody::Buffered(b) => {
                let b = b.clone();
                let ct = content_type.clone();
                let ce = content_encoding.clone();
                body_cpu(move || {
                    hamsy_core::to_payload(&b, ct.as_deref(), ce.as_deref(), max_body_bytes)
                })
                .await
            }
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

    if let Some(resolution) = &conn.app_resolution {
        resolution.attach(&ctx, flow.summary.id);
    }

    // ----- Step 2: request-phase rules -----
    let mut outcome = if need_req_body {
        let rules = ruleset.clone();
        let method = method_str.clone();
        let url = url.clone();
        let headers = req_headers.clone();
        let body = req_body_for_rules.clone();
        body_cpu(move || {
            rules.apply_request(RequestCtx {
                method: &method,
                url: &url,
                headers: &headers,
                body: body.as_deref(),
                resource_type,
            })
        })
        .await
    } else {
        ruleset.apply_request(RequestCtx {
            method: &method_str,
            url: &url,
            headers: &req_headers,
            body: None,
            resource_type,
        })
    };
    // An outcome carries the input body even if no action changed it.
    // Preserve the original wire bytes and Content-Encoding in that case.
    if outcome.body.as_deref() == req_body_for_rules.as_deref() {
        outcome.body = None;
    }

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

    apply_outcome_url_and_headers(
        &mut url,
        &mut host,
        &mut port,
        &mut req_headers,
        &outcome.url,
        &outcome.headers,
    );
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

    // `flow.request` was seeded with the pre-rule client request at capture
    // time and never touched again on the old code path, so a rewrite (URL,
    // method, headers, or body) was invisible in captured data even though
    // `outcome.modified` said otherwise. Record the effective, post-mutation
    // request now - strictly before `dispatch` below starts streaming the
    // outbound body - so both the success path and `finalize_error` (neither
    // of which otherwise touches `f.request`) leave the flow holding what
    // was actually sent, and so this can never race the streamed-body
    // backfill closure just below (which only ever assigns `.body`).
    if outcome.modified {
        let effective = {
            let method = outcome.method.clone();
            let url = url.clone();
            let version = http_version_str.clone();
            let headers = req_headers.clone();
            let bytes = outcome.body.clone();
            let recorded = req_record.body.clone();
            let ct = content_type.clone();
            body_cpu(move || {
                effective_request_record(
                    &method,
                    &url,
                    &version,
                    &headers,
                    bytes.as_deref(),
                    &recorded,
                    ct.as_deref(),
                    max_body_bytes,
                )
            })
            .await
        };
        if let Some(summary) = ctx.flows.update(flow_id, |f| {
            f.request = Some(effective);
        }) {
            let _ = ctx
                .events
                .send(hamsy_core::ServerEvent::Flow { flow: summary });
        }
    }

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
            let finalize = move |capture| {
                let payload = capture_payload(
                    &state,
                    capture,
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
            crate::box_body(CaptureBody::new(outbound_body, finalize))
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
    let need_resp_body = ruleset.needs_response_body(ResponseCtx {
        method: &method_str,
        url: &url,
        headers: &req_headers,
        body: req_body_for_rules.as_deref(),
        resource_type,
        status: resp_parts.status.as_u16(),
        resp_headers: &resp_headers,
        resp_body: None,
    });
    let status_text = resp_parts
        .status
        .canonical_reason()
        .unwrap_or("")
        .to_string();
    let resp_http_version = format!("{:?}", resp_parts.version);

    if need_resp_body {
        let bytes = match collect_for_rules(resp_body, HARD_BUFFER_CAP).await {
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
        let prepared = {
            let bytes = bytes.clone();
            let encoding = resp_content_encoding.clone();
            let ct = resp_content_type.clone();
            let rules = ruleset.clone();
            let method = method_str.clone();
            let url = url.clone();
            let headers = req_headers.clone();
            let request_body = req_body_for_rules.clone();
            let response_headers = resp_headers.clone();
            let status = resp_parts.status.as_u16();
            body_cpu(move || {
                let decoded = decode_for_rules(&bytes, encoding.as_deref())?;
                let record_body =
                    hamsy_core::to_payload(&decoded, ct.as_deref(), None, max_body_bytes);
                let mut result = rules.apply_response(ResponseCtx {
                    method: &method,
                    url: &url,
                    headers: &headers,
                    body: request_body.as_deref(),
                    resource_type,
                    status,
                    resp_headers: &response_headers,
                    resp_body: Some(&decoded),
                });
                if result.body.as_deref() == Some(decoded.as_ref()) {
                    result.body = None;
                }
                Ok::<_, ProxyError>((record_body, result))
            })
            .await
        };
        let (mut record_body, mut resp_outcome) = match prepared {
            Ok(value) => value,
            Err(e) => {
                return finalize_error(
                    &ctx,
                    flow_id,
                    original_request,
                    outcome.matched.clone(),
                    outcome.modified,
                    &e,
                )
                .await
            }
        };
        record_body.encoding = resp_content_encoding.clone();
        let resp_record = ResponseRecord {
            status: resp_parts.status.as_u16(),
            status_text: status_text.clone(),
            http_version: resp_http_version.clone(),
            headers: resp_headers.clone(),
            body: record_body,
        };

        let original_response = if resp_outcome.modified {
            Some(resp_record.clone())
        } else {
            None
        };
        let mut final_headers = resp_outcome.headers.clone();
        // `Bytes` rather than `Vec<u8>`: the unmodified path below just
        // reuses the already-materialized `bytes` via a cheap refcount
        // clone instead of copying the whole body again.
        let final_body_bytes: Bytes = match resp_outcome.body.take() {
            Some(b) => {
                remove_header(&mut final_headers, "content-encoding");
                set_header(&mut final_headers, "Content-Length", &b.len().to_string());
                Bytes::from(b)
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
            body: {
                let bytes = final_body_bytes.clone();
                let ct = header_value(&final_headers, "content-type").map(str::to_string);
                let ce = header_value(&final_headers, "content-encoding").map(str::to_string);
                body_cpu(move || {
                    hamsy_core::to_payload(&bytes, ct.as_deref(), ce.as_deref(), max_body_bytes)
                })
                .await
            },
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
    let finalize = move |capture| {
        let payload = capture_payload(
            &tee_state,
            capture,
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
    let final_body = crate::box_body(CaptureBody::new(tee_body, finalize));
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
    let mock_body = Bytes::from(mocked.body);
    let capture_bytes = mock_body.clone();
    let payload = body_cpu(move || {
        hamsy_core::to_payload(
            &capture_bytes,
            content_type.as_deref(),
            None,
            max_body_bytes,
        )
    })
    .await;
    let resp_record = ResponseRecord {
        status: mocked.status,
        status_text,
        http_version,
        headers: mocked.headers.clone(),
        body: payload,
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
    build_client_response(mocked.status, &mocked.headers, crate::full_body(mock_body))
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
        fn needs_body(rules: &RuleSet) -> bool {
            rules.needs_request_body(RequestCtx {
                method: "GET",
                url: &url::Url::parse("http://example.com/").unwrap(),
                headers: &[],
                body: None,
                resource_type: ResourceType::Other,
            })
        }
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
        assert!(needs_body(&with_cond));

        let with_action = RuleSet::new(vec![sample_rule(
            vec![Action::ReplaceInRequestBody {
                find: "a".to_string(),
                replace: "b".to_string(),
                regex: false,
            }],
            Matcher::default(),
        )]);
        assert!(needs_body(&with_action));

        let without = RuleSet::new(vec![sample_rule(
            vec![Action::SetRequestHeader {
                name: "X".to_string(),
                value: "1".to_string(),
            }],
            Matcher::default(),
        )]);
        assert!(!needs_body(&without));
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
            app_resolution: None,
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
            app_resolution: None,
        };
        let url = build_target_url(&parts, &conn).unwrap();
        assert_eq!(url.as_str(), "https://example.com:8443/path?x=1");
    }

    #[test]
    fn self_loop_response_is_508_with_labeled_json_body() {
        let resp = self_loop_response("127.0.0.1", 19080);
        assert_eq!(resp.status(), StatusCode::LOOP_DETECTED);
        assert_eq!(error_label(resp.status()), "loop_detected");
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

    // ----- Bug 1: URL-rewrite outcome must preserve the original Host -----

    #[test]
    fn apply_outcome_url_and_headers_preserves_original_host_on_rewrite() {
        // Simulates a rule rewriting the target to a different host (Map
        // Remote-style, e.g. a CDN URL rewritten to a local stitcher): the
        // outcome's header list carries the *original* Host, and that must
        // survive the rewrite unchanged rather than being replaced with the
        // rewrite target's own host.
        let mut url = url::Url::parse("https://dcs4s-live.mp.lura.live/orig").unwrap();
        let mut host = "dcs4s-live.mp.lura.live".to_string();
        let mut port: u16 = 443;
        let mut req_headers = vec![
            HeaderPair::new("Host", "dcs4s-live.mp.lura.live"),
            HeaderPair::new("X-Custom", "1"),
        ];
        let outcome_headers = vec![
            HeaderPair::new("Host", "dcs4s-live.mp.lura.live"),
            HeaderPair::new("X-Custom", "1"),
        ];

        apply_outcome_url_and_headers(
            &mut url,
            &mut host,
            &mut port,
            &mut req_headers,
            "http://0.0.0.0:8082/orig",
            &outcome_headers,
        );

        // The connection target out-params still point at the rewrite target.
        assert_eq!(host, "0.0.0.0");
        assert_eq!(port, 8082);
        // But the Host *header* stays the original, so a stitcher deriving
        // signed CDN URLs from it signs for the right origin.
        assert_eq!(
            header_value(&req_headers, "host"),
            Some("dcs4s-live.mp.lura.live"),
            "final headers must keep the original host, not the rewrite target's"
        );
        // Non-Host headers from the outcome are still preserved.
        assert_eq!(header_value(&req_headers, "x-custom"), Some("1"));
    }

    #[test]
    fn apply_outcome_url_and_headers_no_rewrite_keeps_outcome_headers_as_is() {
        let mut url = url::Url::parse("https://example.com/orig").unwrap();
        let mut host = "example.com".to_string();
        let mut port: u16 = 443;
        let mut req_headers = vec![HeaderPair::new("Host", "example.com")];
        let outcome_headers = vec![HeaderPair::new("Host", "example.com")];

        apply_outcome_url_and_headers(
            &mut url,
            &mut host,
            &mut port,
            &mut req_headers,
            "https://example.com/orig",
            &outcome_headers,
        );

        assert_eq!(host, "example.com");
        assert_eq!(header_value(&req_headers, "host"), Some("example.com"));
    }

    #[test]
    fn apply_outcome_url_and_headers_synthesizes_host_from_original_url_when_missing() {
        // Simulates an HTTP/2 client's request (no Host header at all, h2
        // uses :authority instead) hitting a cross-host rewrite rule. Since
        // outcome_headers carries no Host, one must be synthesized - but
        // from the *original* URL's host/port, not the rewrite target's.
        let mut url = url::Url::parse("https://dcs4s-live.mp.lura.live:9443/orig").unwrap();
        let mut host = "dcs4s-live.mp.lura.live".to_string();
        let mut port: u16 = 9443;
        let mut req_headers = vec![HeaderPair::new("X-Custom", "1")];
        let outcome_headers = vec![HeaderPair::new("X-Custom", "1")];

        apply_outcome_url_and_headers(
            &mut url,
            &mut host,
            &mut port,
            &mut req_headers,
            "http://0.0.0.0:8082/orig",
            &outcome_headers,
        );

        assert_eq!(host, "0.0.0.0");
        assert_eq!(port, 8082);
        assert_eq!(
            header_value(&req_headers, "host"),
            Some("dcs4s-live.mp.lura.live:9443"),
            "synthesized Host must reflect the original URL (with its non-default port), not the rewrite target"
        );
    }

    #[test]
    fn apply_outcome_url_and_headers_explicit_rule_host_survives_rewrite() {
        // A setRequestHeader rule action supplying an explicit Host arrives
        // as part of outcome_headers, and must win over both the original
        // and the rewrite target's host.
        let mut url = url::Url::parse("https://dcs4s-live.mp.lura.live/orig").unwrap();
        let mut host = "dcs4s-live.mp.lura.live".to_string();
        let mut port: u16 = 443;
        let mut req_headers = vec![HeaderPair::new("Host", "dcs4s-live.mp.lura.live")];
        let outcome_headers = vec![HeaderPair::new("Host", "explicit.example.org")];

        apply_outcome_url_and_headers(
            &mut url,
            &mut host,
            &mut port,
            &mut req_headers,
            "http://0.0.0.0:8082/orig",
            &outcome_headers,
        );

        assert_eq!(host, "0.0.0.0");
        assert_eq!(port, 8082);
        assert_eq!(
            header_value(&req_headers, "host"),
            Some("explicit.example.org"),
            "an explicit setRequestHeader Host action must survive the rewrite unchanged"
        );
    }

    // ----- Bug 2: outbound request must match the actual sender's version -----

    #[test]
    fn adapt_request_to_sender_downgrades_h2_to_h1_and_sets_host() {
        let url = url::Url::parse("http://localhost:8082/path?x=1").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .version(http::Version::HTTP_2)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_11);
        assert!(parts.uri.authority().is_none(), "should be origin-form");
        assert_eq!(parts.uri.path_and_query().unwrap(), "/path?x=1");
        assert_eq!(
            parts.headers.get(http::header::HOST).unwrap(),
            "localhost:8082"
        );
    }

    #[test]
    fn adapt_request_to_sender_downgrade_via_proxy_keeps_absolute_form_but_sets_host() {
        let url = url::Url::parse("http://localhost:8082/path").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .version(http::Version::HTTP_2)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, true, &url);

        assert_eq!(parts.version, http::Version::HTTP_11);
        assert!(
            parts.uri.authority().is_some(),
            "via_proxy must keep absolute-form"
        );
        assert_eq!(
            parts.headers.get(http::header::HOST).unwrap(),
            "localhost:8082"
        );
    }

    #[test]
    fn adapt_request_to_sender_does_not_overwrite_existing_host() {
        let url = url::Url::parse("http://localhost:8082/path").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .version(http::Version::HTTP_2)
            .header(http::header::HOST, "custom-host")
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, false, &url);

        assert_eq!(
            parts.headers.get(http::header::HOST).unwrap(),
            "custom-host"
        );
    }

    #[test]
    fn adapt_request_to_sender_downgrade_prefers_preserved_authority_over_url() {
        // Simulates the full h2 rewrite-then-downgrade path: `url` is the
        // rewrite target (what `dispatch` connects to), but the request's
        // `:authority` already holds the original host - exactly what it
        // holds after `align_h2_authority_with_host` folded a preserved
        // `Host` into it at build time. The downgrade must recover *that*,
        // not resynthesize a Host from `url` and silently reintroduce the
        // rewrite-target-Host bug one hop later.
        let url = url::Url::parse("http://0.0.0.0:8082/x.m3u8").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("https://dcs4-live.mp.lura.live/x.m3u8")
            .version(http::Version::HTTP_2)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_11);
        assert!(parts.uri.authority().is_none(), "should be origin-form");
        assert_eq!(parts.uri.path_and_query().unwrap(), "/x.m3u8");
        assert_eq!(
            parts.headers.get(http::header::HOST).unwrap(),
            "dcs4-live.mp.lura.live",
            "must recover the preserved original host, not the rewrite target"
        );
    }

    #[test]
    fn adapt_request_to_sender_downgrade_falls_back_to_url_when_uri_has_no_authority() {
        // No `:authority` to prefer (the request was never put in
        // absolute-form) - falls back to the old `url`-derived behavior.
        let url = url::Url::parse("http://localhost:9000/path?x=1").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/path?x=1")
            .version(http::Version::HTTP_2)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_11);
        assert_eq!(
            parts.headers.get(http::header::HOST).unwrap(),
            "localhost:9000"
        );
    }

    #[test]
    fn adapt_request_to_sender_upgrades_to_h2_uses_absolute_form() {
        let url = url::Url::parse("https://example.com/foo?bar=1").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/foo?bar=1")
            .version(http::Version::HTTP_11)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http2, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_2);
        assert_eq!(parts.uri, http::Uri::try_from(url.as_str()).unwrap());
    }

    // ----- Bug 3: h2 requests must not carry a Host that disagrees with
    // :authority (RFC 9113 §8.3.1) - see `align_h2_authority_with_host`. -----

    #[test]
    fn build_outbound_request_http2_moves_differing_host_into_authority() {
        // Reproduces the 502: a rewriteUrl rule retargets the connection to
        // dcs4s-live but (per `apply_outcome_url_and_headers`) keeps the
        // original Host header naming dcs4-live. Over h2 this must not
        // surface as a Host/:authority mismatch.
        let url = url::Url::parse("https://dcs4s-live.mp.lura.live/x.m3u8").unwrap();
        let headers = vec![HeaderPair::new("Host", "dcs4-live.mp.lura.live")];
        let req = build_outbound_request(
            &Method::GET,
            &url,
            &headers,
            false,
            http::Version::HTTP_2,
            crate::empty_body(),
        )
        .unwrap();

        assert_eq!(
            req.uri().authority().map(|a| a.as_str()),
            Some("dcs4-live.mp.lura.live"),
            ":authority must carry the original origin identity"
        );
        assert_eq!(req.uri().scheme_str(), Some("https"));
        assert_eq!(req.uri().path(), "/x.m3u8");
        assert!(
            req.headers().get(http::header::HOST).is_none(),
            "the now-redundant Host header must be dropped"
        );
    }

    #[test]
    fn build_outbound_request_http11_keeps_host_header_and_origin_form() {
        // Same inputs as above, but HTTP/1.1: must be entirely unaffected by
        // the h2 authority alignment (guards against regressing the h1 path
        // that `apply_outcome_url_and_headers` was written for).
        let url = url::Url::parse("https://dcs4s-live.mp.lura.live/x.m3u8").unwrap();
        let headers = vec![HeaderPair::new("Host", "dcs4-live.mp.lura.live")];
        let req = build_outbound_request(
            &Method::GET,
            &url,
            &headers,
            false,
            http::Version::HTTP_11,
            crate::empty_body(),
        )
        .unwrap();

        assert!(req.uri().authority().is_none(), "should be origin-form");
        assert_eq!(req.uri().path(), "/x.m3u8");
        assert_eq!(
            req.headers().get(http::header::HOST).unwrap(),
            "dcs4-live.mp.lura.live"
        );
    }

    #[test]
    fn adapt_request_to_sender_upgrade_to_h2_aligns_differing_host() {
        let url = url::Url::parse("https://dcs4s-live.mp.lura.live/x.m3u8").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/x.m3u8")
            .version(http::Version::HTTP_11)
            .header(http::header::HOST, "dcs4-live.mp.lura.live")
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();

        adapt_request_to_sender(&mut parts, HttpVersion::Http2, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_2);
        assert_eq!(
            parts.uri.authority().map(|a| a.as_str()),
            Some("dcs4-live.mp.lura.live")
        );
        assert!(parts.headers.get(http::header::HOST).is_none());
    }

    #[test]
    fn align_h2_authority_with_host_malformed_host_drops_header_without_panicking() {
        let mut parts = Request::builder()
            .uri("https://example.com/path")
            .version(http::Version::HTTP_2)
            .header(http::header::HOST, "not a host")
            .body(())
            .unwrap()
            .into_parts()
            .0;

        align_h2_authority_with_host(&mut parts);

        assert!(
            parts.headers.get(http::header::HOST).is_none(),
            "a malformed Host must still be removed rather than left conflicting"
        );
        // The URI is left as whatever it already was - untouched, not panicked.
        assert_eq!(
            parts.uri.authority().map(|a| a.as_str()),
            Some("example.com")
        );
    }

    #[test]
    fn adapt_request_to_sender_matching_versions_is_a_no_op() {
        let url = url::Url::parse("http://localhost:8082/path").unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("/path")
            .version(http::Version::HTTP_11)
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();
        let original_uri = parts.uri.clone();

        adapt_request_to_sender(&mut parts, HttpVersion::Http1, false, &url);

        assert_eq!(parts.version, http::Version::HTTP_11);
        assert_eq!(parts.uri, original_uri);
        assert!(parts.headers.get(http::header::HOST).is_none());
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

    // ----- Bug 3: `flow.request` must reflect the effective (post-rule)
    // request, not stay frozen at the client's pre-rule one -----

    #[test]
    fn effective_request_record_carries_rewritten_url_and_recomputed_query() {
        // Simulates the dcs4s-live -> local stitcher rewrite from the module
        // docs: the effective record must carry the rewrite target's URL and
        // a query recomputed from it, not the client's original one.
        let rewritten = url::Url::parse("http://0.0.0.0:8082/x.m3u8?token=abc").unwrap();
        let headers = vec![HeaderPair::new("Host", "dcs4s-live.mp.lura.live")];
        let fallback_body = BodyPayload::default();

        let effective = effective_request_record(
            "GET",
            &rewritten,
            "HTTP/1.1",
            &headers,
            None,
            &fallback_body,
            None,
            1024,
        );

        assert_eq!(effective.url, "http://0.0.0.0:8082/x.m3u8?token=abc");
        assert_eq!(effective.query, vec![HeaderPair::new("token", "abc")]);
        assert_eq!(effective.method, "GET");
        assert_eq!(effective.headers, headers);
    }

    #[test]
    fn effective_request_record_falls_back_to_recorded_body_when_untouched() {
        // No rule replaced the body: the effective record must reuse the
        // body already captured from the client rather than re-decoding
        // anything, so the streamed-body backfill closure (which later
        // assigns into this same field) still has something sane to land on.
        let url = url::Url::parse("http://localhost/echo").unwrap();
        let fallback_body = hamsy_core::to_payload(b"original", Some("text/plain"), None, 1024);

        let effective = effective_request_record(
            "GET",
            &url,
            "HTTP/1.1",
            &[],
            None,
            &fallback_body,
            Some("text/plain"),
            1024,
        );

        assert_eq!(effective.body.data, "original");
    }

    #[test]
    fn effective_request_record_reencodes_rule_replaced_body_without_encoding() {
        // A rule that replaces the body also strips Content-Encoding before
        // forwarding (see the call site in `handle_captured_request`), so
        // the replacement bytes must be re-decoded with `None` rather than
        // whatever encoding the original body arrived with.
        let url = url::Url::parse("http://localhost/echo").unwrap();
        let fallback_body = BodyPayload::default();

        let effective = effective_request_record(
            "POST",
            &url,
            "HTTP/1.1",
            &[],
            Some(b"replaced"),
            &fallback_body,
            Some("text/plain"),
            1024,
        );

        assert_eq!(effective.body.data, "replaced");
        assert!(effective.body.encoding.is_none());
    }
}
