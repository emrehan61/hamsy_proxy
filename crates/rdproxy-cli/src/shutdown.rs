//! Waits for whichever OS signal/event means "stop now", reporting which
//! one fired. Every signal that can plausibly reach rdproxy while it's
//! holding the system proxy pointed at itself is caught here, not just
//! Ctrl-C -- anything uncaught leaves the machine unable to reach the
//! network until a human notices and fixes the OS proxy settings by
//! hand. `SIGKILL` (and losing power) is the one thing genuinely
//! impossible to catch from userspace; that case is instead handled by
//! the on-disk marker file recovered the *next* time `rdproxy` starts
//! (see `rdproxy_api::sysproxy_state::recover_stale`).

use std::fmt;

/// Which signal/event triggered shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    CtrlC,
    #[cfg(unix)]
    Sigterm,
    #[cfg(unix)]
    Sighup,
    #[cfg(unix)]
    Sigquit,
    #[cfg(windows)]
    CtrlBreak,
    #[cfg(windows)]
    CtrlClose,
    #[cfg(windows)]
    CtrlLogoff,
    #[cfg(windows)]
    CtrlShutdown,
}

impl fmt::Display for ShutdownSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ShutdownSignal::CtrlC => "Ctrl-C",
            #[cfg(unix)]
            ShutdownSignal::Sigterm => "SIGTERM",
            #[cfg(unix)]
            ShutdownSignal::Sighup => "SIGHUP",
            #[cfg(unix)]
            ShutdownSignal::Sigquit => "SIGQUIT",
            #[cfg(windows)]
            ShutdownSignal::CtrlBreak => "Ctrl-Break",
            #[cfg(windows)]
            ShutdownSignal::CtrlClose => "console close",
            #[cfg(windows)]
            ShutdownSignal::CtrlLogoff => "logoff",
            #[cfg(windows)]
            ShutdownSignal::CtrlShutdown => "system shutdown",
        })
    }
}

/// Resolves once any recognized shutdown signal fires. Each
/// platform-specific handler is installed independently; if installing
/// one fails, that one falls back to `pending()` (never resolves) after
/// logging a warning, rather than taking the others down with it -- e.g.
/// a failed SIGHUP handler still leaves Ctrl-C/SIGTERM working.
pub async fn wait_for_shutdown_signal() -> ShutdownSignal {
    #[cfg(unix)]
    {
        wait_unix().await
    }
    #[cfg(windows)]
    {
        wait_windows().await
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::future::pending::<ShutdownSignal>().await
    }
}

#[cfg(unix)]
async fn wait_for_one_signal(
    kind: tokio::signal::unix::SignalKind,
    name: &'static str,
    which: ShutdownSignal,
) -> ShutdownSignal {
    match tokio::signal::unix::signal(kind) {
        Ok(mut sig) => {
            sig.recv().await;
            which
        }
        Err(err) => {
            tracing::warn!(%err, name, "failed to install signal handler");
            std::future::pending::<ShutdownSignal>().await
        }
    }
}

#[cfg(unix)]
async fn wait_unix() -> ShutdownSignal {
    use tokio::signal::unix::SignalKind;
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
        ShutdownSignal::CtrlC
    };
    tokio::select! {
        sig = ctrl_c => sig,
        sig = wait_for_one_signal(SignalKind::terminate(), "SIGTERM", ShutdownSignal::Sigterm) => sig,
        sig = wait_for_one_signal(SignalKind::hangup(), "SIGHUP", ShutdownSignal::Sighup) => sig,
        sig = wait_for_one_signal(SignalKind::quit(), "SIGQUIT", ShutdownSignal::Sigquit) => sig,
    }
}

#[cfg(windows)]
async fn wait_windows() -> ShutdownSignal {
    let ctrl_c = async {
        match tokio::signal::windows::ctrl_c() {
            Ok(mut sig) => {
                sig.recv().await;
                ShutdownSignal::CtrlC
            }
            Err(err) => {
                tracing::warn!(%err, "failed to install Ctrl-C handler");
                std::future::pending::<ShutdownSignal>().await
            }
        }
    };
    let ctrl_break = async {
        match tokio::signal::windows::ctrl_break() {
            Ok(mut sig) => {
                sig.recv().await;
                ShutdownSignal::CtrlBreak
            }
            Err(err) => {
                tracing::warn!(%err, "failed to install Ctrl-Break handler");
                std::future::pending::<ShutdownSignal>().await
            }
        }
    };
    let ctrl_close = async {
        match tokio::signal::windows::ctrl_close() {
            Ok(mut sig) => {
                sig.recv().await;
                ShutdownSignal::CtrlClose
            }
            Err(err) => {
                tracing::warn!(%err, "failed to install console-close handler");
                std::future::pending::<ShutdownSignal>().await
            }
        }
    };
    let ctrl_logoff = async {
        match tokio::signal::windows::ctrl_logoff() {
            Ok(mut sig) => {
                sig.recv().await;
                ShutdownSignal::CtrlLogoff
            }
            Err(err) => {
                tracing::warn!(%err, "failed to install logoff handler");
                std::future::pending::<ShutdownSignal>().await
            }
        }
    };
    let ctrl_shutdown = async {
        match tokio::signal::windows::ctrl_shutdown() {
            Ok(mut sig) => {
                sig.recv().await;
                ShutdownSignal::CtrlShutdown
            }
            Err(err) => {
                tracing::warn!(%err, "failed to install system-shutdown handler");
                std::future::pending::<ShutdownSignal>().await
            }
        }
    };
    tokio::select! {
        sig = ctrl_c => sig,
        sig = ctrl_break => sig,
        sig = ctrl_close => sig,
        sig = ctrl_logoff => sig,
        sig = ctrl_shutdown => sig,
    }
}
