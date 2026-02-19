# ADR-0016: Plugin Development Contract & Spec Configuration

**Status:** Accepted
**Date:** 2026-01-28

## Context

ADR-0006 established WASM as the plugin runtime. ADR-0008 defined the dispatch interface. ADR-0011 described how plugins are validated and bundled at compile time. What's missing is the **contract a plugin author must follow** — the manifest format, the WASM ABI, the config schema convention, and how spec-level `x-barbacane-*` config flows into the plugin at runtime.

Without a clear contract, plugin development is guesswork. This ADR defines the rules.

## Decision

### Plugin Manifest (`plugin.toml`)

Every plugin ships with a `plugin.toml` alongside its compiled `.wasm` binary. This manifest is the plugin's identity card.

```toml
[plugin]
name = "rate-limit"
version = "1.0.0"
type = "middleware"              # "middleware" | "dispatcher"
description = "Token-bucket rate limiting"

[capabilities]
host_functions = ["log", "context_get"]  # host functions this plugin needs

[config]
schema = "config-schema.json"   # JSON Schema file for accepted config
```

The `name` field is what specs use to reference the plugin:

```yaml
x-barbacane-middlewares:
  - name: rate-limit          # ← matches plugin.name
    config: { ... }
```

### Config Schema (JSON Schema)

Plugins declare their accepted configuration as a **JSON Schema** file shipped alongside `plugin.toml`. This schema is consumed by the compiler, not by the plugin itself.

Example `config-schema.json` for the `rate-limit` plugin:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["quota", "window"],
  "properties": {
    "quota": {
      "type": "integer",
      "minimum": 1,
      "description": "Maximum quota units allowed in the window"
    },
    "window": {
      "type": "integer",
      "minimum": 1,
      "description": "Time window in seconds"
    },
    "quota_unit": {
      "type": "string",
      "enum": ["requests", "content-bytes", "concurrent-requests"],
      "default": "requests",
      "description": "Unit of measurement for the quota"
    },
    "key": {
      "type": "string",
      "description": "Partition key for rate limiting (e.g. header:x-api-key, context:auth.sub)",
      "default": "client_ip"
    },
    "policy_name": {
      "type": "string",
      "default": "default",
      "description": "Policy identifier, appears in RateLimit-Policy response header"
    }
  },
  "additionalProperties": false
}
```

At compile time (`barbacane compile`), every `config` block in the spec is validated against the plugin's JSON Schema. If the schema rejects it, compilation fails. No invalid config reaches the data plane.

### WASM Export Contract

A plugin's `.wasm` module must export specific functions depending on its type.

#### Common (all plugins)

```
init(config_ptr: i32, config_len: i32) -> i32
```

Called once at data plane startup. Receives the plugin's `config` (from the spec) as a serialized JSON buffer. Returns `0` on success, non-zero on failure (data plane refuses to start).

#### Middleware exports

```
on_request(request_ptr: i32, request_len: i32) -> i32
on_response(response_ptr: i32, response_len: i32) -> i32
```

- `on_request` — called for each incoming request, before dispatch. Returns an action: continue (with possibly modified request) or short-circuit (return a response immediately).
- `on_response` — called on the way back, after dispatch. Returns the response (possibly modified).

#### Dispatcher exports

```
dispatch(request_ptr: i32, request_len: i32) -> i32
```

Called with the fully processed request (after all middlewares). Returns a response.

#### Data exchange format

All data crossing the WASM boundary (requests, responses, config) is serialized as **JSON over shared linear memory**. The plugin writes its result into a host-allocated buffer and returns the length. The `barbacane-plugin-sdk` handles this transparently — plugin authors work with typed Rust structs, not raw pointers.

### WASM Import Contract (Host Functions)

Plugins cannot access the network, filesystem, or clock directly. The gateway exposes a set of **host functions** that plugins can import. A plugin only gets access to the host functions listed in its `plugin.toml` `capabilities.host_functions`.

| Host function | Signature | Purpose | Typical consumer |
|---------------|-----------|---------|------------------|
| `log` | `(level: i32, msg_ptr: i32, msg_len: i32)` | Structured logging | All plugins |
| `context_get` | `(key_ptr: i32, key_len: i32) -> i32` | Read a value from the request context | Auth consumers, OPA |
| `context_set` | `(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32)` | Write a value to the request context | Auth producers |
| `get_secret` | `(ref_ptr: i32, ref_len: i32) -> i32` | Fetch a secret by vault reference | Auth plugins |
| `http_call` | `(req_ptr: i32, req_len: i32) -> i32` | Make an outbound HTTP request | `http-upstream`, OAuth2 introspection |
| `kafka_publish` | `(msg_ptr: i32, msg_len: i32) -> i32` | Publish to a Kafka topic | `kafka` dispatcher |
| `nats_publish` | `(msg_ptr: i32, msg_len: i32) -> i32` | Publish to a NATS subject | `nats` dispatcher |
| `clock_now` | `() -> i64` | Current time (monotonic, milliseconds) | Rate limiters, caches |

If a plugin imports a host function not listed in its capabilities, the compiler rejects the build. This is defense in depth on top of WASM sandboxing — a rate-limit plugin has no reason to call `kafka_publish`.

### Context Passing Between Plugins

Middlewares communicate downstream via a per-request **context map** — a flat key-value store of strings.

Convention: keys use a `namespace:key` format.

```
context:auth.sub        → "user-123"
context:auth.roles      → "admin,editor"
context:auth.exp        → "1706500000"
context:tenant_id       → "acme-corp"
```

A middleware sets context with `context_set`. Any plugin later in the chain reads it with `context_get`. The dispatcher can also read context (e.g., to add a header to the upstream request).

Example flow:

```
Request arrives
  → [jwt-auth]        context_set("context:auth.sub", "user-123")
  → [authz-opa]       context_get("context:auth.sub") → use in policy input
  → [http-upstream]    context_get("context:auth.sub") → forward as X-User-Id header
```

This is how the OPA plugin in ADR-0009 gets its `input_mapping` values — `context:auth.sub` is not magic, it's a `context_get` call.

### Spec Configuration Conventions

This section ties it all together: how a plugin declared in a spec ends up receiving its config.

#### Step 1 — Author writes the spec

```yaml
# Global middlewares (applied to all routes)
x-barbacane-middlewares:
  - name: barbacane-auth-jwt           # plugin name from plugin.toml
    config:                            # validated against plugin's config-schema.json
      issuer: https://auth.example.com
      audiences: [api.example.com]
      jwks_uri: https://auth.example.com/.well-known/jwks.json
  - name: rate-limit
    config:
      quota: 100
      window: 60
      key: context:auth.sub

paths:
  /orders:
    post:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://order-service:8080
          timeout: 5s

  /public/health:
    get:
      # Override: skip auth, keep rate-limit with different config
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 1000
            window: 60
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status": "ok"}'
```

#### Step 2 — Compiler validates

For each plugin reference in the spec, the compiler:

1. Resolves the plugin by `name` in the registry
2. Validates `config` against the plugin's `config-schema.json`
3. Checks that all host functions the plugin imports are in its declared capabilities
4. Resolves the middleware chain per route (global chain + per-route overrides)
5. Bundles the `.wasm` binary into the artifact

If anything fails, compilation stops. The spec author gets a clear error:

```
error[E0016]: invalid config for plugin "rate-limit" at path /public/health GET
  --> user-api.yaml:42:9
   |
   | quota: "fast"
   |         ^^^^^^ expected integer, got string
   |
   = schema: rate-limit/config-schema.json#/properties/quota
```

#### Step 3 — Data plane loads

At startup, the data plane:

1. Loads each plugin's `.wasm` and AOT-compiles it
2. Calls `init(config)` with the resolved config for that plugin instance
3. The plugin parses the config and stores it for use during request processing

A plugin referenced multiple times with different configs (e.g., `rate-limit` at global level and at `/public/health`) results in separate instances, each initialized with its own config.

### Plugin SDK (`barbacane-plugin-sdk`)

Writing raw WASM imports/exports is tedious. The `barbacane-plugin-sdk` Rust crate provides:

- **Types**: `Request`, `Response`, `Action`, `Config` — typed structs instead of raw pointers
- **Macros**: `#[barbacane_middleware]` and `#[barbacane_dispatcher]` — generate the WASM ABI boilerplate
- **Host bindings**: `barbacane::log()`, `barbacane::context::get()`, `barbacane::http::call()` — safe Rust wrappers around host function imports

Example middleware plugin using the SDK:

```rust
use barbacane_plugin_sdk::prelude::*;

#[derive(Deserialize)]
struct RateLimitConfig {
    quota: u32,
    window: u32,
    #[serde(default = "default_quota_unit")]
    quota_unit: String,
    #[serde(default = "default_key")]
    key: String,
    #[serde(default = "default_policy_name")]
    policy_name: String,
}

fn default_quota_unit() -> String { "requests".into() }
fn default_key() -> String { "client_ip".into() }
fn default_policy_name() -> String { "default".into() }

#[barbacane_middleware]
fn on_request(req: &Request, config: &RateLimitConfig) -> Action<Request> {
    let key_value = match config.key.as_str() {
        k if k.starts_with("context:") => barbacane::context::get(&k[8..]),
        k if k.starts_with("header:") => req.header(&k[7..]),
        _ => req.client_ip(),
    };

    let remaining = get_remaining(&key_value, config.quota, config.window);

    if remaining == 0 {
        Action::ShortCircuit(Response::new(429)
            .header("content-type", "application/problem+json")
            .header("retry-after", &get_reset_seconds(&key_value, config.window).to_string())
            .body(r#"{"type":"urn:barbacane:error:rate-limited","title":"Too Many Requests","status":429}"#))
    } else {
        Action::Continue(req.clone())
    }
}
```

The `#[barbacane_middleware]` macro generates the `init`, `on_request`, and `on_response` WASM exports, handles JSON serialization across the WASM boundary, and wires up the config deserialization. Plugin authors write plain Rust.

Plugins in other languages (Go, C, JS/TS) can target the raw WASM ABI directly or use community SDKs. The Rust SDK is the reference implementation.

### Plugin Registration

Before a plugin can be referenced in a spec, it must be registered in the control plane:

```bash
barbacane-control plugin register \
  --wasm ./target/wasm32-wasip1/release/rate_limit.wasm \
  --manifest ./plugin.toml
```

The control plane:

1. Validates `plugin.toml` fields
2. Validates that the `.wasm` module exports the required functions for its type
3. Validates that imported host functions match the declared capabilities
4. Stores the plugin (name + version) in the plugin registry
5. Makes it available for spec compilation

Plugins are versioned. Specs can pin a version or use the latest:

```yaml
x-barbacane-middlewares:
  - name: rate-limit                # latest registered version
  - name: rate-limit@1.0.0          # pinned version
```

## Consequences

- **Easier:** Plugin authors have a clear contract (manifest + schema + SDK), config errors are caught at compile time, context passing between plugins is explicit and traceable
- **Harder:** Plugin authors must maintain a JSON Schema for their config, the SDK adds a dependency (though it's optional — raw WASM ABI is always available)
- **Related:** ADR-0006 (WASM architecture), ADR-0008 (dispatch interface), ADR-0011 (compilation model)
