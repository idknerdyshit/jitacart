//! Unified error type for API handlers.
//!
//! Single point of truth for HTTP status mapping. Replaces five per-handler
//! enums that all carried the same six-variant core with identical mappings.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    QuotaExceeded(String),
    Internal(anyhow::Error),
}

impl ApiError {
    pub fn forbidden() -> Self {
        Self::Forbidden("you cannot perform this action".into())
    }

    pub fn not_found() -> Self {
        Self::NotFound("not found".into())
    }

    pub fn internal<E: Into<anyhow::Error>>(e: E) -> Self {
        Self::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "not logged in").into_response(),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg).into_response(),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
            ApiError::QuotaExceeded(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response()
            }
            ApiError::Internal(e) => {
                tracing::error!(error = ?e, "api handler error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            }
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl From<tower_sessions::session::Error> for ApiError {
    fn from(e: tower_sessions::session::Error) -> Self {
        ApiError::Internal(anyhow::Error::new(e))
    }
}
