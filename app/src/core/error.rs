//! One error type for the service functions. The API turns it into an RFC 9457 problem.

use super::http::Problem;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("No session or API key (`Authorization: Bearer mq_...`).")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    /// An engine or a metadata provider did not answer as expected.
    #[error("{0}")]
    Upstream(String),
    #[error("the database did not answer: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn not_found(what: impl Into<String>) -> Self {
        AppError::NotFound(what.into())
    }

    pub fn bad_request(why: impl Into<String>) -> Self {
        AppError::BadRequest(why.into())
    }

    /// The stable machine-readable code of the problem.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "singularr.not_found",
            AppError::BadRequest(_) => "singularr.invalid_request",
            AppError::Unauthorized => "singularr.unauthorized",
            AppError::Forbidden(_) => "singularr.forbidden",
            AppError::Upstream(_) => "singularr.upstream",
            AppError::Db(_) => "singularr.database",
            AppError::Internal(_) => "singularr.internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Upstream(_) => StatusCode::BAD_GATEWAY,
            AppError::Db(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn problem(&self) -> Problem {
        let title = match self {
            AppError::NotFound(_) => "Not found",
            AppError::BadRequest(_) => "Invalid request",
            AppError::Unauthorized => "Sign in required",
            AppError::Forbidden(_) => "Not permitted",
            AppError::Upstream(_) => "Service error",
            AppError::Db(_) => "Database unavailable",
            AppError::Internal(_) => "Internal error",
        };
        // Database and internal details stay in the log; the caller gets the code.
        let detail = match self {
            AppError::Db(_) | AppError::Internal(_) => None,
            other => Some(other.to_string()),
        };
        let mut p = Problem::new(self.status(), self.code(), title);
        p.detail = detail;
        p
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if matches!(self, AppError::Db(_) | AppError::Internal(_)) {
            tracing::error!(error = %self, "request failed");
        }
        self.problem().into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_carry_a_code_and_hide_internals() {
        let p = AppError::not_found("title 7 does not exist").problem();
        assert_eq!((p.status, p.code.as_str()), (404, "singularr.not_found"));
        assert_eq!(p.detail.as_deref(), Some("title 7 does not exist"));
        let p = AppError::Internal("stack trace".into()).problem();
        assert_eq!(p.status, 500);
        assert_eq!(p.detail, None);
        assert_eq!(AppError::Unauthorized.problem().status, 401);
    }
}
