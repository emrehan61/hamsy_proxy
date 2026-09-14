//! Replaying a captured/edited request through the normal pipeline, so
//! users can test rule changes against previously-captured traffic.

use uuid::Uuid;

use hamsy_core::{
    BodyPayload, Flow, FlowId, FlowState, HeaderPair, RequestCtx, ResourceType, ResponseRecord,
    ServerEvent,
};

use crate::config::ProxyContext;
use crate::error::{ProxyError, Result};
use crate::http::{self, ConnInfo};

/// Re-issues `req` through the same upstream path used by the live proxy
/// pipeline (so rules apply exactly as they would to live traffic),
/// creating and recording a new [`Flow`] for it.
///
/// The new flow is marked as a replay by setting
/// [`hamsy_core::FlowSummary::client_addr`] to `"replay"` - there is no
/// dedicated boolean field on `Flow`/`FlowSummary` for this, so the task
/// spec calls for repurposing `client_addr` as the marker.
pub async fn replay(ctx: &ProxyContext, req: hamsy_core::RequestRecord) -> Result<FlowId> {
    let url = url::Url::parse(&req.url)
        .map_err(|e| ProxyError::InvalidTarget(format!("invalid replay url '{}': {e}", req.url)))?;
    let scheme = url.scheme().to_string();
    if scheme != "http" && scheme != "https" {
        return Err(ProxyError::InvalidTarget(format!(
            "unsupported replay scheme '{scheme}'"
        )));
    }

    let conn_info = ConnInfo {
        // A replay has no real client socket; `FlowSummary::client_addr` is
        // overwritten to `"replay"` below, so this value is never observed.
        client_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        scheme: if scheme == "https" { "https" } else { "http" },
        authority: None,
        tls: None,
        mirror_h2: false,
        // Replays have no real client connection to resolve an app from;
        // the resulting flow's `app` stays `None`.
        app: None,
        app_resolution: None,
    };

    let flow_id = Uuid::new_v4();
    run_replay(ctx, flow_id, req, url, conn_info).await?;
    Ok(flow_id)
}

/// Drives one request through the capture/rule/dispatch pipeline directly
/// (there's no real incoming client connection to read an `Incoming` body
/// from), tagging the resulting flow as a replay.
async fn run_replay(
    ctx: &ProxyContext,
    flow_id: FlowId,
    req: hamsy_core::RequestRecord,
    url: url::Url,
    conn_info: ConnInfo,
) -> Result<()> {
    let started_at = http::now_ms();
    let seq = ctx.flows.next_seq();
    let host = url.host_str().unwrap_or("").to_string();
    let port = url
        .port_or_known_default()
        .unwrap_or(http::default_port(url.scheme()));

    let mut headers: Vec<HeaderPair> = req.headers.clone();
    http::set_host_header(&mut headers, &host, port, url.scheme());

    let mut flow = Flow::new_request(
        flow_id,
        seq,
        started_at,
        req.method.clone(),
        url.scheme(),
        host,
        port,
        url.path(),
        url.to_string(),
        req.http_version.clone(),
        "replay",
        req.clone(),
    );
    flow.summary.state = FlowState::Requesting;
    ctx.flows.insert(flow.clone());
    let _ = ctx.events.send(ServerEvent::Flow {
        flow: flow.summary(),
    });

    let ruleset = ctx.ruleset();
    let body_bytes = hamsy_core::from_payload(&req.body);
    let content_type = headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.as_str());
    let resource_type = ResourceType::infer(content_type, url.path());

    let outcome = ruleset.apply_request(RequestCtx {
        method: &req.method,
        url: &url,
        headers: &headers,
        body: Some(&body_bytes),
        resource_type,
    });

    if let Some(reason) = &outcome.blocked {
        let finished_at = http::now_ms();
        let resp_record = ResponseRecord {
            status: 403,
            status_text: "Forbidden".to_string(),
            http_version: req.http_version.clone(),
            headers: vec![],
            body: BodyPayload::default(),
        };
        if let Some(summary) = ctx.flows.update(flow_id, |f| {
            f.mark_complete(resp_record, finished_at);
            f.summary.matched_rules = outcome.matched.clone();
            f.summary.modified = true;
            f.summary.error = Some(format!("blocked: {reason}"));
        }) {
            let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
        }
        return Ok(());
    }

    let final_url = url::Url::parse(&outcome.url).unwrap_or(url);
    let final_method =
        hyper::Method::from_bytes(outcome.method.as_bytes()).unwrap_or(hyper::Method::GET);
    let final_headers = outcome.headers.clone();
    let final_body = outcome.body.clone().unwrap_or(body_bytes);
    let http_version = http::version_from_str(&req.http_version);

    let outbound_body = crate::full_body(final_body);
    let outbound = match http::build_outbound_request(
        &final_method,
        &final_url,
        &final_headers,
        false,
        http_version,
        outbound_body,
    ) {
        Ok(r) => r,
        Err(e) => {
            let finished_at = http::now_ms();
            if let Some(summary) = ctx
                .flows
                .update(flow_id, |f| f.mark_error(e.to_string(), finished_at))
            {
                let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
            }
            return Err(e);
        }
    };

    let upstream_proxy = ctx.settings.read().upstream_proxy.clone();
    match http::dispatch(
        ctx,
        &final_url,
        conn_info.mirror_h2,
        upstream_proxy.as_deref(),
        outbound,
    )
    .await
    {
        Ok((resp, _timings, server_addr)) => {
            let (parts, body) = resp.into_parts();
            let (bytes, _total, _truncated) = crate::tee::collect_capped(body, 64 * 1024 * 1024)
                .await
                .unwrap_or_default();
            let resp_headers = http::header_pairs_from(&parts.headers);
            let content_type = resp_headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-type"))
                .map(|h| h.value.clone());
            let content_encoding = resp_headers
                .iter()
                .find(|h| h.name.eq_ignore_ascii_case("content-encoding"))
                .map(|h| h.value.clone());
            let resp_record = ResponseRecord {
                status: parts.status.as_u16(),
                status_text: parts.status.canonical_reason().unwrap_or("").to_string(),
                http_version: format!("{:?}", parts.version),
                headers: resp_headers,
                body: hamsy_core::to_payload(
                    &bytes,
                    content_type.as_deref(),
                    content_encoding.as_deref(),
                    ctx.max_body_bytes(),
                ),
            };
            let finished_at = http::now_ms();
            if let Some(summary) = ctx.flows.update(flow_id, |f| {
                f.server_addr = server_addr.clone();
                f.mark_complete(resp_record, finished_at);
                f.summary.matched_rules = outcome.matched.clone();
                f.summary.modified = outcome.modified;
            }) {
                let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
            }
        }
        Err(e) => {
            let finished_at = http::now_ms();
            if let Some(summary) = ctx.flows.update(flow_id, |f| {
                f.summary.matched_rules = outcome.matched.clone();
                f.summary.modified = outcome.modified;
                f.mark_error(e.to_string(), finished_at);
            }) {
                let _ = ctx.events.send(ServerEvent::Flow { flow: summary });
            }
        }
    }

    Ok(())
}
