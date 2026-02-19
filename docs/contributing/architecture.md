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
├── barbacane/              # Main CLI (compile, validate, serve)
├── barbacane-control/      # Control plane CLI (spec upload, plugin register)
├── barbacane-compiler/     # Spec compilation & artifact format
├── barbacane-spec-parser/  # OpenAPI/AsyncAPI parsing
├── barbacane-router/       # Prefix trie request routing
├── barbacane-validator/    # Request validation
├── barbacane-wasm/         # WASM plugin runtime (wasmtime)
├── barbacane-plugin-sdk/   # WASM plugin development kit
├── barbacane-plugin-macros/# Proc macros for plugin development
└── barbacane-test/         # Integration test harness
```

### Crate Dependencies

```
barbacane (CLI / data plane)
    ├── barbacane-compiler
    │   ├── barbacane-spec-parser
    │   └── barbacane-router
    ├── barbacane-validator
    ├── barbacane-router
    └── barbacane-wasm
        └── barbacane-plugin-sdk

barbacane-plugin-sdk
    └── barbacane-plugin-macros

barbacane-test
    └── barbacane-compiler
```

## Crate Details

### barbacane-spec-parser

Parses OpenAPI and AsyncAPI specifications and extracts Barbacane extensions.

**Key types:**
- `ApiSpec` - Parsed specification with operations and metadata
- `Operation` - Single API operation with dispatch/middleware config
- `DispatchConfig` - Dispatcher name and configuration
- `MiddlewareConfig` - Middleware name and configuration
- `Channel` - AsyncAPI channel with publish/subscribe operations

**Supported formats:**
- OpenAPI 3.0.x
- OpenAPI 3.1.x
- OpenAPI 3.2.x (draft)
- AsyncAPI 3.x (with Kafka and NATS dispatchers)

### barbacane-router

Prefix trie implementation for fast HTTP request routing.

**Key types:**
- `Router` - The routing trie
- `RouteEntry` - Points to compiled operation index
- `RouteMatch` - Found / MethodNotAllowed / NotFound

**Features:**
- O(path length) lookup
- Static routes take precedence over parameters
- Path parameter extraction
- Path normalization (trailing slashes, double slashes)

### barbacane-compiler

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

**Host functions:**
- `host_set_output` - Plugin writes result to host buffer
- `host_log` - Structured logging with trace context
- `host_context_get/set` - Per-request key-value store
- `host_clock_now` - Monotonic time in milliseconds
- `host_http_call` - Make outbound HTTP requests
- `host_http_read_result` - Read HTTP response data
- `host_get_secret` - Get a resolved secret by reference
- `host_secret_read_result` - Read secret value into plugin memory
- `host_kafka_publish` - Publish messages to Kafka topics
- `host_nats_publish` - Publish messages to NATS subjects
- `host_rate_limit_check` - Check rate limits
- `host_cache_read/write` - Read/write response cache
- `host_metric_counter_inc` - Increment Prometheus counter
- `host_metric_histogram_observe` - Record histogram observation

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
