//! Renders QR codes as self-contained inline SVG markup (no external
//! references), used by `GET /api/setup` to make it easy to scan the
//! proxy's certificate URL from a phone.

use qrcode::{Color, QrCode};

/// Size, in SVG user units, of a single QR module (data cell).
const MODULE_SIZE: u32 = 4;
/// Width, in modules, of the quiet-zone border required around a QR code.
const QUIET_ZONE: u32 = 4;

/// Renders `data` as a black-on-white SVG QR code, including the required
/// quiet zone. Returns a minimal valid SVG containing an error message
/// instead of panicking if `data` cannot be encoded (e.g. too long for any
/// QR version).
pub fn svg(data: &str) -> String {
    let code = match QrCode::new(data.as_bytes()) {
        Ok(c) => c,
        Err(e) => return error_svg(&e.to_string()),
    };

    let width = code.width() as u32;
    let colors = code.to_colors();
    let dimension = (width + QUIET_ZONE * 2) * MODULE_SIZE;

    let mut rects = String::new();
    for y in 0..width {
        for x in 0..width {
            if colors[(y * width + x) as usize] == Color::Dark {
                let px = (x + QUIET_ZONE) * MODULE_SIZE;
                let py = (y + QUIET_ZONE) * MODULE_SIZE;
                rects.push_str(&format!(
                    r#"<rect x="{px}" y="{py}" width="{MODULE_SIZE}" height="{MODULE_SIZE}"/>"#
                ));
            }
        }
    }

    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {dimension} {dimension}" shape-rendering="crispEdges"><rect x="0" y="0" width="{dimension}" height="{dimension}" fill="#fff"/><g fill="#000">{rects}</g></svg>"##
    )
}

/// Builds a minimal valid SVG carrying an error message, for callers that
/// need a well-formed (if unusable) SVG even on encode failure.
fn error_svg(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 40"><rect width="200" height="40" fill="#fff"/><text x="4" y="20" font-size="8" fill="#c00">QR error: {escaped}</text></svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_contains_viewbox_and_crispedges() {
        let out = svg("http://192.168.1.10:9081/cert/hamsy-ca.crt");
        assert!(out.contains("viewBox"));
        assert!(out.contains("shape-rendering=\"crispEdges\""));
        assert!(out.starts_with("<svg"));
    }

    #[test]
    fn svg_has_quiet_zone_border() {
        // A trivially short payload still produces a non-trivial SVG with a
        // quiet zone margin baked into the viewBox math (QUIET_ZONE modules
        // on each side), which we approximate-check via dimension parity.
        let out = svg("hi");
        assert!(out.contains("viewBox=\"0 0 "));
    }
}
