# ADR-0014: Testing Strategy

**Status:** Accepted
**Date:** 2026-01-28

## Context

Testing in Barbacane spans two distinct audiences:

1. **Gateway developers** — testing the Barbacane codebase itself (core, compiler, plugins)
2. **Spec authors** — testing API specs before deploying to production edge nodes

Both need clear workflows. The spec-driven architecture (ADR-0004) and compilation model (ADR-0011) create unique testing opportunities: the spec IS the configuration, so validating the spec validates the gateway behavior.

## Decision

### Part 1: Core Gateway Testing

#### Unit Tests

Standard Rust testing (`cargo test`) for all core components:

| Component | What's tested |
|-----------|--------------|
| Spec parser | OpenAPI/AsyncAPI parsing, extension extraction |
| Compiler | Routing trie generation, schema compilation, plugin resolution |
| Validator | JSON Schema validation logic, edge cases |
| Router | Path matching, parameter extraction, method filtering |
| Middleware chain | Ordering, override resolution, short-circuit behavior |
| WASM host | Plugin loading, host function contracts, sandbox enforcement |

#### Integration Tests

End-to-end tests that compile a spec and run requests through the full stack:

```
Test spec (YAML) → Compile → Load artifact → Send HTTP request → Assert response
```

Integration tests use the `mock` dispatcher (ADR-0008) to avoid external dependencies:

```yaml
# test-spec.yaml
paths:
  /users/{id}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: integer
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"id": 1, "name": "test"}'
```

```rust
#[test]
fn test_path_param_validation() {
    let artifact = compile("test-spec.yaml");
    let gw = TestGateway::from(artifact);

    // Valid request
    let res = gw.get("/users/123");
    assert_eq!(res.status(), 200);

    // Invalid path parameter (string instead of integer)
    let res = gw.get("/users/abc");
    assert_eq!(res.status(), 400);
    assert_eq!(res.json()["type"], ".../validation-failed");
}
```

#### Plugin Tests

Each WASM plugin has its own test suite, run in an isolated WASM runtime:

- Plugin is loaded into a test harness
- Mock requests are sent through the plugin
- Assertions on the plugin's output (modified request, short-circuit response, emitted telemetry)

#### Performance Tests

Benchmark suite using `criterion` (Rust benchmarking framework):

| Benchmark | What's measured |
|-----------|----------------|
| Routing | Trie lookup latency across 10, 100, 1000+ routes |
| Validation | Schema validation for small, medium, large payloads |
| WASM execution | Plugin call overhead |
| Full pipeline | End-to-end request latency (TLS → response) |

Benchmarks run in CI to detect performance regressions.

### Part 2: Spec Author Testing

Spec authors follow the same pipeline locally as in production — no special "dev mode" that could behave differently.

#### Workflow

```
1. Write/edit spec        →  user-api.yaml
2. Compile locally        →  barbacane-control compile --specs user-api.yaml
3. Run locally            →  barbacane --artifact user-api.bca --allow-plaintext-upstream
4. Test with curl/httpie  →  curl http://localhost:8080/users/123
5. Iterate
```

The `--allow-plaintext-upstream` flag is used locally to dispatch to services running on `localhost` without TLS.

#### Compile-Time Feedback

The compiler (ADR-0011) catches most issues before runtime:

| Issue | Caught at |
|-------|-----------|
| Invalid OpenAPI/AsyncAPI spec | Compile |
| Unknown plugin reference | Compile |
| Plugin config doesn't match schema | Compile |
| Missing dispatcher on a route | Compile |
| `http://` upstream in production mode | Compile |
| Unreachable upstream | Runtime (local test) |
| Auth misconfiguration | Runtime (local test) |

#### Spec Validation CLI

Quick validation without full compilation:

```bash
# Validate spec structure and extensions only (no plugin resolution)
barbacane-control validate --specs user-api.yaml

# Full compile (validates everything, produces artifact)
barbacane-control compile --specs user-api.yaml
```

#### Contract Testing

Since specs are the source of truth, they naturally enable contract testing between teams:

- API producer publishes their OpenAPI spec
- Consumer writes tests against the spec
- Barbacane guarantees runtime behavior matches the spec (strict validation, ADR-0004)

No additional contract testing framework needed — the gateway IS the contract enforcer.

### CI/CD Integration

```
┌────────────┐     ┌─────────────┐     ┌──────────────┐     ┌──────────┐
│  Git push  │────▶│   Validate  │────▶│   Compile    │────▶│ Integration│
│            │     │   (lint +   │     │  (artifact)  │     │   tests   │
│            │     │    schema)  │     │              │     │           │
└────────────┘     └─────────────┘     └──────────────┘     └──────────┘
                                                                  │
                                                            Pass? │
                                                                  ▼
                                                           ┌──────────┐
                                                           │  Deploy  │
                                                           └──────────┘
```

Integration tests in CI use the compiled artifact with mock dispatchers — no external services needed.

## Consequences

- **Easier:** Same pipeline locally and in CI (no dev-mode surprises), mock dispatchers enable isolated testing, compile-time validation catches most issues before runtime, specs double as contract tests
- **Harder:** Local workflow requires a compile step (not instant), testing async APIs (Kafka, NATS) requires local broker setup or mock dispatchers
- **Tradeoff:** No "hot reload" dev mode — changes require recompile. This is intentional: the local experience should match production behavior exactly.
