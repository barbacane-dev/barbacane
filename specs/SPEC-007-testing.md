# SPEC-007: Testing & Developer Workflow

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0014

---

## 1. Overview

Testing in Barbacane serves two audiences: gateway developers (testing the core codebase) and spec authors (testing API specs before production). This spec defines the test infrastructure, harnesses, and local development workflow.

---

## 2. Core Gateway Testing

### 2.1 Unit tests

Standard Rust testing via `cargo test`. Every crate in the workspace has unit tests for its public API.

| Crate | What is tested |
|-------|----------------|
| `barbacane-spec-parser` | OpenAPI/AsyncAPI parsing, `x-barbacane-*` extension extraction, `$ref` resolution |
| `barbacane-compiler` | Routing trie generation, schema compilation, middleware chain resolution, plugin config validation |
| `barbacane-validator` | JSON Schema validation for all supported keywords, edge cases (nullable, oneOf, format) |
| `barbacane-router` | Prefix-trie path matching, parameter extraction, method filtering, static-vs-param precedence |
| `barbacane-wasm-host` | Plugin loading, host function contracts, sandbox enforcement (memory limits, time limits) |
| `barbacane-plugin-sdk` | Macro expansion, serialization, Action type handling |

### 2.2 Integration tests

End-to-end tests that compile a spec and run HTTP requests through the full data plane stack.

**Test harness: `TestGateway`**

```rust
use barbacane_test::TestGateway;

#[tokio::test]
async fn test_valid_request() {
    let gw = TestGateway::from_spec("tests/fixtures/user-api.yaml").await;

    let res = gw.get("/users/123").await;
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn test_invalid_path_param() {
    let gw = TestGateway::from_spec("tests/fixtures/user-api.yaml").await;

    let res = gw.get("/users/not-a-number").await;
    assert_eq!(res.status(), 400);
    assert_eq!(res.json()["type"], "urn:barbacane:error:validation-failed");
}

#[tokio::test]
async fn test_method_not_allowed() {
    let gw = TestGateway::from_spec("tests/fixtures/user-api.yaml").await;

    let res = gw.delete("/users/123").await;
    assert_eq!(res.status(), 405);
    assert!(res.header("allow").contains("GET"));
}
```

`TestGateway`:
1. Compiles the spec into an in-memory artifact
2. Boots a data plane on a random port
3. Provides `get()`, `post()`, `put()`, `delete()` helpers that send HTTP requests
4. Automatically shuts down on drop

All integration tests use the `mock` dispatcher to avoid external dependencies.

### 2.3 Plugin tests

Plugins are tested in isolation using a WASM test harness:

```rust
use barbacane_test::PluginHarness;

#[tokio::test]
async fn test_rate_limit_allows_under_limit() {
    let harness = PluginHarness::load("rate-limit", json!({
        "quota": 10,
        "window": 1,
        "key": "client_ip"
    })).await;

    let req = Request::get("/users/123").client_ip("1.2.3.4");
    let action = harness.on_request(&req).await;

    assert!(action.is_continue());
}

#[tokio::test]
async fn test_rate_limit_blocks_over_limit() {
    let harness = PluginHarness::load("rate-limit", json!({
        "quota": 1,
        "window": 1,
        "key": "client_ip"
    })).await;

    let req = Request::get("/users/123").client_ip("1.2.3.4");
    harness.on_request(&req).await; // first request: allowed

    let action = harness.on_request(&req).await; // second request: blocked
    assert!(action.is_short_circuit());
    assert_eq!(action.response().status(), 429);
}
```

`PluginHarness`:
1. Loads the `.wasm` binary into a `wasmtime` runtime
2. Provides mock host functions (context, logging, clock)
3. Calls `init` with the provided config
4. Exposes `on_request()`, `on_response()`, `dispatch()` helpers
5. Allows inspecting context changes, emitted logs, and telemetry

### 2.4 Performance tests

Benchmark suite using `criterion`:

| Benchmark | What is measured |
|-----------|-----------------|
| `bench_routing_10` | Trie lookup across 10 routes |
| `bench_routing_100` | Trie lookup across 100 routes |
| `bench_routing_1000` | Trie lookup across 1000 routes |
| `bench_validation_small` | Schema validation for 100-byte JSON body |
| `bench_validation_large` | Schema validation for 100 KB JSON body |
| `bench_wasm_call_overhead` | Round-trip cost of calling a no-op WASM plugin |
| `bench_middleware_chain_5` | 5-middleware chain execution |
| `bench_full_pipeline` | End-to-end: parse → route → validate → middleware → dispatch → respond |

Benchmarks run in CI on every merge to `main`. Regressions exceeding 10% fail the build.

---

## 3. Spec Author Testing

### 3.1 Local workflow

Spec authors follow the same pipeline locally as in production:

```
1. Edit spec             →  user-api.yaml
2. Validate (fast)       →  barbacane-control validate --specs user-api.yaml
3. Compile               →  barbacane-control compile --specs user-api.yaml --development
4. Run data plane        →  barbacane --artifact artifact.bca --dev --allow-plaintext-upstream
5. Test with curl        →  curl http://localhost:8080/users/123
6. Iterate from step 1
```

Step 2 is optional (step 3 validates too) but faster — it skips plugin resolution.

### 3.2 Compile-time feedback

The compiler catches most issues before runtime:

| Issue | Caught at | Error code |
|-------|-----------|-----------|
| Invalid spec syntax | Compile | `E1001`–`E1004` |
| Invalid extension config | Compile | `E1010`–`E1015` |
| Unknown plugin | Compile | `E1021` |
| Plugin config mismatch | Compile | `E1023` |
| Missing dispatcher | Compile | `E1020` |
| `http://` upstream (production mode) | Compile | `E1031` |
| Auth middleware missing for secured route | Compile | `E1032` |
| Unreachable upstream | Runtime | — |
| Auth misconfiguration (wrong JWKS URI) | Runtime | — |

### 3.3 Development mode behavior

When running with `--dev`:

| Feature | Production | Development |
|---------|-----------|-------------|
| Error detail | Minimal (RFC 9457 base fields only) | Full (field paths, spec references, plugin names) |
| `http://` upstreams | Rejected | Allowed (with `--allow-plaintext-upstream`) |
| Log level | `info` | `debug` |
| Trace sampling | Configurable | 100% |

### 3.4 Fixture specs for testing

The repository includes fixture specs under `tests/fixtures/` that cover common patterns:

| Fixture | Purpose |
|---------|---------|
| `minimal.yaml` | Smallest valid spec (one GET route, mock dispatcher) |
| `full-crud.yaml` | CRUD operations with validation, auth, and rate limiting |
| `async-kafka.yaml` | AsyncAPI spec with Kafka dispatch |
| `multi-spec.yaml` + `multi-spec-2.yaml` | Two specs compiled into one artifact |
| `invalid-*.yaml` | Intentionally broken specs for testing error reporting |

---

## 4. CI/CD Pipeline

### 4.1 Pipeline stages

```
Git push
  │
  ├── cargo fmt --check           (formatting)
  ├── cargo clippy -- -D warnings (linting)
  │
  ▼
cargo test                        (unit + integration tests)
  │
  ▼
cargo bench                       (performance regression check)
  │
  ▼
Build release binaries            (barbacane, barbacane-control)
  │
  ▼
Build plugin .wasm artifacts      (built-in plugins)
  │
  ▼
Integration test with compiled    (compile fixture specs, run requests)
artifacts
  │
  ▼
Publish artifacts                 (container images, binary releases)
```

### 4.2 CI environment

| Requirement | Details |
|-------------|---------|
| Rust toolchain | Stable + `wasm32-wasip1` target |
| `wasmtime` | For WASM tests |
| PostgreSQL | For control plane integration tests |
| No external services | All dispatch tests use `mock` dispatcher |

### 4.3 Contract testing

Since specs are the source of truth and the gateway strictly enforces them, contract testing between API producer and consumer is inherent:

1. Producer publishes their OpenAPI spec to the control plane
2. Consumer writes integration tests against the spec
3. Barbacane guarantees runtime behavior matches the spec

No additional contract testing framework is required. The gateway IS the contract enforcer.

---

## 5. Test Utilities

### 5.1 `barbacane-test` crate

A test utility crate providing:

| Utility | Description |
|---------|-------------|
| `TestGateway` | Full-stack test harness (section 2.2) |
| `PluginHarness` | WASM plugin test harness (section 2.3) |
| `SpecBuilder` | Programmatic spec construction for tests |
| `RequestBuilder` | HTTP request builder with fluent API |
| `assertions` | Custom assertion macros for error types, headers, JSON bodies |

### 5.2 `SpecBuilder` example

```rust
use barbacane_test::SpecBuilder;

let spec = SpecBuilder::new("test-api")
    .route("/users/{id}", Method::GET)
        .path_param("id", SchemaType::Integer)
        .dispatch("mock", json!({"status": 200, "body": "{\"id\": 1}"}))
    .route("/users", Method::POST)
        .body_schema(json!({
            "type": "object",
            "required": ["name", "email"],
            "properties": {
                "name": {"type": "string"},
                "email": {"type": "string", "format": "email"}
            }
        }))
        .dispatch("mock", json!({"status": 201}))
    .build();

let gw = TestGateway::from_built_spec(spec).await;
```

This avoids maintaining YAML fixtures for every edge case — tests can construct the exact spec they need.
