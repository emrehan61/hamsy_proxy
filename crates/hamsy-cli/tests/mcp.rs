//! Exercise the release binary through real stdio and HTTP, not internal mocks of
//! its dispatcher. All stores are temporary; no OS proxy/certificate mutations.
use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use hamsy_api::{ApiState, ReplayHook, StubCert};
use hamsy_core::{FlowStore, RulesStore, Settings};
use parking_lot::RwLock;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdout, Command},
    time::timeout,
};

const BIN: &str = env!("CARGO_BIN_EXE_hamsy");

type Browser =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn browser(api: &str) -> Browser {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = format!("{}/api/sessions/ws", api.replacen("http", "ws", 1))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert("Origin", api.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn advertise(browser: &mut Browser, ids: &[&str]) {
    let sessions: Vec<_> = ids.iter().map(|id| json!({"id":id, "name":"external.har", "flowCount":3, "importedAt":1, "active":true})).collect();
    browser
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"sessions", "sessions":sessions}).to_string(),
        ))
        .await
        .unwrap();
}
async fn browser_request(browser: &mut Browser) -> Value {
    timeout(Duration::from_secs(5), async {
        loop {
            let message = browser.next().await.unwrap().unwrap();
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
            browser.flush().await.unwrap();
        }
    })
    .await
    .unwrap()
}
async fn reply(browser: &mut Browser, request: &Value, result: Value) {
    browser
        .send(tokio_tungstenite::tungstenite::Message::Text(
            json!({"type":"reply", "requestId":request["requestId"], "result":result}).to_string(),
        ))
        .await
        .unwrap();
}
async fn wait_sessions(client: &mut Mcp, expected: usize) -> Value {
    for _ in 0..100 {
        let response = client.call("list_sessions", json!({})).await;
        let value = result(&response);
        if value["sessions"].as_array().unwrap().len() == expected {
            return value.clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session registration did not settle");
}

struct Mcp {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    modern: bool,
    home: tempfile::TempDir,
}
impl Mcp {
    async fn start(api: &str, writes: bool, version: &str) -> Self {
        Self::start_config(api, writes, version, None).await
    }
    async fn start_config(
        api: &str,
        writes: bool,
        version: &str,
        startup: Option<(&std::path::Path, u16)>,
    ) -> Self {
        Self::start_permissions_config(api, writes, false, version, startup).await
    }
    async fn start_permissions(api: &str, writes: bool, replay: bool) -> Self {
        Self::start_permissions_config(api, writes, replay, "2025-11-25", None).await
    }
    async fn start_permissions_config(
        api: &str,
        writes: bool,
        replay: bool,
        version: &str,
        startup: Option<(&std::path::Path, u16)>,
    ) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(BIN);
        cmd.args(["mcp", "--api-url", api]);
        if let Some((profile, port)) = startup {
            cmd.arg("--data-dir")
                .arg(profile)
                .arg("--proxy-port")
                .arg(port.to_string());
        } else {
            cmd.arg("--no-auto-start");
        }
        if writes {
            cmd.arg("--allow-writes");
        }
        if replay {
            cmd.arg("--allow-replay");
        }
        let mut child = cmd
            .env("HAMSY_HOME", home.path())
            .env("RUST_LOG", "debug")
            .current_dir(home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = Self {
            child,
            lines,
            next_id: 0,
            modern: version == "2026-07-28",
            home,
        };
        if client.modern {
            let response = client.rpc("server/discover", json!({})).await;
            assert_eq!(
                response["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "hamsy",
                "{response}"
            );
            return client;
        }
        let response = client
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": version, "capabilities": {},
                    "clientInfo": {"name":"hamsy-test", "version":"1"}
                }),
            )
            .await;
        assert_eq!(response["result"]["protocolVersion"], version, "{response}");
        assert_eq!(response["result"]["serverInfo"]["name"], "hamsy");
        assert!(response["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("untrusted"));
        client
            .send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
            .await;
        client
    }
    async fn send(&mut self, value: Value) {
        let input = self.child.stdin.as_mut().unwrap();
        input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
    }
    async fn rpc(&mut self, method: &str, mut params: Value) -> Value {
        if self.modern {
            params["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientInfo":{"name":"hamsy-test","version":"1"},
                "io.modelcontextprotocol/clientCapabilities":{}
            });
        }
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await;
        timeout(Duration::from_secs(15), async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("MCP stdout closed");
                let message: Value = serde_json::from_str(&line)
                    .expect("stdout must contain JSON-RPC only, even with debug logging");
                if message["id"] == id {
                    return message;
                }
            }
        })
        .await
        .expect("MCP response timeout")
    }
    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":name, "arguments":arguments}))
            .await
    }
    async fn close(mut self) {
        self.child.stdin.take();
        let status = timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "MCP should exit cleanly at EOF");
        assert_eq!(
            std::fs::read_dir(self.home.path()).unwrap().count(),
            0,
            "bridge must not create stores or certificates"
        );
    }
}

fn result(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    &response["result"]["structuredContent"]
}
fn tool_error(response: &Value) {
    // SDK schema/unknown-tool validation can return protocol errors; operational
    // errors must be visible as tool errors. Both must leave state unchanged.
    assert!(
        response.get("error").is_some() || response["result"]["isError"] == true,
        "{response}"
    );
}

#[tokio::test]
async fn open_sessions_route_reads_without_mixing_live_capture_or_masking_ids() {
    let app = App::start().await;
    let id = app.seed();
    let session = uuid::Uuid::new_v4().to_string();
    let selected = format!("app:{session}");
    let mut browser = browser(&app.url).await;
    advertise(&mut browser, &[&session]).await;
    let mut client = Mcp::start_permissions(&app.url, true, true).await;
    let listed = wait_sessions(&mut client, 2).await;
    assert_eq!(listed["sessions"][0]["sessionId"], "app:live");
    assert_eq!(listed["sessions"][1]["sessionId"], selected);
    assert_eq!(listed["sessions"][1]["readOnly"], true);

    let (response, ()) = tokio::join!(
        client.call("list_flows", json!({"sessionId":selected,"limit":2})),
        async {
            let request = browser_request(&mut browser).await;
            assert_eq!(request["sessionId"], session);
            assert_eq!(request["operation"], "list_flows");
            assert_eq!(request["query"]["limit"], "3");
            assert!(request["query"].get("afterSeq").is_none());
            reply(
                &mut browser,
                &request,
                json!({"flows":[{"seq":0},{"seq":1},{"seq":2}]}),
            )
            .await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert_eq!(result(&response)["flows"], json!([{"seq":0},{"seq":1}]));
    assert_eq!(result(&response)["limited"], true);
    assert_eq!(result(&response)["lastSeq"], 1);

    let (response, ()) = tokio::join!(
        client.call(
            "get_flow",
            json!({"sessionId":selected,"id":id,"includeBodies":true})
        ),
        async {
            let request = browser_request(&mut browser).await;
            assert_eq!(request["operation"], "get_flow");
            assert_eq!(request["query"]["id"], id);
            assert_eq!(request["query"]["includeBodies"], "true");
            reply(&mut browser, &request, json!({"id":id,"request":{"headers":[{"name":"Authorization","value":"BROWSER_SECRET"}],
                "body":{"kind":"text","size":80,"data":"{\"sessionId\":\"CAPTURE_SECRET\",\"message\":\"from HAR\"}"}}})).await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert!(response.to_string().contains("from HAR"));
    assert!(!response.to_string().contains("SECRET"));
    let (response, ()) = tokio::join!(
        client.call("export_har", json!({"sessionId":selected,"ids":[id]})),
        async {
            let request = browser_request(&mut browser).await;
            assert_eq!(request["operation"], "export_har");
            assert_eq!(request["query"]["ids"], id);
            reply(&mut browser, &request, json!({"log":{"entries":[{"response":{"content":{"mimeType":"text/plain","text":"SECRET"}}}]}})).await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert!(!response.to_string().contains("SECRET"));
    tool_error(
        &client
            .call("replay_request", json!({"sessionId":selected,"id":id}))
            .await,
    );
    tool_error(
        &client
            .call(
                "get_flow",
                json!({"sessionId":"app:../../settings","id":id}),
            )
            .await,
    );
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 0);
    assert_eq!(app.state.flows().len(), 1);
    // Default reads still address live traffic, even if an archive has the same flow ID.
    let response = client
        .call("get_flow", json!({"id":id,"includeBodies":true}))
        .await;
    assert_eq!(result(&response)["sessionId"], "app:live");
    assert!(response.to_string().contains("hello"));
    advertise(&mut browser, &[]).await;
    wait_sessions(&mut client, 1).await;
    tool_error(
        &client
            .call("get_flow", json!({"sessionId":selected,"id":id}))
            .await,
    );
    browser.close(None).await.unwrap();
    client.close().await;
}

#[tokio::test]
async fn sessions_track_multiple_windows_disconnects_and_reject_foreign_origins() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let app = App::start().await;
    let session = uuid::Uuid::new_v4().to_string();
    let mut first = browser(&app.url).await;
    let mut second = browser(&app.url).await;
    advertise(&mut first, &[&session]).await;
    advertise(&mut second, &[&session]).await;
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    for _ in 0..100 {
        let value = wait_sessions(&mut client, 2).await;
        if value["sessions"][1]["openWindows"] == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let response = client.call("list_sessions", json!({})).await;
    assert_eq!(result(&response)["sessions"][1]["openWindows"], 2);
    first.close(None).await.unwrap();
    for _ in 0..100 {
        let response = client.call("list_sessions", json!({})).await;
        if result(&response)["sessions"][1]["openWindows"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let response = client.call("list_sessions", json!({})).await;
    assert_eq!(result(&response)["sessions"][1]["openWindows"], 1);
    let (response, ()) = tokio::join!(
        client.call("list_flows", json!({"sessionId":format!("app:{session}")})),
        async {
            let _request = browser_request(&mut second).await;
            second.close(None).await.unwrap();
        }
    );
    tool_error(&response); // in-flight readers wake immediately on disconnect
    wait_sessions(&mut client, 1).await;
    for origin in [None, Some("https://foreign.example")] {
        let mut request = format!("{}/api/sessions/ws", app.url.replacen("http", "ws", 1))
            .into_client_request()
            .unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("Origin", origin.parse().unwrap());
        }
        let error = tokio_tungstenite::connect_async(request).await.unwrap_err();
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), 403)
            }
            other => panic!("unexpected failure: {other}"),
        }
    }
    client.close().await;
}

#[tokio::test]
async fn standalone_viewer_discovery_works_without_capture_and_checks_identity() {
    let mut client = Mcp::start("http://127.0.0.1:1", false, "2025-11-25").await;
    let mut viewer = Command::new(BIN)
        .args(["har-viewer-serve", "--data-dir"])
        .arg(client.home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let file = client.home.path().join("har-viewer/service.json");
    let discovery: Value = timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) = std::fs::read(&file) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    break value;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let url = format!("http://127.0.0.1:{}", discovery["port"]);
    let mut browser = browser(&url).await;
    let session = uuid::Uuid::new_v4().to_string();
    advertise(&mut browser, &[&session]).await;
    let listed = wait_sessions(&mut client, 1).await;
    let selected = format!("viewer:{session}");
    assert_eq!(listed["sessions"][0]["sessionId"], selected);
    assert!(listed["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["source"] == "app" && s["available"] == false));
    assert!(!listed
        .to_string()
        .contains(discovery["token"].as_str().unwrap()));
    let (response, ()) = tokio::join!(
        client.call("list_flows", json!({"sessionId":selected})),
        async {
            let request = browser_request(&mut browser).await;
            reply(
                &mut browser,
                &request,
                json!({"flows":[{"seq":0,"url":"https://external.test/"}]}),
            )
            .await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert_eq!(result(&response)["flows"][0]["seq"], 0);
    let original = std::fs::read(&file).unwrap();
    let mut tampered = discovery;
    tampered["token"] = json!("wrong proof key");
    std::fs::write(&file, serde_json::to_vec(&tampered).unwrap()).unwrap();
    wait_sessions(&mut client, 0).await;
    tool_error(
        &client
            .call("list_flows", json!({"sessionId":selected}))
            .await,
    );
    std::fs::write(&file, original).unwrap();
    wait_sessions(&mut client, 1).await;
    assert!(!client.home.path().join("settings.json").exists());
    assert!(!client.home.path().join("ca").exists());
    browser.close(None).await.unwrap();
    viewer.kill().await.unwrap();
    viewer.wait().await.unwrap();
    std::fs::remove_dir_all(client.home.path().join("har-viewer")).unwrap();
    client.close().await;
}

#[tokio::test]
async fn offline_discovery_resources_and_protocol_versions() {
    for version in ["2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"] {
        let mut client = Mcp::start("http://127.0.0.1:1", false, version).await;
        let response = client.rpc("tools/list", json!({})).await;
        if client.modern {
            assert_eq!(response["result"]["ttlMs"], 0);
            assert_eq!(response["result"]["cacheScope"], "private");
        }
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 12);
        assert!(tools
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true));
        assert!(tools
            .iter()
            .all(|tool| tool["inputSchema"]["type"] == "object"));
        let response = client.call("get_guide", json!({})).await;
        assert!(result(&response)["guide"]
            .as_str()
            .unwrap()
            .contains("includeBodies"));
        let response = client.rpc("resources/list", json!({})).await;
        assert_eq!(response["result"]["resources"].as_array().unwrap().len(), 2);
        let response = client
            .rpc("resources/read", json!({"uri":"hamsy://docs/rules"}))
            .await;
        if client.modern {
            assert_eq!(response["result"]["ttlMs"], 0);
            assert_eq!(response["result"]["cacheScope"], "private");
        }
        assert!(response["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("mockResponse"));
        assert!(client
            .rpc("resources/read", json!({"uri":"file:///etc/passwd"}))
            .await
            .get("error")
            .is_some());
        tool_error(&client.call("set_capture", json!({"paused":true})).await);
        tool_error(&client.call("get_status", json!({})).await);
        tool_error(&client.call("nonexistent", json!({})).await);
        assert!(client
            .rpc(
                if client.modern {
                    "server/discover"
                } else {
                    "ping"
                },
                json!({})
            )
            .await
            .get("error")
            .is_none());
        client.close().await;
    }
}

struct ReplayCounter(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl ReplayHook for ReplayCounter {
    async fn replay(
        &self,
        _: uuid::Uuid,
        _: Option<hamsy_core::RequestRecord>,
    ) -> Result<uuid::Uuid, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(uuid::Uuid::new_v4())
    }
}

struct App {
    state: ApiState,
    url: String,
    task: tokio::task::JoinHandle<()>,
    replay_count: Arc<AtomicUsize>,
    _home: tempfile::TempDir,
}
impl Drop for App {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl App {
    async fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let flows = Arc::new(FlowStore::new(100));
        let rules = Arc::new(RulesStore::load(&home.path().join("rules.json")));
        let (events, _) = tokio::sync::broadcast::channel(100);
        let replay_count = Arc::new(AtomicUsize::new(0));
        let state = ApiState::new(
            flows,
            rules,
            Arc::new(RwLock::new(Settings::default())),
            home.path().join("settings.json"),
            events,
            Arc::new(ReplayCounter(replay_count.clone())),
            Arc::new(StubCert),
            "test",
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = hamsy_api::router(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            state,
            url,
            task,
            replay_count,
            _home: home,
        }
    }
    fn seed(&self) -> String {
        let har = json!({"log":{"version":"1.2","creator":{"name":"test","version":"1"},"entries":[{
            "startedDateTime":"2026-09-17T00:00:00.000Z", "time":12,
            "request":{"method":"POST", "url":"https://example.test/checkout?token=URL_SECRET&item=1", "httpVersion":"HTTP/1.1",
                "headers":[{"name":"Authorization","value":"Bearer HEADER_SECRET"}], "queryString":[{"name":"token","value":"URL_SECRET"}], "cookies":[],
                "postData":{"mimeType":"application/json","text":"{\"password\":\"BODY_SECRET\",\"message\":\"hello\"}"}, "headersSize":-1,"bodySize":50},
            "response":{"status":500,"statusText":"Error","httpVersion":"HTTP/1.1","headers":[{"name":"Set-Cookie","value":"COOKIE_SECRET"}],"cookies":[],
                "content":{"size":80,"mimeType":"application/json","text":"{\"access_token\":\"RESPONSE_SECRET\",\"error\":\"broken\"}"},"redirectURL":"","headersSize":-1,"bodySize":80},
            "cache":{},"timings":{"send":0,"wait":12,"receive":0}
        }]}});
        let mut flow = hamsy_core::import_har(&har).unwrap().remove(0);
        flow.summary.seq = self.state.flows().next_seq();
        let id = flow.summary.id.to_string();
        self.state.flows().insert(flow);
        id
    }
}

#[tokio::test]
async fn live_reads_are_bounded_redacted_and_do_not_change_capture() {
    let app = App::start().await;
    app.seed();
    let id = app.seed();
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let response = client.call("get_status", json!({})).await;
    assert_eq!(result(&response)["flowCount"], 2);
    assert_eq!(result(&response)["agent"]["allowWrites"], false);
    let response = client
        .call(
            "list_flows",
            json!({"host":"example.test","statusClass":5,"limit":1}),
        )
        .await;
    assert_eq!(result(&response)["flows"].as_array().unwrap().len(), 1);
    assert_eq!(result(&response)["limited"], true);
    assert_eq!(result(&response)["lastSeq"], 2);
    assert!(!response.to_string().contains("URL_SECRET"));
    let response = client.call("get_flow", json!({"id":id})).await;
    assert_eq!(result(&response)["request"]["body"]["agentOmitted"], true);
    let response = client
        .call("get_flow", json!({"id":id,"includeBodies":true}))
        .await;
    let serialized = response.to_string();
    for secret in [
        "URL_SECRET",
        "HEADER_SECRET",
        "BODY_SECRET",
        "COOKIE_SECRET",
        "RESPONSE_SECRET",
    ] {
        assert!(
            !serialized.contains(secret),
            "leaked {secret}: {serialized}"
        );
    }
    assert!(serialized.contains("hello"));
    assert!(serialized.contains("broken"));
    let response = client
        .call(
            "get_flow",
            json!({"id":id,"includeBodies":true,"maxBodyBytes":5}),
        )
        .await;
    assert_eq!(
        result(&response)["response"]["body"]["agentTruncated"],
        true
    );
    let response = client.call("export_har", json!({"ids":[id]})).await;
    assert_eq!(
        result(&response)["har"]["log"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        result(&response)["har"]["log"]["entries"][0]["response"]["content"]
            .get("text")
            .is_none()
    );
    assert!(!response.to_string().contains("SECRET"));
    assert!(result(&response)["har"]["log"]["entries"][0]["request"]["cookies"].is_array());
    for (tool, args) in [
        ("get_flow", json!({"id":"../../settings"})),
        ("list_flows", json!({"limit":201})),
        ("list_flows", json!({"statusClass":9})),
        ("get_flow", json!({"id":id, "maxBodyBytes":0})),
        ("export_har", json!({"ids":[]})),
        ("list_flows", json!({"typo":true})),
        ("get_settings", json!({"change":"anything"})),
        ("set_capture", json!({"paused":true})),
        ("replay_request", json!({"id":id})),
    ] {
        tool_error(&client.call(tool, args).await);
    }
    assert!(!app.state.settings().paused);
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 0);
    let original = app
        .state
        .flows()
        .get(uuid::Uuid::parse_str(&id).unwrap())
        .unwrap();
    assert!(
        serde_json::to_string(&original)
            .unwrap()
            .contains("BODY_SECRET"),
        "original capture must remain intact"
    );
    client.close().await;
}

#[tokio::test]
async fn omitted_live_payloads_do_not_hit_the_bridge_response_limit() {
    let app = App::start().await;
    let id = app.seed();
    let uuid = uuid::Uuid::parse_str(&id).unwrap();
    let body_bytes = 5 * 1024 * 1024;
    app.state.flows().update(uuid, |flow| {
        let body = hamsy_core::BodyPayload {
            kind: hamsy_core::BodyKind::Text,
            data: "x".repeat(body_bytes),
            size: body_bytes as u64,
            ..Default::default()
        };
        flow.request.as_mut().unwrap().body = body.clone();
        flow.response.as_mut().unwrap().body = body;
        flow.original_request = flow.request.clone();
        flow.original_response = flow.response.clone();
        flow.ws_messages.push(hamsy_core::WsMessage {
            direction: hamsy_core::WsDirection::Recv,
            opcode: "text".into(),
            timestamp: 0,
            data: "w".repeat(9 * 1024 * 1024),
            size: 9 * 1024 * 1024,
        });
    });
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let response = client.call("get_flow", json!({"id":id})).await;
    let flow = result(&response);
    for key in ["request", "response", "originalRequest", "originalResponse"] {
        assert_eq!(flow[key]["body"]["agentOmitted"], true);
        assert_eq!(flow[key]["body"]["size"], body_bytes);
    }
    assert_eq!(flow["wsMessages"], json!([]));

    let response = client.call("export_har", json!({"ids":[id]})).await;
    let entry = &result(&response)["har"]["log"]["entries"][0];
    assert_eq!(entry["request"]["bodySize"], body_bytes);
    assert_eq!(entry["response"]["content"]["size"], body_bytes);
    assert!(entry["request"].get("postData").is_none());
    assert!(entry["response"]["content"].get("text").is_none());

    // Opting into bodies retains the fail-closed transfer limit. Previews are
    // redacted before shortening so partial JSON cannot bypass key redaction.
    tool_error(
        &client
            .call("get_flow", json!({"id":id,"includeBodies":true}))
            .await,
    );

    // Ordinary browser/API callers still receive original payloads by default.
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let full: Value = http
        .get(format!("{}/api/flows/{id}", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        full["request"]["body"]["data"].as_str().unwrap().len(),
        body_bytes
    );
    assert_eq!(
        full["originalResponse"]["body"]["data"]
            .as_str()
            .unwrap()
            .len(),
        body_bytes
    );
    assert_eq!(
        full["wsMessages"][0]["data"].as_str().unwrap().len(),
        9 * 1024 * 1024
    );
    let har: Value = http
        .get(format!("{}/api/har?ids={id}", app.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = &har["log"]["entries"][0];
    assert_eq!(
        entry["request"]["postData"]["text"].as_str().unwrap().len(),
        body_bytes
    );
    assert_eq!(
        entry["response"]["content"]["text"].as_str().unwrap().len(),
        body_bytes
    );
    app.state.flows().update(uuid, |flow| {
        for body in [
            &mut flow.request.as_mut().unwrap().body,
            &mut flow.response.as_mut().unwrap().body,
            &mut flow.original_request.as_mut().unwrap().body,
            &mut flow.original_response.as_mut().unwrap().body,
        ] {
            body.data = r#"{"password":"PRIVATE","message":"hello"}"#.into();
        }
    });
    let response = client
        .call("get_flow", json!({"id":id,"includeBodies":true}))
        .await;
    let flow = result(&response);
    assert_eq!(flow["wsMessages"], json!([]));
    assert!(flow["request"]["body"]["data"]
        .as_str()
        .unwrap()
        .contains("hello"));
    assert!(!response.to_string().contains("PRIVATE"));
    client.close().await;
}

#[tokio::test]
async fn writes_share_live_state_and_replay_is_single_shot() {
    let app = App::start().await;
    let flow_id = app.seed();
    let mut client = Mcp::start_permissions(&app.url, true, true).await;
    let response = client.rpc("tools/list", json!({})).await;
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    let rule = json!({"name":"fixture", "match":{"urlOp":"equals", "urlValue":"https://example.test/checkout"},
        "actions":[{"type":"mockResponse","status":200,"body":"ok"}]});
    let response = client.call("create_rule", json!({"rule":rule})).await;
    let id = result(&response)["id"].as_str().unwrap().to_string();
    assert!(app.state.rules().get(&id).is_some());
    let response = client.call("list_rules", json!({})).await;
    assert_eq!(result(&response)["rules"][0]["id"], id);
    let mut updated = rule.clone();
    updated["id"] = json!(id);
    updated["enabled"] = json!(false);
    result(&client.call("update_rule", json!({"rule":updated})).await);
    assert!(!app.state.rules().get(&id).unwrap().enabled);
    let mut invalid = rule;
    invalid["match"] = json!({"urlOp":"regex","urlValue":"["});
    tool_error(&client.call("create_rule", json!({"rule":invalid})).await);
    assert_eq!(app.state.rules().list().len(), 1);
    tool_error(
        &client
            .call("delete_rule", json!({"id":"../settings"}))
            .await,
    );
    updated["actions"][0]["body"] = json!("[REDACTED]");
    tool_error(&client.call("update_rule", json!({"rule":updated})).await);
    result(&client.call("set_capture", json!({"paused":true})).await);
    assert!(app.state.settings().paused);
    let stored: Value =
        serde_json::from_slice(&std::fs::read(app.state.settings_path()).unwrap()).unwrap();
    assert_eq!(stored["paused"], true);
    result(&client.call("replay_request", json!({"id":flow_id})).await);
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 1);
    tool_error(
        &client
            .call(
                "replay_request",
                json!({"id":uuid::Uuid::new_v4().to_string()}),
            )
            .await,
    );
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 1);
    result(&client.call("delete_rule", json!({"id":id})).await);
    assert!(app.state.rules().list().is_empty());
    client.close().await;
}

fn agent_error(response: &Value) -> &Value {
    assert_eq!(response["result"]["isError"], true, "{response}");
    let error = &response["result"]["structuredContent"]["error"];
    assert!(error["code"].is_string(), "{response}");
    assert!(error["message"].is_string());
    assert!(error["retryable"].is_boolean());
    assert!(error["outcomeUnknown"].is_boolean());
    error
}

#[tokio::test]
async fn replay_permission_is_independent_from_capture_and_rule_writes() {
    let app = App::start().await;
    let id = app.seed();
    for (writes, replay) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut client = Mcp::start_permissions(&app.url, writes, replay).await;
        let discovery = client.rpc("tools/list", json!({})).await;
        let names: Vec<_> = discovery["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.contains(&"replay_request"), replay);
        assert_eq!(names.contains(&"set_capture"), writes);
        assert!(names.contains(&"preview_rule"));
        let replay_result = client.call("replay_request", json!({"id":id})).await;
        if replay {
            result(&replay_result);
        } else {
            assert_eq!(agent_error(&replay_result)["code"], "permission_denied");
        }
        let capture_result = client.call("set_capture", json!({"paused":false})).await;
        if writes {
            result(&capture_result);
        } else {
            assert_eq!(agent_error(&capture_result)["code"], "permission_denied");
        }
        let status = client.call("get_status", json!({})).await;
        assert_eq!(result(&status)["agent"]["allowWrites"], writes);
        assert_eq!(result(&status)["agent"]["allowReplay"], replay);
        client.close().await;
    }
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn errors_have_safe_recovery_metadata_for_both_transports() {
    use axum::{
        http::StatusCode,
        routing::{get, post, put},
        Json, Router,
    };
    let applied = Arc::new(AtomicUsize::new(0));
    let counter = applied.clone();
    let router = Router::new()
        .route("/api/settings", get(|| async {
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"code":"busy","detail":"Authorization: SECRET_PROSE"})))
        }))
        .route("/api/rules/{id}", put(|| async { "x".repeat(8 * 1024 * 1024 + 1) }))
        .route("/api/rules", post(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async { (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"code":"internal","detail":"https://user:SECRET_PASSWORD@example.test/?token=SECRET_TOKEN"}))) }
        }))
        .route("/api/flows/{id}/replay", post(|| async { "not JSON SECRET_PAYLOAD" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut client = Mcp::start_permissions(&api, true, true).await;
    let read = client.call("get_settings", json!({})).await;
    assert_eq!(agent_error(&read)["code"], "busy");
    assert_eq!(agent_error(&read)["retryable"], true);
    assert_eq!(agent_error(&read)["outcomeUnknown"], false);
    let write = client
        .call(
            "create_rule",
            json!({"rule":{"name":"test","match":{},"actions":[]}}),
        )
        .await;
    assert_eq!(agent_error(&write)["code"], "api_error");
    assert_eq!(agent_error(&write)["retryable"], false);
    assert_eq!(agent_error(&write)["outcomeUnknown"], true);
    assert_eq!(applied.load(Ordering::SeqCst), 1);
    let oversized = client
        .call(
            "update_rule",
            json!({"rule":{"id":"large","name":"test","match":{},"actions":[]}}),
        )
        .await;
    assert_eq!(agent_error(&oversized)["code"], "response_too_large");
    assert_eq!(agent_error(&oversized)["retryable"], false);
    assert_eq!(agent_error(&oversized)["outcomeUnknown"], true);
    let replay = client
        .call("replay_request", json!({"id":uuid::Uuid::new_v4()}))
        .await;
    assert_eq!(agent_error(&replay)["code"], "invalid_response");
    assert_eq!(agent_error(&replay)["retryable"], false);
    assert_eq!(agent_error(&replay)["outcomeUnknown"], true);
    for response in [&read, &write, &replay] {
        assert!(!response.to_string().contains("SECRET_"));
    }
    let output = Command::new(BIN)
        .args(["agent", "--api-url", &api, "call", "get_settings"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cli["error"], *agent_error(&read));
    let invalid = client.call("list_flows", json!({"limit":0})).await;
    assert_eq!(agent_error(&invalid)["code"], "invalid_arguments");
    let missing = client.call("nonexistent", json!({})).await;
    assert_eq!(agent_error(&missing)["code"], "unknown_tool");
    client.close().await;
    task.abort();
}

#[tokio::test]
async fn preview_evaluates_phases_without_writes_replay_or_large_body_transfer() {
    let app = App::start().await;
    let id = app.seed();
    let uuid = uuid::Uuid::parse_str(&id).unwrap();
    app.state.flows().update(uuid, |flow| {
        let bytes = 5 * 1024 * 1024;
        flow.request.as_mut().unwrap().body.data = "x".repeat(bytes);
        flow.response.as_mut().unwrap().body.data = "y".repeat(bytes);
    });
    let before = serde_json::to_value(app.state.flows().get(uuid).unwrap()).unwrap();
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let rule = json!({"name":"preview","match":{"urlOp":"contains","urlValue":"checkout"},"actions":[{"type":"setResponseHeader","name":"X-Test","value":"yes"},{"type":"delay","ms":60000}]});
    let response = client
        .call("preview_rule", json!({"id":id,"rule":rule}))
        .await;
    let preview = result(&response);
    assert_eq!(preview["request"]["matched"], true);
    assert_eq!(preview["request"]["delayMs"], 60000);
    assert_eq!(preview["request"]["skippedActionIndices"], json!([0]));
    assert_eq!(preview["response"]["matched"], true);
    assert_eq!(preview["response"]["changes"]["headers"], true);
    assert_eq!(preview["response"]["delayMs"], 60000);
    assert!(!response.to_string().contains("SECRET"));
    for (action, reason) in [
        (
            json!({"type":"block","reason":"private"}),
            "request_blocked",
        ),
        (
            json!({"type":"mockResponse","body":"SECRET_BODY","delayMs":60000}),
            "mocked_response",
        ),
    ] {
        let response = client
            .call(
                "preview_rule",
                json!({"id":id,"rule":{"name":"short circuit","match":{},"actions":[action]}}),
            )
            .await;
        assert_eq!(result(&response)["response"]["reason"], reason);
        assert!(!response.to_string().contains("SECRET"));
    }
    let disabled = client
        .call(
            "preview_rule",
            json!({"id":id,"rule":{"name":"disabled","enabled":false,"match":{},"actions":[]}}),
        )
        .await;
    assert_eq!(result(&disabled)["request"]["reason"], "rule_disabled");
    let invalid = client.call("preview_rule", json!({"id":id,"rule":{"name":"bad","match":{"urlOp":"regex","urlValue":"["},"actions":[]}})).await;
    assert_eq!(agent_error(&invalid)["code"], "invalid_arguments");
    assert_eq!(
        serde_json::to_value(app.state.flows().get(uuid).unwrap()).unwrap(),
        before
    );
    assert!(app.state.rules().list().is_empty());
    assert_eq!(app.replay_count.load(Ordering::SeqCst), 0);
    client.close().await;
}

#[tokio::test]
async fn live_changes_include_completed_responses_and_stale_cursor_recovery() {
    let app = App::start().await;
    let id = app.seed();
    let uuid = uuid::Uuid::parse_str(&id).unwrap();
    let seq = app.state.flows().get(uuid).unwrap().summary.seq;
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let baseline = client.call("wait_for_changes", json!({})).await;
    let cursor = result(&baseline)["cursor"].as_str().unwrap().to_string();
    assert_eq!(result(&baseline)["changes"], json!([]));
    let (observed, ()) = tokio::join!(
        client.call("wait_for_changes", json!({"cursor":cursor,"waitMs":1000})),
        async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            app.state.flows().update(uuid, |flow| {
                flow.response.as_mut().unwrap().body.data = "new response".into();
            });
        }
    );
    let event = &result(&observed)["changes"][0];
    assert_eq!(event["kind"], "updated");
    assert_eq!(event["flowId"], id);
    assert_eq!(event["seq"], seq);
    assert!(!observed.to_string().contains("new response"));
    let next = result(&observed)["cursor"].clone();
    let empty = client
        .call("wait_for_changes", json!({"cursor":next,"waitMs":10}))
        .await;
    assert_eq!(result(&empty)["changes"], json!([]));
    assert_eq!(result(&empty)["timedOut"], true);
    app.state.flows().clear();
    let stale = client
        .call("wait_for_changes", json!({"cursor":cursor}))
        .await;
    let error = agent_error(&stale);
    assert_eq!(error["code"], "stale_cursor");
    assert_eq!(error["retryable"], false);
    assert_eq!(error["outcomeUnknown"], false);
    assert_eq!(error["details"]["resetRequired"], true);
    assert_eq!(error["details"]["reason"], "capture_changed");
    assert!(error["details"]["captureId"].is_string());
    assert!(error["details"]["cursor"].is_string());
    let invalid = client
        .call(
            "wait_for_changes",
            json!({"sessionId":format!("app:{}",uuid::Uuid::new_v4())}),
        )
        .await;
    assert_eq!(agent_error(&invalid)["code"], "invalid_arguments");
    client.close().await;
}

#[tokio::test]
async fn context_returns_redacted_match_evidence_for_live_and_browser_sources() {
    let app = App::start().await;
    let id = app.seed();
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let response = client
        .call(
            "get_match_context",
            json!({"id":id,"query":"broken","field":"Response body","maxContextBytes":128}),
        )
        .await;
    let context = result(&response);
    assert_eq!(context["redacted"], true);
    assert_eq!(context["matches"][0]["match"], "broken");
    assert!(!response.to_string().contains("SECRET"));
    let secret = client
        .call(
            "get_match_context",
            json!({"id":id,"query":"RESPONSE_SECRET","field":"Response body"}),
        )
        .await;
    assert_eq!(result(&secret)["matches"], json!([]));
    let session = uuid::Uuid::new_v4().to_string();
    let selected = format!("app:{session}");
    let mut browser = browser(&app.url).await;
    advertise(&mut browser, &[&session]).await;
    wait_sessions(&mut client, 2).await;
    let (response, ()) = tokio::join!(
        client.call("get_match_context", json!({"sessionId":selected,"id":id,"query":"socket","field":"WebSocket message","messageIndex":2})),
        async {
            let request = browser_request(&mut browser).await;
            assert_eq!(request["operation"], "get_match_context");
            let params: Value = serde_json::from_str(request["query"]["params"].as_str().unwrap()).unwrap();
            assert_eq!(params["messageIndex"], 2);
            reply(&mut browser, &request, json!({"flowId":id,"field":"WebSocket message","redacted":true,"matches":[{"match":"socket","before":"","after":"","messageIndex":2}],"scanned":1,"hasMore":false})).await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert_eq!(result(&response)["matches"][0]["messageIndex"], 2);
    browser.close(None).await.unwrap();
    client.close().await;
}

#[tokio::test]
async fn rule_edits_and_explicit_enable_preserve_current_private_values() {
    let app = App::start().await;
    // Simulate a current rule authored in the web UI, rather than by this agent.
    let original = json!({"id":"ui-fixture", "name":"from UI", "group":"fixtures", "notes":"keep notes", "enabled":true,
        "match":{"urlOp":"equals","urlValue":"https://example.test/?token=PRIVATE_TOKEN"},
        "actions":[{"type":"mockResponse","status":201,"body":"PRIVATE_BODY","headers":[{"name":"Authorization","value":"PRIVATE_HEADER"}]}]});
    app.state
        .rules()
        .create(serde_json::from_value(original).unwrap())
        .unwrap();
    let original = serde_json::to_value(app.state.rules().get("ui-fixture").unwrap()).unwrap();
    let mut client = Mcp::start(&app.url, true, "2025-11-25").await;
    let listed = client.call("list_rules", json!({})).await;
    assert_eq!(result(&listed)["rules"][0]["id"], "ui-fixture");
    for secret in ["PRIVATE_TOKEN", "PRIVATE_BODY", "PRIVATE_HEADER"] {
        assert!(!listed.to_string().contains(secret));
    }
    let tools = client.rpc("tools/list", json!({})).await;
    for name in ["edit_rule", "set_rule_enabled"] {
        let tool = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], false);
        assert_eq!(tool["annotations"]["idempotentHint"], true);
    }
    result(
        &client
            .call(
                "edit_rule",
                json!({"id":"ui-fixture","patch":{"name":"renamed","priority":-5,"group":null}}),
            )
            .await,
    );
    for enabled in [false, false, true, true] {
        let response = client
            .call(
                "set_rule_enabled",
                json!({"id":"ui-fixture", "enabled":enabled}),
            )
            .await;
        let ack = result(&response);
        assert_eq!(ack["enabled"], enabled);
        assert!(!response.to_string().contains("PRIVATE_"));
        let current = serde_json::to_value(app.state.rules().get("ui-fixture").unwrap()).unwrap();
        assert_eq!(current["name"], "renamed");
        assert_eq!(current["priority"], -5);
        assert_eq!(current["group"], Value::Null);
        for field in ["id", "notes", "match", "actions"] {
            assert_eq!(current[field], original[field]);
        }
        assert_eq!(
            app.state.rules().ruleset().rules().len(),
            usize::from(enabled)
        );
        let reloaded = RulesStore::load(&app._home.path().join("rules.json"));
        assert_eq!(
            serde_json::to_value(reloaded.get("ui-fixture").unwrap()).unwrap(),
            current
        );
    }
    let actions = json!([{"type":"mockResponse","status":503,"body":"new explicit body"}]);
    result(
        &client
            .call(
                "edit_rule",
                json!({"id":"ui-fixture","patch":{"actions":actions,"notes":null}}),
            )
            .await,
    );
    let current = serde_json::to_value(app.state.rules().get("ui-fixture").unwrap()).unwrap();
    assert_eq!(current["actions"][0]["status"], 503);
    assert_eq!(current["actions"][0]["body"], "new explicit body");
    assert_eq!(current["match"], original["match"]);
    assert_eq!(current["notes"], Value::Null);
    for patch in [
        json!({}),
        json!({"name":" "}),
        json!({"id":"changed"}),
        json!({"typo":true}),
        json!({"actions":null}),
        json!({"match":{"urlOp":"regex","urlValue":"["}}),
        json!({"actions":[{"type":"mockResponse","body":"[rule payload omitted]"}]}),
        json!({"notes":"[REDACTED]"}),
    ] {
        tool_error(
            &client
                .call("edit_rule", json!({"id":"ui-fixture","patch":patch}))
                .await,
        );
        assert_eq!(
            serde_json::to_value(app.state.rules().get("ui-fixture").unwrap()).unwrap(),
            current
        );
    }
    for (tool, args) in [
        ("edit_rule", json!({"id":"missing","patch":{"name":"x"}})),
        (
            "edit_rule",
            json!({"id":"../settings","patch":{"enabled":true}}),
        ),
        ("set_rule_enabled", json!({"id":"missing","enabled":true})),
        (
            "set_rule_enabled",
            json!({"id":"ui-fixture","enabled":null}),
        ),
        (
            "set_rule_enabled",
            json!({"id":"ui-fixture","enabled":false,"typo":true}),
        ),
    ] {
        tool_error(&client.call(tool, args).await);
    }
    let mut reader = Mcp::start(&app.url, false, "2025-11-25").await;
    for (tool, args) in [
        ("create_rule", json!({"rule":original})),
        ("update_rule", json!({"rule":original})),
        (
            "edit_rule",
            json!({"id":"ui-fixture","patch":{"name":"reader"}}),
        ),
        (
            "set_rule_enabled",
            json!({"id":"ui-fixture","enabled":false}),
        ),
    ] {
        tool_error(&reader.call(tool, args).await);
    }
    assert_eq!(
        serde_json::to_value(app.state.rules().get("ui-fixture").unwrap()).unwrap(),
        current
    );
    reader.close().await;
    client.close().await;
}

#[tokio::test]
async fn cli_config_and_json_fallback_work_outside_repository() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .args(["mcp", "--allow-writes", "--allow-replay", "--print-config"])
        .current_dir(home.path())
        .env("HAMSY_HOME", home.path())
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let config: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        std::path::Path::new(config["mcpServers"]["hamsy"]["command"].as_str().unwrap())
            .is_absolute()
    );
    assert_eq!(config["mcpServers"]["hamsy"]["args"][3], "--allow-writes");
    assert!(config["mcpServers"]["hamsy"]["args"]
        .as_array()
        .unwrap()
        .contains(&json!("--allow-replay")));
    let output = Command::new(BIN)
        .arg("agent-guide")
        .current_dir(home.path())
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("MCP beta"));
    let app = App::start().await;
    app.seed();
    let output = Command::new(BIN)
        .args([
            "agent",
            "--api-url",
            &app.url,
            "call",
            "list_flows",
            "--arguments",
            "{\"limit\":1}",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["flows"].as_array().unwrap().len(), 1);
    for arguments in ["{", "{\"limit\":0}"] {
        let output = Command::new(BIN)
            .args([
                "agent",
                "--api-url",
                &app.url,
                "call",
                "list_flows",
                "--arguments",
                arguments,
            ])
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .get("error")
            .is_some());
    }
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn rejects_remote_origins_credentials_and_path_injection() {
    for url in [
        "http://example.com:9081",
        "http://192.168.1.2:9081",
        "https://127.0.0.1:9081",
        "http://user:password@127.0.0.1:9081",
        "http://127.0.0.1:9081/api",
        "http://127.0.0.1:9081/?x=1",
        "http://127.0.0.1:9081/#fragment",
    ] {
        let output = Command::new(BIN)
            .args(["mcp", "--api-url", url, "--print-config"])
            .output()
            .await
            .unwrap();
        assert!(!output.status.success(), "accepted {url}");
        assert!(output.stdout.is_empty());
    }
    for url in ["http://localhost:9081", "http://[::1]:9081"] {
        let output = Command::new(BIN)
            .args(["mcp", "--api-url", url, "--print-config"])
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "rejected {url}");
    }
}

#[tokio::test]
async fn api_redirects_and_oversized_outputs_fail_closed() {
    use axum::{response::Redirect, routing::get, Json, Router};
    let redirects = Arc::new(AtomicUsize::new(0));
    let seen = redirects.clone();
    let router = Router::new()
        .route(
            "/api/settings",
            get(|| async { Redirect::temporary("/redirect-target") }),
        )
        .route(
            "/redirect-target",
            get(move || async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Json(json!({}))
            }),
        )
        .route(
            "/api/flows",
            get(|| async { "x".repeat(8 * 1024 * 1024 + 1) }),
        )
        .route(
            "/api/rules",
            get(|| async { Json(json!({"rules":[{"name":"x".repeat(300_000)}]})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut client = Mcp::start(&api, false, "2025-11-25").await;
    for name in ["get_settings", "list_flows", "list_rules"] {
        tool_error(&client.call(name, json!({})).await);
    }
    assert_eq!(redirects.load(Ordering::SeqCst), 0);
    client.close().await;
    task.abort();
}

#[tokio::test]
async fn regex_search_reads_current_live_content_and_routes_har_sessions() {
    let app = App::start().await;
    let first = app.seed();
    let mut client = Mcp::start(&app.url, false, "2025-11-25").await;
    let response = client
        .call("search_flows", json!({"query":"hello|broken","regex":true}))
        .await;
    let value = result(&response);
    assert_eq!(value["sessionId"], "app:live");
    assert_eq!(value["matches"][0]["flowId"], first);
    assert_eq!(
        value["matches"][0]["fields"],
        json!(["Request body", "Response body"])
    );
    assert!(!response.to_string().contains("SECRET"));
    let last_seq = value["nextAfterSeq"].clone();
    let second = app.seed();
    let response = client
        .call("search_flows", json!({"query":"hello","afterSeq":last_seq}))
        .await;
    assert_eq!(result(&response)["matches"][0]["flowId"], second);
    // Re-reading without a cursor sees responses added to an earlier request.
    app.state
        .flows()
        .update(uuid::Uuid::parse_str(&first).unwrap(), |f| {
            f.response.as_mut().unwrap().body.data = "newly completed response".into()
        });
    let response = client
        .call("search_flows", json!({"query":"newly completed"}))
        .await;
    assert_eq!(result(&response)["matches"][0]["flowId"], first);
    let response = client
        .call(
            "search_flows",
            json!({"query":"hello","excludedHosts":["EXAMPLE.TEST"]}),
        )
        .await;
    assert_eq!(result(&response)["matches"], json!([]));
    for args in [
        json!({"query":"[","regex":true}),
        json!({"query":""}),
        json!({"query":"x","limit":201}),
        json!({"query":"x","typo":true}),
    ] {
        tool_error(&client.call("search_flows", args).await);
    }
    let session = uuid::Uuid::new_v4().to_string();
    let selected = format!("app:{session}");
    let mut browser = browser(&app.url).await;
    advertise(&mut browser, &[&session]).await;
    wait_sessions(&mut client, 2).await;
    let (response, ()) = tokio::join!(
        client.call("search_flows", json!({"sessionId":selected,"query":"error.*timeout","regex":true,"caseSensitive":true,"excludedHosts":["ads.test"]})),
        async {
            let request = browser_request(&mut browser).await;
            assert_eq!(request["operation"], "search_flows");
            let params:Value = serde_json::from_str(request["query"]["params"].as_str().unwrap()).unwrap();
            assert_eq!(params["regex"],true); assert_eq!(params["caseSensitive"],true); assert_eq!(params["excludedHosts"],json!(["ads.test"]));
            reply(&mut browser, &request, json!({"matches":[{"flowId":first,"seq":0,"fields":["Response body"]}],"hasMore":false,"nextAfterSeq":0})).await;
        }
    );
    assert_eq!(result(&response)["sessionId"], selected);
    assert_eq!(result(&response)["matches"][0]["seq"], 0);
    browser.close(None).await.unwrap();
    client.close().await;
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
struct StopManaged(std::path::PathBuf);
impl Drop for StopManaged {
    fn drop(&mut self) {
        let _ = std::process::Command::new(BIN)
            .args(["mcp", "--stop-app", "--data-dir"])
            .arg(&self.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
async fn stop_managed(profile: &std::path::Path) -> std::process::Output {
    Command::new(BIN)
        .args(["mcp", "--stop-app", "--data-dir"])
        .arg(profile)
        .output()
        .await
        .unwrap()
}
#[tokio::test]
async fn auto_start_is_shared_survives_disconnect_and_preserves_profile_settings() {
    let profile = tempfile::tempdir().unwrap();
    let _cleanup = StopManaged(profile.path().to_owned());
    let settings = b"{\"manualProxy\":false,\"bindAddr\":\"0.0.0.0\",\"paused\":false}";
    let marker = br#"{"pid":1,"host":"127.0.0.1","port":9999,"snapshot":{"platform":"unknown"}}"#;
    std::fs::write(profile.path().join("settings.json"), settings).unwrap();
    std::fs::write(profile.path().join("sysproxy-state.json"), marker).unwrap();
    let api = format!("http://127.0.0.1:{}", free_port());
    let port = free_port();
    let (mut first, mut second) = tokio::join!(
        Mcp::start_config(&api, false, "2025-11-25", Some((profile.path(), port))),
        Mcp::start_config(&api, false, "2025-11-25", Some((profile.path(), port)))
    );
    let response = first.call("get_status", json!({})).await;
    assert_eq!(result(&response)["proxyPort"], port);
    assert_eq!(result(&response)["capturing"], true);
    let response = second.call("get_settings", json!({})).await;
    assert_eq!(result(&response)["bindAddr"], "127.0.0.1");
    assert_eq!(result(&response)["manualProxy"], true);
    let descriptor = profile.path().join("mcp-runtime/service.json");
    let original = std::fs::read(&descriptor).unwrap();
    let discovery: Value = serde_json::from_slice(&original).unwrap();
    first.close().await;
    result(&second.call("get_status", json!({})).await);
    assert_eq!(std::fs::read(&descriptor).unwrap(), original);
    second.close().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(http
        .get(format!("{api}/api/state"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    assert_eq!(
        http.post(format!("{api}/api/mcp-runtime/stop"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    // A stale/tampered credential must never stop an unrelated listener.
    let mut tampered = discovery;
    tampered["token"] = json!("wrong token");
    std::fs::write(&descriptor, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert!(!stop_managed(profile.path()).await.status.success());
    assert!(http
        .get(format!("{api}/api/state"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    std::fs::write(&descriptor, &original).unwrap();
    assert!(stop_managed(profile.path()).await.status.success());
    for _ in 0..100 {
        if http.get(format!("{api}/api/state")).send().await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(http.get(format!("{api}/api/state")).send().await.is_err());
    assert_eq!(
        std::fs::read(profile.path().join("settings.json")).unwrap(),
        settings
    );
    assert_eq!(
        std::fs::read(profile.path().join("sysproxy-state.json")).unwrap(),
        marker
    );
    assert!(stop_managed(profile.path()).await.status.success()); // already stopped
    let mut restarted =
        Mcp::start_config(&api, false, "2025-11-25", Some((profile.path(), port))).await;
    result(&restarted.call("get_status", json!({})).await);
    assert_ne!(std::fs::read(&descriptor).unwrap(), original);
    restarted.close().await;
}
#[tokio::test]
async fn automatic_start_reuses_existing_capture_without_creating_a_profile() {
    let app = App::start().await;
    app.seed();
    let profile = tempfile::tempdir().unwrap();
    let mut client = Mcp::start_config(
        &app.url,
        false,
        "2025-11-25",
        Some((profile.path(), free_port())),
    )
    .await;
    assert_eq!(
        result(&client.call("get_status", json!({})).await)["flowCount"],
        1
    );
    client.close().await;
    assert_eq!(std::fs::read_dir(profile.path()).unwrap().count(), 0);
    assert_eq!(app.state.flows().len(), 1);
    assert!(stop_managed(profile.path()).await.status.success());
    assert!(reqwest::get(format!("{}/api/state", app.url))
        .await
        .unwrap()
        .status()
        .is_success());
}
#[tokio::test]
async fn occupied_api_and_proxy_ports_fail_without_overwriting_settings() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let occupied = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            axum::Router::new().fallback(|| async { "not Hamsy" }),
        )
        .await
        .unwrap();
    });
    let profile = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .args([
            "mcp",
            "--api-url",
            &format!("http://127.0.0.1:{occupied}"),
            "--data-dir",
        ])
        .arg(profile.path())
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read_dir(profile.path()).unwrap().count(), 0);
    let settings = b"{\"paused\":true}";
    std::fs::write(profile.path().join("settings.json"), settings).unwrap();
    let output = Command::new(BIN)
        .args([
            "mcp",
            "--api-url",
            &format!("http://127.0.0.1:{}", free_port()),
            "--proxy-port",
            &occupied.to_string(),
            "--data-dir",
        ])
        .arg(profile.path())
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("app.log"));
    assert_eq!(
        std::fs::read(profile.path().join("settings.json")).unwrap(),
        settings
    );
    assert_eq!(std::fs::read_dir(profile.path()).unwrap().count(), 2); // settings + runtime dir; no CA
    task.abort();
}
