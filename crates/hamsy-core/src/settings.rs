//! Global proxy settings, persisted as JSON in the hamsy-proxy data directory.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::error::Result;

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
    /// Maximum total bytes (`requestSize + responseSize` summed across all
    /// stored flows) retained in the in-memory store, enforced alongside
    /// `max_flows` -- whichever bound is hit first evicts the oldest flow.
    /// Guards against a small number of huge bodies (well under
    /// `max_flows`) still ballooning memory use.
    pub max_total_bytes: u64,
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
            max_total_bytes: 512 * 1024 * 1024,
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
        with_compiled_globs(self, |globs| {
            if globs.exclude.is_match(host) {
                return false;
            }
            self.capture_include_hosts.is_empty() || globs.include.is_match(host)
        })
    }

    /// Returns true if `host` matches any passthrough glob (i.e. should
    /// never be MITM'd).
    pub fn host_passthrough(&self, host: &str) -> bool {
        with_compiled_globs(self, |globs| globs.passthrough.is_match(host))
    }
}

/// Precompiled `GlobSet`s for the three host-glob lists on [`Settings`],
/// plus the source pattern lists they were built from (so a later call can
/// detect staleness cheaply -- comparing a couple of short string vectors is
/// far cheaper than recompiling glob syntax).
struct CompiledHostGlobs {
    include_src: Vec<String>,
    exclude_src: Vec<String>,
    passthrough_src: Vec<String>,
    include: GlobSet,
    exclude: GlobSet,
    passthrough: GlobSet,
}

impl CompiledHostGlobs {
    fn compile(settings: &Settings) -> Self {
        CompiledHostGlobs {
            include_src: settings.capture_include_hosts.clone(),
            exclude_src: settings.capture_exclude_hosts.clone(),
            passthrough_src: settings.passthrough_hosts.clone(),
            include: build_glob_set(&settings.capture_include_hosts),
            exclude: build_glob_set(&settings.capture_exclude_hosts),
            passthrough: build_glob_set(&settings.passthrough_hosts),
        }
    }

    /// True if this compile still matches `settings`' current pattern lists.
    fn is_fresh(&self, settings: &Settings) -> bool {
        self.include_src == settings.capture_include_hosts
            && self.exclude_src == settings.capture_exclude_hosts
            && self.passthrough_src == settings.passthrough_hosts
    }
}

/// Builds a `GlobSet` matching any of `patterns` (empty/all-invalid patterns
/// yield a `GlobSet` that matches nothing), mirroring `glob_list_matches`'
/// old per-call semantics: an individual pattern that fails to parse is
/// silently skipped rather than aborting the whole set.
fn build_glob_set(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    // Only fails if the compiled pattern set exceeds an internal regex size
    // limit; falling back to "matches nothing" is safer than panicking on
    // the hot path over a pathological (if unlikely) settings value.
    builder.build().unwrap_or_else(|_| {
        GlobSetBuilder::new()
            .build()
            .expect("an empty GlobSetBuilder always builds successfully")
    })
}

thread_local! {
    /// Per-thread cache of the last compiled [`CompiledHostGlobs`].
    ///
    /// Deliberately *not* a field on [`Settings`]: that would need interior
    /// mutability (these are called from `&self` methods on the hot path)
    /// behind a lock shared across every request, and `Settings` is a plain
    /// data struct many tests construct with a full struct literal (`..
    /// Settings::default()`), which can't reach a private field cross-crate.
    /// A thread-local sidesteps both: each of Tokio's worker threads ends up
    /// with its own compiled copy (a handful of small `GlobSet`s -- cheap),
    /// rebuilt only when `is_fresh` notices the source pattern lists
    /// changed, with no locking at all on the common case.
    static HOST_GLOB_CACHE: RefCell<Option<CompiledHostGlobs>> = const { RefCell::new(None) };
}

/// Runs `f` against the calling thread's cached [`CompiledHostGlobs`] for
/// `settings`, recompiling first if the cache is empty or stale.
fn with_compiled_globs<R>(settings: &Settings, f: impl FnOnce(&CompiledHostGlobs) -> R) -> R {
    HOST_GLOB_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.as_ref().is_some_and(|c| c.is_fresh(settings)) {
            *cache = Some(CompiledHostGlobs::compile(settings));
        }
        f(cache.as_ref().expect("just populated above"))
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
        assert_eq!(s.max_total_bytes, 512 * 1024 * 1024);
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
    fn load_settings_missing_max_total_bytes_defaults() {
        // Simulates a pre-existing `~/.hamsy/settings.json` written before
        // `maxTotalBytes` existed: the field is absent, and `#[serde(default)]`
        // must fill it in rather than failing to parse.
        let path = std::env::temp_dir().join(format!(
            "hamsy-test-legacy-{}.json",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, r#"{"proxyPort":9080,"uiPort":9081}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.max_total_bytes, Settings::default().max_total_bytes);
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
