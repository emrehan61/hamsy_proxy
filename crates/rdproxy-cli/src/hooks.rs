//! Real [`ReplayHook`]/[`CertHook`] implementations, backed by a live
//! `rdproxy-proxy` [`ProxyContext`] and [`CertAuthority`], that [`crate::run::run`]
//! hands to `rdproxy-api`'s `ApiState` so the API can replay flows and serve
//! CA certificate material through the actual running proxy backend.

use std::sync::Arc;

use rdproxy_api::{CertHook, ReplayHook};
use rdproxy_core::{FlowId, RequestRecord};
use rdproxy_proxy::{CertAuthority, ProxyContext};

/// A [`CertHook`] backed by a real, on-disk [`CertAuthority`].
pub struct RealCertHook {
    ca: Arc<CertAuthority>,
}

impl RealCertHook {
    /// Wraps `ca` as a [`CertHook`].
    pub fn new(ca: Arc<CertAuthority>) -> Self {
        RealCertHook { ca }
    }
}

impl CertHook for RealCertHook {
    fn ca_pem(&self) -> String {
        self.ca.pem()
    }

    fn ca_der(&self) -> Vec<u8> {
        self.ca.der()
    }

    fn fingerprint(&self) -> String {
        format!("SHA256:{}", self.ca.fingerprint_sha256())
    }
}

/// A [`ReplayHook`] that re-issues requests through a live [`ProxyContext`]'s
/// normal capture/rule/dispatch pipeline (see [`rdproxy_proxy::replay::replay`]).
pub struct RealReplayHook {
    ctx: ProxyContext,
}

impl RealReplayHook {
    /// Wraps `ctx` as a [`ReplayHook`].
    pub fn new(ctx: ProxyContext) -> Self {
        RealReplayHook { ctx }
    }
}

#[async_trait::async_trait]
impl ReplayHook for RealReplayHook {
    async fn replay(
        &self,
        flow_id: FlowId,
        edited: Option<RequestRecord>,
    ) -> Result<FlowId, String> {
        let req = match edited {
            Some(req) => req,
            None => {
                let flow = self
                    .ctx
                    .flows
                    .get(flow_id)
                    .ok_or_else(|| format!("flow {flow_id} not found"))?;
                flow.request
                    .ok_or_else(|| format!("flow {flow_id} has no stored request to replay"))?
            }
        };
        rdproxy_proxy::replay::replay(&self.ctx, req)
            .await
            .map_err(|e| e.to_string())
    }
}
