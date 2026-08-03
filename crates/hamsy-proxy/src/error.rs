//! Shared error type for the `hamsy-proxy` crate.

use thiserror::Error;

/// Errors that can occur anywhere in `hamsy-proxy`.
///
/// Messages are kept human-readable since several variants (notably
/// [`ProxyError::UpstreamConnect`] and [`ProxyError::InvalidTarget`]) are
/// surfaced directly to the client in a `502` response body.
#[derive(Error, Debug)]
pub enum ProxyError {
    /// Wraps an underlying I/O failure (socket accept, read/write, ...).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Wraps a `hyper` protocol-level failure.
    #[error("http error: {0}")]
    Hyper(#[from] hyper::Error),

    /// Wraps a `rustls` TLS failure.
    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),

    /// Wraps an `hamsy-core` failure (rule/body/settings logic).
    #[error("core error: {0}")]
    Core(#[from] hamsy_core::CoreError),

    /// An operation exceeded its allotted time budget.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Failed to establish (or negotiate) a connection to an upstream host.
    #[error("could not connect to {0}")]
    UpstreamConnect(String),

    /// A request target (URL, authority, host) was structurally invalid or
    /// could not be resolved.
    #[error("invalid target: {0}")]
    InvalidTarget(String),

    /// A certificate authority or certificate-minting operation failed.
    #[error("certificate error: {0}")]
    Cert(String),

    /// Generic catch-all for conditions that don't fit another variant.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias for `Result<T, ProxyError>`.
pub type Result<T> = std::result::Result<T, ProxyError>;

impl From<std::convert::Infallible> for ProxyError {
    fn from(never: std::convert::Infallible) -> Self {
        match never {}
    }
}
