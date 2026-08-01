//! The crate-wide error type.
//!
//! Every fallible public function in picweight returns `Result<_, AppError>`.
//! `AppError` maps 1:1 onto an HTTP status and renders as a small JSON body, so
//! a handler never has to think about status codes:
//!
//! ```ignore
//! pub async fn get_meal(...) -> Result<Json<MealResponse>, AppError> {
//!     let meal = load(&mut conn, id)?;                  // diesel error -> 500/404
//!     let meal = meal.ok_or_else(|| AppError::NotFound("meal".into()))?;
//!     Ok(Json(meal.into()))
//! }
//! ```
//!
//! Internal errors log at `error!` with the full cause and return a generic
//! message to the client; client errors return their message verbatim because
//! they are the API contract.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// Every failure mode the HTTP layer can express.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 404 — the resource does not exist, or belongs to another user.
    ///
    /// Note the deliberate conflation: cross-user reads must not be
    /// distinguishable from misses, or the API leaks the existence of other
    /// users' meals.
    #[error("not found: {0}")]
    NotFound(String),

    /// 401 — no session, or the session token failed validation.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// 403 — authenticated, but not allowed.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// 400 — malformed or semantically invalid request.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// 409 — the request conflicts with existing state (duplicate
    /// `client_uuid`, concurrent revision write).
    #[error("conflict: {0}")]
    Conflict(String),

    /// 429 — an upstream provider refused on quota/rate grounds.
    ///
    /// PRD §5 is explicit that quota exhaustion must be loud: the meal goes to
    /// `failed` with a visible reason rather than stalling silently.
    #[error("rate limited: {0}")]
    RateLimited(String),

    /// 503 — a required upstream (OpenAI, Open Food Facts, the IdP) is
    /// unreachable or returned an unusable response.
    #[error("upstream unavailable: {0}")]
    Upstream(String),

    /// 500 — anything else. The message is logged, not returned.
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// The HTTP status this error renders as.
    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::Upstream(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Stable machine-readable code, for clients that want to branch.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::Unauthorized(_) => "unauthorized",
            AppError::Forbidden(_) => "forbidden",
            AppError::BadRequest(_) => "bad_request",
            AppError::Conflict(_) => "conflict",
            AppError::RateLimited(_) => "rate_limited",
            AppError::Upstream(_) => "upstream_unavailable",
            AppError::Internal(_) => "internal",
        }
    }

    /// Wrap any displayable error as an internal failure.
    pub fn internal<E: std::fmt::Display>(err: E) -> Self {
        AppError::Internal(err.to_string())
    }

    /// Wrap any displayable error as an upstream failure.
    pub fn upstream<E: std::fmt::Display>(err: E) -> Self {
        AppError::Upstream(err.to_string())
    }

    /// True when retrying the same operation could plausibly succeed.
    /// The analysis worker uses this to decide between retry and hard-fail.
    pub fn is_retryable(&self) -> bool {
        matches!(self, AppError::RateLimited(_) | AppError::Upstream(_))
    }
}

/// The JSON body returned for every [`AppError`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code, e.g. `not_found`.
    pub error: String,
    /// Human-readable detail. Generic for 5xx, specific for 4xx.
    pub message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = match &self {
            // Never leak internals to the client — log them instead.
            AppError::Internal(detail) => {
                tracing::error!(error = %detail, "internal server error");
                "internal server error".to_string()
            }
            AppError::Upstream(detail) => {
                tracing::warn!(error = %detail, "upstream unavailable");
                detail.clone()
            }
            other => other.to_string(),
        };
        let body = ErrorBody {
            error: self.code().to_string(),
            message,
        };
        (status, Json(body)).into_response()
    }
}

// ---------------------------------------------------------------------------
// From impls
// ---------------------------------------------------------------------------

impl From<diesel::result::Error> for AppError {
    fn from(err: diesel::result::Error) -> Self {
        use diesel::result::{DatabaseErrorKind, Error};
        match err {
            Error::NotFound => AppError::NotFound("record not found".to_string()),
            Error::DatabaseError(DatabaseErrorKind::UniqueViolation, ref info) => {
                AppError::Conflict(info.message().to_string())
            }
            Error::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, ref info) => {
                AppError::BadRequest(info.message().to_string())
            }
            Error::DatabaseError(DatabaseErrorKind::CheckViolation, ref info) => {
                AppError::BadRequest(info.message().to_string())
            }
            other => AppError::Internal(format!("database error: {other}")),
        }
    }
}

impl From<diesel::r2d2::PoolError> for AppError {
    fn from(err: diesel::r2d2::PoolError) -> Self {
        AppError::Internal(format!("connection pool error: {err}"))
    }
}

impl From<diesel::r2d2::Error> for AppError {
    fn from(err: diesel::r2d2::Error) -> Self {
        AppError::Internal(format!("connection error: {err}"))
    }
}

impl From<diesel::ConnectionError> for AppError {
    fn from(err: diesel::ConnectionError) -> Self {
        AppError::Internal(format!("failed to open database: {err}"))
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Internal(format!("{err:#}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::Internal(format!("json error: {err}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => AppError::NotFound(err.to_string()),
            std::io::ErrorKind::PermissionDenied => {
                AppError::Internal(format!("permission denied: {err}"))
            }
            _ => AppError::Internal(format!("io error: {err}")),
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        if err.status() == Some(reqwest::StatusCode::TOO_MANY_REQUESTS) {
            AppError::RateLimited(err.to_string())
        } else {
            AppError::Upstream(format!("http request failed: {err}"))
        }
    }
}

impl From<image::ImageError> for AppError {
    fn from(err: image::ImageError) -> Self {
        AppError::BadRequest(format!("unreadable image: {err}"))
    }
}

impl From<jsonwebtoken::errors::Error> for AppError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        AppError::Unauthorized(format!("invalid session token: {err}"))
    }
}

impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(err: axum::extract::multipart::MultipartError) -> Self {
        AppError::BadRequest(format!("malformed multipart body: {err}"))
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(err: tokio::task::JoinError) -> Self {
        AppError::Internal(format!("background task failed: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_match_variants() {
        assert_eq!(AppError::NotFound("x".into()).status(), StatusCode::NOT_FOUND);
        assert_eq!(
            AppError::Conflict("x".into()).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            AppError::RateLimited("x".into()).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn diesel_not_found_maps_to_404() {
        let err: AppError = diesel::result::Error::NotFound.into();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn only_transient_failures_are_retryable() {
        assert!(AppError::RateLimited("quota".into()).is_retryable());
        assert!(AppError::Upstream("timeout".into()).is_retryable());
        assert!(!AppError::BadRequest("nope".into()).is_retryable());
    }
}
