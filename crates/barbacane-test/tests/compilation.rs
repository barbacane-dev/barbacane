//! Smoke tests that verify all fixture specs compile successfully.
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

// =========================================================================
// Fixture compilation tests
//
// Verify that every plugin fixture spec compiles and the gateway starts.
// These don't test runtime behavior — just that the plugin config schemas
// are valid and the artifact builds successfully.
// =========================================================================

#[tokio::test]
async fn test_fixture_compiles_mock() {
    let gateway = TestGateway::from_spec(&fixture("mock.yaml"))
        .await
        .expect("mock fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_lambda() {
    let gateway = TestGateway::from_spec(&fixture("lambda.yaml"))
        .await
        .expect("lambda fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_oauth2_auth() {
    let gateway = TestGateway::from_spec(&fixture("oauth2-auth.yaml"))
        .await
        .expect("oauth2-auth fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_oidc_auth() {
    let gateway = TestGateway::from_spec(&fixture("oidc-auth.yaml"))
        .await
        .expect("oidc-auth fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_http_log() {
    let gateway = TestGateway::from_spec(&fixture("http-log.yaml"))
        .await
        .expect("http-log fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_observability() {
    let gateway = TestGateway::from_spec(&fixture("observability.yaml"))
        .await
        .expect("observability fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_correlation_id() {
    let gateway = TestGateway::from_spec(&fixture("correlation-id.yaml"))
        .await
        .expect("correlation-id fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_mcp() {
    let gateway = TestGateway::from_spec(&fixture("mcp.yaml"))
        .await
        .expect("mcp fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_fire_and_forget() {
    let gateway = TestGateway::from_spec(&fixture("fire-and-forget.yaml"))
        .await
        .expect("fire-and-forget fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_s3() {
    let gateway = TestGateway::from_spec(&fixture("s3.yaml"))
        .await
        .expect("s3 fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_proxy() {
    let gateway = TestGateway::from_spec(&fixture("ai-proxy.yaml"))
        .await
        .expect("ai-proxy fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_prompt_guard() {
    let gateway = TestGateway::from_spec(&fixture("ai-prompt-guard.yaml"))
        .await
        .expect("ai-prompt-guard fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_token_limit() {
    let gateway = TestGateway::from_spec(&fixture("ai-token-limit.yaml"))
        .await
        .expect("ai-token-limit fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_cost_tracker() {
    let gateway = TestGateway::from_spec(&fixture("ai-cost-tracker.yaml"))
        .await
        .expect("ai-cost-tracker fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_response_guard() {
    let gateway = TestGateway::from_spec(&fixture("ai-response-guard.yaml"))
        .await
        .expect("ai-response-guard fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_fixture_compiles_ai_gateway_composition() {
    let gateway = TestGateway::from_spec(&fixture("ai-gateway.yaml"))
        .await
        .expect("ai-gateway composition fixture failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn test_shipped_ai_gateway_spec_fragment_compiles() {
    // ADR-0030 §4: schemas/ai-gateway.yaml is the operator-facing spec
    // fragment that declares /v1/chat/completions, /v1/responses, and
    // /v1/models bound to ai-proxy with env://-resolved provider keys.
    // The fragment must compile through the standard pipeline so operators
    // can drop it into their specs/ folder.
    //
    // Set placeholder values for the env vars the fragment reads — the
    // gateway resolves env:// references at WASM instance load, and these
    // tests don't actually call upstream so the values are placeholders.
    std::env::set_var("OPENAI_API_KEY", "sk-test-openai");
    std::env::set_var("ANTHROPIC_API_KEY", "sk-test-anthropic");
    std::env::set_var("OLLAMA_BASE_URL", "http://localhost:11434");

    // Copy the shipped fragment alongside a synthesized barbacane.yaml in a
    // temp dir — operators normally drop the fragment into their own
    // `specs/` folder where the manifest already lives, but the test
    // harness needs a self-contained directory to work with.
    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fragment = repo_root.join("schemas/ai-gateway.yaml");
    let plugins = repo_root.join("plugins");

    let temp = tempfile::TempDir::new().expect("temp dir");
    std::fs::copy(&fragment, temp.path().join("ai-gateway.yaml")).expect("copy fragment");
    std::fs::write(
        temp.path().join("barbacane.yaml"),
        format!(
            "plugins:\n  ai-proxy:\n    path: {}\n",
            plugins.join("ai-proxy/ai-proxy.wasm").display()
        ),
    )
    .expect("write manifest");

    let spec_path = temp.path().join("ai-gateway.yaml");
    let gateway = TestGateway::from_spec(spec_path.to_str().unwrap())
        .await
        .expect("schemas/ai-gateway.yaml failed to compile");
    let resp = gateway.get("/__barbacane/health").await.unwrap();
    assert_eq!(resp.status(), 200);
}
