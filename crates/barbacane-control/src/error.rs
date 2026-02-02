//! RFC 9457 Problem Details error responses.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

/// RFC 9457 Problem Details response.
#[derive(Debug, Clone, Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub error_type: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ValidationIssue>,
}

/// A single validation issue.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

impl ProblemDetails {
    /// Create a 404 Not Found error.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self {
            error_type: "urn:barbacane:error:not-found".into(),
            title: "Not Found".into(),
            status: 404,
            detail: Some(detail.into()),
            instance: None,
            errors: vec![],
        }
    }

    /// Create a 400 Bad Request error.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self {
            error_type: "urn:barbacane:error:bad-request".into(),
            title: "Bad Request".into(),
            status: 400,
            detail: Some(detail.into()),
            instance: None,
            errors: vec![],
        }
    }

    /// Create a 422 Unprocessable Entity error for validation failures.
    #[allow(dead_code)]
    pub fn validation_failed(errors: Vec<ValidationIssue>) -> Self {
        Self {
            error_type: "urn:barbacane:error:spec-invalid".into(),
            title: "Spec Validation Failed".into(),
            status: 422,
            detail: Some(format!("{} validation error(s)", errors.len())),
            instance: None,
            errors,
        }
    }

    /// Create a 409 Conflict error.
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self {
            error_type: "urn:barbacane:error:conflict".into(),
            title: "Conflict".into(),
            status: 409,
            detail: Some(detail.into()),
            instance: None,
            errors: vec![],
        }
    }

    /// Create a 500 Internal Server Error.
    pub fn internal_error() -> Self {
        Self {
            error_type: "urn:barbacane:error:internal-error".into(),
            title: "Internal Server Error".into(),
            status: 500,
            detail: None,
            instance: None,
            errors: vec![],
        }
    }

    /// Create a 500 Internal Server Error with details.
    pub fn internal_error_with_detail(detail: impl Into<String>) -> Self {
        Self {
            error_type: "urn:barbacane:error:internal-error".into(),
            title: "Internal Server Error".into(),
            status: 500,
            detail: Some(detail.into()),
            instance: None,
            errors: vec![],
        }
    }

    /// Create a 503 Service Unavailable error.
    #[allow(dead_code)]
    pub fn service_unavailable(detail: impl Into<String>) -> Self {
        Self {
            error_type: "urn:barbacane:error:service-unavailable".into(),
            title: "Service Unavailable".into(),
            status: 503,
            detail: Some(detail.into()),
            instance: None,
            errors: vec![],
        }
    }
}

impl IntoResponse for ProblemDetails {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut response = Json(&self).into_response();
        *response.status_mut() = status;
        response
            .headers_mut()
            .insert("content-type", "application/problem+json".parse().unwrap());
        response
    }
}

/// Convert database errors to ProblemDetails.
impl From<sqlx::Error> for ProblemDetails {
    fn from(err: sqlx::Error) -> Self {
        tracing::error!(error = %err, "database error");
        match err {
            sqlx::Error::RowNotFound => Self::not_found("resource not found"),
            sqlx::Error::Database(db_err) => {
                if db_err.is_unique_violation() {
                    Self::conflict("resource already exists")
                } else if db_err.is_foreign_key_violation() {
                    Self::conflict("referenced resource does not exist")
                } else {
                    Self::internal_error()
                }
            }
            _ => Self::internal_error(),
        }
    }
}
