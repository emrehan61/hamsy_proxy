//! Serves the built web UI (or a helpful placeholder page when it hasn't
//! been built yet), with SPA-style fallback to `index.html`.

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::path::PathBuf;

use crate::state::ApiState;

#[cfg(feature = "embed-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/dist"]
struct EmbeddedUi;

/// Resolves the directory a built UI lives in, checking (in order):
/// `$HAMSY_UI_DIR`, `<cwd>/ui/dist`, `<exe_dir>/ui/dist`.
fn resolve_ui_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HAMSY_UI_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let path = cwd.join("ui").join("dist");
        if path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join("ui").join("dist");
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(feature = "embed-ui")]
fn embedded_asset(rel_path: &str) -> Option<Vec<u8>> {
    EmbeddedUi::get(rel_path).map(|file| file.data.into_owned())
}

#[cfg(not(feature = "embed-ui"))]
fn embedded_asset(_rel_path: &str) -> Option<Vec<u8>> {
    None
}

/// The catch-all `GET /*` handler: serves static UI assets from disk (or an
/// embedded bundle when built with `--features embed-ui`), SPA-falling-back
/// to `index.html` for any path that doesn't resolve to a real file, and
/// finally falling back to a minimal built-in placeholder page if no UI is
/// available at all. Only reached for requests that didn't match `/api/*`
/// or `/cert/*`, since those are registered as their own nested routers
/// with their own fallbacks.
pub async fn asset_handler(State(state): State<ApiState>, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let rel_path = if path.is_empty() { "index.html" } else { path };

    if let Some(dir) = resolve_ui_dir() {
        let candidate = dir.join(rel_path);
        if candidate.is_file() {
            if let Ok(bytes) = tokio::fs::read(&candidate).await {
                return asset_response(rel_path, bytes);
            }
        }
        if let Ok(bytes) = tokio::fs::read(dir.join("index.html")).await {
            return html_response(bytes);
        }
    }

    if let Some(bytes) = embedded_asset(rel_path) {
        return asset_response(rel_path, bytes);
    }
    if let Some(bytes) = embedded_asset("index.html") {
        return html_response(bytes);
    }

    html_response(fallback_page(state.settings().proxy_port).into_bytes())
}

/// Builds a response for a concrete static asset, with the right MIME type
/// and cache policy (long-lived immutable caching for hashed `/assets/`
/// bundle files, no-cache for everything else, notably `index.html`).
fn asset_response(rel_path: &str, bytes: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(rel_path).first_or_octet_stream();
    let cache_control = if rel_path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.essence_str().to_string()),
            (header::CACHE_CONTROL, cache_control.to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// Builds a 200 OK `text/html` response, used for `index.html` and the
/// built-in placeholder page (both always `no-cache`).
fn html_response(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        bytes,
    )
        .into_response()
}

/// A minimal, dark-styled placeholder page shown when no built UI is
/// available on disk or embedded in the binary.
fn fallback_page(proxy_port: u16) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>hamsy-proxy</title>
<style>
  body {{ background: #111; color: #ddd; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
          display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; }}
  main {{ max-width: 32rem; padding: 2rem; text-align: center; }}
  h1 {{ font-size: 1.25rem; font-weight: 600; }}
  code {{ background: #222; padding: 0.15rem 0.4rem; border-radius: 0.25rem; }}
  a {{ color: #7db3ff; }}
</style>
</head>
<body>
<main>
  <h1>Web UI not built</h1>
  <p>Run <code>pnpm --dir ui build</code> to build the web UI.</p>
  <p>The MITM proxy is listening on port <code>{proxy_port}</code>.</p>
  <p><a href="/cert/hamsy-ca.crt">Download the hamsy-proxy root CA certificate</a></p>
</main>
</body>
</html>
"#
    )
}
