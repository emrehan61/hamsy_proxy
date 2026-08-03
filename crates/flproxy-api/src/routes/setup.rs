//! `/api/setup` and `/cert/*` routes: device-onboarding helpers.

use std::net::{IpAddr, Ipv4Addr};

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::qr;
use crate::ApiState;

/// Interface names commonly used for a machine's primary LAN connection,
/// preferred when multiple candidate addresses are otherwise tied.
const PREFERRED_INTERFACES: [&str; 3] = ["en0", "wlan0", "eth0"];

/// Discovers non-loopback, non-link-local IPv4 LAN addresses, ordered with
/// the "most likely to be the right one" first: private-range (RFC1918)
/// addresses before public ones, and addresses on a commonly-primary
/// interface name (`en0`/`wlan0`/`eth0`) before others. Ties keep discovery
/// order (a stable sort).
pub fn lan_addresses() -> Vec<String> {
    let interfaces = if_addrs::get_if_addrs().unwrap_or_default();

    let mut candidates: Vec<(String, Ipv4Addr)> = interfaces
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) if !v4.is_link_local() => Some((iface.name, v4)),
            _ => None,
        })
        .collect();

    candidates.sort_by_key(|(name, ip)| {
        let private_rank = u8::from(!ip.is_private());
        let name_rank = u8::from(!PREFERRED_INTERFACES.contains(&name.as_str()));
        (private_rank, name_rank)
    });

    candidates
        .into_iter()
        .map(|(_, ip)| ip.to_string())
        .collect()
}

/// `GET /api/setup` — returns everything a client needs to point a device
/// at this proxy and trust its certificate: host/port, LAN addresses, a
/// certificate download URL, its fingerprint, and a scannable QR code.
pub async fn setup(State(state): State<ApiState>) -> Json<Value> {
    let settings = state.settings();
    let addresses = lan_addresses();
    let proxy_host = addresses
        .first()
        .cloned()
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let cert_url = format!(
        "http://{proxy_host}:{}/cert/flproxy-ca.crt",
        settings.ui_port
    );
    let qr_svg = qr::svg(&cert_url);

    Json(json!({
        "proxyHost": proxy_host,
        "proxyPort": settings.proxy_port,
        "lanAddresses": addresses,
        "certUrl": cert_url,
        "caFingerprint": state.cert_hook().fingerprint(),
        "qrSvg": qr_svg,
    }))
}

/// `GET /cert/flproxy-ca.pem`.
pub async fn cert_pem(State(state): State<ApiState>) -> Response {
    let pem = state.cert_hook().ca_pem();
    ([(header::CONTENT_TYPE, "application/x-pem-file")], pem).into_response()
}

/// `GET /cert/flproxy-ca.crt` — same DER content as `.der`, served with the
/// MIME type iOS/Android profile installers key off of.
pub async fn cert_crt(State(state): State<ApiState>) -> Response {
    let der = state.cert_hook().ca_der();
    ([(header::CONTENT_TYPE, "application/x-x509-ca-cert")], der).into_response()
}

/// `GET /cert/flproxy-ca.der`.
pub async fn cert_der(State(state): State<ApiState>) -> Response {
    let der = state.cert_hook().ca_der();
    ([(header::CONTENT_TYPE, "application/x-x509-ca-cert")], der).into_response()
}
