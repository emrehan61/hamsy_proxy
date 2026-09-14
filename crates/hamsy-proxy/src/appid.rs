//! Resolves a client's local TCP connection to the display name of the
//! macOS application that originated it (e.g. `"Google Chrome"`, `"Safari"`,
//! `"curl"`), for the UI's "app" filter.
//!
//! Only loopback clients can be resolved at all: a remote/LAN device's
//! process obviously isn't visible to this machine's process table. For a
//! loopback client, the mechanism is: map the client's local TCP port to its
//! owning pid (via `libproc`'s `proc_listpids`/`PROC_PIDLISTFDS`/
//! `PROC_PIDFDSOCKETINFO`, mirroring what `lsof -iTCP` does under the hood),
//! then map that pid to its executable path, then the path to a friendly app
//! name (see [`app_name_from_path`]).
//!
//! The full pid/fd/socket scan is blocking and relatively expensive (it
//! walks every running process), so it always runs under
//! [`tokio::task::spawn_blocking`], and its result is cached and shared
//! across lookups where possible. A *time-based* cache TTL is not enough to
//! make that safe, though: ephemeral ports are reused quickly, so a scan
//! taken even a second ago can describe a *different*, already-closed
//! socket that happened to reuse this exact port, and serving that stale
//! association would misattribute the connection. Instead, a cached
//! port->pid entry is only trusted when the scan that produced it is proven
//! to have *started after* this connection was accepted (see
//! [`resolve_client_app`]'s `accepted_at`) - such a scan is guaranteed to
//! have observed the connection's socket, since it already existed before
//! the scan began. A burst of connections all accepted before one such scan
//! starts therefore still shares that one scan.

use std::net::SocketAddr;
use std::time::Instant;

/// A connection-scoped attribution result. Traffic never waits for the lookup;
/// recorded flows receive a metadata update when it completes.
#[derive(Clone, Debug)]
pub struct AppResolution(tokio::sync::watch::Receiver<Option<Option<String>>>);

impl AppResolution {
    pub(crate) fn start(client: SocketAddr) -> Self {
        static LOOKUPS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
        let (tx, rx) = tokio::sync::watch::channel(None);
        // Saturation only drops optional metadata, never traffic. Bound both
        // concurrent fallback children and lookup tasks waiting on the scan gate.
        let permit = LOOKUPS
            .get_or_init(|| tokio::sync::Semaphore::new(128))
            .try_acquire();
        if let Ok(permit) = permit {
            tokio::spawn(async move {
                let _permit = permit;
                let app = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    resolve_client_app(client),
                )
                .await
                .unwrap_or(None);
                let _ = tx.send(Some(app));
            });
        } else {
            let _ = tx.send(Some(None));
        }
        Self(rx)
    }

    /// Called after the initial flow event, so a late result cannot be
    /// overwritten by the initial unattributed summary. Uses the exact flow
    /// ID instead of a port lookup (ports can be reused after disconnect).
    pub(crate) fn attach(&self, ctx: &crate::config::ProxyContext, id: hamsy_core::FlowId) {
        self.attach_to(ctx.flows.clone(), ctx.events.clone(), id);
    }

    fn attach_to(
        &self,
        flows: std::sync::Arc<hamsy_core::FlowStore>,
        events: tokio::sync::broadcast::Sender<hamsy_core::ServerEvent>,
        id: hamsy_core::FlowId,
    ) {
        let mut rx = self.0.clone();
        tokio::spawn(async move {
            let result = loop {
                if let Some(result) = rx.borrow_and_update().clone() {
                    break result;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            };
            if let Some(app) = result {
                if let Some(flow) = flows.update(id, |flow| flow.summary.app = Some(app)) {
                    let _ = events.send(hamsy_core::ServerEvent::Flow { flow });
                }
            }
        });
    }
}

/// Resolves `client` to the display name of the local application that
/// owns that socket, best-effort.
///
/// Returns `None` when `client` isn't a loopback address (nothing we can
/// resolve from here), when running on a non-macOS platform, or when the
/// owning process/executable couldn't be determined.
pub async fn resolve_client_app(client: SocketAddr) -> Option<String> {
    // Captured immediately: callers invoke this right after `accept()`, so
    // this is effectively this connection's accept time, and is the proof
    // point a cached scan must postdate before it can be trusted for this
    // specific port (see the module docs).
    let accepted_at = Instant::now();

    if !client.ip().is_loopback() {
        return None;
    }
    let port = client.port();

    #[cfg(target_os = "macos")]
    {
        macos::resolve_port(port, accepted_at).await
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (port, accepted_at);
        None
    }
}

/// Maps an executable path to a human-friendly application display name.
///
/// Prefers the first `*.app` bundle component found in the path (stripping
/// the `.app` suffix): this correctly attributes helper/XPC processes nested
/// inside an app bundle (e.g. one of Chrome's many per-site
/// `Google Chrome Helper.app` processes) to their parent app rather than the
/// helper itself. Falls back to the executable's basename when there is no
/// enclosing `.app`, further mapped through [`friendly_system_name`] for a
/// handful of known system networking processes that don't live inside one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn app_name_from_path(path: &str) -> String {
    if let Some(app) = path.split('/').find(|seg| seg.ends_with(".app")) {
        return app.trim_end_matches(".app").to_string();
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    friendly_system_name(basename)
        .map(str::to_string)
        .unwrap_or_else(|| basename.to_string())
}

/// Small, easily-extended table of known system networking processes that
/// don't run from inside an `.app` bundle, mapped to the user-facing app
/// they act on behalf of. Add more entries here as they're discovered.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn friendly_system_name(basename: &str) -> Option<&'static str> {
    match basename {
        // Safari's (and other WebKit-based apps') shared network process.
        "com.apple.WebKit.Networking" => Some("Safari"),
        // macOS's shared background URL-session daemon.
        "nsurlsessiond" => Some("System"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! The real (macOS-only) resolution mechanism: `libproc` first,
    //! falling back to shelling out to `lsof` if `libproc` can't place a
    //! given port (e.g. it failed outright, or lacks permission to inspect
    //! the owning process).

    use std::collections::HashMap;
    use std::sync::OnceLock;
    use std::time::Instant;

    use parking_lot::Mutex;

    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::file_info::{pidfdinfo, ListFDs, ProcFDType};
    use libproc::libproc::net_info::{SocketFDInfo, SocketInfoKind};
    use libproc::libproc::proc_pid::{listpidinfo, pidinfo, pidpath};
    use libproc::processes::{pids_by_type, ProcFilter};

    use super::app_name_from_path;

    /// Upper bound on the retained port->pid entries. If a scan somehow
    /// finds more open TCP sockets than this (pathological), the map is
    /// truncated rather than left to grow without bound.
    const PORT_CACHE_MAX: usize = 4096;
    /// Upper bound on the retained pid->app-name entries. Unlike the port
    /// cache this one is long-lived (a pid's executable never changes), so
    /// it's simply cleared and rebuilt once it gets too large.
    const PID_CACHE_MAX: usize = 4096;

    /// The two caches described in the module docs: a port->owning-pid map
    /// (refreshed by a full system scan, trusted only per the freshness
    /// rule below) and a long-lived pid->display-name map.
    struct Cache {
        port_to_pid: HashMap<u16, u32>,
        /// When the scan that produced `port_to_pid` *started* (not when it
        /// finished). Recording the start, rather than the finish, is what
        /// makes the freshness check in `is_fresh_for` sound: the scan
        /// necessarily observed system state at-or-after this instant, so
        /// anything that already existed at this instant is guaranteed to
        /// have been seen.
        scan_started_at: Option<Instant>,
        pid_to_app: HashMap<u32, String>,
    }

    impl Cache {
        /// True if this cache's scan is proven to have started after
        /// `accepted_at` - i.e. the connection already existed when the
        /// scan ran, so the scan's view of `port_to_pid` (hit or miss) can
        /// be trusted for it. A scan that started at or before
        /// `accepted_at` cannot make that guarantee (the connection might
        /// not have existed yet), no matter how recently it ran.
        fn is_fresh_for(&self, accepted_at: Instant) -> bool {
            self.scan_started_at
                .is_some_and(|started| started > accepted_at)
        }
    }

    fn cache() -> &'static Mutex<Cache> {
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        CACHE.get_or_init(|| {
            Mutex::new(Cache {
                port_to_pid: HashMap::new(),
                scan_started_at: None,
                pid_to_app: HashMap::new(),
            })
        })
    }

    /// Single-flight gate around performing an actual rescan: whichever
    /// caller decides a rescan is needed gets here first and does the scan;
    /// everyone else waits, then (re-checking the same freshness condition)
    /// finds it was just refreshed and skips straight past their own scan.
    fn scan_gate() -> &'static std::sync::Arc<tokio::sync::Mutex<()>> {
        static GATE: OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
        GATE.get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
    }

    pub(super) async fn resolve_port(port: u16, accepted_at: Instant) -> Option<String> {
        if let Some(pid) = pid_for_port(port, accepted_at).await {
            if let Ok(Some(app)) = tokio::task::spawn_blocking(move || app_for_pid(pid)).await {
                return Some(app);
            }
        }
        // `libproc` couldn't place this port even after a rescan proven to
        // postdate the connection (the scan failed outright, or we lack
        // permission to inspect the owning process) - fall back to a
        // single targeted `lsof` lookup for just this port.
        lsof_lookup(port).await
    }

    /// Looks up `port`'s owning pid, rescanning first if the cache can't be
    /// proven to already reflect a state at-or-after `accepted_at`.
    async fn pid_for_port(port: u16, accepted_at: Instant) -> Option<u32> {
        {
            let guard = cache().lock();
            if guard.is_fresh_for(accepted_at) {
                return guard.port_to_pid.get(&port).copied();
            }
        }
        rescan(accepted_at).await;
        // `rescan` guarantees that, on return, the cache is fresh for
        // `accepted_at` (or the scan failed, in which case there's nothing
        // to look up either way).
        let guard = cache().lock();
        guard
            .is_fresh_for(accepted_at)
            .then(|| guard.port_to_pid.get(&port).copied())
            .flatten()
    }

    /// Performs a blocking full-system scan and installs its result,
    /// unless another caller already did so (proven fresh for
    /// `accepted_at`) while we were waiting for the single-flight gate.
    async fn rescan(accepted_at: Instant) {
        let permit = scan_gate().clone().lock_owned().await;
        if cache().lock().is_fresh_for(accepted_at) {
            return; // someone else already scanned after we were accepted.
        }
        // Recorded *before* running the (possibly slow) scan: the scan
        // itself runs at-or-after this instant, so anything that already
        // existed at this instant - including this connection, since
        // `accepted_at` was captured even earlier - is guaranteed visible
        // to it.
        let started = Instant::now();
        let _ = tokio::task::spawn_blocking(move || {
            // Keep single-flight ownership through publication, including
            // when the async lookup is canceled.
            let _permit = permit;
            let Ok(map) = scan_tcp_ports() else {
                return;
            };
            let mut guard = cache().lock();
            // A full replacement (not a merge) so a port whose owner
            // changed - or that closed - since the last scan can't
            // misattribute to a stale, reused-port entry.
            guard.port_to_pid = if map.len() > PORT_CACHE_MAX {
                map.into_iter().take(PORT_CACHE_MAX).collect()
            } else {
                map
            };
            guard.scan_started_at = Some(started);
        })
        .await;
    }

    fn app_for_pid(pid: u32) -> Option<String> {
        if let Some(app) = cache().lock().pid_to_app.get(&pid).cloned() {
            return Some(app);
        }
        let path = pidpath(pid as i32).ok()?;
        let app = app_name_from_path(&path);
        let mut guard = cache().lock();
        if guard.pid_to_app.len() >= PID_CACHE_MAX {
            guard.pid_to_app.clear();
        }
        guard.pid_to_app.insert(pid, app.clone());
        Some(app)
    }

    /// Walks every running process's open file descriptors looking for TCP
    /// sockets, building a map from local port to owning pid.
    ///
    /// Blocking and syscall-heavy (proportional to system-wide process/fd
    /// count) - always run this under `spawn_blocking`, never on an async
    /// executor thread.
    fn scan_tcp_ports() -> Result<HashMap<u16, u32>, String> {
        let deadline = Instant::now() + std::time::Duration::from_millis(500);
        let pids = pids_by_type(ProcFilter::All).map_err(|e| e.to_string())?;
        let mut map = HashMap::new();
        for pid in pids {
            if Instant::now() >= deadline {
                return Err("process scan timed out".into());
            }
            let ipid = pid as i32;
            let Ok(info) = pidinfo::<BSDInfo>(ipid, 0) else {
                continue;
            };
            let Ok(fds) = listpidinfo::<ListFDs>(ipid, info.pbi_nfiles as usize) else {
                continue;
            };
            for fd in fds {
                if Instant::now() >= deadline {
                    return Err("process scan timed out".into());
                }
                if !matches!(ProcFDType::from(fd.proc_fdtype), ProcFDType::Socket) {
                    continue;
                }
                let Ok(socket) = pidfdinfo::<SocketFDInfo>(ipid, fd.proc_fd) else {
                    continue;
                };
                if !matches!(
                    SocketInfoKind::from(socket.psi.soi_kind),
                    SocketInfoKind::Tcp
                ) {
                    continue;
                }
                // Safe: `soi_kind` was just checked to be `Tcp`, so `pri_tcp`
                // is the union's active variant.
                let tcp = unsafe { socket.psi.soi_proto.pri_tcp };
                // `insi_lport` holds the local port in network byte order.
                let port = u16::from_be(tcp.tcpsi_ini.insi_lport as u16);
                if port != 0 {
                    map.insert(port, pid);
                }
            }
        }
        Ok(map)
    }

    /// Fallback for when the `libproc` scan can't place `port`: asks `lsof`
    /// directly for whatever owns it on loopback.
    async fn lsof_lookup(port: u16) -> Option<String> {
        let filter = format!("-iTCP@127.0.0.1:{port}");
        let output = tokio::process::Command::new("lsof")
            .args(["-nP", &filter, "-Fpc"])
            .kill_on_drop(true)
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // `-Fpc` emits one `p<pid>` line followed by one `c<command>` line
        // per matching process; we only need the first match's command.
        let command = text.lines().find_map(|line| line.strip_prefix('c'))?;
        Some(app_name_from_path(command))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_attribution_does_not_block_flow_and_backfills_exact_id() {
        use hamsy_core::{BodyPayload, Flow, FlowStore, RequestRecord, ServerEvent};
        let flows = std::sync::Arc::new(FlowStore::new(10));
        let (events, mut event_rx) = tokio::sync::broadcast::channel(10);
        let (tx, rx) = tokio::sync::watch::channel(None);
        let resolution = AppResolution(rx);
        let id = uuid::Uuid::new_v4();
        flows.insert(Flow::new_request(
            id,
            1,
            0,
            "GET",
            "http",
            "example.com",
            80,
            "/",
            "http://example.com/",
            "HTTP/1.1",
            "127.0.0.1:12345",
            RequestRecord {
                method: "GET".into(),
                url: "http://example.com/".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![],
                body: BodyPayload::default(),
                query: vec![],
            },
        ));
        resolution.attach_to(flows.clone(), events, id);
        // Metadata is still pending; flow processing and updates remain available.
        flows.update(id, |f| f.summary.status = Some(200));
        assert_eq!(flows.get(id).unwrap().summary.app, None);
        tx.send(Some(Some("Safari".into()))).unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            ServerEvent::Flow { flow } => {
                assert_eq!(flow.id, id);
                assert_eq!(flow.app.as_deref(), Some("Safari"));
                assert_eq!(flow.status, Some(200));
            }
            _ => panic!("expected flow update"),
        }
        // Already resolved results also reach later requests on this connection.
        let (events, mut event_rx) = tokio::sync::broadcast::channel(10);
        resolution.attach_to(flows.clone(), events, id);
        tokio::time::timeout(std::time::Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn chrome_helper_path_uses_first_app_component() {
        let path = "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/120.0.0.0/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
        assert_eq!(app_name_from_path(path), "Google Chrome");
    }

    #[test]
    fn plain_binary_uses_basename() {
        assert_eq!(app_name_from_path("/usr/bin/curl"), "curl");
    }

    #[test]
    fn webkit_networking_process_maps_to_safari() {
        let path = "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking";
        assert_eq!(app_name_from_path(path), "Safari");
    }

    #[test]
    fn nsurlsessiond_maps_to_system() {
        assert_eq!(app_name_from_path("/usr/libexec/nsurlsessiond"), "System");
    }

    #[tokio::test]
    async fn non_loopback_client_is_never_resolved() {
        let addr: SocketAddr = "93.184.216.34:12345".parse().unwrap();
        assert_eq!(resolve_client_app(addr).await, None);
    }

    /// Regression test for the "every new connection falls through to
    /// `lsof`" perf bug: a cache miss must itself trigger a rescan (proven
    /// to postdate the connection), so a burst of brand-new connections -
    /// whose ephemeral ports can't appear in any scan taken before they
    /// existed - shares one rescan instead of spawning `lsof` (or a full
    /// pid scan) once per connection.
    ///
    /// All `N` real `curl` child processes connect (and are accepted)
    /// before any resolution is attempted, and all `N` resolutions are then
    /// driven *concurrently* (mirroring `server.rs`, where each accepted
    /// connection is resolved independently in its own spawned task) rather
    /// than one after another - a sequential loop would let each call's
    /// `accepted_at` postdate the previous call's rescan and thus force its
    /// own rescan every time, which isn't how a real connection burst
    /// behaves. Run concurrently, only the single scan that wins the
    /// single-flight gate should actually execute; every other lookup
    /// should be a cheap cache hit once that scan lands, so the total time
    /// should stay roughly flat rather than scaling with `N`.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn burst_of_new_connections_shares_one_scan() {
        const N: usize = 12;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let mut children = Vec::with_capacity(N);
        for _ in 0..N {
            children.push(
                tokio::process::Command::new("curl")
                    .args(["-s", "--max-time", "5", &format!("http://{addr}/")])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("spawn curl"),
            );
        }

        let mut peers = Vec::with_capacity(N);
        for _ in 0..N {
            let (stream, peer_addr) = listener.accept().await.expect("accept");
            peers.push((stream, peer_addr));
        }

        let start = std::time::Instant::now();
        let apps = futures_util::future::join_all(
            peers
                .iter()
                .map(|(_, peer_addr)| resolve_client_app(*peer_addr)),
        )
        .await;
        let elapsed = start.elapsed();

        for app in &apps {
            assert_eq!(app.as_deref(), Some("curl"));
        }
        // One full scan on this kind of machine takes on the order of tens
        // of ms (measured ~55ms during development); one `lsof` spawn is
        // similar. If each of the `N` connections paid for its own
        // scan/spawn this would take north of a second; comfortably
        // bounding it well below that catches a regression back to
        // per-connection spawns without being sensitive to exact timings.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "resolving {N} brand-new connections took {elapsed:?}; \
             expected them to share ~1 scan, not one per connection"
        );

        drop(peers);
        for mut child in children {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    /// End-to-end sanity check of the real (non-mocked) macOS mechanism: a
    /// genuine child process (`curl`) opens a real loopback TCP connection,
    /// and [`resolve_client_app`] is asked to resolve it while that
    /// connection is still alive (curl is left waiting on a response we
    /// never send, so its socket - and thus the pid owning that port -
    /// stays put for the duration of the lookup).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn resolves_curl_child_process_over_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let mut child = tokio::process::Command::new("curl")
            .args(["-s", "--max-time", "2", &format!("http://{addr}/")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn curl");

        let (stream, peer_addr) = listener.accept().await.expect("accept");
        let app = resolve_client_app(peer_addr).await;
        drop(stream);
        let _ = child.kill().await;
        let _ = child.wait().await;

        assert_eq!(app.as_deref(), Some("curl"));
    }
}
