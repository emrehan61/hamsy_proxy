//! Shared, cloneable proxy state handed to every connection/request task.

use std::sync::Arc;

use flproxy_core::{RuleSet, RulesStore, ServerEvent, Settings};
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
    pub flows: Arc<flproxy_core::FlowStore>,
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

    /// Returns true if `host` should be captured (a [`Flow`](flproxy_core::Flow)
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
}
