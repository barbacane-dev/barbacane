//! Integration tests for authentication plugins — JWT, API key, basic auth, OAuth2, secrets.
//!
//! Run with: `cargo test -p barbacane-test`

use barbacane_test::TestGateway;

fn fixture(name: &str) -> String {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

// JWT Authentication Tests

/// Helper to create a JWT token for testing.
/// Creates unsigned tokens since we use skip_signature_validation: true in tests.
fn create_test_jwt(sub: &str, iss: &str, aud: &str, exp: u64, nbf: Option<u64>) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let mut claims = serde_json::json!({
        "sub": sub,
        "iss": iss,
        "aud": aud,
        "exp": exp
    });
    if let Some(nbf_val) = nbf {
        claims["nbf"] = serde_json::json!(nbf_val);
    }

    let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());
    let claims_b64 = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
    // Signature is just filler since we skip validation
    let sig_b64 = URL_SAFE_NO_PAD.encode(b"test_signature");

    format!("{}.{}.{}", header_b64, claims_b64, sig_b64)
}

/// Get current Unix timestamp.
fn now_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn test_jwt_auth_missing_token() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Request without Authorization header should get 401
    let resp = gateway.get("/protected").await.unwrap();
    assert_eq!(resp.status(), 401);

    // Check WWW-Authenticate header
    let www_auth = resp.headers().get("www-authenticate");
    assert!(www_auth.is_some(), "expected WWW-Authenticate header");
    let www_auth_val = www_auth.unwrap().to_str().unwrap();
    assert!(
        www_auth_val.contains("Bearer"),
        "expected Bearer scheme in WWW-Authenticate"
    );
}

#[tokio::test]
async fn test_jwt_auth_invalid_header_format() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Request with invalid Authorization format should get 401
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", "Basic dXNlcjpwYXNz")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_jwt_auth_malformed_token() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Request with malformed token should get 401
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", "Bearer not.a.valid.jwt")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_jwt_auth_valid_token() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Create a valid token
    let exp = now_timestamp() + 3600; // Expires in 1 hour
    let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, None);

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Access granted");
}

#[tokio::test]
async fn test_jwt_auth_expired_token() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Create an expired token (expired 2 minutes ago, beyond 60s clock skew)
    let exp = now_timestamp() - 120;
    let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, None);

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Check error response body
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("expired"));
}

#[tokio::test]
async fn test_jwt_auth_not_yet_valid() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Create a token that's not valid yet (starts 2 minutes from now)
    let exp = now_timestamp() + 3600;
    let nbf = now_timestamp() + 120;
    let token = create_test_jwt("user-123", "test-issuer", "test-audience", exp, Some(nbf));

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"]
        .as_str()
        .unwrap_or("")
        .contains("not yet valid"));
}

#[tokio::test]
async fn test_jwt_auth_invalid_issuer() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Create a token with wrong issuer
    let exp = now_timestamp() + 3600;
    let token = create_test_jwt("user-123", "wrong-issuer", "test-audience", exp, None);

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("issuer"));
}

#[tokio::test]
async fn test_jwt_auth_invalid_audience() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Create a token with wrong audience
    let exp = now_timestamp() + 3600;
    let token = create_test_jwt("user-123", "test-issuer", "wrong-audience", exp, None);

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("audience"));
}

#[tokio::test]
async fn test_jwt_auth_public_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("jwt-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Public endpoint should work without a token
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Public access");
}

// ==================== API Key Auth Tests ====================

#[tokio::test]
async fn test_apikey_auth_valid_key() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("X-API-Key", "test-key-123")
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("Expected 200 but got {}. Body: {}", status, body);
    }
}

#[tokio::test]
async fn test_apikey_auth_missing_key() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Request without API key
    let resp = gateway.get("/protected").await.unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("required"));
}

#[tokio::test]
async fn test_apikey_auth_invalid_key() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("X-API-Key", "wrong-key")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("Invalid"));
}

#[tokio::test]
async fn test_apikey_auth_public_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Public endpoint should work without a key
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Public access");
}

#[tokio::test]
async fn test_apikey_auth_query_param_valid() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .get("/query-auth?api_key=query-key-789")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Query auth granted");
}

#[tokio::test]
async fn test_apikey_auth_query_param_missing() {
    let gateway = TestGateway::from_spec(&fixture("apikey-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Missing query param
    let resp = gateway.get("/query-auth").await.unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("required"));
}

// ==================== Basic Auth Tests ====================

/// Encode username:password as base64 for Basic auth header.
fn basic_auth_header(username: &str, password: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let encoded = STANDARD.encode(format!("{}:{}", username, password));
    format!("Basic {}", encoded)
}

#[tokio::test]
async fn test_basic_auth_valid_credentials() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic_auth_header("admin", "secret123"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        panic!("Expected 200 but got {}. Body: {}", status, body);
    }
}

#[tokio::test]
async fn test_basic_auth_valid_credentials_reader() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic_auth_header("reader", "readonly456"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_basic_auth_missing_header() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/protected").await.unwrap();
    assert_eq!(resp.status(), 401);

    // Verify WWW-Authenticate header
    let www_auth = resp
        .headers()
        .get("www-authenticate")
        .expect("missing WWW-Authenticate");
    let www_auth_str = www_auth.to_str().unwrap();
    assert!(www_auth_str.starts_with("Basic realm=\"test-api\""));

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:barbacane:error:authentication-failed");
    assert!(body["detail"]
        .as_str()
        .unwrap_or("")
        .contains("credentials required"));
}

#[tokio::test]
async fn test_basic_auth_invalid_credentials() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", basic_auth_header("admin", "wrongpassword"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"]
        .as_str()
        .unwrap_or("")
        .contains("Invalid username or password"));
}

#[tokio::test]
async fn test_basic_auth_unknown_user() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header(
            "Authorization",
            basic_auth_header("nonexistent", "anything"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_basic_auth_malformed_header() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    // Wrong auth scheme
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", "Bearer some-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn test_basic_auth_public_endpoint() {
    let gateway = TestGateway::from_spec(&fixture("basic-auth.yaml"))
        .await
        .expect("failed to start gateway");

    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Public access");
}

// ==================== OAuth2 Auth Tests ====================

/// Create a temporary spec file for OAuth2 auth testing with dynamic introspection URL.
fn create_oauth2_spec(introspection_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("oauth2-auth.yaml");

    // Get absolute paths to plugins (relative to this test file's location)
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let mock_path = plugins_dir.join("mock/mock.wasm");
    let oauth2_path = plugins_dir.join("oauth2-auth/oauth2-auth.wasm");

    // Create barbacane.yaml manifest in temp dir
    let manifest_path = temp_dir.path().join("barbacane.yaml");
    let manifest_content = format!(
        r#"# Test manifest for OAuth2 auth tests

plugins:
  mock:
    path: {}
  oauth2-auth:
    path: {}
"#,
        mock_path.display(),
        oauth2_path.display()
    );
    std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest file");

    let spec_content = format!(
        r#"openapi: "3.0.3"
info:
  title: OAuth2 Auth Test API
  version: "1.0.0"
  description: API for testing OAuth2 token introspection middleware

x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      introspection_endpoint: "{}"
      client_id: "test-client"
      client_secret: "test-secret"
      timeout: 5

paths:
  /protected:
    get:
      summary: Protected endpoint requiring OAuth2 auth
      operationId: getProtected
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Access granted"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
        "401":
          description: Unauthorized
        "403":
          description: Forbidden

  /scoped:
    get:
      summary: Endpoint requiring specific scopes
      operationId: getScoped
      x-barbacane-middlewares:
        - name: oauth2-auth
          config:
            introspection_endpoint: "{}"
            client_id: "test-client"
            client_secret: "test-secret"
            required_scopes: "read write"
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Scoped access granted"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
        "403":
          description: Forbidden

  /public:
    get:
      summary: Public endpoint (no auth)
      operationId: getPublic
      x-barbacane-middlewares: []
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "Public access"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
        introspection_url, introspection_url
    );

    std::fs::write(&spec_path, spec_content).expect("failed to write spec file");
    (temp_dir, spec_path)
}

#[tokio::test]
async fn test_oauth2_auth_valid_token() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Start mock introspection server
    let mock_server = MockServer::start().await;

    // Mock active token response
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .and(body_string_contains("token=valid-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "sub": "user-123",
            "scope": "read write",
            "client_id": "my-client"
        })))
        .mount(&mock_server)
        .await;

    let introspection_url = format!("{}/introspect", mock_server.uri());
    let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", "Bearer valid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Access granted");
}

#[tokio::test]
async fn test_oauth2_auth_missing_token() {
    use wiremock::MockServer;

    // Start mock introspection server (won't be called)
    let mock_server = MockServer::start().await;
    let introspection_url = format!("{}/introspect", mock_server.uri());
    let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Request without token
    let resp = gateway.get("/protected").await.unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"]
        .as_str()
        .unwrap_or("")
        .contains("Bearer token required"));
}

#[tokio::test]
async fn test_oauth2_auth_inactive_token() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Mock inactive token response
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": false
        })))
        .mount(&mock_server)
        .await;

    let introspection_url = format!("{}/introspect", mock_server.uri());
    let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .request_builder(reqwest::Method::GET, "/protected")
        .header("Authorization", "Bearer invalid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("not active"));
}

#[tokio::test]
async fn test_oauth2_auth_insufficient_scope() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let mock_server = MockServer::start().await;

    // Mock active token with only "read" scope (missing "write")
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "sub": "user-123",
            "scope": "read"  // Missing "write" scope
        })))
        .mount(&mock_server)
        .await;

    let introspection_url = format!("{}/introspect", mock_server.uri());
    let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Access scoped endpoint (requires "read write")
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/scoped")
        .header("Authorization", "Bearer token-with-read-only")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap_or("").contains("scope"));
}

#[tokio::test]
async fn test_oauth2_auth_public_endpoint() {
    use wiremock::MockServer;

    let mock_server = MockServer::start().await;
    let introspection_url = format!("{}/introspect", mock_server.uri());
    let (_temp_dir, spec_path) = create_oauth2_spec(&introspection_url);

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Public endpoint should work without a token
    let resp = gateway.get("/public").await.unwrap();
    assert_eq!(resp.status(), 200);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["message"], "Public access");
}

// ==================== Secrets Tests ====================

/// Create a temporary spec file for secrets testing using oauth2-auth.
/// The oauth2-auth plugin has secrets as values (client_secret), making it ideal for testing.
fn create_oauth2_secrets_spec(
    temp_dir: &std::path::Path,
    introspection_url: &str,
    client_secret: &str,
) -> std::path::PathBuf {
    let spec_path = temp_dir.join("secrets-test.yaml");

    // Get absolute paths to plugins
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let mock_path = plugins_dir.join("mock/mock.wasm");
    let oauth2_path = plugins_dir.join("oauth2-auth/oauth2-auth.wasm");

    // Create barbacane.yaml manifest
    let manifest_path = temp_dir.join("barbacane.yaml");
    let manifest_content = format!(
        r#"plugins:
  mock:
    path: {}
  oauth2-auth:
    path: {}
"#,
        mock_path.display(),
        oauth2_path.display()
    );
    std::fs::write(&manifest_path, manifest_content).expect("failed to write manifest");

    // Create the spec with the provided client_secret (which may be a secret reference)
    let spec_content = format!(
        r#"openapi: "3.0.3"
info:
  title: Secrets Test API
  version: "1.0.0"

paths:
  /test:
    get:
      summary: Test endpoint
      operationId: test
      x-barbacane-middlewares:
        - name: oauth2-auth
          config:
            introspection_endpoint: "{}"
            client_id: test-client
            client_secret: "{}"
            timeout: 5.0
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"message": "success"}}'
          content_type: application/json
      responses:
        "200":
          description: Success
"#,
        introspection_url, client_secret
    );

    std::fs::write(&spec_path, spec_content).expect("failed to write spec");
    spec_path
}

#[tokio::test]
async fn test_secrets_env_reference_resolved() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Set up environment variable for the secret
    std::env::set_var("TEST_CLIENT_SECRET", "my-secret-value");

    // Start mock introspection server
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "sub": "user-123"
        })))
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let introspection_url = format!("{}/introspect", mock_server.uri());
    let spec_path = create_oauth2_secrets_spec(
        temp_dir.path(),
        &introspection_url,
        "env://TEST_CLIENT_SECRET",
    );

    // Gateway should start successfully with the resolved secret
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway with env secret");

    // Make a request with a valid token
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/test")
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Clean up
    std::env::remove_var("TEST_CLIENT_SECRET");
}

#[tokio::test]
async fn test_secrets_file_reference_resolved() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Start mock introspection server
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true,
            "sub": "user-123"
        })))
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");

    // Create a secret file
    let secret_file = temp_dir.path().join("client-secret.txt");
    std::fs::write(&secret_file, "file-based-secret\n").expect("failed to write secret file");

    // Use file:// reference
    let secret_ref = format!("file://{}", secret_file.display());
    let introspection_url = format!("{}/introspect", mock_server.uri());
    let spec_path = create_oauth2_secrets_spec(temp_dir.path(), &introspection_url, &secret_ref);

    // Gateway should start successfully with the resolved secret
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway with file secret");

    // Make a request with a valid token
    let resp = gateway
        .request_builder(reqwest::Method::GET, "/test")
        .header("Authorization", "Bearer test-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_secrets_missing_env_var_fails_startup() {
    // Make sure the env var doesn't exist
    std::env::remove_var("NONEXISTENT_SECRET_VAR_12345");

    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = create_oauth2_secrets_spec(
        temp_dir.path(),
        "http://localhost:9999/introspect",
        "env://NONEXISTENT_SECRET_VAR_12345",
    );

    // Gateway should fail to start
    let result = TestGateway::from_spec(spec_path.to_str().unwrap()).await;
    match result {
        Ok(_) => {
            panic!("gateway should fail with missing env var");
        }
        Err(e) => {
            let err_str = format!("{}", e);
            assert!(
                err_str.contains("secret")
                    || err_str.contains("environment")
                    || err_str.contains("not found"),
                "error should mention secrets or environment: {}",
                err_str
            );
        }
    }
}

#[tokio::test]
async fn test_secrets_missing_file_fails_startup() {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = create_oauth2_secrets_spec(
        temp_dir.path(),
        "http://localhost:9999/introspect",
        "file:///nonexistent/path/to/secret.txt",
    );

    // Gateway should fail to start
    let result = TestGateway::from_spec(spec_path.to_str().unwrap()).await;
    match result {
        Ok(_) => panic!("gateway should fail with missing file"),
        Err(e) => {
            let err_str = format!("{}", e);
            assert!(
                err_str.contains("secret")
                    || err_str.contains("file")
                    || err_str.contains("not found"),
                "error should mention secrets or file: {}",
                err_str
            );
        }
    }
}
