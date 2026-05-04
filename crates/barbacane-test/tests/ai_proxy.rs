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
          api_key: "sk-test"
          base_url: "{base_url}/primary-fail"
          fallback:
            - provider: ollama
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

  /chat/routes:
    post:
      operationId: chatRoutes
      summary: Routes table — model glob picks the upstream base_url (ADR-0030 §3)
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          routes:
            - pattern: "claude-*"
              provider: ollama
              base_url: "{base_url}/route-claude"
            - pattern: "gpt-*"
              provider: ollama
              base_url: "{base_url}/route-gpt"
            - pattern: "*"
              provider: ollama
              base_url: "{base_url}/route-catchall"
          timeout: 10
          max_tokens: 512
      responses:
        "200":
          description: Completion via the matched route

  /chat/routes-no-fallthrough:
    post:
      operationId: chatRoutesNoFallthrough
      summary: Routes without catch-all and no default — non-matching model gets 400 no_route
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          routes:
            - pattern: "claude-*"
              provider: ollama
              base_url: "{base_url}/route-claude"
          timeout: 10
      responses:
        "400":
          description: no_route

  /chat/route-with-deny:
    post:
      operationId: chatRouteWithDeny
      summary: Route's deny list — blocks claude-opus-* with 403 model_not_permitted
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          routes:
            - pattern: "claude-*"
              provider: ollama
              base_url: "{base_url}/route-claude"
              deny: ["claude-opus-*"]
          timeout: 10
      responses:
        "200":
          description: Allowed
        "403":
          description: model_not_permitted
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

// =========================================================================
// ADR-0030 §3 — routes-based dispatch end-to-end
// =========================================================================

/// Send a chat request whose `model` field matches the given glob pattern's
/// example, and assert it landed at the right route's base_url. Used by the
/// `routes` test to exercise each pattern in turn.
async fn assert_routes_to(
    gateway: &TestGateway,
    mock_server: &MockServer,
    model: &str,
    expected_path_prefix: &str,
) {
    let body = format!(
        r#"{{"model":"{}","messages":[{{"role":"user","content":"hi"}}]}}"#,
        model
    );
    let resp = gateway.post("/chat/routes", &body).await.unwrap();
    assert_eq!(
        resp.status(),
        200,
        "routes dispatch for model {:?} should succeed",
        model
    );
    // wiremock records every received request; assert the most recent one
    // landed at the expected route's path prefix.
    let received = mock_server.received_requests().await.unwrap();
    let last = received
        .last()
        .unwrap_or_else(|| panic!("no upstream request received for model {:?}", model));
    assert!(
        last.url.path().starts_with(expected_path_prefix),
        "model {:?} should route to {}, hit {} instead",
        model,
        expected_path_prefix,
        last.url.path()
    );
}

#[tokio::test]
async fn test_ai_proxy_routes_first_match_wins() {
    let mock_server = MockServer::start().await;

    // One mock per route prefix. Each path matches anything under it so
    // /route-claude/v1/chat/completions, /route-gpt/v1/chat/completions, etc.
    for prefix in ["/route-claude", "/route-gpt", "/route-catchall"] {
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex(format!("^{}/", prefix)))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(OPENAI_COMPLETION_BODY)
                    .insert_header("content-type", "application/json"),
            )
            .mount(&mock_server)
            .await;
    }

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // claude-* glob → /route-claude
    assert_routes_to(&gateway, &mock_server, "claude-sonnet-4-6", "/route-claude").await;
    // gpt-* glob → /route-gpt
    assert_routes_to(&gateway, &mock_server, "gpt-4o", "/route-gpt").await;
    // anything else → catch-all
    assert_routes_to(&gateway, &mock_server, "mistral", "/route-catchall").await;
}

#[tokio::test]
async fn test_ai_proxy_400_when_body_omits_model() {
    let mock_server = MockServer::start().await;
    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // No `model` field in the request body — caller-owned-model says 400.
    let resp = gateway
        .post(
            "/chat/flat",
            r#"{"messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "model_required");
    assert_eq!(body["type"], "urn:barbacane:error:model_required");
}

#[tokio::test]
async fn test_ai_proxy_400_no_route_when_model_does_not_match() {
    let mock_server = MockServer::start().await;
    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Spec configures `routes: [{pattern: "claude-*"}]` and nothing else —
    // no catch-all, no default_target, no flat. A request with `model: gpt-4o`
    // hits the no_route case.
    let resp = gateway
        .post(
            "/chat/routes-no-fallthrough",
            r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "no_route");
    assert_eq!(body["type"], "urn:barbacane:error:no_route");
}

#[tokio::test]
async fn test_ai_proxy_403_model_not_permitted_does_not_reach_upstream() {
    let mock_server = MockServer::start().await;

    // Mount the upstream mock with `expect(0)` — if the deny check fails
    // and the request leaks through, this assertion fires on drop.
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("^/route-claude/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_COMPLETION_BODY))
        .expect(0)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_ai_proxy_spec(&mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    // Route's deny: ["claude-opus-*"] should reject this model.
    let resp = gateway
        .post(
            "/chat/route-with-deny",
            r#"{"model":"claude-opus-4-6","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "model_not_permitted");
    assert_eq!(body["type"], "urn:barbacane:error:model_not_permitted");
}

#[tokio::test]
async fn test_ai_proxy_403_does_not_fall_through_to_next_route() {
    // Spec: `claude-*` route with deny on opus, then a `*` catch-all to ollama.
    // A claude-opus model must return 403 — NOT escalate to the catch-all.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("^/route-catchall/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_COMPLETION_BODY))
        .expect(0) // catch-all must NOT be reached
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex("^/route-claude/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OPENAI_COMPLETION_BODY))
        .expect(0) // claude route also must NOT be reached (denied before dispatch)
        .mount(&mock_server)
        .await;

    let temp_dir = tempfile::TempDir::new().expect("temp dir");
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
    .unwrap();

    let spec_path = temp_dir.path().join("spec.yaml");
    std::fs::write(
        &spec_path,
        format!(
            r#"openapi: "3.0.3"
info:
  title: routes-deny-no-fallthrough
  version: "1.0.0"
paths:
  /chat:
    post:
      operationId: chat
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          routes:
            - pattern: "claude-*"
              provider: ollama
              base_url: "{base}/route-claude"
              deny: ["claude-opus-*"]
            - pattern: "*"
              provider: ollama
              base_url: "{base}/route-catchall"
          timeout: 10
      responses:
        "200":
          description: ok
"#,
            base = mock_server.uri()
        ),
    )
    .unwrap();

    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("failed to start gateway");

    let resp = gateway
        .post(
            "/chat",
            r#"{"model":"claude-opus-4-6","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), 403, "deny must return 403, not escalate");
}

// =========================================================================
// ADR-0030 §2 — Responses API at POST /v1/responses
// =========================================================================

/// Build a temp spec exposing `/v1/responses` bound to `ai-proxy` with the
/// given provider + base_url. The path is the canonical OpenAI Responses
/// path so the dispatcher's path-match (PR-4) routes through the Responses
/// adapter.
fn create_responses_spec(
    provider: &str,
    base_url: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins");
    let ai_proxy_path = plugins_dir.join("ai-proxy/ai-proxy.wasm");

    std::fs::write(
        temp_dir.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  ai-proxy:\n    path: {}\n",
            ai_proxy_path.display()
        ),
    )
    .unwrap();

    let spec_path = temp_dir.path().join("responses.yaml");
    let api_key_line = match provider {
        "anthropic" | "openai" => "          api_key: \"sk-test\"\n",
        _ => "",
    };
    std::fs::write(
        &spec_path,
        format!(
            r#"openapi: "3.0.3"
info:
  title: Responses API integration
  version: "1.0.0"
paths:
  /v1/responses:
    post:
      operationId: responses
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          provider: {provider}
{api_key_line}          base_url: "{base_url}"
          timeout: 10
          max_tokens: 1024
      responses:
        "200":
          description: ok
        "400":
          description: client error
"#,
            provider = provider,
            api_key_line = api_key_line,
            base_url = base_url,
        ),
    )
    .unwrap();
    (temp_dir, spec_path)
}

#[tokio::test]
async fn test_ai_proxy_responses_openai_passthrough_rewrites_id() {
    // ADR-0030 §2 — the gateway is uniformly stateless. Even on the OpenAI
    // passthrough path we must rewrite the upstream `id` to a synthetic
    // `resp_<uuid-v7>`; otherwise OpenAI's real id leaks to the client and
    // they could send it back as `previous_response_id` (which we 400 on).
    let mock_server = MockServer::start().await;
    let upstream_id = "resp_real_openai_should_not_leak";
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!(
                    r#"{{"id":"{}","object":"response","output":[],"usage":{{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}}"#,
                    upstream_id
                ))
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("openai", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    let resp = gateway
        .post(
            "/v1/responses",
            r#"{"model":"gpt-4o","input":[{"type":"input_text","role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    let id = body["id"].as_str().unwrap();
    assert!(
        id.starts_with("resp_"),
        "id should be a synthetic resp_<uuid>: {}",
        id
    );
    assert_ne!(
        id, upstream_id,
        "upstream id leaked to client — gateway is no longer stateless"
    );
}

#[tokio::test]
async fn test_ai_proxy_responses_400_on_previous_response_id() {
    // The mock must NOT be reached — the preflight check rejects this body
    // before target resolution.
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("openai", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    let resp = gateway
        .post(
            "/v1/responses",
            r#"{"model":"gpt-4o","input":[],"previous_response_id":"resp_old"}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "previous_response_id_not_supported");
}

#[tokio::test]
async fn test_ai_proxy_responses_400_on_ollama_provider() {
    let mock_server = MockServer::start().await;
    // Ollama doesn't have a Responses surface — the mock must NOT be reached.
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(0)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("ollama", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    let resp = gateway
        .post(
            "/v1/responses",
            r#"{"model":"mistral","input":[{"type":"input_text","role":"user","content":"hi"}]}"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "responses_not_supported_for_provider");
}

#[tokio::test]
async fn test_ai_proxy_responses_anthropic_translation_roundtrip() {
    // Mock Anthropic /v1/messages returning a Messages-format response. The
    // gateway must translate it into Responses format for the client.
    let mock_server = MockServer::start().await;
    let messages_response = r#"{
        "id":"msg_xyz","type":"message","role":"assistant","model":"claude-sonnet-4-6",
        "content":[{"type":"text","text":"Hello!"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":4,"output_tokens":2}
    }"#;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(messages_response)
                .insert_header("content-type", "application/json"),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("anthropic", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    let resp = gateway
        .post(
            "/v1/responses",
            r#"{
                "model":"claude-sonnet-4-6",
                "store":false,
                "input":[{"type":"input_text","role":"user","content":"Hi"}]
            }"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "response");
    let id = body["id"].as_str().unwrap();
    assert!(id.starts_with("resp_"), "synthetic id: {}", id);
    assert_eq!(body["model"], "claude-sonnet-4-6");
    assert_eq!(body["output"][0]["type"], "output_text");
    assert_eq!(body["output"][0]["text"], "Hello!");
    assert_eq!(body["usage"]["input_tokens"], 4);
    assert_eq!(body["usage"]["output_tokens"], 2);
}

#[tokio::test]
async fn test_ai_proxy_responses_warning_header_on_store_downgrade() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"id":"msg","model":"claude","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
                )
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("anthropic", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    // store: true is the OpenAI default — gateway downgrades and tells the client.
    let resp = gateway
        .post(
            "/v1/responses",
            r#"{
                "model":"claude-sonnet-4-6",
                "store":true,
                "input":[{"type":"input_text","role":"user","content":"hi"}]
            }"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let warning = resp
        .headers()
        .get("warning")
        .expect("warning header set")
        .to_str()
        .unwrap();
    assert!(
        warning.contains("store ignored"),
        "warning should announce the store downgrade: {}",
        warning
    );
}

#[tokio::test]
async fn test_ai_proxy_responses_warning_header_on_reasoning_dropped() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(
                    r#"{"id":"msg","model":"claude","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
                )
                .insert_header("content-type", "application/json"),
        )
        .mount(&mock_server)
        .await;

    let (_tmp, spec_path) = create_responses_spec("anthropic", &mock_server.uri());
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("gateway");

    let resp = gateway
        .post(
            "/v1/responses",
            r#"{
                "model":"claude-sonnet-4-6",
                "store":false,
                "input":[
                    {"type":"reasoning","summary":"thinking..."},
                    {"type":"input_text","role":"user","content":"hi"}
                ]
            }"#,
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let warning = resp
        .headers()
        .get("warning")
        .expect("warning header set")
        .to_str()
        .unwrap();
    assert!(
        warning.contains("reasoning items dropped"),
        "warning should announce reasoning drop: {}",
        warning
    );
}
