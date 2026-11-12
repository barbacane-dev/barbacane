# Barbacane — Claude Code Instructions

Barbacane is a spec-driven API gateway that compiles OpenAPI/AsyncAPI specs into `.bca` artifacts and runs them with WASM middleware plugins.

## Architecture

9 workspace crates + an out-of-workspace `plugins/` directory:

| Crate | Role |
|-------|------|
| `barbacane` | Data plane binary — router, validator, TLS, WebSocket proxy |
| `barbacane-control` | Control plane binary — REST API, PostgreSQL, spec management |
| `barbacane-compiler` | Compiles specs into `.bca` artifacts (includes spec parser) |
| `barbacane-wasm` | WASM plugin runtime (wasmtime), host functions, sandboxing |
| `barbacane-telemetry` | OpenTelemetry tracing, Prometheus metrics |
| `barbacane-plugin-sdk` | Plugin SDK — `Request`, `Response`, `Action` types |
| `barbacane-plugin-macros` | Proc macros — `#[barbacane_middleware]`, `#[barbacane_dispatcher]` |
| `barbacane-sigv4` | AWS Signature Version 4 signing, used by the `s3` plugin |
| `barbacane-test` | Integration tests (excluded from `cargo test --workspace`) |

## Rust Conventions

### Error Handling

- Never `unwrap()` or `panic!()` in production code
- Use `.expect("reason")` only for provably infallible operations (static regex, just-set Option, valid ASCII)
- `thiserror` for library error types, `anyhow` for binary/application errors
- `HeaderValue::from_static()` for compile-time known values

### Synchronization

- Use `parking_lot::Mutex`/`RwLock` over `std::sync` (no poisoning = no unwrap needed)

### Code Quality

- No `#[allow(dead_code)]` without justification; prefix unused struct fields with `_`
- No obvious comments; keep doc comments (`///`) and spec/RFC references
- Prefer `.is_some_and()` over `.map(...).unwrap_or(false)`

### Workspace Lints

All crates inherit lints via `[lints] workspace = true`:

```toml
[workspace.lints.clippy]
unwrap_used = "warn"
expect_used = "allow"
panic = "warn"
```

Target: zero clippy warnings on `cargo clippy --lib --bins`.

## Development Workflow

### Pre-push Checklist

```bash
cargo fmt --all
cargo clippy --all-targets
cargo test
cargo audit
```

### Docker Compose

Use `docker-compose` (hyphenated syntax), not `docker compose`.

## Plugin Development

Plugins are WASM modules built with `barbacane-plugin-sdk`. They live in `plugins/` and are excluded from the workspace.

### Plugin Types

- **Middleware** — implements `on_request(Request) -> Action<Request>` and `on_response(Response) -> Response`
- **Dispatcher** — implements `dispatch(Request) -> Response`

### Plugin Manifest (`plugin.toml`)

```toml
[plugin]
name = "my-plugin"
version = "0.1.0"
type = "middleware"    # or "dispatcher"
description = "What it does"
wasm = "my-plugin.wasm"

[capabilities]
host_functions = ["log", "context_get"]
```

### Available Capabilities

`crates/barbacane-wasm/src/manifest.rs` (`capability_to_imports`) is the source of truth for this table.

| Capability | Host Functions |
|-----------|----------------|
| `log` | `host_log` |
| `context_get` | `host_context_get`, `host_context_read_result` |
| `context_set` | `host_context_set` |
| `clock_now` | `host_clock_now`, `host_get_unix_timestamp`, `host_time_now` (deprecated, removed in 1.2.0 at the earliest) |
| `get_secret` | `host_get_secret`, `host_secret_read_result` |
| `http_call` | `host_http_call`, `host_http_stream`, `host_http_read_result`, `host_http_request_body_set`, `host_http_response_body_len`, `host_http_response_body_read` |
| `http_pool` | `host_http_pool_call`, `host_http_read_result`, `host_http_request_body_set`, `host_http_response_body_len`, `host_http_response_body_read` |
| `ws_upgrade` | `host_ws_upgrade`, `host_http_read_result` |
| `kafka_publish` | `host_kafka_publish`, `host_broker_read_result` |
| `nats_publish` | `host_nats_publish`, `host_broker_read_result` |
| `ldap` | `host_ldap_bind`, `host_ldap_search`, `host_ldap_read_result` |
| `telemetry` | `host_metric_counter_inc`, `host_metric_histogram_observe`, `host_span_start`, `host_span_end`, `host_span_set_attribute` |
| `generate_uuid` | `host_uuid_generate`, `host_uuid_read_result` |
| `verify_signature` | `host_verify_signature` |
| `hash` | `host_sha256` |
| `rate_limit` | `host_rate_limit_check`, `host_rate_limit_read_result` |
| `cache` | `host_cache_get`, `host_cache_set`, `host_cache_read_result` |

Every plugin may import, without a capability, `host_set_output` and the body side-channel (`host_body_len`, `host_body_read`, `host_body_set`, `host_body_clear`), and seven WASI functions from `wasi_snapshot_preview1`: `random_get`, `clock_time_get`, `fd_write`, `sched_yield`, `environ_get`, `environ_sizes_get`, `proc_exit`. Any other import fails compilation with `E1043`.

`body_access` is not a host function capability: it is a manifest-level flag (SPEC-008). When `false` (default for middleware), the host withholds the request body from `on_request` and the response body from `on_response`, reducing WASM memory usage. Dispatchers always receive the body regardless. Set `body_access = true` for middleware that reads or modifies a body.

The SDK macros embed the plugin ABI version in a `barbacane_abi` custom section; the compiler and the data plane refuse a plugin without it or built for another version (`E1045`). Plugins export `memory`, `alloc`, `init`, and `on_request` + `on_response` (middleware) or `dispatch` (dispatcher).

### Building Plugins

```bash
cd plugins/my-plugin
cargo build --target wasm32-unknown-unknown --release
```

Plugins are not part of the workspace. Run `cargo check`, `cargo test`, and `cargo clippy` from within the plugin directory.

After adding or modifying a plugin's `config-schema.json`, regenerate the vacuum ruleset validators:

```bash
docs/rulesets/generate.mjs
docs/rulesets/tests/run-tests.sh
```

## Testing

- **Workspace unit tests**: `cargo test --workspace --exclude barbacane-test`
- **Integration tests**: `cargo test -p barbacane-test` (requires Docker services)
- **Plugin tests**: run from each plugin directory individually
