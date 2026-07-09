//! RFC 9457 `application/problem+json` error responses.
//!
//! Plugins repeatedly hand-rolled the same `problem+json` body + content-type.
//! [`ProblemDetails`] builds that consistently and converts to a [`Response`].
//!
//! ```
//! use barbacane_plugin_sdk::errors::ProblemDetails;
//! let resp = ProblemDetails::new(403, "urn:barbacane:error:acl-denied", "Forbidden")
//!     .detail("Access denied by ACL policy")
//!     .with("consumer", "alice")
//!     .into_response();
//! assert_eq!(resp.status, 403);
//! assert_eq!(resp.headers.get("content-type").unwrap(), "application/problem+json");
//! ```

use std::collections::BTreeMap;

use crate::types::Response;

/// Builder for an RFC 9457 problem-details error response.
#[derive(Debug, Clone)]
pub struct ProblemDetails {
    status: u16,
    type_uri: String,
    title: String,
    detail: Option<String>,
    /// Extra members merged into the problem object (RFC 9457 §3.2).
    extensions: BTreeMap<String, serde_json::Value>,
}

impl ProblemDetails {
    /// Start a problem with the given HTTP status, `type` URI, and title.
    pub fn new(status: u16, type_uri: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            status,
            type_uri: type_uri.into(),
            title: title.into(),
            detail: None,
            extensions: BTreeMap::new(),
        }
    }

    /// Set the human-readable `detail` member.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Add an extension member (any JSON-serializable value).
    pub fn with(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.extensions.insert(key.into(), value.into());
        self
    }

    /// Serialize to the problem-details JSON object.
    pub fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), self.type_uri.clone().into());
        obj.insert("title".into(), self.title.clone().into());
        obj.insert("status".into(), self.status.into());
        if let Some(detail) = &self.detail {
            obj.insert("detail".into(), detail.clone().into());
        }
        for (k, v) in &self.extensions {
            obj.insert(k.clone(), v.clone());
        }
        serde_json::Value::Object(obj)
    }

    /// Build the `Response`, setting status, `content-type: application/problem+json`,
    /// and the serialized body. Extra response headers can be added afterward.
    pub fn into_response(self) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );
        // A problem object always serializes; fall back to a minimal body if not.
        let body = serde_json::to_vec(&self.to_json()).unwrap_or_else(|_| {
            format!(
                r#"{{"type":"{}","title":"{}","status":{}}}"#,
                self.type_uri, self.title, self.status
            )
            .into_bytes()
        });
        Response {
            status: self.status,
            headers,
            body: Some(body),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_problem_response() {
        let resp = ProblemDetails::new(403, "urn:barbacane:error:acl-denied", "Forbidden")
            .detail("Access denied by ACL policy")
            .with("consumer", "alice")
            .into_response();

        assert_eq!(resp.status, 403);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:acl-denied");
        assert_eq!(body["title"], "Forbidden");
        assert_eq!(body["status"], 403);
        assert_eq!(body["detail"], "Access denied by ACL policy");
        assert_eq!(body["consumer"], "alice");
    }

    #[test]
    fn omits_detail_when_unset() {
        let json =
            ProblemDetails::new(429, "urn:barbacane:error:rate-limited", "Too Many Requests")
                .to_json();
        assert_eq!(json.get("detail"), None);
        assert_eq!(json["status"], 429);
    }
}
