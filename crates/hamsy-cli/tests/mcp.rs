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

struct Mcp {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    modern: bool,
    home: tempfile::TempDir,
}
impl Mcp {
    async fn start(api: &str, writes: bool, version: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(BIN);
        cmd.args(["mcp", "--api-url", api]);
        if writes {
            cmd.arg("--allow-writes");
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
async fn offline_discovery_resources_and_protocol_versions() {
    for version in ["2025-03-26", "2025-06-18", "2025-11-25", "2026-07-28"] {
        let mut client = Mcp::start("http://127.0.0.1:1", false, version).await;
        let response = client.rpc("tools/list", json!({})).await;
        if client.modern {
            assert_eq!(response["result"]["ttlMs"], 0);
            assert_eq!(response["result"]["cacheScope"], "private");
        }
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7);
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
async fn writes_share_live_state_and_replay_is_single_shot() {
    let app = App::start().await;
    let flow_id = app.seed();
    let mut client = Mcp::start(&app.url, true, "2025-11-25").await;
    let response = client.rpc("tools/list", json!({})).await;
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 12);
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

#[tokio::test]
async fn cli_config_and_json_fallback_work_outside_repository() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(BIN)
        .args(["mcp", "--allow-writes", "--print-config"])
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
