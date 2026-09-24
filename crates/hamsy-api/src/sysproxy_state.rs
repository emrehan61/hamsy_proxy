//! Persisted marker recording that *hamsy-proxy* (via any of its three
//! enable-the-system-proxy call sites: `hamsy run` startup, the web
//! UI's `POST /api/system-proxy`, or `hamsy proxy on`) is the reason the
//! OS system proxy currently points at this machine's hamsy-proxy instance --
//! plus the `sysproxy::Snapshot` needed to put back whatever was
//! configured immediately before that happened.
//!
//! The marker's mere existence on disk is the ownership signal: as long
//! as it's there, *something* -- a still-running `hamsy run`, or a
//! crashed one -- currently has the system proxy pointed at hamsy-proxy and
//! it needs to be undone. This replaces an in-memory "did *I* enable
//! this" flag, which missed both cross-process ownership (started via the
//! web UI or another shell's `hamsy proxy on`) and hard kills (nothing
//! in-process survives a SIGKILL to react to).
//!
//! If hamsy-proxy is killed hard enough that no code in this process ever
//! runs again (SIGKILL, a segfault, a power cut), this file on disk is
//! the *only* possible recovery mechanism: there is no signal handler for
//! SIGKILL, so the next `hamsy` invocation that starts is the earliest
//! point anything can notice and fix it -- see `recover_stale`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sysproxy::{self, Snapshot};

const MARKER_FILE: &str = "sysproxy-state.json";

/// On-disk contents of the marker file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerState {
    /// The OS proxy configuration exactly as it was immediately before
    /// hamsy-proxy enabled it -- what gets put back.
    pub snapshot: Snapshot,
    /// PID of the process that wrote this marker, for the log line
    /// printed when a stale one is found on startup.
    pub pid: u32,
    pub host: String,
    pub port: u16,
}

fn marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MARKER_FILE)
}

fn write_marker(data_dir: &Path, marker: &MarkerState) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(marker).map_err(|e| e.to_string())?;
    std::fs::write(marker_path(data_dir), json).map_err(|e| e.to_string())
}

fn read_marker(path: &Path) -> Option<MarkerState> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Reads and deletes the marker in `data_dir`, if present.
fn take_marker(data_dir: &Path) -> Option<MarkerState> {
    let path = marker_path(data_dir);
    let marker = read_marker(&path)?;
    let _ = std::fs::remove_file(&path);
    Some(marker)
}

/// Enables the OS system proxy and records a marker so a later `release`
/// (from any process) knows what to put back. Best-effort: a snapshot
/// failure doesn't abort enabling, it just means restore-on-exit can only
/// turn the proxy off rather than reapply a prior custom configuration
/// (mirrors `sysproxy::enable`'s existing "never abort startup over
/// system-proxy bookkeeping" contract).
pub fn acquire(data_dir: &Path, host: &str, port: u16, bypass: &[String]) -> Result<(), String> {
    // If a previous hamsy run left a marker behind (crash/kill without
    // cleanup), restore *that* first. Otherwise the snapshot we're about
    // to take would capture the dead run's leftover proxy settings as if
    // *they* were the user's original configuration, permanently losing
    // the real one trapped in the stale marker.
    recover_stale(data_dir);

    let snapshot = sysproxy::snapshot().unwrap_or_else(|err| {
        tracing::warn!(%err, "could not snapshot the prior system proxy configuration; restoring on exit will only be able to turn the proxy off");
        Snapshot::Unknown
    });

    sysproxy::enable(host, port, bypass)?;

    let marker = MarkerState {
        snapshot,
        pid: std::process::id(),
        host: host.to_string(),
        port,
    };
    if let Err(err) = write_marker(data_dir, &marker) {
        tracing::warn!(%err, "failed to persist the system-proxy marker file; a hard kill of this process won't be able to auto-restore on next start");
    }
    Ok(())
}

/// Restores from the marker if present, deleting it either way; falls
/// back to a plain `sysproxy::disable()` if there's no marker. Use this
/// when the caller's intent is "make sure the system proxy ends up
/// off/restored, no matter who turned it on" -- `hamsy proxy off` and
/// the web UI's explicit toggle-off both want this.
pub fn release(data_dir: &Path) -> Result<(), String> {
    match take_marker(data_dir) {
        Some(marker) => sysproxy::restore(&marker.snapshot),
        None => sysproxy::disable(),
    }
}

/// Restores from the marker *only if one exists*, deleting it; does
/// nothing (`Ok(false)`) otherwise. Use this for automatic
/// restore-on-shutdown/panic, where a plain `hamsy run` that never
/// enabled the system proxy must never disable a proxy it had nothing to
/// do with. Returns `Ok(true)` iff a marker was found and successfully
/// restored.
pub fn restore_if_marked(data_dir: &Path) -> Result<bool, String> {
    match take_marker(data_dir) {
        Some(marker) => sysproxy::restore(&marker.snapshot).map(|()| true),
        None => Ok(false),
    }
}

/// An MCP-started process must not restore another instance's recovery marker.
/// It may still restore changes explicitly made through its own web UI.
pub fn restore_owned_if_marked(data_dir: &Path) -> Result<bool, String> {
    if read_marker(&marker_path(data_dir)).is_some_and(|m| m.pid == std::process::id()) {
        restore_if_marked(data_dir)
    } else {
        Ok(false)
    }
}

/// Called once at startup, before anything else touches the system
/// proxy. If a marker is present, a previous hamsy-proxy process died without
/// running its own shutdown path. This is the only mitigation possible
/// for that case -- there is no signal handler for SIGKILL -- so the best
/// available fix is noticing the leftover marker the next time *any*
/// hamsy-proxy process starts.
pub fn recover_stale(data_dir: &Path) {
    let Some(marker) = take_marker(data_dir) else {
        return;
    };
    tracing::warn!(
        pid = marker.pid,
        host = %marker.host,
        port = marker.port,
        "restored system proxy settings left behind by a previous hamsy run (pid {})",
        marker.pid
    );
    if let Err(err) = sysproxy::restore(&marker.snapshot) {
        tracing::warn!(%err, "failed to restore system proxy settings left behind by a previous hamsy run");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh, unique temp dir for one test, matching
    /// `hamsy-api/tests/common/mod.rs`'s `temp_path` pattern.
    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "hamsy-sysproxy-state-test-{}",
            uuid::Uuid::new_v4()
        ))
    }

    /// Only the pure marker-file I/O is tested here -- never `acquire`/
    /// `release`/`recover_stale`/anything in `sysproxy`, since those shell
    /// out to real OS commands that would mutate whatever machine runs
    /// the test suite.
    #[test]
    fn marker_roundtrips_and_take_deletes_it() {
        let dir = temp_dir();
        let marker = MarkerState {
            snapshot: Snapshot::Unknown,
            pid: 4242,
            host: "127.0.0.1".to_string(),
            port: 9080,
        };

        write_marker(&dir, &marker).unwrap();
        let path = marker_path(&dir);
        assert!(path.exists());

        let read_back = read_marker(&path).expect("marker should still be readable");
        assert_eq!(read_back.snapshot, marker.snapshot);
        assert_eq!(read_back.pid, marker.pid);
        assert_eq!(read_back.host, marker.host);
        assert_eq!(read_back.port, marker.port);

        let taken = take_marker(&dir).expect("marker should be present to take");
        assert_eq!(taken.pid, marker.pid);
        assert!(!path.exists(), "take_marker should delete the file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_marker_missing_file_returns_none() {
        let dir = temp_dir();
        assert!(read_marker(&marker_path(&dir)).is_none());
    }
}
