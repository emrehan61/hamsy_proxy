//! Extension points that let a proxy backend (or certificate authority)
//! plug into the API without this crate depending on it directly.
//!
//! `flproxy-api` is intentionally self-contained: it defines these small
//! traits and ships no-op default implementations ([`NoopReplay`],
//! [`StubCert`]) so the crate builds and runs standalone. A real proxy
//! backend crate implements these traits and hands `Arc<dyn ReplayHook>` /
//! `Arc<dyn CertHook>` to [`crate::state::ApiState::new`].

use flproxy_core::{FlowId, RequestRecord};

/// Replays a previously captured request, optionally with edits, through
/// whatever proxy backend is attached.
#[async_trait::async_trait]
pub trait ReplayHook: Send + Sync {
    /// Replays the request that produced `flow_id`. When `edited` is
    /// `Some`, it is used in place of the flow's original request; when
    /// `None`, the implementation is expected to fall back to the flow's
    /// own stored request. Returns the id of the newly created flow, or a
    /// human-readable error message on failure.
    async fn replay(
        &self,
        flow_id: FlowId,
        edited: Option<RequestRecord>,
    ) -> Result<FlowId, String>;
}

/// Exposes the MITM root certificate authority material.
pub trait CertHook: Send + Sync {
    /// Returns the CA certificate, PEM-encoded.
    fn ca_pem(&self) -> String;
    /// Returns the CA certificate, DER-encoded.
    fn ca_der(&self) -> Vec<u8>;
    /// Returns a human-readable fingerprint of the CA certificate, e.g.
    /// `"SHA256:AB:CD:..."`.
    fn fingerprint(&self) -> String;
}

/// A [`ReplayHook`] that always fails, for running the API without a proxy
/// backend attached.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopReplay;

#[async_trait::async_trait]
impl ReplayHook for NoopReplay {
    async fn replay(
        &self,
        _flow_id: FlowId,
        _edited: Option<RequestRecord>,
    ) -> Result<FlowId, String> {
        Err("replay not supported: no proxy backend attached".to_string())
    }
}

/// A [`CertHook`] that returns clearly-placeholder certificate material, for
/// running the API without a real certificate authority attached.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubCert;

impl CertHook for StubCert {
    fn ca_pem(&self) -> String {
        "-----BEGIN CERTIFICATE-----\n\
         PLACEHOLDER: no certificate authority attached to flproxy-api\n\
         -----END CERTIFICATE-----\n"
            .to_string()
    }

    fn ca_der(&self) -> Vec<u8> {
        b"PLACEHOLDER: no certificate authority attached to flproxy-api".to_vec()
    }

    fn fingerprint(&self) -> String {
        "SHA256:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00".to_string()
    }
}
