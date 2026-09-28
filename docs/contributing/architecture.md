# Architecture

This document describes Barbacane's system architecture for contributors.

## High-Level Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         Control Plane                            │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐  │
│  │   OpenAPI   │───▶│   Parser    │───▶│      Compiler       │  │
│  │    Specs    │    │             │    │  (validation, trie) │  │
│  └─────────────┘    └─────────────┘    └──────────┬──────────┘  │
│                                                    │             │
│                                                    ▼             │
│                                           ┌───────────────┐     │
│                                           │  .bca Artifact │     │
│                                           └───────┬───────┘     │
└───────────────────────────────────────────────────┼─────────────┘
                                                    │
                                                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                          Data Plane                              │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────────────┐  │
│  │   Artifact  │───▶│   Router    │───▶│    Dispatchers      │  │
│  │   Loader    │    │   (trie)    │    │  (mock, http, ...)  │  │
│  └─────────────┘    └─────────────┘    └─────────────────────┘  │
│         │                  │                      │              │
│         │                  ▼                      ▼              │
│         │           ┌─────────────┐    ┌─────────────────────┐  │
│         │           │ Middlewares │◀──▶│   Plugin Runtime    │  │
│         │           │   Chain     │    │      (WASM)         │  │
│         │           └─────────────┘    └─────────────────────┘  │
│         │                                                        │
│         ▼                                                        │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    HTTP Server (hyper)                   │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

## Crate Structure

The project is organized as a Cargo workspace with specialized crates:

```
crates/
├── barbacane/              # Data-plane binary + CLI — router (prefix trie), validator, TLS, WebSocket proxy; serve/compile/validate/dev
├── barbacane-control/      # Control-plane binary — REST API, PostgreSQL, spec/artifact management
├── barbacane-compiler/     # Spec compilation & .bca artifact format (includes the OpenAPI/AsyncAPI spec parser)
├── barbacane-wasm/         # WASM plugin runtime (wasmtime), host functions, sandboxing
├── barbacane-telemetry/    # OpenTelemetry tracing + Prometheus metrics
├── barbacane-plugin-sdk/   # WASM plugin SDK (Request/Response/Action + log/http/errors/jwt helpers)
├── barbacane-plugin-macros/# Proc macros (#[barbacane_middleware] / #[barbacane_dispatcher])
├── barbacane-sigv4/        # AWS SigV4 request signing (used by the s3 / lambda dispatchers)
└── barbacane-test/         # Integration test harness (incl. the adversarial security suite)
```

Routing, request validation, and spec parsing are modules within `barbacane` /
`barbacane-compiler`, not separate crates.

### Crate Dependencies

```
barbacane (CLI / data plane)      — router + validator modules live here
    ├── barbacane-compiler        — includes the spec parser
    ├── barbacane-wasm
    │   └── barbacane-plugin-sdk
    │       └── barbacane-plugin-macros
    └── barbacane-telemetry

barbacane-control
    ├── barbacane-compiler
    └── barbacane-telemetry

barbacane-test
    └── barbacane-compiler (+ builds fixture plugins)
```

## Crate Details

### barbacane-compiler

Parses OpenAPI/AsyncAPI specs (spec-parser module) and compiles them into
deployable `.bca` artifacts.

**Key spec types:**
- `ApiSpec` - Parsed specification with operations and metadata
- `Operation` - Single API operation with dispatch/middleware config
- `DispatchConfig` / `MiddlewareConfig` - Dispatcher / middleware name + config

**Supported formats:** OpenAPI 3.0.x / 3.1.x / 3.2.x (draft), AsyncAPI 3.x (Kafka/NATS).

**Routing** is a prefix-trie module in `barbacane` (`router/trie.rs`): O(path-length)
lookup, static routes take precedence over parameters, path-parameter extraction,
and path normalization.

Compiles parsed specs into deployable artifacts.

**Responsibilities:**
- Validate dispatcher requirements (every operation needs dispatch)
- Detect routing conflicts (same path+method in multiple specs)
- Build routing trie
- Package into `.bca` archive

**Artifact format (.bca):**
```
artifact.bca (tar.gz)
├── manifest.json       # Metadata, checksums, bundled plugins
├── routes.json         # Compiled operations
├── specs/              # Embedded source specs
│   ├── api.yaml
│   └── ...
└── plugins/            # Bundled WASM plugins (optional)
    ├── rate-limit.wasm
    └── ...
```

### barbacane

Main CLI with three subcommands:
- `compile` - Compile specs to artifact
- `validate` - Validate specs without compilation
- `serve` - Run the gateway

### barbacane (serve)

Data plane binary - the actual gateway.

**Startup flow:**
1. Load artifact from disk
2. Load compiled routes from artifact
3. Load bundled plugins from artifact
4. Compile WASM modules (AOT)
5. **Resolve secrets** - scan configs for `env://` and `file://` references
6. Create plugin instance pool with resolved secrets
7. Start HTTP server

If any secret cannot be resolved in step 5, the gateway exits with code 13.

**Request flow:**
1. Receive HTTP request
2. Check reserved endpoints (`/__barbacane/*`)
3. Route lookup in trie
4. Apply middleware chain
5. Dispatch to handler
6. Apply response middlewares
7. Send response

### barbacane-wasm

WASM plugin runtime built on wasmtime.

**Key types:**
- `WasmEngine` - Configured wasmtime engine with AOT compilation
- `InstancePool` - Instance pooling per (plugin_name, config_hash)
- `PluginInstance` - Single WASM instance with host function bindings
- `MiddlewareChain` - Ordered middleware execution

**Host functions** (grouped; `*_read_result` variants copy a staged value into plugin memory):
- Output: `host_set_output` - plugin writes its result to the host buffer
- Logging: `host_log` - structured logging with trace context
- Context: `host_context_get`/`host_context_set`/`host_context_read_result` - per-request key-value store
- Clock: `host_clock_now`/`host_get_unix_timestamp`/`host_time_now` - time access
- Secrets: `host_get_secret`/`host_secret_read_result` - resolved secret by reference
- HTTP: `host_http_call`/`host_http_read_result`/`host_http_stream`/`host_http_request_body_set`/`host_http_response_body_len`/`host_http_response_body_read` - outbound HTTP requests
- Cache: `host_cache_get`/`host_cache_set`/`host_cache_read_result` - response cache
- Rate limiting: `host_rate_limit_check`/`host_rate_limit_read_result`
- Brokers: `host_kafka_publish`/`host_nats_publish`/`host_broker_read_result`
- Metrics: `host_metric_counter_inc`/`host_metric_histogram_observe`
- Spans: `host_span_start`/`host_span_end`/`host_span_set_attribute`
- UUID: `host_uuid_generate`/`host_uuid_read_result`
- Crypto: `host_verify_signature` - signature verification (e.g. RS256/384/512)
- Hashing: `host_sha256` - SHA-256 of plugin memory, at a cost that does not grow with the input
- WebSocket: `host_ws_upgrade`
- Body access: `host_body_get`/`host_body_set`/`host_body_len`/`host_body_clear`

**Resource limits:**
- 16 MB linear memory
- 1 MB stack
- 100ms execution timeout (via fuel)

### barbacane-plugin-sdk

SDK for developing WASM plugins (dispatchers and middlewares).

**Provides:**
- `Request`, `Response`, `Action` types
- `#[barbacane_middleware]` macro - generates WASM exports for middlewares
- `#[barbacane_dispatcher]` macro - generates WASM exports for dispatchers
- Host function FFI bindings
- Helper modules (0.8+):
  - `log` - host logging
  - `http` - outbound HTTP via `host_http_call`
  - `errors::ProblemDetails` - RFC 9457 problem+json builder
  - `jwt` - `Audience` / `Bearer` / base64url / claims parsing
  - `body` - request/response body side-channel

### barbacane-plugin-macros

Proc macros for plugin development (used by barbacane-plugin-sdk).

**Generates:**
- `init(ptr, len) -> i32` - Initialize with JSON config
- `on_request(ptr, len) -> i32` - Process request (0=continue, 1=short-circuit)
- `on_response(ptr, len) -> i32` - Process response
- `dispatch(ptr, len) -> i32` - Handle request and return response

### barbacane-test

Integration testing harness.

**Key types:**
- `TestGateway` - Spins up gateway with compiled artifact on random port
- Request helpers for easy HTTP testing

## Request Lifecycle

```
┌──────────────────────────────────────────────────────────────────┐
│                         Request Flow                              │
└──────────────────────────────────────────────────────────────────┘

    Client Request
          │
          ▼
    ┌───────────┐
    │  Receive  │  TCP accept, HTTP parse
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │  Reserved │  /__barbacane/* check
    │  Endpoint │  (health, openapi, etc.)
    └─────┬─────┘
          │ Not reserved
          ▼
    ┌───────────┐
    │   Route   │  Trie lookup: path + method
    │   Lookup  │  Returns: Found / NotFound / MethodNotAllowed
    └─────┬─────┘
          │ Found
          ▼
    ┌───────────┐
    │ Middleware│  Global middlewares
    │  (Global) │  auth, rate-limit, cors, etc.
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │ Middleware│  Operation-specific middlewares
    │ (Operation│  May override global config
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │ Dispatch  │  mock, http, custom plugins
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │ Response  │  Reverse middleware chain
    │ Middleware│  Transform response
    └─────┬─────┘
          │
          ▼
    ┌───────────┐
    │   Send    │  HTTP response to client
    └───────────┘
```

## Plugin Architecture

Plugins are WebAssembly (WASM) modules that implement dispatchers or middlewares.

```
┌─────────────────────────────────────────────────────────┐
│                    Plugin Contract                       │
├─────────────────────────────────────────────────────────┤
│  Middleware exports:                                     │
│    - on_request(ctx) -> Continue | Respond | Error      │
│    - on_response(ctx) -> Continue | Modify | Error      │
│                                                          │
│  Dispatcher exports:                                     │
│    - dispatch(ctx) -> Response | Error                  │
│                                                          │
│  Common:                                                 │
│    - init(config) -> Ok | Error                         │
├─────────────────────────────────────────────────────────┤
│  Host functions (provided by runtime):                   │
│    - http_call(req) -> Response                         │
│    - log(level, message)                                │
│    - get_secret(name) -> Value                          │
│    - context_get(key) -> Value                          │
│    - context_set(key, value)                            │
└─────────────────────────────────────────────────────────┘
```

## Key Design Decisions

### Compilation Model

**Decision**: Compile specs to artifacts at build time, not runtime.

**Rationale**:
- Fail fast: catch configuration errors before deployment
- Reproducible: artifact is immutable, version-controlled
- Fast startup: no parsing at runtime
- Secure: no spec files needed in production

### Prefix Trie Routing

**Decision**: Use a prefix trie for routing instead of linear search.

**Rationale**:
- O(path length) lookup regardless of route count
- Natural handling of path parameters
- Easy static-over-param precedence

### WASM Plugins

**Decision**: Use WebAssembly for plugin sandboxing.

**Rationale**:
- Language agnostic (Rust, Go, AssemblyScript, etc.)
- Secure sandbox (no filesystem, network without host functions)
- Near-native performance
- Portable across platforms

### Embedded Specs

**Decision**: Embed source specs in the artifact.

**Rationale**:
- Self-documenting: `/__barbacane/specs` always works
- No external dependencies at runtime
- Version consistency

## Testing Strategy

```
Unit Tests (per crate)
    ├── Parser: various OpenAPI versions, edge cases
    ├── Router: routing scenarios, parameters, precedence
    └── Compiler: validation, conflict detection

Integration Tests (barbacane-test)
    └── TestGateway: full request/response cycles
        ├── Health endpoint
        ├── Mock dispatcher
        ├── 404 / 405 handling
        └── Path parameters
```

Run all tests:
```bash
cargo test --workspace
```

## Performance Considerations

- **Zero-copy routing**: Trie lookup doesn't allocate
- **Connection reuse**: HTTP/1.1 keep-alive by default
- **Async I/O**: Tokio runtime, non-blocking everything
- **Plugin caching**: WASM modules compiled once, instantiated per-request

## Tech Debt

### Schema composition not interpreted at compile time

`allOf`, `oneOf`, `anyOf`, and `discriminator` are stored as opaque JSON values. The `jsonschema` crate handles them correctly at runtime validation, but the compiler cannot analyze or optimize polymorphic schemas.

## Future Directions

- **gRPC passthrough**: Transparent proxying for gRPC services
- **Hot reload**: Reload artifacts without restart via control plane notifications
- **Cluster mode**: Distributed configuration across multiple nodes
