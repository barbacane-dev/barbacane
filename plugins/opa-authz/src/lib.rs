//! OPA authorization middleware plugin for Barbacane API gateway.
//!
//! Evaluates access control policies via the Open Policy Agent REST API.
//! Typically placed after an authentication middleware (jwt-auth, oauth2-auth, etc.)
//! in the middleware chain so that auth claims are available as OPA input.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// OPA authorization middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct OpaAuthz {
    /// OPA Data API endpoint URL (e.g., `http://opa:8181/v1/data/authz/allow`).
    opa_url: String,

    /// HTTP request timeout in seconds for OPA calls.
    #[serde(default = "default_timeout")]
    timeout: f64,

    /// Include the request body in the OPA input payload.
    #[serde(default)]
    include_body: bool,

    /// Include parsed `x-auth-claims` header in the OPA input payload.
    #[serde(default = "default_true")]
    include_claims: bool,

    /// Custom message returned in the 403 response when OPA denies access.
    #[serde(default = "default_deny_message")]
    deny_message: String,
}

fn default_timeout() -> f64 {
    5.0
}

fn default_true() -> bool {
    true
}

fn default_deny_message() -> String {
    "Authorization denied by policy".to_string()
}

// ---------------------------------------------------------------------------
// HTTP types for host_http_call
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    #[allow(dead_code)]
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// OPA input/output types
// ---------------------------------------------------------------------------

/// Input payload sent to OPA (wrapped in `{"input": ...}`).
#[derive(Serialize)]
struct OpaInput {
    method: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    headers: BTreeMap<String, String>,
    client_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    claims: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Serialize)]
struct OpaRequest {
    input: OpaInput,
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum OpaError {
    /// OPA service is unavailable or returned an error.
    ServiceError(String),
}

impl OpaError {
    fn status_code(&self) -> u16 {
        match self {
            OpaError::ServiceError(_) => 503,
        }
    }
}

// ---------------------------------------------------------------------------
// Middleware implementation
// ---------------------------------------------------------------------------

impl OpaAuthz {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.evaluate_policy(&req) {
            Ok(true) => Action::Continue(req),
            Ok(false) => Action::ShortCircuit(self.denied_response()),
            Err(e) => Action::ShortCircuit(self.error_response(&e)),
        }
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Build OPA input from the request and call the OPA Data API.
    /// Returns `Ok(true)` if allowed, `Ok(false)` if denied.
    fn evaluate_policy(&self, req: &Request) -> Result<bool, OpaError> {
        let opa_input = self.build_input(req);
        let opa_request = OpaRequest { input: opa_input };

        let request_json = serde_json::to_string(&opa_request)
            .map_err(|e| OpaError::ServiceError(format!("input serialization: {}", e)))?;

        let response_body = self.call_opa(&request_json)?;

        self.parse_decision(&response_body)
    }

    /// Construct the OPA input from request context.
    fn build_input(&self, req: &Request) -> OpaInput {
        let claims = if self.include_claims {
            req.headers
                .get("x-auth-claims")
                .and_then(|v| serde_json::from_str(v).ok())
        } else {
            None
        };

        let body = if self.include_body {
            req.body.clone()
        } else {
            None
        };

        OpaInput {
            method: req.method.clone(),
            path: req.path.clone(),
            query: req.query.clone(),
            headers: req.headers.clone(),
            client_ip: req.client_ip.clone(),
            claims,
            body,
        }
    }

    /// POST to the OPA endpoint and return the raw response body.
    fn call_opa(&self, request_json: &str) -> Result<Vec<u8>, OpaError> {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("accept".to_string(), "application/json".to_string());

        let http_request = HttpRequest {
            method: "POST".to_string(),
            url: self.opa_url.clone(),
            headers,
            body: Some(request_json.to_string()),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

        let serialized = serde_json::to_vec(&http_request)
            .map_err(|e| OpaError::ServiceError(format!("request serialization: {}", e)))?;

        let result_len =
            unsafe { host_http_call(serialized.as_ptr() as i32, serialized.len() as i32) };

        if result_len < 0 {
            return Err(OpaError::ServiceError(
                "OPA service unreachable".to_string(),
            ));
        }

        let mut buf = vec![0u8; result_len as usize];
        let bytes_read = unsafe { host_http_read_result(buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return Err(OpaError::ServiceError(
                "failed to read OPA response".to_string(),
            ));
        }

        let http_response: HttpResponse = serde_json::from_slice(&buf[..bytes_read as usize])
            .map_err(|e| OpaError::ServiceError(format!("invalid response format: {}", e)))?;

        if http_response.status != 200 {
            return Err(OpaError::ServiceError(format!(
                "OPA returned status {}",
                http_response.status
            )));
        }

        http_response
            .body
            .ok_or_else(|| OpaError::ServiceError("empty OPA response body".to_string()))
    }

    /// Parse the OPA decision from the response body.
    ///
    /// OPA Data API returns `{"result": <value>}` where `<value>` is the
    /// evaluation result of the queried document. We treat the result as a
    /// boolean: `true` means allow, anything else means deny.
    ///
    /// If the queried document is undefined (no matching rule), OPA returns
    /// `{}` with no `result` key — which we treat as deny.
    fn parse_decision(&self, body: &[u8]) -> Result<bool, OpaError> {
        let response: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| OpaError::ServiceError(format!("invalid OPA JSON: {}", e)))?;

        match response.get("result") {
            Some(serde_json::Value::Bool(allowed)) => Ok(*allowed),
            // OPA may return `{"result": {}}` for partial rules — treat non-bool as deny
            Some(_) => Ok(false),
            // No `result` key means the document is undefined — deny
            None => Ok(false),
        }
    }

    /// 403 Forbidden response for policy denial.
    fn denied_response(&self) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:opa-denied",
            "title": "Forbidden",
            "status": 403,
            "detail": self.deny_message
        });

        Response {
            status: 403,
            headers,
            body: Some(body.to_string()),
        }
    }

    /// 503 Service Unavailable response for OPA errors.
    fn error_response(&self, error: &OpaError) -> Response {
        let status = error.status_code();
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let detail = match error {
            OpaError::ServiceError(msg) => msg.clone(),
        };

        let body = serde_json::json!({
            "type": "urn:barbacane:error:opa-unavailable",
            "title": "Service Unavailable",
            "status": status,
            "detail": detail
        });

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Host function declarations
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn create_config() -> OpaAuthz {
        OpaAuthz {
            opa_url: "http://opa:8181/v1/data/authz/allow".to_string(),
            timeout: 5.0,
            include_body: false,
            include_claims: true,
            deny_message: "Authorization denied by policy".to_string(),
        }
    }

    fn create_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("x-auth-consumer".to_string(), "alice".to_string());

        Request {
            method: "GET".to_string(),
            path: "/admin/users".to_string(),
            headers,
            body: None,
            query: Some("page=1".to_string()),
            path_params: BTreeMap::new(),
            client_ip: "10.0.0.1".to_string(),
        }
    }

    // --- Config deserialization ---

    #[test]
    fn config_deserialization_minimal() {
        let json = r#"{"opa_url": "http://opa:8181/v1/data/authz/allow"}"#;
        let config: OpaAuthz = serde_json::from_str(json).unwrap();

        assert_eq!(config.opa_url, "http://opa:8181/v1/data/authz/allow");
        assert_eq!(config.timeout, 5.0);
        assert!(!config.include_body);
        assert!(config.include_claims);
        assert_eq!(config.deny_message, "Authorization denied by policy");
    }

    #[test]
    fn config_deserialization_full() {
        let json = r#"{
            "opa_url": "http://localhost:8181/v1/data/my/policy",
            "timeout": 10,
            "include_body": true,
            "include_claims": false,
            "deny_message": "Access denied"
        }"#;
        let config: OpaAuthz = serde_json::from_str(json).unwrap();

        assert_eq!(config.opa_url, "http://localhost:8181/v1/data/my/policy");
        assert_eq!(config.timeout, 10.0);
        assert!(config.include_body);
        assert!(!config.include_claims);
        assert_eq!(config.deny_message, "Access denied");
    }

    #[test]
    fn config_deserialization_missing_required() {
        let json = r#"{"timeout": 5}"#;
        let result: Result<OpaAuthz, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- OPA input construction ---

    #[test]
    fn build_input_basic() {
        let config = create_config();
        let req = create_request();

        let input = config.build_input(&req);

        assert_eq!(input.method, "GET");
        assert_eq!(input.path, "/admin/users");
        assert_eq!(input.query, Some("page=1".to_string()));
        assert_eq!(input.client_ip, "10.0.0.1");
        assert!(input.claims.is_none()); // no x-auth-claims header
        assert!(input.body.is_none());
    }

    #[test]
    fn build_input_with_claims() {
        let config = create_config();
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"sub":"alice","roles":["admin"]}"#.to_string(),
        );

        let input = config.build_input(&req);

        let claims = input.claims.unwrap();
        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims["roles"][0], "admin");
    }

    #[test]
    fn build_input_claims_disabled() {
        let mut config = create_config();
        config.include_claims = false;

        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"sub":"alice"}"#.to_string(),
        );

        let input = config.build_input(&req);
        assert!(input.claims.is_none());
    }

    #[test]
    fn build_input_with_body() {
        let mut config = create_config();
        config.include_body = true;

        let mut req = create_request();
        req.body = Some(r#"{"name":"test"}"#.to_string());

        let input = config.build_input(&req);
        assert_eq!(input.body, Some(r#"{"name":"test"}"#.to_string()));
    }

    #[test]
    fn build_input_body_excluded_by_default() {
        let config = create_config();
        let mut req = create_request();
        req.body = Some(r#"{"name":"test"}"#.to_string());

        let input = config.build_input(&req);
        assert!(input.body.is_none());
    }

    #[test]
    fn build_input_invalid_claims_json_skipped() {
        let config = create_config();
        let mut req = create_request();
        req.headers
            .insert("x-auth-claims".to_string(), "not-valid-json".to_string());

        let input = config.build_input(&req);
        assert!(input.claims.is_none());
    }

    // --- OPA input serialization ---

    #[test]
    fn opa_request_serialization() {
        let config = create_config();
        let req = create_request();
        let input = config.build_input(&req);
        let opa_request = OpaRequest { input };

        let json = serde_json::to_string(&opa_request).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(parsed["input"]["method"].is_string());
        assert_eq!(parsed["input"]["method"], "GET");
        assert_eq!(parsed["input"]["path"], "/admin/users");
        // claims and body should be absent (skip_serializing_if)
        assert!(parsed["input"].get("claims").is_none());
        assert!(parsed["input"].get("body").is_none());
    }

    // --- Decision parsing ---

    #[test]
    fn parse_decision_allowed() {
        let config = create_config();
        let body = br#"{"result": true}"#;
        assert_eq!(config.parse_decision(body).unwrap(), true);
    }

    #[test]
    fn parse_decision_denied() {
        let config = create_config();
        let body = br#"{"result": false}"#;
        assert_eq!(config.parse_decision(body).unwrap(), false);
    }

    #[test]
    fn parse_decision_undefined_document() {
        let config = create_config();
        // OPA returns empty object when document is undefined
        let body = br#"{}"#;
        assert_eq!(config.parse_decision(body).unwrap(), false);
    }

    #[test]
    fn parse_decision_non_boolean_result() {
        let config = create_config();
        // Partial rule returning an object instead of boolean
        let body = br#"{"result": {"allow": true, "reason": "admin"}}"#;
        assert_eq!(config.parse_decision(body).unwrap(), false);
    }

    #[test]
    fn parse_decision_invalid_json() {
        let config = create_config();
        let body = b"not json";
        assert!(config.parse_decision(body).is_err());
    }

    // --- Error responses ---

    #[test]
    fn denied_response_format() {
        let config = create_config();
        let resp = config.denied_response();

        assert_eq!(resp.status, 403);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/problem+json"
        );

        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:opa-denied");
        assert_eq!(body["title"], "Forbidden");
        assert_eq!(body["status"], 403);
        assert_eq!(body["detail"], "Authorization denied by policy");
    }

    #[test]
    fn denied_response_custom_message() {
        let mut config = create_config();
        config.deny_message = "Custom deny message".to_string();

        let resp = config.denied_response();
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["detail"], "Custom deny message");
    }

    #[test]
    fn error_response_service_unavailable() {
        let config = create_config();
        let error = OpaError::ServiceError("connection refused".to_string());
        let resp = config.error_response(&error);

        assert_eq!(resp.status, 503);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:opa-unavailable");
        assert_eq!(body["title"], "Service Unavailable");
        assert_eq!(body["detail"], "connection refused");
    }

    // --- on_response passthrough ---

    #[test]
    fn on_response_passthrough() {
        let mut config = create_config();
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("ok".to_string()),
        };

        let result = config.on_response(response);
        assert_eq!(result.status, 200);
        assert_eq!(result.body, Some("ok".to_string()));
    }

    // --- Default values ---

    #[test]
    fn default_values() {
        assert_eq!(default_timeout(), 5.0);
        assert!(default_true());
        assert_eq!(default_deny_message(), "Authorization denied by policy");
    }

    // --- Error type status codes ---

    #[test]
    fn error_status_codes() {
        assert_eq!(
            OpaError::ServiceError("test".to_string()).status_code(),
            503
        );
    }
}
