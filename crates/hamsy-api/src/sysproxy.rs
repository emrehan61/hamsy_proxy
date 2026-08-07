//! Cross-platform control of the operating system's HTTP/HTTPS proxy
//! settings, so hamsy-proxy can point the whole machine at itself.
//!
//! Every public function here is infallible in the panic sense: failures
//! are always reported as `Err(String)`, never a panic.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Returns a short identifier for the current OS: `"macos"`, `"windows"`,
/// `"linux"`, or `"unknown"`.
pub fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// Returns true if this platform has a system-proxy control implementation.
pub fn supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ))
}

/// Returns whether *some* system HTTP proxy currently appears to be
/// enabled, regardless of who set it up or what it points at.
///
/// This does NOT mean "hamsy-proxy's proxy is on" -- another application
/// (or the user, by hand) can hold the OS proxy just as well. Use
/// `status_for` when the question is specifically "is the OS proxy
/// pointed at *this* hamsy instance"; that's almost always the more
/// useful question for anything user-facing. This generic form exists for
/// `hamsy proxy status`, which intentionally reports on the OS-wide
/// setting.
///
/// Errors (rather than `Ok(false)`) are returned when the platform is
/// unsupported or the underlying command fails to run at all, so callers
/// can distinguish "definitely off" from "couldn't check".
pub fn status() -> Result<bool, String> {
    imp::status()
}

/// Returns whether the system HTTP proxy is currently enabled *and*
/// pointed at `host:port` -- i.e. whether the OS proxy is this hamsy
/// instance's doing, not merely "some proxy is on". `host`/`port` should
/// be exactly what this instance would pass to `enable` (always
/// `"127.0.0.1"` and `settings.proxy_port` at every call site today).
///
/// Same error semantics as `status()`: `Err` means "couldn't check", not
/// "not ours".
pub fn status_for(host: &str, port: u16) -> Result<bool, String> {
    imp::status_for(host, port)
}

/// Whether `actual` (a hostname/address reported back by the OS) refers
/// to the same host as `expected` (what this hamsy instance configured,
/// always a loopback address in practice). Only tolerant of the common
/// `127.0.0.1` <-> `localhost` spelling difference; anything else must
/// match exactly (case-insensitively).
fn host_matches(expected: &str, actual: &str) -> bool {
    fn normalize(h: &str) -> std::borrow::Cow<'_, str> {
        if h.eq_ignore_ascii_case("127.0.0.1") || h.eq_ignore_ascii_case("localhost") {
            std::borrow::Cow::Borrowed("127.0.0.1")
        } else {
            std::borrow::Cow::Borrowed(h)
        }
    }
    normalize(expected).eq_ignore_ascii_case(&normalize(actual))
}

/// Points the OS system HTTP/HTTPS proxy at `host:port`, bypassing `bypass`
/// (a list of host patterns that should connect directly).
pub fn enable(host: &str, port: u16, bypass: &[String]) -> Result<(), String> {
    imp::enable(host, port, bypass)
}

/// Disables the OS system HTTP/HTTPS proxy.
pub fn disable() -> Result<(), String> {
    imp::disable()
}

/// Captures the OS system-proxy configuration as it currently is, so it
/// can be put back later via `restore`. Called immediately before
/// `enable`, never after.
pub fn snapshot() -> Result<Snapshot, String> {
    imp::snapshot()
}

/// Restores a previously-captured `Snapshot`. `Snapshot::Unknown` falls
/// back to `disable()`.
pub fn restore(snapshot: &Snapshot) -> Result<(), String> {
    match snapshot {
        Snapshot::Unknown => disable(),
        _ => imp::restore(snapshot),
    }
}

/// A captured OS system-proxy configuration, platform-specific in shape,
/// that can later be handed back to `restore` to put things back exactly
/// as they were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "camelCase")]
pub enum Snapshot {
    Macos {
        services: Vec<MacosServiceSnapshot>,
    },
    Windows {
        proxy_enable: Option<String>,
        proxy_server: Option<String>,
        proxy_override: Option<String>,
    },
    Linux {
        mode: Option<String>,
        http_host: Option<String>,
        http_port: Option<String>,
        https_host: Option<String>,
        https_port: Option<String>,
        ignore_hosts: Option<String>,
    },
    /// A snapshot could not be taken (e.g. `networksetup`/`reg`/`gsettings`
    /// failed). `restore` falls back to a plain `disable()` for this
    /// variant -- the best we can do without knowing what was there
    /// before.
    Unknown,
}

/// Per-network-service proxy configuration on macOS, as reported by
/// `networksetup -getwebproxy`/`-getsecurewebproxy`/`-getproxybypassdomains`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosServiceSnapshot {
    pub service: String,
    pub web_enabled: bool,
    pub web_server: Option<String>,
    pub web_port: Option<u16>,
    pub secure_enabled: bool,
    pub secure_server: Option<String>,
    pub secure_port: Option<u16>,
    pub bypass_domains: Vec<String>,
}

/// Runs `cmd` with `args`, returning its stdout as a `String` on success (a
/// non-zero exit status is treated as failure, mirroring `Command::status`
/// semantics but capturing output for diagnostics).
fn run(cmd: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run `{cmd}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`{cmd} {}` failed: {stderr}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::collections::HashMap;

    use super::run;
    use crate::sysproxy::{MacosServiceSnapshot, Snapshot};

    fn network_services() -> Result<Vec<String>, String> {
        let out = run("networksetup", &["-listallnetworkservices"])?;
        Ok(out
            .lines()
            .skip(1) // First line is an informational header, not a service.
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('*')) // '*' = disabled service.
            .map(str::to_string)
            .collect())
    }

    pub fn status() -> Result<bool, String> {
        let services = network_services()?;
        let Some(first) = services.first() else {
            return Ok(false);
        };
        let out = run("networksetup", &["-getwebproxy", first])?;
        Ok(out
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("Enabled: Yes")))
    }

    /// Only checks the first active network service (same simplification
    /// `status`/`enable`/`disable` already make in this file) -- a proxy
    /// that's ours on one service and not another is not a case this
    /// reports separately.
    pub fn status_for(host: &str, port: u16) -> Result<bool, String> {
        let services = network_services()?;
        let Some(first) = services.first() else {
            return Ok(false);
        };
        let kv = parse_kv(&run("networksetup", &["-getwebproxy", first])?);
        Ok(matches_expected(&kv, host, port))
    }

    /// True iff `kv` (parsed `-getwebproxy` output) shows the proxy
    /// enabled and pointed at `host:port`.
    fn matches_expected(kv: &HashMap<String, String>, host: &str, port: u16) -> bool {
        let enabled = kv
            .get("Enabled")
            .is_some_and(|v| v.eq_ignore_ascii_case("yes"));
        if !enabled {
            return false;
        }
        let server_matches = kv
            .get("Server")
            .is_some_and(|s| super::host_matches(host, s));
        let port_matches = kv.get("Port").and_then(|p| p.parse::<u16>().ok()) == Some(port);
        server_matches && port_matches
    }

    pub fn enable(host: &str, port: u16, bypass: &[String]) -> Result<(), String> {
        let services = network_services()?;
        if services.is_empty() {
            return Err("no active network services found".to_string());
        }
        let port_str = port.to_string();
        for svc in &services {
            run(
                "networksetup",
                &["-setwebproxy", svc, host, &port_str, "off"],
            )?;
            run(
                "networksetup",
                &["-setsecurewebproxy", svc, host, &port_str, "off"],
            )?;
            let args = bypass_domains_args(svc, bypass);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run("networksetup", &arg_refs)?;
            run("networksetup", &["-setwebproxystate", svc, "on"])?;
            run("networksetup", &["-setsecurewebproxystate", svc, "on"])?;
        }
        Ok(())
    }

    /// Builds the argv for `networksetup -setproxybypassdomains` that
    /// applies `bypass` to `service`. Always produces a call that actively
    /// sets the OS-level list -- including an empty `bypass`, which is
    /// encoded as the literal `"Empty"` keyword `networksetup` documents
    /// for clearing bypass-domain entries (`man networksetup`: "Specify
    /// \"Empty\" for domain1 to clear all Domain Name entries."). This
    /// must never be skipped for an empty list: skipping leaves whatever
    /// bypass domains the OS already had (e.g. from a prior `hamsy run`),
    /// so clearing `systemProxyBypass` in settings would silently do
    /// nothing.
    fn bypass_domains_args(service: &str, bypass: &[String]) -> Vec<String> {
        let mut args = vec!["-setproxybypassdomains".to_string(), service.to_string()];
        if bypass.is_empty() {
            args.push("Empty".to_string());
        } else {
            args.extend(bypass.iter().cloned());
        }
        args
    }

    pub fn disable() -> Result<(), String> {
        let services = network_services()?;
        for svc in &services {
            run("networksetup", &["-setwebproxystate", svc, "off"])?;
            run("networksetup", &["-setsecurewebproxystate", svc, "off"])?;
        }
        Ok(())
    }

    /// Parses a `networksetup -getwebproxy`/`-getsecurewebproxy`-style
    /// output block (`"Key: Value"` lines) into a lookup map, splitting
    /// each line on the *first* `:` so values that themselves contain a
    /// colon (unlikely here, but cheap to get right) aren't truncated.
    fn parse_kv(out: &str) -> HashMap<String, String> {
        out.lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(':')?;
                Some((key.trim().to_string(), value.trim().to_string()))
            })
            .collect()
    }

    /// Parses `networksetup -getproxybypassdomains` output: one domain per
    /// line, or a "There aren't any ..." message when none are set.
    fn parse_bypass_domains(out: &str) -> Vec<String> {
        if out.trim_start().starts_with("There aren't any") {
            return vec![];
        }
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn snapshot() -> Result<Snapshot, String> {
        let services = network_services()?;
        let mut snapshots = Vec::with_capacity(services.len());
        for svc in &services {
            let web = parse_kv(&run("networksetup", &["-getwebproxy", svc])?);
            let secure = parse_kv(&run("networksetup", &["-getsecurewebproxy", svc])?);
            let bypass =
                parse_bypass_domains(&run("networksetup", &["-getproxybypassdomains", svc])?);

            let web_enabled = web
                .get("Enabled")
                .is_some_and(|v| v.eq_ignore_ascii_case("yes"));
            let secure_enabled = secure
                .get("Enabled")
                .is_some_and(|v| v.eq_ignore_ascii_case("yes"));
            let web_server = web.get("Server").filter(|v| !v.is_empty()).cloned();
            let secure_server = secure.get("Server").filter(|v| !v.is_empty()).cloned();
            let web_port = web.get("Port").and_then(|v| v.parse::<u16>().ok());
            let secure_port = secure.get("Port").and_then(|v| v.parse::<u16>().ok());

            snapshots.push(MacosServiceSnapshot {
                service: svc.clone(),
                web_enabled,
                web_server,
                web_port,
                secure_enabled,
                secure_server,
                secure_port,
                bypass_domains: bypass,
            });
        }
        Ok(Snapshot::Macos {
            services: snapshots,
        })
    }

    pub fn restore(snapshot: &Snapshot) -> Result<(), String> {
        let Snapshot::Macos { services } = snapshot else {
            return Err("snapshot is not a macOS snapshot".to_string());
        };
        for svc in services {
            let args = bypass_domains_args(&svc.service, &svc.bypass_domains);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run("networksetup", &arg_refs)?;

            if svc.web_enabled {
                if let (Some(server), Some(port)) = (&svc.web_server, svc.web_port) {
                    let port_str = port.to_string();
                    run(
                        "networksetup",
                        &["-setwebproxy", &svc.service, server, &port_str, "off"],
                    )?;
                    run("networksetup", &["-setwebproxystate", &svc.service, "on"])?;
                } else {
                    run("networksetup", &["-setwebproxystate", &svc.service, "off"])?;
                }
            } else {
                run("networksetup", &["-setwebproxystate", &svc.service, "off"])?;
            }

            if svc.secure_enabled {
                if let (Some(server), Some(port)) = (&svc.secure_server, svc.secure_port) {
                    let port_str = port.to_string();
                    run(
                        "networksetup",
                        &["-setsecurewebproxy", &svc.service, server, &port_str, "off"],
                    )?;
                    run(
                        "networksetup",
                        &["-setsecurewebproxystate", &svc.service, "on"],
                    )?;
                } else {
                    run(
                        "networksetup",
                        &["-setsecurewebproxystate", &svc.service, "off"],
                    )?;
                }
            } else {
                run(
                    "networksetup",
                    &["-setsecurewebproxystate", &svc.service, "off"],
                )?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_kv_reads_realistic_block() {
            let block =
                "Enabled: Yes\nServer: 127.0.0.1\nPort: 9080\nAuthenticated Proxy Enabled: 0\n";
            let kv = parse_kv(block);
            assert_eq!(kv.get("Enabled").map(String::as_str), Some("Yes"));
            assert_eq!(kv.get("Server").map(String::as_str), Some("127.0.0.1"));
            assert_eq!(kv.get("Port").map(String::as_str), Some("9080"));
            assert_eq!(
                kv.get("Authenticated Proxy Enabled").map(String::as_str),
                Some("0")
            );
        }

        #[test]
        fn parse_bypass_domains_reads_multiline_list() {
            let domains = parse_bypass_domains("localhost\n127.0.0.1\n*.local\n");
            assert_eq!(domains, vec!["localhost", "127.0.0.1", "*.local"]);
        }

        #[test]
        fn parse_bypass_domains_empty_state_returns_empty_vec() {
            let domains = parse_bypass_domains("There aren't any bypass domains set.\n\n");
            assert!(domains.is_empty());
        }

        #[test]
        fn bypass_domains_args_empty_bypass_uses_empty_keyword_not_skipped() {
            // This is the argv that must be produced -- and must always be
            // produced, never skipped -- for an empty `systemProxyBypass`,
            // so clearing the setting actually clears the OS bypass list.
            let args = bypass_domains_args("Wi-Fi", &[]);
            assert_eq!(args, vec!["-setproxybypassdomains", "Wi-Fi", "Empty"]);
        }

        #[test]
        fn bypass_domains_args_nonempty_bypass_lists_each_domain() {
            let args =
                bypass_domains_args("Wi-Fi", &["localhost".to_string(), "127.0.0.1".to_string()]);
            assert_eq!(
                args,
                vec!["-setproxybypassdomains", "Wi-Fi", "localhost", "127.0.0.1"]
            );
        }

        #[test]
        fn matches_expected_true_when_enabled_and_host_port_match() {
            let kv = parse_kv("Enabled: Yes\nServer: 127.0.0.1\nPort: 9080\n");
            assert!(matches_expected(&kv, "127.0.0.1", 9080));
        }

        #[test]
        fn matches_expected_false_when_port_differs() {
            let kv = parse_kv("Enabled: Yes\nServer: 127.0.0.1\nPort: 8281\n");
            assert!(!matches_expected(&kv, "127.0.0.1", 9080));
        }

        #[test]
        fn matches_expected_false_when_host_differs() {
            let kv = parse_kv("Enabled: Yes\nServer: 10.0.0.5\nPort: 9080\n");
            assert!(!matches_expected(&kv, "127.0.0.1", 9080));
        }

        #[test]
        fn matches_expected_false_when_disabled() {
            // Enabled: No, even with a matching host/port -- e.g. hamsy
            // configured the proxy and then it was toggled off elsewhere.
            let kv = parse_kv("Enabled: No\nServer: 127.0.0.1\nPort: 9080\n");
            assert!(!matches_expected(&kv, "127.0.0.1", 9080));
        }

        #[test]
        fn matches_expected_true_for_loopback_spelling_equivalence() {
            let kv = parse_kv("Enabled: Yes\nServer: localhost\nPort: 9080\n");
            assert!(matches_expected(&kv, "127.0.0.1", 9080));
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::run;
    use crate::sysproxy::Snapshot;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    pub fn status() -> Result<bool, String> {
        let out = run("reg", &["query", KEY, "/v", "ProxyEnable"])?;
        Ok(out.contains("0x1"))
    }

    pub fn status_for(host: &str, port: u16) -> Result<bool, String> {
        let enabled = run("reg", &["query", KEY, "/v", "ProxyEnable"])?.contains("0x1");
        let server = query_value("ProxyServer")?;
        Ok(matches_expected(enabled, server.as_deref(), host, port))
    }

    /// True iff `enabled` and `server` (a Windows `ProxyServer` registry
    /// value) points at `host:port`. `enable` here only ever writes the
    /// plain `"host:port"` form (applying to all protocols); Windows can
    /// also hold a per-protocol form like
    /// `"http=127.0.0.1:9080;https=127.0.0.1:9080"` (e.g. set by some
    /// other tool), which this does not attempt to parse -- it falls
    /// through to the conservative "not ours" answer instead.
    fn matches_expected(enabled: bool, server: Option<&str>, host: &str, port: u16) -> bool {
        if !enabled {
            return false;
        }
        let Some(server) = server else {
            return false;
        };
        let Some((s_host, s_port)) = server.rsplit_once(':') else {
            return false;
        };
        s_port.parse::<u16>().ok() == Some(port) && super::host_matches(host, s_host)
    }

    pub fn enable(host: &str, port: u16, bypass: &[String]) -> Result<(), String> {
        let server = format!("{host}:{port}");
        run(
            "reg",
            &[
                "add",
                KEY,
                "/v",
                "ProxyEnable",
                "/t",
                "REG_DWORD",
                "/d",
                "1",
                "/f",
            ],
        )?;
        run(
            "reg",
            &[
                "add",
                KEY,
                "/v",
                "ProxyServer",
                "/t",
                "REG_SZ",
                "/d",
                &server,
                "/f",
            ],
        )?;
        let override_list = bypass.join(";");
        let args = proxy_override_args(&override_list);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run("reg", &arg_refs)?;
        // NOTE: this does not broadcast `INTERNET_OPTION_SETTINGS_CHANGED` /
        // `INTERNET_OPTION_REFRESH` via `InternetSetOptionW`, since doing so
        // would require a `winapi`/`windows` crate dependency we're avoiding
        // here. Some already-running applications (notably older IE/Edge
        // WinINet-based ones) may not notice the change until restarted.
        // This is an accepted limitation.
        Ok(())
    }

    /// Builds the argv for `reg add ... /v ProxyOverride ...` that writes
    /// `override_list` (a `;`-joined bypass list, possibly empty). This is
    /// always run unconditionally -- never skipped for an empty
    /// `override_list` -- because an empty `ProxyOverride` value is
    /// Windows' normal, correct way of expressing "no bypass exceptions"
    /// at the WinINet layer; skipping the write would leave whatever
    /// override list was already in the registry (e.g. from a prior
    /// `hamsy run`) instead of clearing it.
    fn proxy_override_args(override_list: &str) -> Vec<String> {
        vec![
            "add".to_string(),
            KEY.to_string(),
            "/v".to_string(),
            "ProxyOverride".to_string(),
            "/t".to_string(),
            "REG_SZ".to_string(),
            "/d".to_string(),
            override_list.to_string(),
            "/f".to_string(),
        ]
    }

    pub fn disable() -> Result<(), String> {
        run(
            "reg",
            &[
                "add",
                KEY,
                "/v",
                "ProxyEnable",
                "/t",
                "REG_DWORD",
                "/d",
                "0",
                "/f",
            ],
        )?;
        Ok(())
    }

    /// Parses `reg query`'s output for a single value: finds the line whose
    /// trimmed start is the value's `name`, and returns its last
    /// whitespace-separated token (the value itself, e.g. `"0x1"` or a
    /// path-like string) -- the line looks like
    /// `"    ProxyEnable    REG_DWORD    0x1"`.
    fn parse_reg_value(out: &str, name: &str) -> Option<String> {
        out.lines()
            .map(str::trim)
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().last())
            .map(str::to_string)
    }

    /// Reads a single registry value under `KEY`. `Ok(None)` means the
    /// value (or the key) doesn't exist -- `reg query` exits non-zero for
    /// that, which `run` reports as `Err`, but it isn't a real failure the
    /// way "failed to even spawn `reg`" is.
    fn query_value(name: &str) -> Result<Option<String>, String> {
        match run("reg", &["query", KEY, "/v", name]) {
            Ok(out) => Ok(parse_reg_value(&out, name)),
            Err(err) if err.starts_with("failed to run") => Err(err),
            Err(_) => Ok(None),
        }
    }

    pub fn snapshot() -> Result<Snapshot, String> {
        Ok(Snapshot::Windows {
            proxy_enable: query_value("ProxyEnable")?,
            proxy_server: query_value("ProxyServer")?,
            proxy_override: query_value("ProxyOverride")?,
        })
    }

    pub fn restore(snapshot: &Snapshot) -> Result<(), String> {
        let Snapshot::Windows {
            proxy_enable,
            proxy_server,
            proxy_override,
        } = snapshot
        else {
            return Err("snapshot is not a Windows snapshot".to_string());
        };

        match proxy_enable {
            Some(v) => run(
                "reg",
                &[
                    "add",
                    KEY,
                    "/v",
                    "ProxyEnable",
                    "/t",
                    "REG_DWORD",
                    "/d",
                    v,
                    "/f",
                ],
            )
            .map(|_| ())?,
            None => {
                let _ = run("reg", &["delete", KEY, "/v", "ProxyEnable", "/f"]);
            }
        }
        match proxy_server {
            Some(v) => run(
                "reg",
                &[
                    "add",
                    KEY,
                    "/v",
                    "ProxyServer",
                    "/t",
                    "REG_SZ",
                    "/d",
                    v,
                    "/f",
                ],
            )
            .map(|_| ())?,
            None => {
                let _ = run("reg", &["delete", KEY, "/v", "ProxyServer", "/f"]);
            }
        }
        match proxy_override {
            Some(v) => run(
                "reg",
                &[
                    "add",
                    KEY,
                    "/v",
                    "ProxyOverride",
                    "/t",
                    "REG_SZ",
                    "/d",
                    v,
                    "/f",
                ],
            )
            .map(|_| ())?,
            None => {
                let _ = run("reg", &["delete", KEY, "/v", "ProxyOverride", "/f"]);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_reg_value_reads_realistic_output() {
            let out = "HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings\n    ProxyEnable    REG_DWORD    0x1\n";
            assert_eq!(parse_reg_value(out, "ProxyEnable"), Some("0x1".to_string()));
        }

        #[test]
        fn proxy_override_args_empty_bypass_writes_empty_string_not_skipped() {
            // An empty bypass list must still produce a write (of an empty
            // value), never be omitted -- omitting it would leave a stale
            // ProxyOverride value from a prior run in place.
            let args = proxy_override_args("");
            assert_eq!(
                args,
                vec![
                    "add",
                    KEY,
                    "/v",
                    "ProxyOverride",
                    "/t",
                    "REG_SZ",
                    "/d",
                    "",
                    "/f",
                ]
            );
        }

        #[test]
        fn proxy_override_args_nonempty_bypass_joins_with_semicolons() {
            let args = proxy_override_args("localhost;127.0.0.1");
            let d_index = args.iter().position(|a| a == "/d").unwrap();
            assert_eq!(args[d_index + 1], "localhost;127.0.0.1");
        }

        #[test]
        fn matches_expected_true_when_enabled_and_host_port_match() {
            assert!(matches_expected(
                true,
                Some("127.0.0.1:9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_port_differs() {
            assert!(!matches_expected(
                true,
                Some("127.0.0.1:8281"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_host_differs() {
            assert!(!matches_expected(
                true,
                Some("10.0.0.5:9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_disabled() {
            assert!(!matches_expected(
                false,
                Some("127.0.0.1:9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_true_for_loopback_spelling_equivalence() {
            assert!(matches_expected(
                true,
                Some("localhost:9080"),
                "127.0.0.1",
                9080
            ));
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::run;
    use crate::sysproxy::Snapshot;

    pub fn status() -> Result<bool, String> {
        let out = run("gsettings", &["get", "org.gnome.system.proxy", "mode"])?;
        Ok(out.trim().trim_matches('\'') == "manual")
    }

    pub fn status_for(host: &str, port: u16) -> Result<bool, String> {
        let mode = run("gsettings", &["get", "org.gnome.system.proxy", "mode"])?;
        let enabled = mode.trim().trim_matches('\'') == "manual";
        let http_host = gsettings_get("org.gnome.system.proxy.http", "host");
        let http_port = gsettings_get("org.gnome.system.proxy.http", "port");
        Ok(matches_expected(
            enabled,
            http_host.as_deref(),
            http_port.as_deref(),
            host,
            port,
        ))
    }

    /// True iff `enabled` (GNOME proxy `mode` is `manual`) and the
    /// `org.gnome.system.proxy.http` `host`/`port` (raw gsettings text,
    /// e.g. `"'127.0.0.1'"` / `"9080"`) match `host`/`port`. Only the
    /// `http` sub-schema is checked, not `https` -- `enable` always
    /// writes the same host:port to both, so checking one is enough to
    /// detect what `enable` itself can produce; this mirrors the
    /// "check the first active service" simplification already used on
    /// macOS.
    fn matches_expected(
        enabled: bool,
        http_host: Option<&str>,
        http_port: Option<&str>,
        host: &str,
        port: u16,
    ) -> bool {
        if !enabled {
            return false;
        }
        let Some(http_host) = http_host else {
            return false;
        };
        let Some(http_port) = http_port else {
            return false;
        };
        let actual_host = http_host.trim().trim_matches('\'');
        let actual_port: Option<u16> = http_port.trim().trim_matches('\'').parse().ok();
        super::host_matches(host, actual_host) && actual_port == Some(port)
    }

    /// Formats `entries` as a gsettings/GVariant string-array literal, e.g.
    /// `['localhost', '127.0.0.1', '::1', '*.local']` -- the text form
    /// `gsettings set <schema> <key>` expects for an `as` (array-of-string)
    /// value such as `ignore-hosts`.
    fn gvariant_string_array(entries: &[String]) -> String {
        let quoted: Vec<String> = entries.iter().map(|e| format!("'{e}'")).collect();
        format!("[{}]", quoted.join(", "))
    }

    pub fn enable(host: &str, port: u16, bypass: &[String]) -> Result<(), String> {
        let port_str = port.to_string();
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "mode", "manual"],
        )
        .map_err(|_| {
            "gsettings not found; set http_proxy/https_proxy environment variables manually"
                .to_string()
        })?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.http", "host", host],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.http", "port", &port_str],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.https", "host", host],
        )?;
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy.https", "port", &port_str],
        )?;
        // Always set `ignore-hosts`, even for an empty `bypass` -- an
        // empty GVariant array (`[]`) actively clears the ignore list at
        // the gsettings/dconf level, so it must never be skipped. Skipping
        // it would leave whatever `ignore-hosts` the OS already had (e.g.
        // from a prior `hamsy run`), silently keeping localhost bypassed.
        let literal = gvariant_string_array(bypass);
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "ignore-hosts", &literal],
        )?;
        Ok(())
    }

    pub fn disable() -> Result<(), String> {
        run(
            "gsettings",
            &["set", "org.gnome.system.proxy", "mode", "none"],
        )
        .map_err(|_| {
            "gsettings not found; set http_proxy/https_proxy environment variables manually"
                .to_string()
        })?;
        Ok(())
    }

    /// Reads a single `gsettings` value, returning `None` on any failure
    /// (e.g. `gsettings` isn't installed) rather than propagating an
    /// error -- a best-effort snapshot with some fields missing is more
    /// useful than no snapshot at all.
    fn gsettings_get(schema: &str, key: &str) -> Option<String> {
        run("gsettings", &["get", schema, key])
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn snapshot() -> Result<Snapshot, String> {
        Ok(Snapshot::Linux {
            mode: gsettings_get("org.gnome.system.proxy", "mode"),
            http_host: gsettings_get("org.gnome.system.proxy.http", "host"),
            http_port: gsettings_get("org.gnome.system.proxy.http", "port"),
            https_host: gsettings_get("org.gnome.system.proxy.https", "host"),
            https_port: gsettings_get("org.gnome.system.proxy.https", "port"),
            ignore_hosts: gsettings_get("org.gnome.system.proxy", "ignore-hosts"),
        })
    }

    pub fn restore(snapshot: &Snapshot) -> Result<(), String> {
        let Snapshot::Linux {
            mode,
            http_host,
            http_port,
            https_host,
            https_port,
            ignore_hosts,
        } = snapshot
        else {
            return Err("snapshot is not a Linux snapshot".to_string());
        };

        // The stored value is gsettings' own GVariant text form (e.g.
        // `'manual'`), so passing it straight back to `gsettings set`
        // round-trips correctly. `None` fields are skipped silently --
        // nothing to restore for a value we never captured.
        if let Some(v) = mode {
            run("gsettings", &["set", "org.gnome.system.proxy", "mode", v])?;
        }
        if let Some(v) = http_host {
            run(
                "gsettings",
                &["set", "org.gnome.system.proxy.http", "host", v],
            )?;
        }
        if let Some(v) = http_port {
            run(
                "gsettings",
                &["set", "org.gnome.system.proxy.http", "port", v],
            )?;
        }
        if let Some(v) = https_host {
            run(
                "gsettings",
                &["set", "org.gnome.system.proxy.https", "host", v],
            )?;
        }
        if let Some(v) = https_port {
            run(
                "gsettings",
                &["set", "org.gnome.system.proxy.https", "port", v],
            )?;
        }
        if let Some(v) = ignore_hosts {
            run(
                "gsettings",
                &["set", "org.gnome.system.proxy", "ignore-hosts", v],
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn gvariant_string_array_formats_default_bypass_list() {
            let entries = vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
                "*.local".to_string(),
            ];
            assert_eq!(
                gvariant_string_array(&entries),
                "['localhost', '127.0.0.1', '::1', '*.local']"
            );
        }

        #[test]
        fn gvariant_string_array_formats_single_entry() {
            assert_eq!(
                gvariant_string_array(&["localhost".to_string()]),
                "['localhost']"
            );
        }

        #[test]
        fn gvariant_string_array_formats_empty_list() {
            assert_eq!(gvariant_string_array(&[]), "[]");
        }

        #[test]
        fn matches_expected_true_when_enabled_and_host_port_match() {
            assert!(matches_expected(
                true,
                Some("'127.0.0.1'"),
                Some("9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_port_differs() {
            assert!(!matches_expected(
                true,
                Some("'127.0.0.1'"),
                Some("8281"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_host_differs() {
            assert!(!matches_expected(
                true,
                Some("'10.0.0.5'"),
                Some("9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_false_when_disabled() {
            assert!(!matches_expected(
                false,
                Some("'127.0.0.1'"),
                Some("9080"),
                "127.0.0.1",
                9080
            ));
        }

        #[test]
        fn matches_expected_true_for_loopback_spelling_equivalence() {
            assert!(matches_expected(
                true,
                Some("'localhost'"),
                Some("9080"),
                "127.0.0.1",
                9080
            ));
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    use crate::sysproxy::Snapshot;

    pub fn status() -> Result<bool, String> {
        Err("system proxy control is not supported on this platform".to_string())
    }

    pub fn status_for(_host: &str, _port: u16) -> Result<bool, String> {
        Err("system proxy control is not supported on this platform".to_string())
    }

    pub fn enable(_host: &str, _port: u16, _bypass: &[String]) -> Result<(), String> {
        Err("system proxy control is not supported on this platform".to_string())
    }

    pub fn disable() -> Result<(), String> {
        Err("system proxy control is not supported on this platform".to_string())
    }

    pub fn snapshot() -> Result<Snapshot, String> {
        Err("system proxy control is not supported on this platform".to_string())
    }

    pub fn restore(_snapshot: &Snapshot) -> Result<(), String> {
        Err("system proxy control is not supported on this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_is_one_of_known_values() {
        assert!(["macos", "windows", "linux", "unknown"].contains(&platform()));
    }

    #[test]
    fn supported_matches_platform() {
        assert_eq!(supported(), platform() != "unknown");
    }

    #[test]
    fn snapshot_unknown_roundtrips_through_json() {
        let snapshot = Snapshot::Unknown;
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, back);
    }

    #[test]
    fn host_matches_exact_match() {
        assert!(host_matches("127.0.0.1", "127.0.0.1"));
    }

    #[test]
    fn host_matches_loopback_spelling_equivalence() {
        assert!(host_matches("127.0.0.1", "localhost"));
        assert!(host_matches("localhost", "127.0.0.1"));
    }

    #[test]
    fn host_matches_case_insensitive() {
        assert!(host_matches("127.0.0.1", "LOCALHOST"));
    }

    #[test]
    fn host_matches_false_for_different_host() {
        assert!(!host_matches("127.0.0.1", "192.168.1.5"));
    }

    #[test]
    fn snapshot_macos_roundtrips_through_json() {
        let snapshot = Snapshot::Macos {
            services: vec![MacosServiceSnapshot {
                service: "Wi-Fi".to_string(),
                web_enabled: true,
                web_server: Some("127.0.0.1".to_string()),
                web_port: Some(9080),
                secure_enabled: false,
                secure_server: None,
                secure_port: None,
                bypass_domains: vec!["localhost".to_string(), "*.local".to_string()],
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snapshot, back);
    }
}
