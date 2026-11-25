//! The `run` command: wires the proxy engine ([`hamsy_proxy`]) and the
//! REST/WebSocket API + web UI server ([`hamsy_api`]) together into one
//! running hamsy-proxy instance, sharing a single [`ProxyContext`] between them.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Args;
use parking_lot::RwLock;
use tokio::net::TcpListener;

use hamsy_core::{FlowStore, RulesStore, Settings};
use hamsy_proxy::upstream::Connector;
use hamsy_proxy::{CertAuthority, ProxyContext, ProxyServer};

use crate::hooks::{RealCertHook, RealReplayHook};
use crate::resolve_data_dir;
use crate::shutdown;

/// Options for `hamsy run` (and the bare `hamsy` invocation, which is
/// equivalent to `hamsy run` with whatever flags were given at the top
/// level).
#[derive(Args, Debug, Clone, Default)]
pub struct RunArgs {
    /// Override the proxy listener port.
    #[arg(short = 'p', long = "proxy-port")]
    pub proxy_port: Option<u16>,
    /// Override the web UI/API listener port.
    #[arg(short = 'u', long = "ui-port")]
    pub ui_port: Option<u16>,
    /// Override the address both servers bind to.
    #[arg(short = 'b', long = "bind")]
    pub bind: Option<String>,
    /// Override the hamsy-proxy data directory (default: `$HAMSY_HOME` or `~/.hamsy`).
    #[arg(long = "data-dir")]
    pub data_dir: Option<PathBuf>,
    /// Force-enable the OS system proxy on start, restoring its prior configuration on
    /// shutdown. This is now the default behaviour, so passing this flag is redundant
    /// except to override a persisted `manualProxy: true` setting; kept for backward
    /// compatibility with existing scripts/muscle memory.
    #[arg(long = "system-proxy")]
    pub system_proxy: bool,
    /// Don't touch the OS system proxy; configure your client to use it manually.
    #[arg(long = "manual", alias = "no-system-proxy")]
    pub manual: bool,
    /// Don't open a browser tab once the servers are listening.
    #[arg(long = "no-open")]
    pub no_open: bool,
    /// Start with capture paused.
    #[arg(long)]
    pub paused: bool,
    /// Disable HTTPS/TLS interception (blind-tunnel HTTPS instead of MITM'ing it).
    #[arg(long = "no-https")]
    pub no_https: bool,
    /// Internal background startup mode used by MCP.
    #[arg(long, hide = true)]
    pub agent_managed: bool,
}

/// RAII guard: while alive, this process may have enabled the OS system
/// proxy via `sysproxy_state::acquire`. Its `Drop` restores from the
/// marker on *any* exit path this process takes, including an early `?`
/// return out of `run()` after the proxy was enabled and a panic
/// unwind. The shutdown sequence below also calls `restore()`
/// explicitly and immediately on the first signal (before draining);
/// the `AtomicBool` makes both call sites idempotent -- whichever runs
/// first does the real work, the other is a no-op.
struct SystemProxyGuard {
    data_dir: PathBuf,
    restored: std::sync::atomic::AtomicBool,
    owned_only: bool,
}

impl SystemProxyGuard {
    fn new(data_dir: PathBuf, owned_only: bool) -> Self {
        SystemProxyGuard {
            data_dir,
            owned_only,
            restored: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Restores via `spawn_blocking` (the underlying OS commands are
    /// synchronous `std::process::Command` calls) so this doesn't stall
    /// the async runtime while the drain is trying to make progress.
    /// Returns `None` if this guard already restored (via this method
    /// or `Drop`) -- callers use that to skip printing anything.
    async fn restore(&self) -> Option<Result<bool, String>> {
        use std::sync::atomic::Ordering;
        if self.restored.swap(true, Ordering::SeqCst) {
            return None;
        }
        let data_dir = self.data_dir.clone();
        let owned_only = self.owned_only;
        match tokio::task::spawn_blocking(move || {
            if owned_only {
                hamsy_api::sysproxy_state::restore_owned_if_marked(&data_dir)
            } else {
                hamsy_api::sysproxy_state::restore_if_marked(&data_dir)
            }
        })
        .await
        {
            Ok(result) => Some(result),
            Err(join_err) => Some(Err(format!(
                "system-proxy restore task panicked: {join_err}"
            ))),
        }
    }
}

impl Drop for SystemProxyGuard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        // Fallback for the early-return-via-`?`/panic-unwind path, where
        // there's no async context to `spawn_blocking` from. A no-op on
        // the normal shutdown path, since `restore()` above already ran.
        if self.restored.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(err) = if self.owned_only {
            hamsy_api::sysproxy_state::restore_owned_if_marked(&self.data_dir)
        } else {
            hamsy_api::sysproxy_state::restore_if_marked(&self.data_dir)
        } {
            tracing::warn!(%err, "failed to restore the system proxy while unwinding");
        }
    }
}

/// Runs the proxy and API/UI servers until interrupted (`Ctrl-C`/`SIGTERM`),
/// per `args`.
pub async fn run(args: RunArgs) -> Result<()> {
    println!("hamsi proxy runlanıyor");
    let data_dir = resolve_data_dir(args.data_dir.as_deref());
    let control = if args.agent_managed {
        Some(crate::mcp::startup::Control::from_env()?)
    } else {
        None
    };
    let _profile_lock = if args.agent_managed {
        Some(crate::mcp::startup::claim_profile(&data_dir)?)
    } else {
        None
    };
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data dir {}", data_dir.display()))?;

    // A previous hamsy-proxy process may have died (crash/hard kill) without
    // running its own shutdown path, leaving the OS system proxy pointed
    // at a now-dead instance. This is the earliest point any code can
    // notice and fix that -- see `sysproxy_state`'s module doc.
    if !args.agent_managed {
        hamsy_api::sysproxy_state::recover_stale(&data_dir);
    }

    // Registered as early as possible so signal handlers are live for as
    // much of this process's lifetime as practical.
    let shutdown_signal_task = tokio::spawn(shutdown::wait_for_shutdown_signal());
    let system_proxy_guard = SystemProxyGuard::new(data_dir.clone(), args.agent_managed);

    let mut settings = Settings::load(&data_dir.join("settings.json"));
    if let Some(port) = args.proxy_port {
        settings.proxy_port = port;
    }
    if let Some(port) = args.ui_port {
        settings.ui_port = port;
    }
    if let Some(bind) = args.bind.clone() {
        settings.bind_addr = bind;
    }
    if args.paused {
        settings.paused = true;
    }
    if args.no_https {
        settings.intercept_https = false;
    }
    if args.agent_managed {
        anyhow::ensure!(
            settings
                .bind_addr
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()),
            "MCP startup requires a loopback bind address"
        );
        settings.manual_proxy = true;
    }

    let bind_addr = settings.bind_addr.clone();
    let proxy_port = settings.proxy_port;
    let ui_port = settings.ui_port;
    let manual_proxy_setting = settings.manual_proxy;
    let max_flows = settings.max_flows;
    let system_proxy_bypass = settings.system_proxy_bypass.clone();

    // Bind both listeners before printing anything, so a port conflict is
    // reported cleanly before any partial startup state is visible.
    let proxy_listener = TcpListener::bind((bind_addr.as_str(), proxy_port))
        .await
        .with_context(|| {
            format!(
                "port {proxy_port} is already in use — pass --proxy-port to use a different one"
            )
        })?;
    let ui_listener = TcpListener::bind((bind_addr.as_str(), ui_port))
        .await
        .with_context(|| {
            format!("port {ui_port} is already in use — pass --ui-port to use a different one")
        })?;

    if !args.agent_managed {
        settings
            .save(&data_dir.join("settings.json"))
            .context("failed to save settings")?;
    }

    // Build the shared handles once, mirroring
    // `hamsy-proxy/tests/common/mod.rs::spawn_proxy_trusting`.
    let settings = Arc::new(RwLock::new(settings));
    let flows = Arc::new(FlowStore::new(max_flows));
    let rules = Arc::new(RulesStore::load(&data_dir.join("rules.json")));
    let (events, _rx) = tokio::sync::broadcast::channel(4096);
    let ca = Arc::new(
        CertAuthority::load_or_generate(&data_dir).context("failed to load or generate the CA")?,
    );
    let upstream = Arc::new(Connector::new().context("failed to build the upstream connector")?);

    let ctx = ProxyContext {
        settings: settings.clone(),
        rules: rules.clone(),
        flows: flows.clone(),
        events: events.clone(),
        ca: ca.clone(),
        upstream,
    };

    let replay_hook = Arc::new(RealReplayHook::new(ctx.clone()));
    let cert_hook = Arc::new(RealCertHook::new(ca.clone()));
    let api_state = hamsy_api::ApiState::new(
        flows.clone(),
        rules.clone(),
        settings.clone(),
        data_dir.join("settings.json"),
        events.clone(),
        replay_hook,
        cert_hook,
        env!("CARGO_PKG_VERSION"),
    );

    if let Some(control) = &control {
        let address = std::net::SocketAddr::new(bind_addr.parse()?, ui_port);
        control.publish(&data_dir, format!("http://{address}/"))?;
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let proxy_task = tokio::spawn({
        let shutdown = shutdown_future(shutdown_rx.clone());
        async move {
            if let Err(err) = ProxyServer::new(ctx).serve(proxy_listener, shutdown).await {
                tracing::error!(%err, "proxy server exited with an error");
            }
        }
    });

    let api_control = control.clone();
    let api_task = tokio::spawn({
        let shutdown = shutdown_future(shutdown_rx.clone());
        async move {
            let mut router = hamsy_api::router(api_state);
            if let Some(control) = api_control {
                router = router.merge(control.routes());
            }
            if let Err(err) = axum::serve(ui_listener, router)
                .with_graceful_shutdown(shutdown)
                .await
            {
                tracing::error!(%err, "api server exited with an error");
            }
        }
    });

    let lan_ip = hamsy_api::lan_addresses().into_iter().next();
    println!(
        "{}",
        format_banner(
            env!("CARGO_PKG_VERSION"),
            proxy_port,
            ui_port,
            lan_ip.as_deref(),
            &ca.fingerprint_sha256()
        )
    );

    if !args.no_open && !args.agent_managed {
        let url = format!("http://127.0.0.1:{ui_port}");
        if let Err(err) = open::that(&url) {
            tracing::warn!(%err, url, "failed to open the browser");
        }
    }

    // System proxy is on by default. `--manual` (or a persisted
    // `manualProxy: true`) opts out; `--system-proxy` forces it on
    // regardless (e.g. to override a persisted opt-out for one run).
    let system_proxy_requested = if args.manual || args.agent_managed {
        false
    } else if args.system_proxy {
        true
    } else {
        !manual_proxy_setting
    };
    if system_proxy_requested {
        match hamsy_api::sysproxy_state::acquire(
            &data_dir,
            "127.0.0.1",
            proxy_port,
            &system_proxy_bypass,
        ) {
            Ok(()) => {
                tracing::info!("enabled the OS system proxy");
                println!("  System proxy enabled (127.0.0.1:{proxy_port}).");
            }
            Err(err) => {
                tracing::warn!(%err, "failed to enable the OS system proxy");
                eprintln!(
                    "\n  Warning: could not set the OS system proxy automatically.\n    {err}\n    Configure your client manually: HTTP/HTTPS proxy 127.0.0.1:{proxy_port}\n    (macOS needs an admin account; Linux needs GNOME/gsettings.)\n"
                );
            }
        }
    }

    let signal = tokio::select! {
        signal = shutdown_signal_task => signal.expect("shutdown-signal watcher task panicked").to_string(),
        _ = async { if let Some(control) = control { control.stopped.notified().await; } else { std::future::pending::<()>().await; } } => "MCP stop request".to_string(),
    };
    println!("\n  Shutting down…");
    tracing::info!(%signal, "received shutdown signal, restoring system proxy and draining connections");

    match system_proxy_guard.restore().await {
        Some(Ok(true)) => println!("  System proxy restored."),
        Some(Ok(false)) => {} // this run never enabled it; nothing to restore
        Some(Err(err)) => {
            tracing::warn!(%err, "failed to restore the OS system proxy on shutdown");
            println!("  Warning: failed to restore the system proxy automatically ({err}); run `hamsy proxy off` to fix it manually.");
        }
        None => {} // already restored (shouldn't happen at this call site, it's the first call)
    }

    let _ = shutdown_tx.send(true);

    tokio::select! {
        _ = async { let _ = tokio::join!(proxy_task, api_task); } => {
            println!("  Stopped.");
        }
        second = shutdown::wait_for_shutdown_signal() => {
            // The proxy was already restored above, before draining
            // started, so it's safe to skip the drain entirely here and
            // exit immediately -- the failure mode that restore exists to
            // prevent (the machine routed through a dead proxy) has
            // already been avoided regardless of what happens to the
            // in-flight connections now.
            tracing::warn!(%second, "received a second shutdown signal; exiting immediately without draining");
            eprintln!("  Second signal received, exiting immediately.");
            std::process::exit(130);
        }
    }

    tracing::info!("hamsy stopped, goodbye");
    Ok(())
}

/// Resolves once `rx`'s value flips to `true`, suitable for
/// [`ProxyServer::serve`] and `axum::serve(..).with_graceful_shutdown`.
async fn shutdown_future(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|ready| *ready).await;
}

/// Formats the compact startup banner printed to stdout once both listeners
/// are bound.
fn format_banner(
    version: &str,
    proxy_port: u16,
    ui_port: u16,
    lan_ip: Option<&str>,
    fingerprint: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("\n  hamsy {version}\n\n"));
    out.push_str(&format!("  Proxy      http://127.0.0.1:{proxy_port}\n"));
    out.push_str(&format!("  Web UI     http://127.0.0.1:{ui_port}\n"));
    let display_ip = lan_ip.unwrap_or("127.0.0.1");
    out.push_str(&format!(
        "  CA cert    http://{display_ip}:{ui_port}/cert/hamsy-ca.crt\n"
    ));
    out.push_str(&format!("  SHA-256    {fingerprint}\n\n"));
    match lan_ip {
        Some(ip) => {
            out.push_str(&format!(
                "  Devices on your network: point their proxy at {ip}:{proxy_port}\n"
            ));
        }
        None => {
            out.push_str(
                "  No LAN address detected; other devices may not be able to reach this proxy.\n",
            );
        }
    }
    out.push_str("  Press Ctrl-C to stop.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_includes_lan_ip_when_present() {
        let banner = format_banner("0.1.0", 9080, 9081, Some("192.168.1.42"), "AB:CD");
        assert!(banner.contains("hamsy 0.1.0"));
        assert!(banner.contains("http://127.0.0.1:9080"));
        assert!(banner.contains("http://192.168.1.42:9081/cert/hamsy-ca.crt"));
        assert!(banner.contains("192.168.1.42:9080"));
        assert!(banner.contains("AB:CD"));
    }

    #[test]
    fn banner_falls_back_without_lan_ip() {
        let banner = format_banner("0.1.0", 9080, 9081, None, "AB:CD");
        assert!(banner.contains("No LAN address detected"));
        assert!(banner.contains("http://127.0.0.1:9081/cert/hamsy-ca.crt"));
        assert!(!banner.contains("point their proxy"));
    }
}
