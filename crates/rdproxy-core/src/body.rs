//! Content codecs: compression/decompression and text/binary body encoding.

use std::io::{Read, Write};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::error::{CoreError, Result};
use crate::flow::{BodyKind, BodyPayload};

/// Decodes an HTTP body according to its `Content-Encoding` header value.
///
/// `content_encoding` may be a comma-separated list (e.g. `"gzip, br"`), in
/// which case each token is applied in order. An unrecognized token is
/// treated as a no-op rather than an error, since we would rather show the
/// (possibly still-encoded) bytes than fail the whole capture.
pub fn decode_body(bytes: &[u8], content_encoding: Option<&str>) -> Result<Vec<u8>> {
    let Some(encoding) = content_encoding else {
        return Ok(bytes.to_vec());
    };
    let mut data = bytes.to_vec();
    for token in encoding.split(',') {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        data = decode_single(&data, &token)?;
    }
    Ok(data)
}

/// Decodes a single content-coding token against `bytes`.
fn decode_single(bytes: &[u8], encoding: &str) -> Result<Vec<u8>> {
    match encoding {
        "gzip" | "x-gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(bytes);
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|e| CoreError::Codec(format!("gzip decode failed: {e}")))?;
            Ok(out)
        }
        "deflate" => {
            // The `deflate` content-coding is ambiguous in the wild: most
            // servers send a zlib-wrapped stream, some send raw DEFLATE.
            // Try zlib first and fall back to raw DEFLATE.
            let mut out = Vec::new();
            let mut zlib = flate2::read::ZlibDecoder::new(bytes);
            if zlib.read_to_end(&mut out).is_ok() && !out.is_empty() {
                return Ok(out);
            }
            out.clear();
            let mut raw = flate2::read::DeflateDecoder::new(bytes);
            raw.read_to_end(&mut out)
                .map_err(|e| CoreError::Codec(format!("deflate decode failed: {e}")))?;
            Ok(out)
        }
        "br" => {
            let mut out = Vec::new();
            let mut decompressor = brotli::Decompressor::new(bytes, 4096);
            decompressor
                .read_to_end(&mut out)
                .map_err(|e| CoreError::Codec(format!("brotli decode failed: {e}")))?;
            Ok(out)
        }
        "zstd" => zstd::decode_all(bytes)
            .map_err(|e| CoreError::Codec(format!("zstd decode failed: {e}"))),
        "identity" => Ok(bytes.to_vec()),
        // Unknown coding: pass through unchanged rather than erroring.
        _ => Ok(bytes.to_vec()),
    }
}

/// Encodes `bytes` using the single named content-coding.
///
/// Unlike [`decode_body`] this takes exactly one encoding (matching how a
/// caller re-encodes an edited body for a specific `Content-Encoding`
/// value). An unrecognized encoding is a no-op, mirroring [`decode_body`]'s
/// permissive handling of unknown tokens.
pub fn encode_body(bytes: &[u8], encoding: &str) -> Result<Vec<u8>> {
    let token = encoding.trim().to_ascii_lowercase();
    match token.as_str() {
        "gzip" | "x-gzip" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(bytes)
                .map_err(|e| CoreError::Codec(format!("gzip encode failed: {e}")))?;
            encoder
                .finish()
                .map_err(|e| CoreError::Codec(format!("gzip encode failed: {e}")))
        }
        "deflate" => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder
                .write_all(bytes)
                .map_err(|e| CoreError::Codec(format!("deflate encode failed: {e}")))?;
            encoder
                .finish()
                .map_err(|e| CoreError::Codec(format!("deflate encode failed: {e}")))
        }
        "br" => {
            let mut out = Vec::new();
            {
                let mut writer = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
                writer
                    .write_all(bytes)
                    .map_err(|e| CoreError::Codec(format!("brotli encode failed: {e}")))?;
                writer
                    .flush()
                    .map_err(|e| CoreError::Codec(format!("brotli encode failed: {e}")))?;
            }
            Ok(out)
        }
        "zstd" => zstd::encode_all(bytes, 0)
            .map_err(|e| CoreError::Codec(format!("zstd encode failed: {e}"))),
        "identity" => Ok(bytes.to_vec()),
        _ => Ok(bytes.to_vec()),
    }
}

/// Returns true if a byte slice contains bytes that would be considered
/// "binary" control characters when deciding whether to display content as
/// text (tab, newline, and carriage return are allowed).
fn has_control_bytes(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r')
}

/// Returns true if `mime` denotes a textual content type worth displaying
/// as UTF-8 text rather than base64 (`text/*`, JSON, XML, JS, form data,
/// GraphQL, and any `+json`/`+xml` structured-syntax suffix).
pub fn is_textual_mime(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim().to_ascii_lowercase();
    if mime.starts_with("text/") {
        return true;
    }
    if mime.ends_with("+json") || mime.ends_with("+xml") {
        return true;
    }
    matches!(
        mime.as_str(),
        "application/json"
            | "application/xml"
            | "application/xhtml+xml"
            | "application/javascript"
            | "application/x-javascript"
            | "application/ecmascript"
            | "application/x-www-form-urlencoded"
            | "application/graphql"
            | "application/manifest+json"
            | "image/svg+xml"
    )
}

/// Decodes `bytes` (per `content_encoding`) and packages the result as a
/// [`BodyPayload`] ready for JSON transport.
///
/// The decoded content is classified as text when it is valid UTF-8 *and*
/// either the MIME type is textual or the bytes contain no binary control
/// characters; otherwise it is stored as base64. Content longer than
/// `max_bytes` (post-decode) is truncated, with `size` always reporting the
/// full decoded length.
pub fn to_payload(
    bytes: &[u8],
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    max_bytes: usize,
) -> BodyPayload {
    let decoded = decode_body(bytes, content_encoding).unwrap_or_else(|_| bytes.to_vec());
    let encoding = content_encoding.map(str::to_string);

    if decoded.is_empty() {
        return BodyPayload {
            kind: BodyKind::None,
            data: String::new(),
            size: 0,
            truncated: false,
            encoding,
        };
    }

    let size = decoded.len() as u64;
    let is_text = std::str::from_utf8(&decoded).is_ok()
        && (content_type.map(is_textual_mime).unwrap_or(false) || !has_control_bytes(&decoded));

    if decoded.len() <= max_bytes {
        let (kind, data) = if is_text {
            (BodyKind::Text, String::from_utf8(decoded).unwrap_or_default())
        } else {
            (BodyKind::Base64, BASE64.encode(&decoded))
        };
        return BodyPayload { kind, data, size, truncated: false, encoding };
    }

    // Truncate. For text, cut at the nearest valid UTF-8 char boundary at or
    // before `max_bytes`; for binary, base64-encode the raw byte prefix.
    let prefix = &decoded[..max_bytes];
    let data = if is_text {
        let mut end = prefix.len();
        while end > 0 && std::str::from_utf8(&prefix[..end]).is_err() {
            end -= 1;
        }
        String::from_utf8_lossy(&prefix[..end]).into_owned()
    } else {
        BASE64.encode(prefix)
    };
    BodyPayload { kind: BodyKind::Truncated, data, size, truncated: true, encoding }
}

/// Reconstructs the raw bytes of a [`BodyPayload`], the inverse of
/// [`to_payload`]'s text/base64 encoding step.
///
/// A truncated payload has no recoverable original bytes and yields an
/// empty vector, as does a payload with no body.
pub fn from_payload(p: &BodyPayload) -> Vec<u8> {
    match p.kind {
        BodyKind::Text => p.data.clone().into_bytes(),
        BodyKind::Base64 => BASE64.decode(&p.data).unwrap_or_default(),
        BodyKind::None | BodyKind::Truncated => Vec::new(),
    }
}

/// Pretty-prints a JSON string, returning `None` if it fails to parse.
pub fn pretty_json(s: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(s).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip() {
        let original = b"hello world, this is a test payload".to_vec();
        let encoded = encode_body(&original, "gzip").unwrap();
        assert_ne!(encoded, original);
        let decoded = decode_body(&encoded, Some("gzip")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn deflate_roundtrip() {
        let original = b"another test payload for deflate".to_vec();
        let encoded = encode_body(&original, "deflate").unwrap();
        let decoded = decode_body(&encoded, Some("deflate")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn brotli_roundtrip() {
        let original = b"brotli test payload with some repeated repeated text text".to_vec();
        let encoded = encode_body(&original, "br").unwrap();
        let decoded = decode_body(&encoded, Some("br")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn zstd_roundtrip() {
        let original = b"zstd test payload".to_vec();
        let encoded = encode_body(&original, "zstd").unwrap();
        let decoded = decode_body(&encoded, Some("zstd")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn unknown_encoding_passes_through() {
        let original = b"unchanged".to_vec();
        let decoded = decode_body(&original, Some("unknown-coding")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn comma_list_applies_in_order() {
        let original = b"layered payload".to_vec();
        let gzipped = encode_body(&original, "gzip").unwrap();
        let brotli_then_gzip = encode_body(&gzipped, "br").unwrap();
        // Content-Encoding: br, gzip means br was applied last on the wire;
        // decoding in the listed order (br, then gzip) reverses that.
        let decoded = decode_body(&brotli_then_gzip, Some("br, gzip")).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn to_payload_text() {
        let payload = to_payload(b"{\"a\":1}", Some("application/json"), None, 1024);
        assert_eq!(payload.kind, BodyKind::Text);
        assert_eq!(payload.data, "{\"a\":1}");
        assert_eq!(payload.size, 7);
        assert!(!payload.truncated);
    }

    #[test]
    fn to_payload_binary() {
        let bytes = [0u8, 159, 146, 150, 1, 2, 3];
        let payload = to_payload(&bytes, Some("application/octet-stream"), None, 1024);
        assert_eq!(payload.kind, BodyKind::Base64);
        assert_eq!(from_payload(&payload), bytes);
    }

    #[test]
    fn to_payload_truncates() {
        let text = "a".repeat(100);
        let payload = to_payload(text.as_bytes(), Some("text/plain"), None, 10);
        assert_eq!(payload.kind, BodyKind::Truncated);
        assert!(payload.truncated);
        assert_eq!(payload.size, 100);
        assert_eq!(payload.data.len(), 10);
    }

    #[test]
    fn to_payload_empty() {
        let payload = to_payload(b"", Some("text/plain"), None, 10);
        assert_eq!(payload.kind, BodyKind::None);
        assert_eq!(payload.size, 0);
    }

    #[test]
    fn from_payload_none_is_empty() {
        let payload = BodyPayload::default();
        assert!(from_payload(&payload).is_empty());
    }

    #[test]
    fn textual_mime_detection() {
        assert!(is_textual_mime("text/plain"));
        assert!(is_textual_mime("application/json; charset=utf-8"));
        assert!(is_textual_mime("application/vnd.api+json"));
        assert!(is_textual_mime("image/svg+xml"));
        assert!(!is_textual_mime("image/png"));
        assert!(!is_textual_mime("application/octet-stream"));
    }

    #[test]
    fn pretty_json_formats() {
        let out = pretty_json("{\"a\":1}").unwrap();
        assert!(out.contains('\n'));
        assert!(pretty_json("not json").is_none());
    }
}
