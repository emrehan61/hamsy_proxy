//! Chrome splits Cookie fields over HTTP/2; HTTP/1 origins need one field.
mod common;

use bytes::Bytes;
use hamsy_core::Settings;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, Version};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls_pki_types::{CertificateDer, ServerName};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn split_h2_cookies_reach_h1_origin_as_one_session_header() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for paused in [false, true] {
            let (origin, cert) = common::spawn_tls_origin(|req| async move {
                assert_eq!(req.version(), Version::HTTP_11);
                let cookies: Vec<_> = req.headers().get_all("cookie").iter().collect();
                // A session middleware reading the first header must see the
                // session even when Chrome sent analytics cookies first.
                let valid_session = cookies.len() == 1
                    && cookies[0] == "_ga=analytics; prefs=dark; _redmine_session=session";
                Response::builder()
                    .status(if valid_session { 200 } else { 422 })
                    .header("set-cookie", "session=next; HttpOnly")
                    .header("set-cookie", "prefs=dark")
                    .body(common::full("ok"))
                    .unwrap()
            })
            .await;
            let proxy = common::spawn_proxy_trusting(
                Settings {
                    paused,
                    ..Default::default()
                },
                &[cert],
            )
            .await;

            let mut stream = tokio::net::TcpStream::connect(proxy.addr).await.unwrap();
            stream
                .write_all(
                    format!(
                        "CONNECT localhost:{} HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
                        origin.port(),
                        origin.port()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut reply = Vec::new();
            while !reply.ends_with(b"\r\n\r\n") {
                reply.push(stream.read_u8().await.unwrap());
            }
            assert!(reply.starts_with(b"HTTP/1.1 200"));

            let mut roots = rustls::RootCertStore::empty();
            roots.add(CertificateDer::from(proxy.ctx.ca.der())).unwrap();
            let mut tls = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            tls.alpn_protocols = vec![b"h2".to_vec()];
            let stream = tokio_rustls::TlsConnector::from(Arc::new(tls))
                .connect(ServerName::try_from("localhost").unwrap(), stream)
                .await
                .unwrap();
            assert_eq!(stream.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
            let (mut sender, connection) =
                hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                    .await
                    .unwrap();
            let driver = tokio::spawn(connection);
            let request = Request::builder()
                .method("POST")
                .uri(format!("https://localhost:{}/login", origin.port()))
                .version(Version::HTTP_2)
                .header("cookie", "_ga=analytics")
                .header("cookie", "prefs=dark")
                .header("cookie", "_redmine_session=session")
                .body(Full::new(Bytes::from_static(b"authenticity_token=token")))
                .unwrap();
            let response = sender.send_request(request).await.unwrap();
            assert_eq!(
                response.status(),
                200,
                "paused={paused}: session cookie lost"
            );
            assert_eq!(response.headers().get_all("set-cookie").iter().count(), 2);
            response.into_body().collect().await.unwrap();
            driver.abort();
        }
    })
    .await
    .expect("cookie forwarding timed out");
}
