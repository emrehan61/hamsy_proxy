//! Runs the actual CLI/daemon handshake without launching browsers or touching
//! certificate/system-proxy commands. Every process uses a temporary data dir.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_hamsy"))
            .args(["open", "--stop", "--data-dir"])
            .arg(&self.0)
            .status();
    }
}

fn terminate(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}
fn open(data: &Path, har: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hamsy"))
        .args(["open", "--no-open", "--data-dir"])
        .arg(data)
        .arg("--")
        .arg(har)
        .output()
        .unwrap()
}
fn stop(data: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hamsy"))
        .args(["open", "--stop", "--data-dir"])
        .arg(data)
        .output()
        .unwrap()
}
fn successful_url(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn descriptor(data: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(data.join("har-viewer/service.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn simultaneous_launches_reuse_service_and_preserve_capture_state() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data with spaces");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("settings.json"), b"existing capture settings").unwrap();
    fs::write(
        data.join("sysproxy-state.json"),
        b"existing proxy recovery marker",
    )
    .unwrap();
    let har = temp.path().join("some capture.har");
    fs::write(&har, r#"{"log":{"version":"1.2","entries":[]}}"#).unwrap();
    let _cleanup = Cleanup(data.clone());
    let data2 = data.clone();
    let har2 = har.clone();
    let first = std::thread::spawn(move || open(&data2, &har2));
    let second = open(&data, &har);
    let url1 = successful_url(first.join().unwrap());
    let url2 = successful_url(second);
    let parsed1 = reqwest::Url::parse(&url1).unwrap();
    let parsed2 = reqwest::Url::parse(&url2).unwrap();
    assert_eq!(parsed1.port(), parsed2.port());
    assert_ne!(parsed1.query(), parsed2.query());
    let d = descriptor(&data);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{}", d["port"]);
    let ticket = parsed1
        .query_pairs()
        .find(|(key, _)| key == "openHar")
        .unwrap()
        .1
        .to_string();
    let response = client
        .get(format!("{base}/api/har/open/{ticket}"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["files"][0]["name"], "some capture.har");
    let state: serde_json::Value = client
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["viewerOnly"], true);
    assert_eq!(state["capturing"], false);
    let third = successful_url(open(&data, &har));
    assert_eq!(reqwest::Url::parse(&third).unwrap().port(), parsed1.port());
    assert_eq!(descriptor(&data)["pid"], d["pid"]);
    assert_eq!(
        client
            .post(format!("{base}/api/system-proxy"))
            .json(&serde_json::json!({"enabled":true}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert_eq!(
        fs::read(data.join("settings.json")).unwrap(),
        b"existing capture settings"
    );
    assert_eq!(
        fs::read(data.join("sysproxy-state.json")).unwrap(),
        b"existing proxy recovery marker"
    );
    assert_eq!(fs::read_dir(&data).unwrap().count(), 3); // two originals + private service dir; no CA
    assert!(!data.join("har-viewer/unused-settings.json").exists());
    // Simulate an unclean exit. The lock must release without deleting the
    // remembered port so a new viewer can recover the same browser origin.
    terminate(d["pid"].as_u64().unwrap() as u32);
    for _ in 0..100 {
        if client
            .get(format!("{base}/api/state"))
            .send()
            .await
            .is_err()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let recovered = successful_url(open(&data, &har));
    assert_eq!(
        reqwest::Url::parse(&recovered).unwrap().port(),
        parsed1.port()
    );
    assert_ne!(descriptor(&data)["pid"], d["pid"]);
    assert!(stop(&data).status.success());
    assert!(client
        .get(format!("{base}/api/state"))
        .send()
        .await
        .is_err());
    assert!(stop(&data).status.success());
}

#[test]
fn invalid_input_does_not_start_viewer() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let har = temp.path().join("broken.har");
    fs::write(&har, "not a HAR").unwrap();
    assert!(!open(&data, &har).status.success());
    assert!(!data.exists());
    assert!(stop(&data).status.success());
    assert!(!data.exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_port_impostor_never_receives_credentials_or_har() {
    use axum::{
        extract::{Request, State},
        response::IntoResponse,
        Router,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{Arc, Mutex},
    };
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let private = data.join("har-viewer");
    fs::create_dir_all(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let record = private.join("service.json");
    fs::write(&record, serde_json::to_vec(&serde_json::json!({"protocol":1,"port":port,"token":"never-share-this-secret","pid":123})).unwrap()).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();
    type Observations = Arc<Mutex<Vec<(String, bool)>>>;
    let observed = Arc::new(Mutex::new(Vec::<(String, bool)>::new()));
    let state = observed.clone();
    let fake = Router::new().fallback(|State(observed): State<Observations>, request: Request| async move {
        observed.lock().unwrap().push((request.uri().path().into(), request.headers().contains_key("authorization")));
        axum::Json(serde_json::json!({"protocol":1,"version":env!("CARGO_PKG_VERSION"),"proof":[]})).into_response()
    }).with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, fake).await.unwrap();
    });
    let har = temp.path().join("secret.har");
    fs::write(&har, r#"{"log":{"entries":[]}}"#).unwrap();
    let _cleanup = Cleanup(data.clone());
    let actual = successful_url(open(&data, &har));
    assert_ne!(reqwest::Url::parse(&actual).unwrap().port(), Some(port));
    let requests = observed.lock().unwrap();
    assert!(!requests.is_empty());
    assert!(requests
        .iter()
        .all(|(path, auth)| path == "/api/har/open-health" && !auth));
    drop(requests);
    assert!(stop(&data).status.success());
    task.abort();
}
