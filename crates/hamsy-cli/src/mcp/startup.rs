//! Shared background capture startup. Credentials and logs remain in the user's
//! private profile; MCP stdout is never inherited by the background process.
use crate::har_open::private_file_options;
#[cfg(unix)]
use crate::har_open::validate_private_dir;
use anyhow::{bail, ensure, Context, Result};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Serialize, Deserialize)]
struct Discovery {
    api_url: String,
    token: String,
    pid: u32,
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()?)
}
fn directory(data: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(data)?;
    let dir = data.join("mcp-runtime");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        validate_private_dir(&dir)?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}
fn discovery(dir: &Path) -> Option<Discovery> {
    let mut file = private_file_options(&dir.join("service.json"), false, false).ok()?;
    let mut text = String::new();
    Read::by_ref(&mut file)
        .take(4096)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}
fn valid_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw)?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    ensure!(
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
            && url.port_or_known_default() != Some(0),
        "Runtime URL must be an HTTP loopback origin with a nonzero port"
    );
    Ok(url)
}
async fn small_json(response: reqwest::Response) -> Result<Value> {
    ensure!(
        response.status().is_success(),
        "Hamsy probe returned an HTTP error"
    );
    ensure!(
        response.content_length().unwrap_or(0) <= 32768,
        "Invalid Hamsy probe response"
    );
    let mut response = response;
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 32768,
            "Invalid Hamsy probe response"
        );
        bytes.extend(chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
// Only connection refusal permits starting a process. An occupied/hung/unrelated
// port produces an error, never a second server or overwritten process.
async fn running(client: &Client, base: &Url) -> Result<bool> {
    let host = base.host_str().unwrap().trim_matches(['[', ']']);
    let address = (
        host.parse::<IpAddr>()?,
        base.port_or_known_default().unwrap(),
    );
    match tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect(address),
    )
    .await
    {
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(false),
        Ok(Ok(stream)) => drop(stream),
        _ => bail!("Cannot probe {base}; check --api-url or use --no-auto-start"),
    }
    let value = small_json(client.get(base.join("api/state")?).send().await?)
        .await
        .context("API port is occupied but is not a responding Hamsy instance; check --api-url")?;
    ensure!(
        value["version"].is_string()
            && value["capturing"].is_boolean()
            && value["proxyPort"].is_u64()
            && value["uiPort"].is_u64()
            && value["flowCount"].is_u64()
            && value["uptimeSecs"].is_u64(),
        "API port belongs to an unrecognized service; check --api-url"
    );
    Ok(true)
}
fn proof(token: &str, challenge: &str) -> Vec<u8> {
    ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, token.as_bytes()),
        challenge.as_bytes(),
    )
    .as_ref()
    .to_vec()
}
async fn authenticated(client: &Client, d: &Discovery) -> Result<bool> {
    let url = valid_url(&d.api_url)?;
    let challenge = uuid::Uuid::new_v4().to_string();
    let response = client
        .get(url.join("api/mcp-runtime/health")?)
        .query(&[("challenge", &challenge)])
        .send()
        .await;
    let Ok(response) = response else {
        return Ok(false);
    };
    let Ok(value) = small_json(response).await else {
        return Ok(false);
    };
    let Ok(bytes) = serde_json::from_value::<Vec<u8>>(value["proof"].clone()) else {
        return Ok(false);
    };
    Ok(ring::hmac::verify(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, d.token.as_bytes()),
        challenge.as_bytes(),
        &bytes,
    )
    .is_ok())
}
async fn lock(dir: &Path) -> Result<File> {
    let file = private_file_options(&dir.join("startup.lock"), false, true)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            _ => bail!("Another MCP startup is busy; reconnect in a moment"),
        }
    }
}
pub fn claim_profile(data: &Path) -> Result<File> {
    let file = private_file_options(&directory(data)?.join("app.lock"), false, true)?;
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("An MCP-started app already owns this profile"))?;
    Ok(file)
}
struct PendingChild {
    process: tokio::process::Child,
    armed: bool,
}
impl Drop for PendingChild {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.process.start_kill();
        }
    }
}
pub async fn ensure_running(base: &Url, data: &Path, proxy_port: Option<u16>) -> Result<()> {
    valid_url(base.as_str())?;
    let client = client()?;
    if running(&client, base).await? {
        return Ok(());
    }
    let dir = directory(data)?;
    let _lock = lock(&dir).await?;
    if running(&client, base).await? {
        return Ok(());
    }
    if let Some(d) = discovery(&dir) {
        ensure!(
            !authenticated(&client, &d).await?,
            "This profile already has an MCP-started Hamsy at {}; set --api-url to that address",
            d.api_url
        );
    }
    let port = proxy_port
        .unwrap_or_else(|| hamsy_core::Settings::load(&data.join("settings.json")).proxy_port);
    ensure!(
        port != 0 && port != base.port_or_known_default().unwrap(),
        "Choose distinct nonzero proxy and API ports using --proxy-port and --api-url"
    );
    let token = uuid::Uuid::new_v4().simple().to_string();
    let log = private_file_options(&dir.join("app.log"), false, true)?;
    log.set_len(0)?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "run",
            "--agent-managed",
            "--manual",
            "--no-open",
            "--bind",
            base.host_str().unwrap().trim_matches(['[', ']']),
            "--ui-port",
            &base.port_or_known_default().unwrap().to_string(),
            "--proxy-port",
            &port.to_string(),
            "--data-dir",
        ])
        .arg(data)
        .env("HAMSY_MCP_TOKEN", &token)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = PendingChild {
        process: command
            .spawn()
            .context("Could not launch the installed Hamsy executable")?,
        armed: true,
    };
    let d = Discovery {
        api_url: base.to_string(),
        token,
        pid: child.process.id().context("missing child PID")?,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        if authenticated(&client, &d).await? {
            break;
        }
        if let Some(status) = child.process.try_wait()? {
            bail!("Hamsy could not start ({status}); inspect {}. Check --proxy-port/--api-url for port conflicts", dir.join("app.log").display());
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "Hamsy startup timed out; inspect {}",
            dir.join("app.log").display()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    child.armed = false; // shared app survives an individual MCP connection
    eprintln!(
        "Hamsy started at {base} (proxy {port}). Stop it with hamsy mcp --stop-app --data-dir {}",
        data.display()
    );
    Ok(())
}
pub async fn stop(data: &Path) -> Result<()> {
    let dir = data.join("mcp-runtime");
    if !dir.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    validate_private_dir(&dir)?;
    let _lock = lock(&dir).await?;
    let Some(d) = discovery(&dir) else {
        return Ok(());
    };
    let client = client()?;
    if !authenticated(&client, &d).await? {
        if !running(&client, &valid_url(&d.api_url)?).await? {
            return Ok(());
        }
        bail!("No authenticated MCP-started app at the saved address; no process was stopped");
    }
    let response = client
        .post(valid_url(&d.api_url)?.join("api/mcp-runtime/stop")?)
        .bearer_auth(&d.token)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "Hamsy refused the stop request"
    );
    eprintln!("Requested shutdown of the MCP-started Hamsy instance.");
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Control {
    token: String,
    pub stopped: Arc<Notify>,
}
impl Control {
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("HAMSY_MCP_TOKEN")
            .context("agent-managed startup requires a private launch token")?;
        std::env::remove_var("HAMSY_MCP_TOKEN");
        ensure!(
            token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid launch token"
        );
        Ok(Self {
            token,
            stopped: Arc::default(),
        })
    }
    // The child publishes its own identity so an interrupted MCP launch cannot
    // leave a running app without an authenticated stop record.
    pub fn publish(&self, data: &Path, api_url: String) -> Result<()> {
        let dir = directory(data)?;
        let d = Discovery {
            api_url,
            token: self.token.clone(),
            pid: std::process::id(),
        };
        let temp = dir.join(format!("service-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = private_file_options(&temp, false, true)?;
        file.write_all(&serde_json::to_vec(&d)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, dir.join("service.json"))?;
        Ok(())
    }
    pub fn routes(&self) -> Router {
        Router::new()
            .route("/api/mcp-runtime/health", get(health))
            .route("/api/mcp-runtime/stop", post(shutdown))
            .with_state(self.clone())
    }
}
async fn health(
    State(control): State<Control>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, StatusCode> {
    let challenge = query
        .get("challenge")
        .filter(|c| !c.is_empty() && c.len() <= 128)
        .ok_or(StatusCode::BAD_REQUEST)?;
    Ok(Json(json!({"proof":proof(&control.token,challenge)})))
}
async fn shutdown(State(control): State<Control>, headers: HeaderMap) -> StatusCode {
    if headers.get("authorization").and_then(|h| h.to_str().ok())
        != Some(format!("Bearer {}", control.token).as_str())
    {
        return StatusCode::FORBIDDEN;
    }
    control.stopped.notify_one();
    StatusCode::ACCEPTED
}
