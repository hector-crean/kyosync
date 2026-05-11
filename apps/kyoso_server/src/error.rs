//! Top-level server error type. Maps domain errors to HTTP responses.
//!
//! Pattern lifted from the bild_server reference: a single `AppError`
//! enum, `From` impls for every underlying error, and `IntoResponse`
//! that emits a JSON body plus the right status code. Internal (5xx)
//! errors are logged here, never leaked to the client.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("postcard codec: {0}")]
    Codec(#[from] postcard::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, public_message) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::PermissionDenied(m) => (StatusCode::FORBIDDEN, m.clone()),
            AppError::Codec(_) => (StatusCode::BAD_REQUEST, "malformed frame".into()),
            AppError::Io(_) | AppError::Internal(_) => {
                tracing::error!(error = %self, "internal server error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(ErrorBody { error: public_message })).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
