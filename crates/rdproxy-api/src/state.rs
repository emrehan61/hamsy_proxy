//! Shared, cheaply-cloneable server state handed to every route handler.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;
use rdproxy_core::{FlowStore, RulesStore, ServerEvent, Settings};
use tokio::sync::broadcast;

use crate::hooks::{CertHook, NoopReplay, ReplayHook, StubCert};

/// Capacity of the broadcast channel used to fan out [`ServerEvent`]s to
/// connected WebSocket clients. Slow subscribers fall behind rather than
/// blocking producers; see [`tokio::sync::broadcast`]'s lagged-receiver
/// semantics, handled in `routes::ws`.
const EVENT_CHANNEL_CAPACITY: usize = 4096;

struct Inner {
    flows: Arc<FlowStore>,
    rules: Arc<RulesStore>,
    settings: Arc<RwLock<Settings>>,
    settings_path: PathBuf,
    events: broadcast::Sender<ServerEvent>,
    replay_hook: Arc<dyn ReplayHook>,
    cert_hook: Arc<dyn CertHook>,
    version: String,
    started_at: Instant,
}

/// Shared server state: flow store, rules store, live settings, the
/// server-sent-event broadcast channel, and the pluggable replay/cert
/// hooks. Cheap to clone (an `Arc` wrapper around a single inner struct).
#[derive(Clone)]
pub struct ApiState {
    inner: Arc<Inner>,
}

impl ApiState {
    /// Builds a new [`ApiState`].
    ///
    /// `settings` and `events` are shared handles: pass the same
    /// `Arc<RwLock<Settings>>` and `broadcast::Sender<ServerEvent>` used by a
    /// `rdproxy_proxy::ProxyContext` so settings changes and captured flows
    /// flow between the proxy backend and this API in both directions.
    ///
    /// `settings_path` is where [`Settings`] are persisted on every mutating
    /// `PUT /api/settings` call and `ClientCommand::Pause`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        flows: Arc<FlowStore>,
        rules: Arc<RulesStore>,
        settings: Arc<RwLock<Settings>>,
        settings_path: PathBuf,
        events: broadcast::Sender<ServerEvent>,
        replay_hook: Arc<dyn ReplayHook>,
        cert_hook: Arc<dyn CertHook>,
        version: impl Into<String>,
    ) -> Self {
        flows.set_capacity(settings.read().max_flows.max(1));
        ApiState {
            inner: Arc::new(Inner {
                flows,
                rules,
                settings,
                settings_path,
                events,
                replay_hook,
                cert_hook,
                version: version.into(),
                started_at: Instant::now(),
            }),
        }
    }

    /// Convenience constructor using [`NoopReplay`] and [`StubCert`] as the
    /// hooks, for running the API standalone (no proxy backend attached).
    /// Builds its own settings lock and broadcast channel, since there is no
    /// proxy backend to share them with.
    pub fn new_standalone(
        flows: Arc<FlowStore>,
        rules: Arc<RulesStore>,
        settings: Settings,
        settings_path: PathBuf,
        version: impl Into<String>,
    ) -> Self {
        let settings = Arc::new(RwLock::new(settings));
        let (events, _rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self::new(
            flows,
            rules,
            settings,
            settings_path,
            events,
            Arc::new(NoopReplay),
            Arc::new(StubCert),
            version,
        )
    }

    /// The flow store.
    pub fn flows(&self) -> &FlowStore {
        &self.inner.flows
    }

    /// The rules store.
    pub fn rules(&self) -> &RulesStore {
        &self.inner.rules
    }

    /// Returns a clone of the current settings.
    pub fn settings(&self) -> Settings {
        self.inner.settings.read().clone()
    }

    /// The path settings are persisted to.
    pub fn settings_path(&self) -> &Path {
        &self.inner.settings_path
    }

    /// The rdproxy data directory: the settings file's parent directory
    /// (`settings.json` always lives directly inside it). Derived rather than
    /// stored separately, so nothing new needs threading through every
    /// `ApiState::new`/`new_standalone` call site just for this.
    pub fn data_dir(&self) -> &Path {
        self.inner
            .settings_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    /// Replaces the current settings in memory, persists them to disk, and
    /// applies `maxFlows` to the flow store's capacity. Does not broadcast;
    /// callers are responsible for broadcasting [`ServerEvent::SettingsChanged`]
    /// when appropriate.
    pub fn save_settings(&self, settings: Settings) -> rdproxy_core::Result<()> {
        settings.save(&self.inner.settings_path)?;
        self.inner.flows.set_capacity(settings.max_flows.max(1));
        *self.inner.settings.write() = settings;
        Ok(())
    }

    /// The configured [`ReplayHook`].
    pub fn replay_hook(&self) -> &Arc<dyn ReplayHook> {
        &self.inner.replay_hook
    }

    /// The configured [`CertHook`].
    pub fn cert_hook(&self) -> &Arc<dyn CertHook> {
        &self.inner.cert_hook
    }

    /// The `rdproxy-api` crate version reported in `GET /api/state`.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// Seconds elapsed since this [`ApiState`] was constructed.
    pub fn uptime_secs(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Broadcasts `event` to all currently-subscribed WebSocket clients.
    /// A send failure (no subscribers) is silently ignored.
    pub fn broadcast(&self, event: ServerEvent) {
        let _ = self.inner.events.send(event);
    }

    /// Subscribes to the server event broadcast channel.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.inner.events.subscribe()
    }

    /// Returns a clone of the shared broadcast sender, so a proxy backend
    /// can be wired to publish into the same event stream this API serves
    /// over WebSocket.
    pub fn event_sender(&self) -> broadcast::Sender<ServerEvent> {
        self.inner.events.clone()
    }

    /// Returns the shared settings handle, so a proxy backend can observe
    /// settings changes made through this API in real time.
    pub fn settings_handle(&self) -> Arc<RwLock<Settings>> {
        self.inner.settings.clone()
    }
}
