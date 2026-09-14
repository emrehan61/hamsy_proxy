//! Content codecs: compression/decompression and text/binary body encoding.

use std::io::{Read, Write};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::error::{CoreError, Result};
use crate::flow::{BodyKind, BodyPayload};

/// Ceiling on decompressed output size, applied per decode step, independent
/// of any caller-supplied display truncation limit. Without this, a tiny,
/// highly-compressible body (a "decompression bomb") would make every codec
/// below `read_to_end`/`decode_all` its way to gigabytes of allocated memory
/// before `to_payload`'s own (much smaller) `max_bytes` truncation ever gets
/// a chance to run. 128 MiB is comfortably above any legitimate captured
/// body while still bounding worst-case memory use per decode.
const MAX_DECODE_OUTPUT: usize = 128 * 1024 * 1024;

/// Decodes an HTTP body according to its `Content-Encoding` header value.
///
/// `content_encoding` may be a comma-separated list (e.g. `"gzip, br"`), in
/// which case decoding reverses the order in which the codings were applied. An unrecognized token is
/// treated as a no-op rather than an error, since we would rather show the
/// (possibly still-encoded) bytes than fail the whole capture.
///
/// Output is capped at `max_output` bytes (see [`MAX_DECODE_OUTPUT`] for the
/// cap `to_payload` uses); the second return value is `true` if decoding hit
/// that cap, meaning the returned bytes are a truncated prefix rather than
/// the complete decoded body.
pub fn decode_body(
    bytes: &[u8],
    content_encoding: Option<&str>,
    max_output: usize,
) -> Result<(Vec<u8>, bool)> {
    let Some(encoding) = content_encoding else {
        return Ok((bytes.to_vec(), false));
    };
    let mut data = bytes.to_vec();
    let mut truncated = false;
    let codings: Vec<_> = encoding
        .split(',')
        .filter(|token| !token.trim().is_empty())
        .collect();
    for (index, token) in codings.iter().rev().enumerate() {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        // Intermediate compressed representations need their own safety cap;
        // the display cap applies only to the final plaintext stage.
        let limit = if index + 1 == codings.len() {
            max_output
        } else {
            MAX_DECODE_OUTPUT
        };
        let (decoded, hit_cap) = decode_single(&data, &token, limit)?;
        data = decoded;
        // A later stage would just be decoding a truncated (and thus almost
        // certainly invalid) compressed stream; stop rather than compound
        // that into a confusing codec error.
        if hit_cap {
            truncated = true;
            break;
        }
    }
    Ok((data, truncated))
}

/// Decodes a single content-coding token against `bytes`, stopping at
/// `max_output` bytes of decompressed output.
fn decode_single(bytes: &[u8], encoding: &str, max_output: usize) -> Result<(Vec<u8>, bool)> {
    match encoding {
        "gzip" | "x-gzip" => {
            let decoder = flate2::read::GzDecoder::new(bytes);
            read_capped(decoder, max_output, "gzip")
        }
        "deflate" => {
            // The `deflate` content-coding is ambiguous in the wild: most
            // servers send a zlib-wrapped stream, some send raw DEFLATE.
            // Try zlib first and fall back to raw DEFLATE.
            let zlib = flate2::read::ZlibDecoder::new(bytes);
            if let Ok((out, hit_cap)) = read_capped(zlib, max_output, "deflate") {
                if !out.is_empty() {
                    return Ok((out, hit_cap));
                }
            }
            let raw = flate2::read::DeflateDecoder::new(bytes);
            read_capped(raw, max_output, "deflate")
        }
        "br" => {
            let decompressor = brotli::Decompressor::new(bytes, 4096);
            read_capped(decompressor, max_output, "brotli")
        }
        "zstd" => {
            let decoder = zstd::stream::read::Decoder::new(bytes)
                .map_err(|e| CoreError::Codec(format!("zstd decode failed: {e}")))?;
            read_capped(decoder, max_output, "zstd")
        }
        "identity" => Ok((bytes.to_vec(), false)),
        // Unknown coding: pass through unchanged rather than erroring.
        _ => Ok((bytes.to_vec(), false)),
    }
}

/// Reads `reader` to end, but stops after `max_output` bytes instead of
/// growing the output buffer without bound -- this is what actually bounds
/// decompression-bomb memory use, using [`Read::take`] so the decoder itself
/// never produces more than `max_output + 1` bytes into memory. The `bool`
/// is `true` if the cap was hit (there may be more undecoded data left in
/// `reader` that was never read).
fn read_capped<R: Read>(reader: R, max_output: usize, codec: &str) -> Result<(Vec<u8>, bool)> {
    // Ask for one byte past the cap so hitting it exactly can be told apart
    // from output that's genuinely exactly `max_output` bytes long.
    let limit = (max_output as u64).saturating_add(1);
    let mut out = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|e| CoreError::Codec(format!("{codec} decode failed: {e}")))?;
    if out.len() > max_output {
        out.truncate(max_output);
        Ok((out, true))
    } else {
        Ok((out, false))
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
    bytes
        .iter()
        .any(|&b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r')
}

/// Returns true if `mime` denotes a textual content type worth displaying
/// as UTF-8 text rather than base64 (`text/*`, JSON, XML, JS, form data,
/// GraphQL, and any `+json`/`+xml` structured-syntax suffix).
pub fn is_textual_mime(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
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
/// `max_bytes` (post-decode) is truncated, with `size` reporting the decoded
/// length when complete. For truncated compressed content, size is a lower
/// bound: decoding stops at the capture limit instead of expanding the entire
/// response just to count bytes.
pub fn to_payload(
    bytes: &[u8],
    content_type: Option<&str>,
    content_encoding: Option<&str>,
    max_bytes: usize,
) -> BodyPayload {
    let (decoded, decode_truncated) = decode_body(
        bytes,
        content_encoding,
        max_bytes.saturating_add(3).min(MAX_DECODE_OUTPUT),
    )
    .unwrap_or_else(|_| (bytes.to_vec(), false));
    let encoding = content_encoding.map(str::to_string);

    if decoded.is_empty() && !decode_truncated {
        return BodyPayload {
            kind: BodyKind::None,
            data: String::new(),
            size: 0,
            truncated: false,
            encoding,
        };
    }

    let size = decoded.len() as u64;
    let valid_text = match std::str::from_utf8(&decoded) {
        Ok(_) => true,
        Err(e) => decode_truncated && e.error_len().is_none(),
    };
    let is_text = valid_text
        && (content_type.map(is_textual_mime).unwrap_or(false) || !has_control_bytes(&decoded));

    if decoded.len() <= max_bytes && !decode_truncated {
        let (kind, data) = if is_text {
            (
                BodyKind::Text,
                String::from_utf8(decoded).unwrap_or_default(),
            )
        } else {
            (BodyKind::Base64, BASE64.encode(&decoded))
        };
        return BodyPayload {
            kind,
            data,
            size,
            truncated: false,
            encoding,
        };
    }

    // Truncate. For text, cut at the nearest valid UTF-8 char boundary at or
    // before `max_bytes`; for binary, base64-encode the raw byte prefix.
    // `decoded` may already be shorter than `max_bytes` here (capped by
    // `MAX_DECODE_OUTPUT` instead), hence the `min`.
    let cutoff = decoded.len().min(max_bytes);
    let prefix = &decoded[..cutoff];
    let data = if is_text {
        let mut end = prefix.len();
        while end > 0 && std::str::from_utf8(&prefix[..end]).is_err() {
            end -= 1;
        }
        String::from_utf8_lossy(&prefix[..end]).into_owned()
    } else {
        BASE64.encode(prefix)
    };
    BodyPayload {
        kind: BodyKind::Truncated,
        data,
        size,
        truncated: true,
        encoding,
    }
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

    /// A generous cap for roundtrip tests that aren't exercising the cap
    /// itself -- large enough that none of these small fixtures ever hit it.
    const NO_PRACTICAL_LIMIT: usize = 1024 * 1024;

    #[test]
    fn gzip_roundtrip() {
        let original = b"hello world, this is a test payload".to_vec();
        let encoded = encode_body(&original, "gzip").unwrap();
        assert_ne!(encoded, original);
        let (decoded, truncated) = decode_body(&encoded, Some("gzip"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn deflate_roundtrip() {
        let original = b"another test payload for deflate".to_vec();
        let encoded = encode_body(&original, "deflate").unwrap();
        let (decoded, truncated) =
            decode_body(&encoded, Some("deflate"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn brotli_roundtrip() {
        let original = b"brotli test payload with some repeated repeated text text".to_vec();
        let encoded = encode_body(&original, "br").unwrap();
        let (decoded, truncated) = decode_body(&encoded, Some("br"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn zstd_roundtrip() {
        let original = b"zstd test payload".to_vec();
        let encoded = encode_body(&original, "zstd").unwrap();
        let (decoded, truncated) = decode_body(&encoded, Some("zstd"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn unknown_encoding_passes_through() {
        let original = b"unchanged".to_vec();
        let (decoded, truncated) =
            decode_body(&original, Some("unknown-coding"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn comma_list_decodes_in_reverse_wire_order() {
        let original = b"layered payload".to_vec();
        let gzipped = encode_body(&original, "gzip").unwrap();
        let gzip_then_brotli = encode_body(&gzipped, "br").unwrap();
        // gzip was applied first, then br; decoding must undo br first.
        let (decoded, truncated) =
            decode_body(&gzip_then_brotli, Some("gzip, br"), NO_PRACTICAL_LIMIT).unwrap();
        assert_eq!(decoded, original);
        assert!(!truncated);
    }

    #[test]
    fn decode_body_caps_decompression_bomb_output() {
        // A highly-compressible payload: decompresses to far more than the
        // tiny cap passed below, so this must come back truncated instead
        // of allocating the full decoded size.
        let original = vec![b'a'; 200_000];
        let encoded = encode_body(&original, "gzip").unwrap();
        assert!(encoded.len() < 1000, "fixture should compress tiny");

        let (decoded, truncated) = decode_body(&encoded, Some("gzip"), 1024).unwrap();
        assert!(truncated);
        assert_eq!(decoded.len(), 1024);
    }

    #[test]
    fn decode_body_exact_cap_is_not_marked_truncated() {
        let original = vec![b'a'; 1024];
        let encoded = encode_body(&original, "gzip").unwrap();
        let (decoded, truncated) = decode_body(&encoded, Some("gzip"), 1024).unwrap();
        assert!(!truncated);
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
    #[test]
    fn compressed_capture_preserves_utf8_prefix_and_layer_order() {
        for encoding in ["gzip", "br", "gzip, br"] {
            let mut wire = "héllo".as_bytes().to_vec();
            for coding in encoding.split(", ") {
                wire = encode_body(&wire, coding).unwrap();
            }
            let payload = to_payload(&wire, Some("text/plain"), Some(encoding), 2);
            assert_eq!(payload.data, "h", "{encoding}");
            assert!(payload.truncated);
        }
    }
}
