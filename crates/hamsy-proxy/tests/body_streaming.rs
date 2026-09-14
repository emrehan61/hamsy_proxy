//! Regression coverage for conditional buffering and ordered body rules.
mod common;

use bytes::Bytes;
use hamsy_core::{Action, BodyCond, BodyCondOp, HeaderCond, HeaderOp, Matcher, Settings, UrlOp};
use http_body_util::BodyExt;
use hyper::{
    body::{Body, Frame},
    Response,
};
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;

struct ChannelBody(mpsc::Receiver<Bytes>);
impl Body for ChannelBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0
            .poll_recv(cx)
            .map(|item| item.map(|data| Ok(Frame::data(data))))
    }
}

#[tokio::test]
async fn unrelated_body_rule_delivers_response_before_origin_eof() {
    let (tx, rx) = mpsc::channel(2);
    let rx = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
    let origin = common::spawn_http_origin(move |_| {
        let body = ChannelBody(rx.lock().unwrap().take().unwrap());
        async move {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(body.boxed())
                .unwrap()
        }
    })
    .await;
    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "unrelated",
            Matcher {
                url_op: UrlOp::Contains,
                url_value: "unrelated.example".into(),
                ..Default::default()
            },
            vec![Action::ReplaceInResponseBody {
                find: "old".into(),
                replace: "new".into(),
                regex: false,
            }],
        ))
        .unwrap();
    tx.send(Bytes::from_static(b"data: first\n\n"))
        .await
        .unwrap();
    let client = common::client_trusting_proxy_ca(&proxy);
    let first = tokio::time::timeout(Duration::from_secs(3), async {
        let mut response = client
            .get(format!("http://localhost:{}/events", origin.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.chunk().await.unwrap().unwrap()
    })
    .await
    .expect("headers and first chunk must arrive while origin is still open");
    assert_eq!(first.as_ref(), b"data: first\n\n");
    // Sender stays alive throughout assertion: EOF cannot have unblocked it.
    drop(tx);
}

#[tokio::test]
async fn rule_buffer_overflow_errors_without_waiting_for_eof() {
    // Exercise the same collector used with the production 64 MiB limit,
    // with a small limit to avoid a large test allocation.
    let (tx, rx) = mpsc::channel(2);
    tx.send(Bytes::from_static(b"1234")).await.unwrap();
    tx.send(Bytes::from_static(b"5")).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        hamsy_proxy::tee::collect_for_rules(ChannelBody(rx), 4),
    )
    .await
    .expect("overflow must fail immediately instead of draining the origin");
    let error = result.expect_err("truncated successful bodies are forbidden");
    assert!(error.to_string().contains("body exceeds rule buffer limit"));
    drop(tx);
}

#[tokio::test]
async fn response_metadata_rule_enables_later_body_predicate() {
    let origin =
        common::spawn_http_origin(|_| async { Response::new(common::full("original payload")) })
            .await;
    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "metadata",
            Matcher::default(),
            vec![
                Action::SetStatus { status: 201 },
                Action::SetResponseHeader {
                    name: "x-stage".into(),
                    value: "ready".into(),
                },
            ],
        ))
        .unwrap();
    let mut body_rule = common::simple_rule(
        "body",
        Matcher {
            status_codes: vec!["201".into()],
            response_headers: vec![HeaderCond {
                name: "x-stage".into(),
                op: HeaderOp::Equals,
                value: Some("ready".into()),
            }],
            response_body: Some(BodyCond {
                op: BodyCondOp::Contains,
                value: "original".into(),
            }),
            ..Default::default()
        },
        vec![Action::ReplaceInResponseBody {
            find: "original".into(),
            replace: "changed".into(),
            regex: false,
        }],
    );
    body_rule.priority = 1;
    proxy.ctx.rules.create(body_rule).unwrap();
    let response = common::client_trusting_proxy_ca(&proxy)
        .get(format!("http://localhost:{}/", origin.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(response.headers()["x-stage"], "ready");
    assert_eq!(response.text().await.unwrap(), "changed payload");
}

#[tokio::test]
async fn request_url_rewrite_enables_later_body_predicate() {
    let origin = common::spawn_http_origin(|request| async move {
        assert_eq!(request.uri().path(), "/rewritten");
        assert_eq!(
            request
                .headers()
                .get("x-body-match")
                .and_then(|h| h.to_str().ok()),
            Some("yes")
        );
        let body = request.into_body().collect().await.unwrap().to_bytes();
        Response::new(common::full(body))
    })
    .await;
    let proxy = common::spawn_proxy(Settings::default()).await;
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "rewrite",
            Matcher::default(),
            vec![Action::RewriteUrl {
                find: "/original".into(),
                replace: "/rewritten".into(),
                regex: false,
            }],
        ))
        .unwrap();
    let mut body_rule = common::simple_rule(
        "body",
        Matcher {
            url_op: UrlOp::EndsWith,
            url_value: "/rewritten".into(),
            request_body: Some(BodyCond {
                op: BodyCondOp::Equals,
                value: "posted payload".into(),
            }),
            ..Default::default()
        },
        vec![Action::SetRequestHeader {
            name: "x-body-match".into(),
            value: "yes".into(),
        }],
    );
    body_rule.priority = 1;
    proxy.ctx.rules.create(body_rule).unwrap();
    let response = common::client_trusting_proxy_ca(&proxy)
        .post(format!("http://localhost:{}/original", origin.port()))
        .body("posted payload")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "posted payload");
}

#[tokio::test]
async fn compressed_body_rules_preserve_unchanged_wire_and_decode_replacements() {
    let text = "<html>https://dcs-live.mp.lura.live/manifest.m3u8</html>";
    for encoding in ["gzip", "br", "deflate", "zstd", "gzip, br"] {
        let encoded = encoding
            .split(", ")
            .fold(text.as_bytes().to_vec(), |bytes, coding| {
                hamsy_core::encode_body(&bytes, coding).unwrap()
            });
        for scenario in ["unrelated", "unchanged", "replaced"] {
            let wire = encoded.clone();
            let origin = common::spawn_http_origin(move |_| {
                let wire = wire.clone();
                async move {
                    Response::builder()
                        .header("content-encoding", encoding)
                        .header("content-type", "text/html")
                        .header("content-length", wire.len())
                        .body(common::full(wire))
                        .unwrap()
                }
            })
            .await;
            let proxy = common::spawn_proxy(Settings::default()).await;
            proxy
                .ctx
                .rules
                .create(common::simple_rule(
                    "replace",
                    Matcher {
                        url_op: UrlOp::Contains,
                        url_value: if scenario == "unrelated" {
                            "other.example"
                        } else {
                            "localhost"
                        }
                        .into(),
                        ..Default::default()
                    },
                    vec![Action::ReplaceInResponseBody {
                        find: if scenario == "unchanged" {
                            "absent.example"
                        } else {
                            "https://dcs-live.mp.lura.live"
                        }
                        .into(),
                        replace: "http://localtest.me:8082".into(),
                        regex: false,
                    }],
                ))
                .unwrap();
            let response = common::client_builder(&proxy)
                .no_gzip()
                .build()
                .unwrap()
                .get(format!("http://localhost:{}/", origin.port()))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let replaced = scenario == "replaced";
            assert_eq!(
                response
                    .headers()
                    .get("content-encoding")
                    .and_then(|h| h.to_str().ok()),
                if replaced { None } else { Some(encoding) },
                "{encoding}/{scenario}"
            );
            let expected = if replaced {
                text.replace("https://dcs-live.mp.lura.live", "http://localtest.me:8082")
                    .into_bytes()
            } else {
                encoded.clone()
            };
            assert_eq!(response.content_length(), Some(expected.len() as u64));
            assert_eq!(
                response.bytes().await.unwrap().as_ref(),
                expected.as_slice(),
                "{encoding}/{scenario}"
            );
        }
    }
}

#[tokio::test]
async fn later_request_rewrite_enables_earlier_response_body_condition() {
    let origin = common::spawn_http_origin(|request| async move {
        assert_eq!(request.uri().path(), "/new");
        let body = request.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), b"posted payload");
        Response::new(common::full("origin response"))
    })
    .await;
    let proxy = common::spawn_proxy(Settings::default()).await;
    // This rule cannot match during the first request-phase pass: the URL
    // still ends in /old. It must see the retained POST body when the
    // response-phase matcher evaluates the final rewritten request URL.
    proxy
        .ctx
        .rules
        .create(common::simple_rule(
            "earlier-response",
            Matcher {
                url_op: UrlOp::EndsWith,
                url_value: "/new".into(),
                request_body: Some(BodyCond {
                    op: BodyCondOp::Equals,
                    value: "posted payload".into(),
                }),
                ..Default::default()
            },
            vec![Action::SetResponseHeader {
                name: "x-request-body-matched".into(),
                value: "yes".into(),
            }],
        ))
        .unwrap();
    let mut rewrite = common::simple_rule(
        "later-rewrite",
        Matcher::default(),
        vec![Action::RewriteUrl {
            find: "/old".into(),
            replace: "/new".into(),
            regex: false,
        }],
    );
    rewrite.priority = 1;
    proxy.ctx.rules.create(rewrite).unwrap();
    let response = common::client_trusting_proxy_ca(&proxy)
        .post(format!("http://localhost:{}/old", origin.port()))
        .body("posted payload")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-request-body-matched")
            .and_then(|h| h.to_str().ok()),
        Some("yes")
    );
    assert_eq!(response.text().await.unwrap(), "origin response");
}
