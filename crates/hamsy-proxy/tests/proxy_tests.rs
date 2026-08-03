//! End-to-end integration tests: real TCP sockets, a real TLS handshake for
//! MITM'd traffic, and a real `reqwest` client driving everything through
//! an actual `ProxyServer`.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hyper::{Request, Response};

use flproxy_core::{Action, BodyKind, Matcher, PayloadEncoding, Settings};

/// 1. Plain HTTP GET through the proxy (absolute-form request, non-`CONNECT`).
#[tokio::test]
async fn plain_http_get_through_proxy() {
    let origin_addr = common::spawn_http_origin(|_req: Request<hyper::body::Incoming>| async {
        Response::new(common::full("hello world"))
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    let client = common::client_trusting_proxy_ca(&proxy);

    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "hello world");

    let flows = proxy.ctx.flows.list(&Default::default());
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].status, Some(200));
    assert_eq!(flows[0].scheme, "http");
}

/// 2. HTTPS GET through MITM (CONNECT + TLS interception).
#[tokio::test]
async fn https_get_through_mitm() {
    let (origin_addr, origin_cert) =
        common::spawn_tls_origin(|_req: Request<hyper::body::Incoming>| async {
            Response::new(common::full("secure hello"))
        })
        .await;

    // The proxy's own upstream TLS connector must trust the throwaway
    // origin's self-signed cert to re-originate the connection during MITM.
    let proxy = common::spawn_proxy_trusting(Settings::default(), &[origin_cert]).await;
    let client = common::client_trusting_proxy_ca(&proxy);

    let resp = client
        .get(format!("https://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "secure hello");

    let flows = proxy.ctx.flows.list(&Default::default());
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].status, Some(200));
    assert_eq!(flows[0].scheme, "https");

    let flow = proxy.ctx.flows.get(flows[0].id).expect("flow");
    assert_eq!(
        flow.summary.url,
        format!("https://localhost:{}/", origin_addr.port())
    );
    let tls = flow.tls.expect("tls info recorded");
    assert_eq!(tls.sni.as_deref(), Some("localhost"));
    assert!(tls.version.is_some());
}

/// 3. A `setRequestHeader` rule taking effect - origin observes the header.
#[tokio::test]
async fn set_request_header_rule_takes_effect() {
    let seen = Arc::new(Mutex::new(None::<String>));
    let seen2 = seen.clone();
    let origin_addr = common::spawn_http_origin(move |req: Request<hyper::body::Incoming>| {
        let seen2 = seen2.clone();
        async move {
            let value = req
                .headers()
                .get("x-injected")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            *seen2.lock().unwrap() = value;
            Response::new(common::full("ok"))
        }
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "set-header",
            Matcher::default(),
            vec![Action::SetRequestHeader {
                name: "X-Injected".to_string(),
                value: "hello".to_string(),
            }],
        ))
        .expect("create rule");

    let client = common::client_trusting_proxy_ca(&proxy);
    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(seen.lock().unwrap().clone(), Some("hello".to_string()));
}

/// 4. A `mockResponse` rule short-circuiting - origin never receives the request.
#[tokio::test]
async fn mock_response_short_circuits_before_upstream() {
    let hit = Arc::new(AtomicBool::new(false));
    let hit2 = hit.clone();
    let origin_addr = common::spawn_http_origin(move |_req: Request<hyper::body::Incoming>| {
        hit2.store(true, Ordering::SeqCst);
        async move { Response::new(common::full("should never be seen")) }
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "mock",
            Matcher::default(),
            vec![Action::MockResponse {
                status: 201,
                headers: vec![],
                body: "mocked".to_string(),
                encoding: PayloadEncoding::Text,
                delay_ms: 0,
            }],
        ))
        .expect("create rule");

    let client = common::client_trusting_proxy_ca(&proxy);
    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201);
    assert_eq!(resp.text().await.expect("body"), "mocked");
    assert!(
        !hit.load(Ordering::SeqCst),
        "origin should never have been contacted"
    );

    let flows = proxy.ctx.flows.list(&Default::default());
    assert_eq!(flows.len(), 1);
    assert!(flows[0].from_cache);
}

/// 5. A `block` rule returning `403` to the client.
#[tokio::test]
async fn block_rule_returns_403() {
    let origin_addr = common::spawn_http_origin(|_req: Request<hyper::body::Incoming>| async {
        Response::new(common::full("ok"))
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "blocker",
            Matcher::default(),
            vec![Action::Block {
                reason: "nope".to_string(),
            }],
        ))
        .expect("create rule");

    let client = common::client_trusting_proxy_ca(&proxy);
    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 403);

    let flows = proxy.ctx.flows.list(&Default::default());
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].status, Some(403));
    assert!(flows[0].modified);
}

/// 6. A `replaceInResponseBody` rule mutating the body the client receives.
#[tokio::test]
async fn replace_in_response_body_mutates_client_view() {
    let origin_addr = common::spawn_http_origin(|_req: Request<hyper::body::Incoming>| async {
        Response::new(common::full("hello world"))
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "replace",
            Matcher::default(),
            vec![Action::ReplaceInResponseBody {
                find: "world".to_string(),
                replace: "rust".to_string(),
                regex: false,
            }],
        ))
        .expect("create rule");

    let client = common::client_trusting_proxy_ca(&proxy);
    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.text().await.expect("body"), "hello rust");
}

/// 7. A redirect action retargeting the request to a DIFFERENT origin server.
#[tokio::test]
async fn redirect_action_retargets_to_different_origin() {
    let origin_a = common::spawn_http_origin(|_req: Request<hyper::body::Incoming>| async {
        Response::new(common::full("origin A"))
    })
    .await;
    let origin_b = common::spawn_http_origin(|_req: Request<hyper::body::Incoming>| async {
        Response::new(common::full("origin B"))
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    let target = format!("http://localhost:{}/", origin_b.port());
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "redirect",
            Matcher::default(),
            vec![Action::Redirect { to: target }],
        ))
        .expect("create rule");

    let client = common::client_trusting_proxy_ca(&proxy);
    let resp = client
        .get(format!("http://localhost:{}/", origin_a.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.text().await.expect("body"), "origin B");
}

/// 8. A gzip-encoded response: decoded correctly in the recorded flow, but
/// delivered byte-for-byte intact (still gzipped) to the client.
#[tokio::test]
async fn gzip_response_decoded_in_flow_but_delivered_intact() {
    use std::io::Write;

    let original = b"this text is repeated repeated repeated for compressibility".to_vec();
    let compressed = {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&original).unwrap();
        encoder.finish().unwrap()
    };

    let compressed_for_origin = compressed.clone();
    let origin_addr = common::spawn_http_origin(move |_req: Request<hyper::body::Incoming>| {
        let body = compressed_for_origin.clone();
        async move {
            Response::builder()
                .header("Content-Encoding", "gzip")
                .header("Content-Type", "text/plain")
                .body(common::full(body))
                .unwrap()
        }
    })
    .await;

    let proxy = common::spawn_proxy(Settings::default()).await;
    // Disable reqwest's automatic gzip decompression so we can inspect the
    // exact bytes that crossed the wire to the client.
    let client = common::client_builder(&proxy)
        .no_gzip()
        .build()
        .expect("client");

    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.headers()
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
    let delivered = resp.bytes().await.expect("body").to_vec();
    assert_eq!(
        delivered, compressed,
        "client must receive the exact gzipped bytes, untouched"
    );

    let flows = proxy.ctx.flows.list(&Default::default());
    let flow = proxy.ctx.flows.get(flows[0].id).expect("flow");
    let payload = flow.response.expect("response recorded").body;
    assert_eq!(payload.kind, BodyKind::Text);
    assert_eq!(payload.data.as_bytes(), original.as_slice());
    assert_eq!(payload.encoding.as_deref(), Some("gzip"));
}

/// 9. A response larger than `max_body_bytes`: recorded truncated but
/// delivered whole/uncorrupted to the client.
#[tokio::test]
async fn oversized_response_truncated_in_flow_but_delivered_whole() {
    let big = "A".repeat(10_000);
    let big_for_origin = big.clone();
    let origin_addr = common::spawn_http_origin(move |_req: Request<hyper::body::Incoming>| {
        let body = big_for_origin.clone();
        async move { Response::new(common::full(body)) }
    })
    .await;

    let settings = Settings {
        max_body_bytes: 100,
        ..Settings::default()
    };
    let proxy = common::spawn_proxy(settings).await;
    let client = common::client_trusting_proxy_ca(&proxy);

    let resp = client
        .get(format!("http://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    let delivered = resp.text().await.expect("body");
    assert_eq!(delivered.len(), 10_000);
    assert_eq!(
        delivered, big,
        "client must receive the whole, uncorrupted body"
    );

    let flows = proxy.ctx.flows.list(&Default::default());
    let flow = proxy.ctx.flows.get(flows[0].id).expect("flow");
    let payload = flow.response.expect("response recorded").body;
    assert_eq!(payload.kind, BodyKind::Truncated);
    assert!(payload.truncated);
    assert_eq!(payload.size, 10_000);
    assert!(payload.data.len() <= 100);
}

/// 10. A passthrough host is NOT intercepted: the client validates the
/// origin's own certificate through a raw tunnel, not flproxy's.
#[tokio::test]
async fn passthrough_host_is_not_intercepted() {
    let (origin_addr, origin_cert_der) =
        common::spawn_tls_origin(|_req: Request<hyper::body::Incoming>| async {
            Response::new(common::full("origin secure"))
        })
        .await;

    let settings = Settings {
        passthrough_hosts: vec!["localhost".to_string()],
        ..Settings::default()
    };
    let proxy = common::spawn_proxy(settings).await;

    // Trust the ORIGIN's own self-signed cert directly - if the proxy were
    // MITM'ing this connection, this handshake would fail (it doesn't trust
    // the proxy's CA at all).
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://{}", proxy.addr)).unwrap())
        .add_root_certificate(reqwest::Certificate::from_der(&origin_cert_der).unwrap())
        .build()
        .unwrap();

    let resp = client
        .get(format!("https://localhost:{}/", origin_addr.port()))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "origin secure");
    drop(client); // encourage the connection (and thus the raw tunnel) to close promptly

    let flow = common::wait_for_terminal_flow(&proxy).await;
    assert_eq!(flow.method, "CONNECT");
    assert_eq!(flow.state, flproxy_core::FlowState::Complete);
}
