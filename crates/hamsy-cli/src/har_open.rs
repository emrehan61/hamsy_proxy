//! Private, loopback-only HAR handoff service. This never enters `run()`.
use anyhow::{bail, Context, Result};
use axum::{
    extract::{DefaultBodyLimit, Path as RoutePath, Query, Request, State},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use clap::Args;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[cfg(windows)]
fn clear_standard_handle_inheritance() -> Result<()> {
    // Redirecting the daemon's stdio does not stop CreateProcess from
    // inheriting the opener's original capture-pipe handles. Clear inheritance
    // on this short-lived CLI's standard handles so those pipes reach EOF as
    // soon as the opener exits; the handles remain usable by this process.
    use windows_sys::Win32::{
        Foundation::{
            GetLastError, SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        },
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    for standard in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(standard) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
        }
    }
    Ok(())
}

#[cfg(windows)]
#[path = "windows_security.rs"]
mod windows_security;

const PROTOCOL: u32 = 1;
const MAX_FILES: usize = 32;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const QUEUE_BYTES: usize = 128 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(5 * 60);
const IDLE: Duration = Duration::from_secs(30 * 60);

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// HAR files to open as separate viewer tabs.
    #[arg(required_unless_present = "stop", num_args = 1.., conflicts_with = "stop")]
    pub files: Vec<PathBuf>,
    /// Print the viewer URL without launching a browser.
    #[arg(long)]
    pub no_open: bool,
    /// Stop the background HAR viewer without changing capture or proxy settings.
    #[arg(long, conflicts_with_all = ["files", "no_open"])]
    pub stop: bool,
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    #[arg(long)]
    pub data_dir: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
struct HarFile {
    name: String,
    text: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Bundle {
    files: Vec<HarFile>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Discovery {
    protocol: u32,
    port: u16,
    token: String,
    pid: u32,
}
struct Pending {
    created: Instant,
    bytes: usize,
    bundle: Bundle,
}
struct Viewer {
    discovery: Discovery,
    pending: Mutex<HashMap<String, Pending>>,
    last_request: Mutex<Instant>,
    shutdown: tokio::sync::Notify,
}

fn validate(bundle: &Bundle) -> Result<usize> {
    if bundle.files.is_empty() || bundle.files.len() > MAX_FILES {
        bail!("open between 1 and {MAX_FILES} HAR files at once");
    }
    let bytes = bundle.files.iter().map(|f| f.text.len()).sum::<usize>();
    if bytes > MAX_BYTES {
        bail!("HAR files exceed the combined 64 MiB limit");
    }
    for file in &bundle.files {
        if file.name.is_empty() || file.name.len() > 1024 || file.name.contains(['/', '\\']) {
            bail!("invalid HAR filename");
        }
        let doc: serde_json::Value = serde_json::from_str(&file.text)
            .with_context(|| format!("{} is not valid JSON", file.name))?;
        if !doc
            .get("log")
            .and_then(|v| v.get("entries"))
            .is_some_and(|v| v.is_array())
        {
            bail!(
                "{} is not a HAR document: expected log.entries array",
                file.name
            );
        }
        // Validate using the same parser as API HAR imports, without inserting flows.
        hamsy_core::import_har(&doc).with_context(|| format!("{} is not valid HAR", file.name))?;
    }
    Ok(bytes)
}

fn read_files(paths: &[PathBuf]) -> Result<Bundle> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        bail!("open between 1 and {MAX_FILES} HAR files at once");
    }
    let mut files = Vec::new();
    let mut remaining = MAX_BYTES;
    for path in paths {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        if !file.metadata()?.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        let mut bytes = Vec::new();
        file.take(remaining as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > remaining {
            bail!("HAR files exceed the combined 64 MiB limit");
        }
        remaining -= bytes.len();
        files.push(HarFile {
            name: path
                .file_name()
                .context("HAR file has no name")?
                .to_string_lossy()
                .into_owned(),
            text: String::from_utf8(bytes)
                .with_context(|| format!("{} is not UTF-8", path.display()))?,
        });
    }
    let bundle = Bundle { files };
    validate(&bundle)?;
    Ok(bundle)
}

// The private directory and no-follow opens prevent another local user from
// reading credentials or redirecting discovery/log/lock writes through symlinks.
#[cfg(unix)]
fn private_dir(data_dir: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    fs::create_dir_all(data_dir)?;
    let dir = data_dir.join("har-viewer");
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    validate_private_dir(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn validate_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        bail!(
            "{} must be an owner-only directory (mode 700), owned by this user",
            dir.display()
        );
    }
    Ok(())
}
#[cfg(windows)]
fn private_dir(data_dir: &Path) -> Result<PathBuf> {
    windows_security::private_dir(data_dir)
}

#[cfg(not(any(unix, windows)))]
fn private_dir(_: &Path) -> Result<PathBuf> {
    bail!("HAR desktop opening is currently supported on macOS, Linux, and Windows");
}

fn private_file(path: &Path, append: bool) -> Result<File> {
    private_file_options(path, append, true)
}

#[cfg(windows)]
fn private_file_options(path: &Path, append: bool, create: bool) -> Result<File> {
    windows_security::open_private_file(path, append, create)
}

#[cfg(not(windows))]
fn private_file_options(path: &Path, append: bool, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            bail!("{} must be an owner-only regular file", path.display());
        }
    }
    Ok(file)
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

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?)
}

#[derive(Serialize, Deserialize)]
struct Health {
    protocol: u32,
    #[serde(default)]
    version: String,
    #[serde(default)]
    proof: Vec<u8>,
}

fn health_payload(challenge: &str, protocol: u32, version: &str) -> Vec<u8> {
    serde_json::to_vec(&("hamsy-har-health", challenge, protocol, version))
        .expect("serializable health fields")
}

async fn health_response(
    State(viewer): State<Arc<Viewer>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(challenge) = query
        .get("challenge")
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let version = env!("CARGO_PKG_VERSION").to_string();
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, viewer.discovery.token.as_bytes());
    let proof = ring::hmac::sign(&key, &health_payload(challenge, PROTOCOL, &version));
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(Health {
            protocol: PROTOCOL,
            version,
            proof: proof.as_ref().to_vec(),
        }),
    )
        .into_response()
}

async fn health(client: &reqwest::Client, d: &Discovery) -> Option<Health> {
    // Authenticate the listener before disclosing discovery credentials or HAR
    // contents: a stale remembered port may belong to an unrelated process.
    let challenge = Uuid::new_v4().simple().to_string();
    let response = client
        .get(format!("http://127.0.0.1:{}/api/har/open-health", d.port))
        .query(&[("challenge", &challenge)])
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let health: Health = response.json().await.ok()?;
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, d.token.as_bytes());
    ring::hmac::verify(
        &key,
        &health_payload(&challenge, health.protocol, &health.version),
        &health.proof,
    )
    .ok()?;
    Some(health)
}

fn ensure_compatible(health: &Health) -> Result<()> {
    if health.protocol != PROTOCOL || health.version != env!("CARGO_PKG_VERSION") {
        bail!("A different Hamsy HAR viewer version is running. Run `hamsy open --stop` (with the same --data-dir, if used), then reopen the HAR file.");
    }
    Ok(())
}

async fn healthy(client: &reqwest::Client, d: &Discovery) -> Result<bool> {
    let Some(health) = health(client, d).await else {
        return Ok(false);
    };
    ensure_compatible(&health)?;
    Ok(true)
}

async fn stop(data_dir: &Path) -> Result<()> {
    let dir = data_dir.join("har-viewer");
    match fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
        Ok(_) => {}
    }
    #[cfg(unix)]
    validate_private_dir(&dir)?;
    #[cfg(windows)]
    windows_security::validate_private_dir(&dir)?;
    let Some(d) = discovery(&dir) else {
        return Ok(());
    };
    let client = client()?;
    if health(&client, &d).await.is_none() {
        return Ok(());
    }
    // Notification may close the listener before its response is flushed, so
    // release of the existing service lock is the authoritative stop result.
    let response = client
        .post(format!("http://127.0.0.1:{}/api/har/open-stop", d.port))
        .bearer_auth(&d.token)
        .send()
        .await;
    if let Ok(response) = response {
        if !response.status().is_success() {
            bail!("HAR viewer rejected shutdown ({})", response.status());
        }
    }
    let lock = private_file_options(&dir.join("service.lock"), false, false)?;
    for _ in 0..100 {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(e) => return Err(e.into()),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("HAR viewer did not stop within five seconds")
}

pub async fn open(args: OpenArgs) -> Result<()> {
    let data_dir = crate::resolve_data_dir(args.data_dir.as_deref());
    if args.stop {
        return stop(&data_dir).await;
    }
    // Fail before spawning a process or modifying the data directory for bad input.
    let bundle = read_files(&args.files)?;
    let dir = private_dir(&data_dir)?;
    let client = client()?;
    let mut ready = None;
    if let Some(d) = discovery(&dir) {
        if healthy(&client, &d).await? {
            ready = Some(d);
        }
    }
    if ready.is_none() {
        let mut log = private_file(&dir.join("viewer.log"), true)?;
        // Bound logs between starts. No request URLs or HAR contents are logged.
        if log.metadata()?.len() > 1024 * 1024 {
            log.set_len(0)?;
            log.seek(std::io::SeekFrom::Start(0))?;
        }
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("har-viewer-serve")
            .arg("--data-dir")
            .arg(&data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Detach from the launching terminal; all standard streams are redirected.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Keep the detached viewer out of the caller's console. Its three
            // standard streams are already redirected to the private log.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(windows)]
        clear_standard_handle_inheritance()?;
        let mut child = command.spawn().context("could not start the HAR viewer")?;
        for _ in 0..100 {
            if let Some(d) = discovery(&dir) {
                if healthy(&client, &d).await? {
                    ready = Some(d);
                    break;
                }
            }
            let _ = child.try_wait();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let d = ready.with_context(|| format!("HAR viewer did not start; see {} (if a different viewer protocol is running, close that process first)", dir.join("viewer.log").display()))?;
    let response = client
        .post(format!("http://127.0.0.1:{}/api/har/open-upload", d.port))
        .bearer_auth(&d.token)
        .json(&bundle)
        .send()
        .await?;
    if !response.status().is_success() {
        bail!(
            "HAR viewer rejected upload ({}): {}",
            response.status(),
            response.text().await?
        );
    }
    let result: serde_json::Value = response.json().await?;
    let ticket = result["ticket"]
        .as_str()
        .context("invalid viewer handoff response")?;
    let url = format!("http://127.0.0.1:{}/?openHar={ticket}", d.port);
    println!("{url}");
    if !args.no_open {
        ::open::that(&url)
            .with_context(|| format!("Could not launch browser. Open this URL manually: {url}"))?;
    }
    Ok(())
}

async fn guard(State(viewer): State<Arc<Viewer>>, request: Request, next: Next) -> Response {
    let expected_host = format!("127.0.0.1:{}", viewer.discovery.port);
    let expected_origin = format!("http://{expected_host}");
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(expected_host.as_str())
        || request
            .headers()
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(expected_origin.as_str()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let path = request.uri().path();
    let private = matches!(path, "/api/har/open-upload" | "/api/har/open-stop");
    if private {
        let expected = format!("Bearer {}", viewer.discovery.token);
        if request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    } else if request.method() != Method::GET && request.method() != Method::HEAD {
        return (StatusCode::FORBIDDEN, "HAR viewer is read-only").into_response();
    }
    *viewer.last_request.lock() = Instant::now();
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    if private {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}

async fn upload(State(viewer): State<Arc<Viewer>>, Json(bundle): Json<Bundle>) -> Response {
    let bytes = match validate(&bundle) {
        Ok(n) => n,
        Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let mut queue = viewer.pending.lock();
    queue.retain(|_, p| p.created.elapsed() < TTL);
    if queue.len() >= 64 || queue.values().map(|p| p.bytes).sum::<usize>() + bytes > QUEUE_BYTES {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "HAR handoff queue is full; retry after five minutes",
        )
            .into_response();
    }
    let ticket = Uuid::new_v4().simple().to_string();
    queue.insert(
        ticket.clone(),
        Pending {
            created: Instant::now(),
            bytes,
            bundle,
        },
    );
    Json(serde_json::json!({"ticket": ticket})).into_response()
}

async fn fetch(
    State(viewer): State<Arc<Viewer>>,
    RoutePath(ticket): RoutePath<String>,
) -> Response {
    let mut queue = viewer.pending.lock();
    queue.retain(|_, p| p.created.elapsed() < TTL);
    let response = match queue.get(&ticket) {
        Some(p) => Json(p.bundle.clone()).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "HAR handoff expired or was not found; open the file again",
        )
            .into_response(),
    };
    ([(header::CACHE_CONTROL, "no-store")], response).into_response()
}

fn router(viewer: Arc<Viewer>, dir: &Path) -> Router {
    let settings = hamsy_core::Settings {
        paused: true,
        manual_proxy: true,
        bind_addr: "127.0.0.1".into(),
        ui_port: viewer.discovery.port,
        ..Default::default()
    };
    let api = hamsy_api::ApiState::new_standalone(
        Arc::new(hamsy_core::FlowStore::new(1)),
        Arc::new(hamsy_core::RulesStore::load(&dir.join("unused-rules.json"))),
        settings,
        dir.join("unused-settings.json"),
        env!("CARGO_PKG_VERSION"),
    )
    .into_viewer();
    // Explicit read-only routes avoid exposing mutations, certificate setup,
    // filesystem writes or request tracing (ticket URLs must not enter logs).
    let reads = Router::new()
        .route("/state", get(hamsy_api::routes::state::get_state))
        .route("/settings", get(hamsy_api::routes::settings::get_settings))
        .route("/rules", get(hamsy_api::routes::rules::list))
        .route("/flows", get(hamsy_api::routes::flows::list))
        .route("/flows/{id}", get(hamsy_api::routes::flows::get))
        .route("/ws", get(hamsy_api::routes::ws::ws_handler))
        .fallback(|| async { StatusCode::NOT_FOUND });
    Router::new()
        .nest("/api", reads)
        .fallback(hamsy_api::assets::asset_handler)
        .with_state(api)
        .merge(
            Router::new()
                .route("/api/har/open-health", get(health_response))
                .route(
                    "/api/har/open-stop",
                    post(|State(viewer): State<Arc<Viewer>>| async move {
                        viewer.shutdown.notify_one();
                        StatusCode::ACCEPTED
                    }),
                )
                .route("/api/har/open-upload", post(upload))
                .route("/api/har/open/{ticket}", get(fetch))
                .with_state(viewer.clone())
                // Valid JSON text escapes at most two-fold; allow extra filename overhead.
                .layer(DefaultBodyLimit::max(MAX_BYTES * 2 + 65536)),
        )
        .layer(middleware::from_fn_with_state(viewer, guard))
}

pub async fn serve(args: ServeArgs) -> Result<()> {
    let dir = private_dir(&args.data_dir)?;
    let lock = private_file(&dir.join("service.lock"), false)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    let preferred = discovery(&dir).map(|d| d.port).unwrap_or(0);
    let listener =
        match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, preferred)).await {
            Ok(listener) => listener,
            Err(_) => tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?,
        };
    let d = Discovery {
        protocol: PROTOCOL,
        port: listener.local_addr()?.port(),
        token: Uuid::new_v4().simple().to_string(),
        pid: std::process::id(),
    };
    let temp = dir.join(format!("service-{}.tmp", Uuid::new_v4().simple()));
    let mut file = private_file(&temp, false)?;
    file.write_all(&serde_json::to_vec(&d)?)?;
    file.sync_all()?;
    #[cfg(windows)]
    windows_security::replace_file(&temp, &dir.join("service.json"))?;
    #[cfg(not(windows))]
    fs::rename(temp, dir.join("service.json"))?;
    let viewer = Arc::new(Viewer {
        discovery: d,
        pending: Mutex::new(HashMap::new()),
        last_request: Mutex::new(Instant::now()),
        shutdown: tokio::sync::Notify::new(),
    });
    let app = router(viewer.clone(), &dir);
    // Drop the server on idle (including WebSockets); browser HAR tabs persist in
    // IndexedDB. Discovery remains to preserve the browser origin on next start.
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => (),
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        result = axum::serve(listener, app) => { result?; },
        _ = async {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                viewer.pending.lock().retain(|_, p| p.created.elapsed() < TTL);
                if viewer.last_request.lock().elapsed() >= IDLE { break; }
            }
        } => {},
        _ = viewer.shutdown.notified() => {},
        _ = ctrl_c => {},
    }
    drop(lock);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use tower::ServiceExt;

    fn bundle() -> Bundle {
        Bundle {
            files: vec![HarFile {
                name: "session.har".into(),
                text: r#"{"log":{"version":"1.2","entries":[]}}"#.into(),
            }],
        }
    }

    fn viewer() -> Arc<Viewer> {
        Arc::new(Viewer {
            discovery: Discovery {
                protocol: PROTOCOL,
                port: 12345,
                token: "secret".into(),
                pid: 1,
            },
            pending: Mutex::new(HashMap::new()),
            last_request: Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
        })
    }

    fn request(method: &str, path: &str, body: String, token: bool) -> Request {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:12345")
            .header(header::CONTENT_TYPE, "application/json");
        if token {
            builder = builder.header(header::AUTHORIZATION, "Bearer secret");
        }
        builder.body(Body::from(body)).unwrap()
    }

    #[test]
    fn incompatible_versions_explain_restart() {
        let mismatch = Health {
            protocol: PROTOCOL,
            version: "older".into(),
            proof: Vec::new(),
        };
        assert!(ensure_compatible(&mismatch)
            .unwrap_err()
            .to_string()
            .contains("hamsy open --stop"));
        let current = Health {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
            proof: Vec::new(),
        };
        assert!(ensure_compatible(&current).is_ok());
    }

    #[test]
    fn reads_explicit_files_and_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("some file.har");
        fs::write(&path, &bundle().files[0].text).unwrap();
        assert_eq!(
            read_files(std::slice::from_ref(&path)).unwrap().files[0].name,
            "some file.har"
        );
        fs::write(&path, "{}").unwrap();
        assert!(read_files(std::slice::from_ref(&path)).is_err());
        fs::write(&path, "not json").unwrap();
        assert!(read_files(std::slice::from_ref(&path)).is_err());
        assert!(read_files(&[dir.path().join("absent.har")]).is_err());
        assert!(read_files(&[dir.path().to_path_buf()]).is_err());
        assert!(read_files(&[]).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let fifo = dir.path().join("pipe.har");
            let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
            assert!(read_files(&[fifo]).is_err());
        }
        let mut invalid = bundle();
        invalid.files[0].name = "../../secret".into();
        assert!(validate(&invalid).is_err());
        invalid.files = vec![bundle().files[0].clone(); MAX_FILES + 1];
        assert!(validate(&invalid).is_err());
    }

    #[tokio::test]
    async fn upload_auth_ticket_expiry_and_no_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let viewer = viewer();
        let app = router(viewer.clone(), dir.path());
        let payload = serde_json::to_string(&bundle()).unwrap();
        assert_eq!(
            app.clone()
                .oneshot(request("POST", "/api/har/open-stop", String::new(), false))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let nonce = "0123456789abcdef0123456789abcdef";
        let response = app
            .clone()
            .oneshot(request(
                "GET",
                &format!("/api/har/open-health?challenge={nonce}"),
                String::new(),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let health: Health =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, b"secret");
        assert!(ring::hmac::verify(
            &key,
            &health_payload(nonce, health.protocol, &health.version),
            &health.proof
        )
        .is_ok());
        assert!(ring::hmac::verify(
            &key,
            &health_payload("different nonce", health.protocol, &health.version),
            &health.proof
        )
        .is_err());
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/har/open-upload",
                payload.clone(),
                false,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(request("POST", "/api/har/open-upload", payload, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024).await.unwrap()).unwrap();
        let ticket = value["ticket"].as_str().unwrap();
        let path = format!("/api/har/open/{ticket}");
        // Repeat fetches are allowed until expiry, for browser/network retries.
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(request("GET", &path, String::new(), false))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let actual: Bundle =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(actual.files[0].text, bundle().files[0].text);
        }
        viewer.pending.lock().get_mut(ticket).unwrap().created = Instant::now() - TTL;
        let response = app
            .clone()
            .oneshot(request("GET", &path, String::new(), false))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        for path in [
            "/api/system-proxy",
            "/api/settings",
            "/api/rules",
            "/api/har/import",
        ] {
            let response = app
                .clone()
                .oneshot(request("POST", path, "{}".into(), false))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let response = app
            .clone()
            .oneshot(request("GET", "/api/state", String::new(), false))
            .await
            .unwrap();
        let state: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(state["viewerOnly"], true);
        assert_eq!(state["capturing"], false);
        assert_eq!(state["systemProxy"]["supported"], false);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        for raw in [
            "/../../Cargo.toml",
            "/%2e%2e/Cargo.toml",
            "/assets/%2E%2E/secret",
            "/assets%5c..%5csecret",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request("GET", raw, String::new(), false))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        let mut bad_host = request("GET", &path, String::new(), false);
        bad_host
            .headers_mut()
            .insert(header::HOST, "attacker.example:12345".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(bad_host).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let mut bad_origin = request("GET", &path, String::new(), false);
        bad_origin
            .headers_mut()
            .insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert_eq!(
            app.oneshot(bad_origin).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn expired_entries_are_reclaimed_and_queue_is_bounded() {
        let viewer = viewer();
        viewer.pending.lock().insert(
            "old".into(),
            Pending {
                created: Instant::now() - TTL,
                bytes: QUEUE_BYTES,
                bundle: bundle(),
            },
        );
        assert_eq!(
            upload(State(viewer.clone()), Json(bundle())).await.status(),
            StatusCode::OK
        );
        assert!(!viewer.pending.lock().contains_key("old"));
        viewer.pending.lock().insert(
            "full".into(),
            Pending {
                created: Instant::now(),
                bytes: QUEUE_BYTES,
                bundle: bundle(),
            },
        );
        assert_eq!(
            upload(State(viewer), Json(bundle())).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_files_reject_symlinks_and_public_directories() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let dir = private_dir(temp.path()).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = private_file(&dir.join("lock"), false).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        symlink(dir.join("lock"), dir.join("link")).unwrap();
        assert!(private_file(&dir.join("link"), false).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(temp.path()).is_err());
    }
}
