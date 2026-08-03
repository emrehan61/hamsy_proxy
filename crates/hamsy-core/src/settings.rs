//! Global proxy settings, persisted as JSON in the hamsy-proxy data directory.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::rule::build_glob;

/// Global, persisted proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Port the MITM proxy listens on.
    pub proxy_port: u16,
    /// Port the UI/API server listens on.
    pub ui_port: u16,
    /// Address to bind both servers to (`"0.0.0.0"` so phones on the LAN
    /// can reach it).
    pub bind_addr: String,
    /// Maximum number of flows retained in the in-memory store.
    pub max_flows: usize,
    /// Maximum body size (bytes) captured before truncation.
    pub max_body_bytes: usize,
    /// Whether to MITM HTTPS traffic (vs. blind-tunnel it).
    pub intercept_https: bool,
    /// Host globs that are never MITM'd, even if `intercept_https` is true.
    pub passthrough_hosts: Vec<String>,
    /// If non-empty, only these host globs are captured.
    pub capture_include_hosts: Vec<String>,
    /// Host globs that are never captured.
    pub capture_exclude_hosts: Vec<String>,
    /// Whether to leave the OS system proxy alone (opt out of the default
    /// "point the whole machine at hamsy on startup" behaviour).
    ///
    /// This used to be `autoSystemProxy`, defaulting to `false` (system
    /// proxy off by default). Every existing `~/.hamsy/settings.json` on
    /// disk therefore has an explicit `"autoSystemProxy": false` in it,
    /// indistinguishable from a deliberate opt-out -- there's no way to
    /// tell "never set" apart from "user turned it off on purpose". Renaming
    /// the key to `manualProxy` sidesteps that: serde silently ignores the
    /// now-unknown old key (this struct is `#[serde(default)]`), so a
    /// missing `manualProxy` deserializes to `false`, meaning "system proxy
    /// on" -- flipping every existing install over to the new default-on
    /// behaviour cleanly, with no migration code needed.
    pub manual_proxy: bool,
    /// Whether to capture WebSocket frames.
    pub capture_websockets: bool,
    /// UI theme name.
    pub theme: String,
    /// Optional upstream proxy to chain through, e.g. `"http://host:port"`.
    pub upstream_proxy: Option<String>,
    /// Whether capture is currently paused.
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            proxy_port: 9080,
            ui_port: 9081,
            bind_addr: "0.0.0.0".to_string(),
            max_flows: 10_000,
            max_body_bytes: 5 * 1024 * 1024,
            intercept_https: true,
            passthrough_hosts: Vec::new(),
            capture_include_hosts: Vec::new(),
            capture_exclude_hosts: Vec::new(),
            manual_proxy: false,
            capture_websockets: true,
            theme: "dark".to_string(),
            upstream_proxy: None,
            paused: false,
        }
    }
}

impl Settings {
    /// Loads settings from `path`. Returns [`Settings::default`] if the
    /// file is missing or fails to parse.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Settings::default(),
        }
    }

    /// Atomically writes settings to `path` (write to a temp file, then
    /// rename over the destination).
    pub fn save(&self, path: &Path) -> Result<()> {
        atomic_write_json(path, self)
    }

    /// Returns true if `host` should be captured: not excluded, and either
    /// the include list is empty or `host` matches an entry in it.
    pub fn host_captured(&self, host: &str) -> bool {
        if glob_list_matches(&self.capture_exclude_hosts, host) {
            return false;
        }
        self.capture_include_hosts.is_empty()
            || glob_list_matches(&self.capture_include_hosts, host)
    }

    /// Returns true if `host` matches any passthrough glob (i.e. should
    /// never be MITM'd).
    pub fn host_passthrough(&self, host: &str) -> bool {
        glob_list_matches(&self.passthrough_hosts, host)
    }
}

fn glob_list_matches(patterns: &[String], host: &str) -> bool {
    patterns.iter().any(|pattern| match build_glob(pattern) {
        Ok(glob) => glob.is_match(host),
        Err(_) => false,
    })
}

/// Serializes `value` to `path` atomically: writes to a sibling temp file
/// then renames it into place, so readers never observe a partial write.
pub(crate) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, json)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Returns the hamsy-proxy data directory: `$HAMSY_HOME` if set, otherwise
/// `~/.hamsy` (using `$HOME` on Unix or `%USERPROFILE%` on Windows).
/// Creates the directory if it doesn't already exist.
pub fn data_dir() -> PathBuf {
    let dir = if let Ok(custom) = std::env::var("HAMSY_HOME") {
        PathBuf::from(custom)
    } else {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        Path::new(&home).join(".hamsy")
    };
    let _ = fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_match_spec() {
        let s = Settings::default();
        assert_eq!(s.proxy_port, 9080);
        assert_eq!(s.ui_port, 9081);
        assert_eq!(s.bind_addr, "0.0.0.0");
        assert_eq!(s.max_flows, 10_000);
        assert_eq!(s.max_body_bytes, 5 * 1024 * 1024);
        assert!(s.intercept_https);
        assert!(!s.manual_proxy);
        assert!(s.capture_websockets);
        assert_eq!(s.theme, "dark");
        assert!(s.upstream_proxy.is_none());
        assert!(!s.paused);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let path = std::env::temp_dir().join(format!(
            "hamsy-test-missing-{}.json",
            uuid::Uuid::new_v4()
        ));
        let s = Settings::load(&path);
        assert_eq!(s.proxy_port, Settings::default().proxy_port);
    }

    #[test]
    fn load_corrupt_file_returns_default() {
        let path = std::env::temp_dir().join(format!(
            "hamsy-test-corrupt-{}.json",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "not json").unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.proxy_port, Settings::default().proxy_port);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "hamsy-test-roundtrip-{}.json",
            uuid::Uuid::new_v4()
        ));
        let s = Settings {
            proxy_port: 12345,
            theme: "light".to_string(),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.proxy_port, 12345);
        assert_eq!(loaded.theme, "light");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn host_captured_respects_include_and_exclude() {
        let excluded_only = Settings {
            capture_exclude_hosts: vec!["*.ads.com".to_string()],
            ..Settings::default()
        };
        assert!(excluded_only.host_captured("example.com"));
        assert!(!excluded_only.host_captured("track.ads.com"));

        let with_include = Settings {
            capture_exclude_hosts: vec!["*.ads.com".to_string()],
            capture_include_hosts: vec!["*.api.com".to_string()],
            ..Settings::default()
        };
        assert!(!with_include.host_captured("example.com"));
        assert!(with_include.host_captured("v1.api.com"));
        assert!(!with_include.host_captured("track.ads.com"));
    }

    #[test]
    fn host_passthrough_matches_glob() {
        let s = Settings {
            passthrough_hosts: vec!["*.bank.com".to_string()],
            ..Settings::default()
        };
        assert!(s.host_passthrough("secure.bank.com"));
        assert!(!s.host_passthrough("example.com"));
    }
}
