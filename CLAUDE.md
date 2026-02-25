# Barbacane — Claude Code Instructions

Barbacane is a spec-driven API gateway that compiles OpenAPI/AsyncAPI specs into `.bca` artifacts and runs them with WASM middleware plugins.

## Architecture

8 workspace crates + an out-of-workspace `plugins/` directory:

| Crate | Role |
|-------|------|
| `barbacane` | Data plane binary — router, validator, TLS, WebSocket proxy |
| `barbacane-control` | Control plane binary — REST API, PostgreSQL, spec management |
| `barbacane-compiler` | Compiles specs into `.bca` artifacts (includes spec parser) |
| `barbacane-wasm` | WASM plugin runtime (wasmtime), host functions, sandboxing |
| `barbacane-telemetry` | OpenTelemetry tracing, Prometheus metrics |
| `barbacane-plugin-sdk` | Plugin SDK — `Request`, `Response`, `Action` types |
| `barbacane-plugin-macros` | Proc macros — `#[barbacane_middleware]`, `#[barbacane_dispatcher]` |
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
log = true
context_get = true
```

### Available Capabilities

| Capability | Host Functions |
|-----------|----------------|
| `log` | `host_log` |
| `context_get` | `host_context_get`, `host_context_read_result` |
| `context_set` | `host_context_set` |
| `clock_now` | `host_clock_now` |
| `get_secret` | `host_get_secret`, `host_secret_read_result` |
| `http_call` | `host_http_call`, `host_http_read_result` |
| `kafka_publish` | `host_kafka_publish` |
| `nats_publish` | `host_nats_publish` |
| `telemetry` | `host_metric_counter_inc`, `host_metric_histogram_observe`, `host_span_*` |
| `generate_uuid` | `host_uuid_generate`, `host_uuid_read_result` |
| `verify_signature` | `host_verify_signature` |
| `rate_limit` | `host_rate_limit_check`, `host_rate_limit_read_result` |
| `cache` | `host_cache_get`, `host_cache_set`, `host_cache_read_result` |

### Building Plugins

```bash
cd plugins/my-plugin
cargo build --target wasm32-unknown-unknown --release
```

Plugins are not part of the workspace. Run `cargo check`, `cargo test`, and `cargo clippy` from within the plugin directory.

## Testing

- **Workspace unit tests**: `cargo test --workspace --exclude barbacane-test`
- **Integration tests**: `cargo test -p barbacane-test` (requires Docker services)
- **Plugin tests**: run from each plugin directory individually
