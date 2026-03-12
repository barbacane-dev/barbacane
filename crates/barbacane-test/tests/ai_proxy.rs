//! Integration tests for the ai-proxy dispatcher plugin.
//!
//! Uses wiremock to simulate OpenAI-compatible LLM API responses without
//! real network calls. The plugin is configured with a custom `base_url`
//! pointing at the local mock server.
//!
//! Run with: `cargo test -p barbacane-test`

use barbacane_test::TestGateway;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal OpenAI-compatible chat completion response body.
const OPENAI_COMPLETION_BODY: &str = r#"{
  "id": "chatcmpl-test",
  "object": "chat.completion",
  "created": 1700000000,
  "model": "llama3",
  "choices": [{
    "index": 0,
    "message": { "role": "assistant", "content": "Hello from mock!" },
    "finish_reason": "stop"
  }],
  "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
}"#;

/// Minimal OpenAI-compatible chat request body (non-streaming).
const CHAT_REQUEST: &str = r#"{"model":"llama3","messages":[{"role":"user","content":"hi"}]}"#;

/// Build a temporary spec + barbacane.yaml pointing at a wiremock server.
fn create_ai_proxy_spec(base_url: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let spec_path = temp_dir.path().join("ai-proxy-test.yaml");

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let ai_proxy_path = plugins_dir.join("ai-proxy/ai-proxy.wasm");

    let manifest_path = temp_dir.path().join("barbacane.yaml");
    std::fs::write(
        &manifest_path,
        format!(
            "plugins:\n  ai-proxy:\n    path: {}\n",
            ai_proxy_path.display()
        ),
    )
    .expect("failed to write manifest");

    let spec_content = format!(
        r#"openapi: "3.0.3"
info:
  title: AI Proxy Integration Test
  version: "1.0.0"
paths:
  /chat/flat:
    post:
      operationId: chatFlat
      summary: Flat provider config pointing to mock LLM
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          provider: ollama
          model: llama3
          base_url: "{base_url}"
          timeout: 10
          max_tokens: 512
      responses:
        "200":
          description: Completion

  /chat/named-target:
    post:
      operationId: chatNamedTarget
      summary: Named-target config — default_target selects the mock LLM
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          default_target: local
          targets:
            local:
              provider: ollama
              model: llama3
              base_url: "{base_url}"
          timeout: 10
          max_tokens: 512
      responses:
        "200":
          description: Completion

  /chat/with-fallback:
    post:
      operationId: chatFallback
      summary: Fallback — primary returns 503, fallback mock returns 200
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          provider: openai
          model: gpt-4o
          api_key: "sk-test"
          base_url: "{base_url}/primary-fail"
          fallback:
            - provider: ollama
              model: llama3
              base_url: "{base_url}"
          timeout: 10
          max_tokens: 512
      responses:
        "200":
          description: Completion (from fallback)

  /chat/no-provider:
    post:
      operationId: chatNoProvider
      summary: No provider configured — expects 500
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          timeout: 5
      responses:
        "500":
          description: Misconfiguration error
"#,
        base_url = base_url
    );

    std::fs::write(&spec_path, spec_content).expect("failed to write spec");
    (temp_dir, spec_path)
}

// =========================================================================
// Tests
// =========================================================================

#[tokio::test]
async fn test_ai_proxy_flat_config_returns_completion() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/flat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(OPENAI_COMPLETION_BODY)
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway.post("/chat/flat", CHAT_REQUEST).await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Hello from mock!");
}

#[tokio::test]
async fn test_ai_proxy_default_target_routes_correctly() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/named-target"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(OPENAI_COMPLETION_BODY)
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post("/chat/named-target", CHAT_REQUEST)
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
}

#[tokio::test]
async fn test_ai_proxy_fallback_on_primary_5xx() {
    let mock_server = MockServer::start().await;

    // Primary path returns 503
    Mock::given(method("POST"))
        .and(path("/primary-fail/chat/with-fallback"))
        .respond_with(ResponseTemplate::new(503).set_body_string(r#"{"error":"overloaded"}"#))
        .mount(&mock_server)
        .await;

    // Fallback path returns 200
    Mock::given(method("POST"))
        .and(path("/chat/with-fallback"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(OPENAI_COMPLETION_BODY)
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post("/chat/with-fallback", CHAT_REQUEST)
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "should have fallen back to the second provider"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
}

#[tokio::test]
async fn test_ai_proxy_no_provider_returns_500() {
    let mock_server = MockServer::start().await;

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post("/chat/no-provider", CHAT_REQUEST)
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        500,
        "misconfigured dispatcher must return 500"
    );
}
