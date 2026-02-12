//! HTTP Basic authentication middleware plugin for Barbacane API gateway.
//!
//! Validates credentials from the `Authorization: Basic` header (RFC 7617)
//! and rejects unauthenticated requests with 401 Unauthorized.

use barbacane_plugin_sdk::prelude::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use std::collections::BTreeMap;

/// HTTP Basic authentication middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct BasicAuth {
    /// The authentication realm shown in the WWW-Authenticate challenge.
    #[serde(default = "default_realm")]
    realm: String,

    /// Remove the Authorization header before forwarding to upstream.
    #[serde(default = "default_strip_credentials")]
    strip_credentials: bool,

    /// Map of username to credential entry (password and optional roles).
    #[serde(default)]
    credentials: BTreeMap<String, CredentialEntry>,
}

/// Credential entry for a single user.
#[derive(Debug, Clone, Deserialize)]
struct CredentialEntry {
    /// Password for this user.
    password: String,

    /// Optional roles/permissions for this user.
    #[serde(default)]
    roles: Vec<String>,
}

fn default_realm() -> String {
    "api".to_string()
}

fn default_strip_credentials() -> bool {
    true
}

/// Basic auth validation error.
#[derive(Debug)]
enum BasicAuthError {
    MissingAuthHeader,
    InvalidAuthHeader,
    InvalidBase64,
    InvalidCredentialFormat,
    InvalidCredentials,
}

impl BasicAuthError {
    fn as_str(&self) -> &'static str {
        match self {
            BasicAuthError::MissingAuthHeader => "missing_credentials",
            BasicAuthError::InvalidAuthHeader => "invalid_request",
            BasicAuthError::InvalidBase64 => "invalid_request",
            BasicAuthError::InvalidCredentialFormat => "invalid_request",
            BasicAuthError::InvalidCredentials => "invalid_credentials",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            BasicAuthError::MissingAuthHeader => "Basic credentials required",
            BasicAuthError::InvalidAuthHeader => "Invalid Authorization header format",
            BasicAuthError::InvalidBase64 => "Invalid base64 encoding in credentials",
            BasicAuthError::InvalidCredentialFormat => {
                "Invalid credentials format (expected user:password)"
            }
            BasicAuthError::InvalidCredentials => "Invalid username or password",
        }
    }
}

impl BasicAuth {
    /// Handle incoming request - validate Basic credentials.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        match self.validate_request(&req) {
            Ok((username, entry)) => {
                let mut modified_req = req;

                // Strip Authorization header before forwarding
                if self.strip_credentials {
                    modified_req.headers.remove("authorization");
                    modified_req.headers.remove("Authorization");
                }

                // Inject auth context headers for downstream use
                modified_req
                    .headers
                    .insert("x-auth-user".to_string(), username.clone());
                if !entry.roles.is_empty() {
                    let roles_csv = entry.roles.join(",");
                    modified_req
                        .headers
                        .insert("x-auth-roles".to_string(), roles_csv.clone());
                    modified_req
                        .headers
                        .insert("x-auth-consumer-groups".to_string(), roles_csv);
                }

                // Standard consumer header for ACL and downstream middlewares
                modified_req
                    .headers
                    .insert("x-auth-consumer".to_string(), username);

                Action::Continue(modified_req)
            }
            Err(e) => Action::ShortCircuit(self.unauthorized_response(&e)),
        }
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Validate the Basic credentials in the request.
    fn validate_request(
        &self,
        req: &Request,
    ) -> Result<(String, &CredentialEntry), BasicAuthError> {
        let (username, password) = self.extract_credentials(req)?;
        let entry = self
            .credentials
            .get(&username)
            .ok_or(BasicAuthError::InvalidCredentials)?;
        if entry.password != password {
            return Err(BasicAuthError::InvalidCredentials);
        }
        Ok((username, entry))
    }

    /// Extract username and password from the Authorization header.
    fn extract_credentials(&self, req: &Request) -> Result<(String, String), BasicAuthError> {
        // Try exact match first, then case-insensitive
        let auth_header = req
            .headers
            .get("authorization")
            .or_else(|| req.headers.get("Authorization"))
            .ok_or(BasicAuthError::MissingAuthHeader)?;

        // Must start with "Basic " (case-insensitive)
        if !auth_header.starts_with("Basic ") && !auth_header.starts_with("basic ") {
            return Err(BasicAuthError::InvalidAuthHeader);
        }

        // Decode base64 (standard encoding per RFC 7617)
        let encoded = auth_header[6..].trim();
        let decoded_bytes = STANDARD
            .decode(encoded)
            .map_err(|_| BasicAuthError::InvalidBase64)?;
        let decoded =
            String::from_utf8(decoded_bytes).map_err(|_| BasicAuthError::InvalidBase64)?;

        // Split on first ':' — password may contain colons
        let (username, password) = decoded
            .split_once(':')
            .ok_or(BasicAuthError::InvalidCredentialFormat)?;

        if username.is_empty() {
            return Err(BasicAuthError::InvalidCredentialFormat);
        }

        Ok((username.to_string(), password.to_string()))
    }

    /// Generate a 401 Unauthorized response.
    fn unauthorized_response(&self, error: &BasicAuthError) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());

        // RFC 7617: WWW-Authenticate: Basic realm="..."
        let www_auth = format!(
            "Basic realm=\"{}\", error=\"{}\", error_description=\"{}\"",
            self.realm,
            error.as_str(),
            error.description()
        );
        headers.insert("www-authenticate".to_string(), www_auth);

        let body = serde_json::json!({
            "type": "urn:barbacane:error:authentication-failed",
            "title": "Authentication failed",
            "status": 401,
            "detail": error.description()
        });

        Response {
            status: 401,
            headers,
            body: Some(body.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};

    /// Helper: build a BasicAuth instance with test credentials.
    fn test_plugin() -> BasicAuth {
        let mut credentials = BTreeMap::new();
        credentials.insert(
            "admin".to_string(),
            CredentialEntry {
                password: "secret123".to_string(),
                roles: vec!["admin".to_string(), "editor".to_string()],
            },
        );
        credentials.insert(
            "reader".to_string(),
            CredentialEntry {
                password: "readonly456".to_string(),
                roles: vec![],
            },
        );
        BasicAuth {
            realm: "test-api".to_string(),
            strip_credentials: true,
            credentials,
        }
    }

    /// Helper: build a minimal request with the given headers.
    fn request_with_headers(headers: BTreeMap<String, String>) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            query: None,
            headers,
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    /// Helper: build an Authorization: Basic header value.
    fn basic_header(user: &str, pass: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{}:{}", user, pass)))
    }

    // ==================== extract_credentials ====================

    #[test]
    fn extract_valid_credentials() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            basic_header("admin", "secret123"),
        );
        let req = request_with_headers(headers);

        let (user, pass) = plugin.extract_credentials(&req).unwrap();
        assert_eq!(user, "admin");
        assert_eq!(pass, "secret123");
    }

    #[test]
    fn extract_credentials_case_insensitive_header() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "Authorization".to_string(),
            basic_header("admin", "secret123"),
        );
        let req = request_with_headers(headers);

        let (user, _) = plugin.extract_credentials(&req).unwrap();
        assert_eq!(user, "admin");
    }

    #[test]
    fn extract_credentials_password_with_colons() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            format!("Basic {}", STANDARD.encode("user:pass:with:colons")),
        );
        let req = request_with_headers(headers);

        let (user, pass) = plugin.extract_credentials(&req).unwrap();
        assert_eq!(user, "user");
        assert_eq!(pass, "pass:with:colons");
    }

    #[test]
    fn extract_credentials_missing_header() {
        let plugin = test_plugin();
        let req = request_with_headers(BTreeMap::new());

        let err = plugin.extract_credentials(&req).unwrap_err();
        assert_eq!(err.as_str(), "missing_credentials");
    }

    #[test]
    fn extract_credentials_bearer_token_rejected() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer some-jwt-token".to_string(),
        );
        let req = request_with_headers(headers);

        let err = plugin.extract_credentials(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_request");
    }

    #[test]
    fn extract_credentials_invalid_base64() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Basic !!!not-base64!!!".to_string(),
        );
        let req = request_with_headers(headers);

        let err = plugin.extract_credentials(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_request");
    }

    #[test]
    fn extract_credentials_no_colon_separator() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            format!("Basic {}", STANDARD.encode("usernameonly")),
        );
        let req = request_with_headers(headers);

        let err = plugin.extract_credentials(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_request");
    }

    #[test]
    fn extract_credentials_empty_username() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            format!("Basic {}", STANDARD.encode(":password")),
        );
        let req = request_with_headers(headers);

        let err = plugin.extract_credentials(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_request");
    }

    // ==================== validate_request ====================

    #[test]
    fn validate_valid_credentials() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            basic_header("admin", "secret123"),
        );
        let req = request_with_headers(headers);

        let (user, entry) = plugin.validate_request(&req).unwrap();
        assert_eq!(user, "admin");
        assert_eq!(entry.roles, vec!["admin", "editor"]);
    }

    #[test]
    fn validate_wrong_password() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert("authorization".to_string(), basic_header("admin", "wrong"));
        let req = request_with_headers(headers);

        let err = plugin.validate_request(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_credentials");
    }

    #[test]
    fn validate_unknown_user() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert("authorization".to_string(), basic_header("unknown", "pass"));
        let req = request_with_headers(headers);

        let err = plugin.validate_request(&req).unwrap_err();
        assert_eq!(err.as_str(), "invalid_credentials");
    }

    // ==================== on_request ====================

    #[test]
    fn on_request_success_strips_auth_header() {
        let mut plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            basic_header("admin", "secret123"),
        );
        let req = request_with_headers(headers);

        match plugin.on_request(req) {
            Action::Continue(modified) => {
                assert!(!modified.headers.contains_key("authorization"));
                assert!(!modified.headers.contains_key("Authorization"));
                assert_eq!(modified.headers.get("x-auth-user").unwrap(), "admin");
                assert_eq!(
                    modified.headers.get("x-auth-roles").unwrap(),
                    "admin,editor"
                );
                assert_eq!(modified.headers.get("x-auth-consumer").unwrap(), "admin");
                assert_eq!(
                    modified.headers.get("x-auth-consumer-groups").unwrap(),
                    "admin,editor"
                );
            }
            Action::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[test]
    fn on_request_success_no_roles_for_reader() {
        let mut plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            basic_header("reader", "readonly456"),
        );
        let req = request_with_headers(headers);

        match plugin.on_request(req) {
            Action::Continue(modified) => {
                assert_eq!(modified.headers.get("x-auth-user").unwrap(), "reader");
                assert!(!modified.headers.contains_key("x-auth-roles"));
                assert_eq!(modified.headers.get("x-auth-consumer").unwrap(), "reader");
                assert!(!modified.headers.contains_key("x-auth-consumer-groups"));
            }
            Action::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[test]
    fn on_request_preserves_auth_header_when_strip_disabled() {
        let mut plugin = test_plugin();
        plugin.strip_credentials = false;
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            basic_header("admin", "secret123"),
        );
        let req = request_with_headers(headers);

        match plugin.on_request(req) {
            Action::Continue(modified) => {
                assert!(modified.headers.contains_key("authorization"));
            }
            Action::ShortCircuit(_) => panic!("expected Continue"),
        }
    }

    #[test]
    fn on_request_missing_header_returns_401() {
        let mut plugin = test_plugin();
        let req = request_with_headers(BTreeMap::new());

        match plugin.on_request(req) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 401);
                assert!(resp
                    .headers
                    .get("www-authenticate")
                    .unwrap()
                    .contains("test-api"));
            }
            Action::Continue(_) => panic!("expected ShortCircuit"),
        }
    }

    // ==================== unauthorized_response ====================

    #[test]
    fn unauthorized_response_format() {
        let plugin = test_plugin();
        let resp = plugin.unauthorized_response(&BasicAuthError::InvalidCredentials);

        assert_eq!(resp.status, 401);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/json"
        );

        let www_auth = resp.headers.get("www-authenticate").unwrap();
        assert!(www_auth.contains("Basic realm=\"test-api\""));
        assert!(www_auth.contains("invalid_credentials"));

        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["status"], 401);
        assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
    }

    // ==================== config deserialization ====================

    #[test]
    fn config_defaults() {
        let config: BasicAuth = serde_json::from_str("{}").unwrap();
        assert_eq!(config.realm, "api");
        assert!(config.strip_credentials);
        assert!(config.credentials.is_empty());
    }

    #[test]
    fn config_full() {
        let json = r#"{
            "realm": "myapp",
            "strip_credentials": false,
            "credentials": {
                "alice": { "password": "p@ss", "roles": ["admin"] },
                "bob": { "password": "secret" }
            }
        }"#;
        let config: BasicAuth = serde_json::from_str(json).unwrap();
        assert_eq!(config.realm, "myapp");
        assert!(!config.strip_credentials);
        assert_eq!(config.credentials.len(), 2);
        assert_eq!(config.credentials["alice"].roles, vec!["admin"]);
        assert!(config.credentials["bob"].roles.is_empty());
    }
}
