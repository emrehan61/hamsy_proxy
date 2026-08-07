//! Shared, cloneable proxy state handed to every connection/request task.

use std::net::IpAddr;
use std::sync::Arc;

use hamsy_core::{RuleSet, RulesStore, ServerEvent, Settings};
use parking_lot::RwLock;
use tokio::sync::broadcast;

use crate::ca::CertAuthority;
use crate::upstream::Connector;

/// Shared handles threaded through the whole proxy pipeline.
///
/// Cheap to clone (everything inside is an `Arc` or a `Sender`), so each
/// accepted connection/request gets its own owned copy rather than sharing a
/// reference across task boundaries.
///
/// Note: `upstream` is not listed in the original spec's sketch of this
/// struct, but the outbound connector/pool needs a home shared across every
/// request, so it's added here alongside the other shared handles.
#[derive(Clone)]
pub struct ProxyContext {
    /// Live, mutable settings (bind address, capture filters, body caps...).
    pub settings: Arc<RwLock<Settings>>,
    /// Persisted rules plus a cached compiled [`RuleSet`].
    pub rules: Arc<RulesStore>,
    /// In-memory ring buffer of captured flows.
    pub flows: Arc<hamsy_core::FlowStore>,
    /// Broadcast channel for pushing live updates to API/UI subscribers.
    pub events: broadcast::Sender<ServerEvent>,
    /// Certificate authority used to mint per-host MITM leaf certificates.
    pub ca: Arc<CertAuthority>,
    /// Outbound connector and connection pool.
    pub upstream: Arc<Connector>,
}

impl ProxyContext {
    /// Returns true if capture is currently paused (no flows are recorded
    /// and no rules are applied, but traffic still flows transparently).
    pub fn is_paused(&self) -> bool {
        self.settings.read().paused
    }

    /// Returns the currently configured max body capture size, in bytes.
    pub fn max_body_bytes(&self) -> usize {
        self.settings.read().max_body_bytes
    }

    /// Returns true if `host` should be captured (a [`Flow`](hamsy_core::Flow)
    /// created and rules applied), given the current include/exclude globs
    /// and pause state.
    pub fn should_capture(&self, host: &str) -> bool {
        let settings = self.settings.read();
        !settings.paused && settings.host_captured(host)
    }

    /// Returns true if `host` should be MITM'd (TLS-intercepted) rather than
    /// blind-tunneled, given `intercept_https` and the passthrough glob list.
    pub fn should_intercept(&self, host: &str) -> bool {
        let settings = self.settings.read();
        settings.intercept_https && !settings.host_passthrough(host)
    }

    /// Returns the currently compiled, cached [`RuleSet`].
    pub fn ruleset(&self) -> Arc<RuleSet> {
        self.rules.ruleset()
    }

    /// Classifies whether a request targeting `(host, port)` is aimed at one
    /// of hamsy's own listeners.
    ///
    /// Historically this never mattered: loopback was always excluded from
    /// the OS system-proxy bypass list, so traffic bound for hamsy's own
    /// ports never actually arrived here. Now that
    /// [`Settings::system_proxy_bypass`] can be cleared, it can - so every
    /// call site that forwards a request needs to know whether it's about
    /// to hand hamsy a request pointed at itself, and if so, which of the
    /// two very different situations that is (see [`OwnListener`]'s doc).
    ///
    /// This is the single place that comparison is made; callers in
    /// `http.rs`, `connect.rs`, and `websocket.rs` all go through this
    /// method rather than re-deriving the address comparison themselves.
    pub fn own_listener(&self, host: &str, port: u16) -> OwnListener {
        let settings = self.settings.read();
        classify_own_listener(
            host,
            port,
            &settings.bind_addr,
            settings.proxy_port,
            settings.ui_port,
        )
    }
}

/// Which of hamsy's own listeners (if any) a request's `(host, port)`
/// target refers to, as returned by [`ProxyContext::own_listener`].
///
/// The two non-[`None`](OwnListener::None) cases get *opposite* treatment
/// from every call site, which is why they're distinguished here rather
/// than collapsed into a single boolean:
///
/// - [`ProxyPort`](OwnListener::ProxyPort): forwarding here would make the
///   proxy dial itself, and the dialed request would in turn be accepted,
///   handled, and dialed again by this same code path - unbounded
///   recursion that exhausts sockets/file descriptors. Callers must refuse
///   the request outright (see `http::self_loop_response`) rather than
///   forward it.
/// - [`UiPort`](OwnListener::UiPort): forwarding here is completely
///   legitimate - it's how a browser tab open on hamsy's own UI, or the
///   UI's `/api/ws` flow-event WebSocket, actually gets its response - and
///   must go through untouched. But it must never be *captured* as a flow:
///   capturing broadcasts a `ServerEvent` over `/api/ws`, which (once that
///   very connection is itself a captured WS flow) is recorded as a
///   `ws_messages` entry on that flow, which mutates the flow and
///   broadcasts another event - a self-amplifying feedback loop that spins
///   both the proxy and the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnListener {
    /// Not one of hamsy's own listeners; handle the request normally.
    None,
    /// hamsy's own MITM proxy listener (`Settings::proxy_port`).
    ProxyPort,
    /// hamsy's own UI/API listener (`Settings::ui_port`), including the
    /// flow-event WebSocket at `/api/ws`.
    UiPort,
}

/// Pure classification logic behind [`ProxyContext::own_listener`], taking
/// the individual settings values directly (rather than a whole
/// `ProxyContext`, which needs a `RwLock` read and - for other fields - a
/// CA/connector/etc. to even construct) so it's trivially unit-testable.
fn classify_own_listener(
    host: &str,
    port: u16,
    bind_addr: &str,
    proxy_port: u16,
    ui_port: u16,
) -> OwnListener {
    if !is_local_host(host, bind_addr) {
        return OwnListener::None;
    }
    if port == proxy_port {
        OwnListener::ProxyPort
    } else if port == ui_port {
        OwnListener::UiPort
    } else {
        OwnListener::None
    }
}

/// True if `host` refers to this machine's own network stack - matched
/// generously, since a client can end up dialing a local listener under any
/// of several spellings depending on how it was configured:
///
/// - the literal `"localhost"`;
/// - a loopback or unspecified ("any") IP address: `127.0.0.0/8`, `::1`
///   (loopback), `0.0.0.0`, `::` (unspecified/"any", what hamsy itself
///   binds to by default);
/// - `bind_addr` itself - the address hamsy's own listeners are actually
///   bound to, which may be a specific LAN IP rather than `0.0.0.0` in a
///   custom setup, so a peer on the LAN dialing that exact address is
///   dialing hamsy just as surely as `127.0.0.1` would be locally.
///
/// Brackets around a literal IPv6 host (as produced by
/// [`url::Url::host_str`] and accepted in a `CONNECT` authority) are
/// stripped before parsing.
fn is_local_host(host: &str, bind_addr: &str) -> bool {
    let host = strip_brackets(host);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if !bind_addr.is_empty() && host.eq_ignore_ascii_case(strip_brackets(bind_addr)) {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_unspecified(),
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || v6.is_unspecified(),
        Err(_) => false,
    }
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIND: &str = "0.0.0.0";
    const PROXY_PORT: u16 = 19080;
    const UI_PORT: u16 = 19081;

    fn classify(host: &str, port: u16) -> OwnListener {
        classify_own_listener(host, port, BIND, PROXY_PORT, UI_PORT)
    }

    #[test]
    fn loopback_zero_and_localhost_forms_on_proxy_port_match() {
        for host in [
            "127.0.0.1",
            "127.5.5.5", // still within 127.0.0.0/8
            "::1",
            "0.0.0.0",
            "localhost",
            "LOCALHOST",
        ] {
            assert_eq!(
                classify(host, PROXY_PORT),
                OwnListener::ProxyPort,
                "host {host} on the proxy port should match"
            );
        }
    }

    #[test]
    fn bind_addr_form_on_proxy_port_matches() {
        assert_eq!(classify(BIND, PROXY_PORT), OwnListener::ProxyPort);
        // A custom (non-0.0.0.0) bind_addr must also be recognized as local.
        assert_eq!(
            classify_own_listener(
                "192.168.1.5",
                PROXY_PORT,
                "192.168.1.5",
                PROXY_PORT,
                UI_PORT
            ),
            OwnListener::ProxyPort
        );
    }

    #[test]
    fn same_hosts_on_ui_port_classify_as_ui_listener() {
        for host in ["127.0.0.1", "::1", "0.0.0.0", "localhost", BIND] {
            assert_eq!(
                classify(host, UI_PORT),
                OwnListener::UiPort,
                "host {host} on the UI port should classify as UiPort"
            );
        }
    }

    #[test]
    fn unrelated_host_on_a_matching_port_number_does_not_match() {
        assert_eq!(classify("example.com", PROXY_PORT), OwnListener::None);
        assert_eq!(classify("example.com", UI_PORT), OwnListener::None);
        // A LAN IP that doesn't equal `bind_addr` isn't "local" just because
        // it happens to look like a private address.
        assert_eq!(classify("192.168.1.5", PROXY_PORT), OwnListener::None);
    }

    #[test]
    fn local_host_on_an_unrelated_port_does_not_match() {
        assert_eq!(classify("127.0.0.1", 9999), OwnListener::None);
        assert_eq!(classify("localhost", 443), OwnListener::None);
    }

    #[test]
    fn bracketed_ipv6_host_is_recognized_as_local() {
        assert_eq!(classify("[::1]", PROXY_PORT), OwnListener::ProxyPort);
        assert_eq!(classify("[::1]", UI_PORT), OwnListener::UiPort);
    }
}
