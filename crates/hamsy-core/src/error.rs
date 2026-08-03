//! Shared error type for the `flproxy-core` crate.

use thiserror::Error;

/// Errors that can occur anywhere in `flproxy-core`.
///
/// All fallible operations that touch untrusted input (rule definitions,
/// HAR files, settings files, HTTP bodies, regexes, globs, ...) return this
/// type rather than panicking.
#[derive(Error, Debug)]
pub enum CoreError {
    /// Wraps an underlying I/O failure (reading/writing settings, rules, ...).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Wraps a JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Wraps a regex compilation or execution failure.
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),

    /// A glob pattern failed to compile.
    #[error("glob error: {0}")]
    Glob(String),

    /// A rule (or part of a rule) was structurally invalid.
    #[error("invalid rule: {0}")]
    InvalidRule(String),

    /// The requested resource (flow, rule, ...) does not exist.
    #[error("not found")]
    NotFound,

    /// A body codec (compression/decompression) operation failed.
    #[error("codec error: {0}")]
    Codec(String),
}

/// Convenience alias for `Result<T, CoreError>`.
pub type Result<T> = std::result::Result<T, CoreError>;
