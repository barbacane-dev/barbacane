//! RFC 9457 Problem Details error responses.

use axum::{
    http::{header, StatusCode},
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
}

/// Largest error body read to become a problem's `detail`.
const MAX_DETAIL_BYTES: usize = 4096;

/// Turn an error response that is not JSON (an extractor rejection, the body
/// limit, an unknown route or method) into problem+json, keeping its headers.
/// The text of a 4xx becomes the `detail`. A JSON body that does not fit the
/// operation is a 400, as the API's own validation errors are.
pub async fn problem_json_errors(response: Response) -> Response {
    let status = response.status();
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let media = value.split(';').next().unwrap_or_default().trim();
            media.eq_ignore_ascii_case("application/json")
                || media.eq_ignore_ascii_case("application/problem+json")
        });
    if !(status.is_client_error() || status.is_server_error()) || is_json {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let detail = axum::body::to_bytes(body, MAX_DETAIL_BYTES)
        .await
        .ok()
        .filter(|_| status.is_client_error())
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .filter(|text| !text.is_empty());
    let status = match status {
        StatusCode::UNPROCESSABLE_ENTITY => StatusCode::BAD_REQUEST,
        other => other,
    };
    let problem = ProblemDetails::for_status(status, detail);
    parts.headers.remove(header::CONTENT_TYPE);
    parts.headers.remove(header::CONTENT_LENGTH);
    let mut response = problem.into_response();
    response.headers_mut().extend(parts.headers);
    response
}

impl ProblemDetails {
    /// The problem for `status`, typed after it.
    fn for_status(status: StatusCode, detail: Option<String>) -> Self {
        let title = match status {
            StatusCode::PAYLOAD_TOO_LARGE => "Content Too Large",
            s => s.canonical_reason().unwrap_or("Error"),
        };
        let slug = match status {
            StatusCode::BAD_REQUEST => "bad-request".to_string(),
            StatusCode::NOT_FOUND => "not-found".to_string(),
            StatusCode::PAYLOAD_TOO_LARGE => "content-too-large".to_string(),
            s if s.is_server_error() => "internal-error".to_string(),
            _ => title.to_ascii_lowercase().replace(' ', "-"),
        };
        Self {
            error_type: format!("urn:barbacane:error:{slug}"),
            title: title.into(),
            status: status.as_u16(),
            detail,
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
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
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
                    // Foreign key violations can happen in two cases:
                    // 1. INSERT/UPDATE referencing a non-existent resource
                    // 2. DELETE when the resource is still referenced by other records
                    // Check the error message to provide a more specific error
                    let message = db_err.message();
                    if message.contains("update or delete")
                        || message.contains("is still referenced")
                    {
                        Self::conflict("resource is in use and cannot be deleted")
                    } else {
                        Self::conflict("referenced resource does not exist")
                    }
                } else {
                    Self::internal_error()
                }
            }
            _ => Self::internal_error(),
        }
    }
}
