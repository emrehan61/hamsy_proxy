//! The API's unified error type and its HTTP response mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// An error produced while handling an API request.
///
/// Converts to a JSON body of the shape `{ "error": "<category>", "detail":
/// "<human message>" }` with an appropriate HTTP status code via
/// [`IntoResponse`].
#[derive(Debug)]
pub enum ApiError {
    /// Malformed or invalid request input (400).
    BadRequest(String),
    /// The requested resource does not exist (404).
    NotFound(String),
    /// The request body exceeded an allowed size (413).
    PayloadTooLarge(String),
    /// An unexpected internal failure (500).
    Internal(String),
    /// The requested operation is recognized but not implemented by the
    /// currently configured backend (501).
    NotImplemented(String),
    /// A downstream system operation (e.g. shelling out to configure the OS
    /// system proxy) failed (502).
    BadGateway(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::NotImplemented(_) => StatusCode::NOT_IMPLEMENTED,
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn category(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "bad_request",
            ApiError::NotFound(_) => "not_found",
            ApiError::PayloadTooLarge(_) => "payload_too_large",
            ApiError::Internal(_) => "internal",
            ApiError::NotImplemented(_) => "not_implemented",
            ApiError::BadGateway(_) => "bad_gateway",
        }
    }

    fn detail(&self) -> &str {
        match self {
            ApiError::BadRequest(s)
            | ApiError::NotFound(s)
            | ApiError::PayloadTooLarge(s)
            | ApiError::Internal(s)
            | ApiError::NotImplemented(s)
            | ApiError::BadGateway(s) => s,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    detail: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ErrorBody {
            error: self.category(),
            detail: self.detail(),
        };
        (status, axum::Json(body)).into_response()
    }
}

impl From<rdproxy_core::CoreError> for ApiError {
    fn from(err: rdproxy_core::CoreError) -> Self {
        match err {
            rdproxy_core::CoreError::NotFound => ApiError::NotFound("not found".to_string()),
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        ApiError::BadRequest(format!("invalid JSON: {err}"))
    }
}
