# SPEC-003: Plugin System — WASM Contract

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0006, ADR-0008, ADR-0016

---

## 1. Overview

All extensibility in Barbacane (middlewares and dispatchers) is delivered through WebAssembly plugins running in `wasmtime`. This spec defines the exact contract a plugin must follow: the manifest format, the WASM ABI, the host functions available, and the config-to-plugin data flow.

---

## 2. Plugin Anatomy

A plugin is a directory containing:

```
my-plugin/
├── plugin.toml             # manifest (required)
├── config-schema.json      # JSON Schema for accepted config (required)
└── my_plugin.wasm          # compiled WASM binary (required)
```

### 2.1 `plugin.toml`

```toml
[plugin]
name = "my-plugin"                    # unique identifier, lowercase, kebab-case
version = "1.0.0"                     # semver
type = "middleware"                    # "middleware" | "dispatcher"
description = "Short description"     # optional, for registry display
wasm = "my_plugin.wasm"               # path to WASM binary, relative to this file

[capabilities]
host_functions = ["log", "context_get", "context_set"]  # host functions this plugin imports
```

**Field rules:**

| Field | Required | Constraints |
|-------|----------|-------------|
| `plugin.name` | Yes | `^[a-z][a-z0-9-]*$`, max 64 chars |
| `plugin.version` | Yes | Valid semver (e.g. `1.0.0`, `0.2.0-beta.1`) |
| `plugin.type` | Yes | `middleware` or `dispatcher` |
| `plugin.description` | No | Max 256 chars |
| `plugin.wasm` | Yes | Relative path to `.wasm` file |
| `capabilities.host_functions` | Yes | Array of strings, can be empty (`[]`) |

### 2.2 `config-schema.json`

A JSON Schema (Draft 2020-12) describing the `config` object the plugin accepts in specs. The compiler validates every `config` block referencing this plugin against this schema.

If the plugin takes no config, the schema should be:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false
}
```

### 2.3 WASM binary

The `.wasm` file compiled from the plugin's source code. Must target `wasm32-wasip1`. The binary must export the functions defined in section 3.

---

## 3. WASM Export Contract

### 3.1 All plugins must export

```
init(config_ptr: i32, config_len: i32) -> i32
```

Called once at data plane startup per plugin instance. The `config_ptr` / `config_len` pair points to a JSON-encoded byte buffer in linear memory containing the plugin's config from the spec.

**Return value:**
- `0` — initialization succeeded
- Any non-zero — initialization failed; the data plane logs the error and refuses to start (exit code `14`)

A plugin referenced N times in the spec (with different configs) results in N separate WASM instances, each receiving its own `init` call.

### 3.2 Middleware exports

```
on_request(request_ptr: i32, request_len: i32) -> i32
on_response(response_ptr: i32, response_len: i32) -> i32
```

**`on_request`:**
- Called for each incoming request after validation, in middleware chain order.
- Input: JSON-encoded `Request` object (see section 5.1).
- The plugin writes its result to the host output buffer via `host_set_output`.
- Return value: action code.

| Return code | Meaning |
|-------------|---------|
| `0` | `Continue` — pass the request (from output buffer) to the next middleware |
| `1` | `ShortCircuit` — stop the chain, return the response in the output buffer |

**`on_response`:**
- Called after dispatch, in reverse middleware chain order.
- Input: JSON-encoded `Response` object (see section 5.2).
- The plugin writes its (possibly modified) response to the output buffer.
- Return value: always `0` (response phase cannot short-circuit).

A middleware that does not need to process responses can export a no-op `on_response` that copies input to output unchanged.

### 3.3 Dispatcher exports

```
dispatch(request_ptr: i32, request_len: i32) -> i32
```

- Called with the fully processed request (after all middlewares).
- Input: JSON-encoded `Request` object.
- The plugin performs dispatch (via host functions like `http_call`, `kafka_publish`, etc.) and writes a `Response` to the output buffer.
- Return value: `0` on success, non-zero on failure (gateway returns `500`).

---

## 4. Host Functions (WASM Imports)

Plugins import host functions from the `barbacane` namespace. Each function is only available if listed in the plugin's `capabilities.host_functions`. Importing a function not declared in capabilities causes a compilation-time error (SPEC-001 `E1024`).

### 4.1 Output buffer

```
host_set_output(ptr: i32, len: i32)
```

Writes the plugin's result (request, response, or action payload) to the host output buffer. Every export function (`on_request`, `on_response`, `dispatch`) must call this exactly once before returning.

This function is always available — it is not a capability.

### 4.2 Logging

```
host_log(level: i32, msg_ptr: i32, msg_len: i32)
```

| `level` value | Meaning |
|---------------|---------|
| `0` | `error` |
| `1` | `warn` |
| `2` | `info` |
| `3` | `debug` |

The message is a UTF-8 string. Logs are emitted as structured JSON with the plugin name, request trace ID, and span ID automatically attached.

**Capability name:** `log`

### 4.3 Request context

```
host_context_get(key_ptr: i32, key_len: i32) -> i32
```

Reads a value from the per-request context map. Returns the length of the value written to a host-managed buffer. The plugin reads the value via `host_context_read_result`. Returns `-1` if the key does not exist.

```
host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32
```

Copies the result of the last `host_context_get` into the plugin's memory. Returns the number of bytes written.

**Capability name:** `context_get`

```
host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32)
```

Writes a key-value pair to the per-request context map. Keys and values are UTF-8 strings. If the key already exists, the value is overwritten.

**Capability name:** `context_set`

### 4.4 Secrets

```
host_get_secret(ref_ptr: i32, ref_len: i32) -> i32
```

Fetches a secret by vault reference (e.g. `vault://secrets/api-keys`). The secret is resolved at startup and cached. Returns the length of the secret value. The plugin reads the value via `host_secret_read_result`.

```
host_secret_read_result(buf_ptr: i32, buf_len: i32) -> i32
```

Copies the resolved secret into the plugin's memory.

**Capability name:** `get_secret`

### 4.5 HTTP call

```
host_http_call(req_ptr: i32, req_len: i32) -> i32
```

Makes a synchronous outbound HTTP request. The input is a JSON-encoded HTTP request (`method`, `url`, `headers`, `body`). Returns the length of the JSON-encoded response. The plugin reads the response via `host_http_read_result`.

```
host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32
```

TLS is mandatory for outbound calls (mirrors upstream TLS policy from SPEC-004).

**Capability name:** `http_call`

### 4.6 Message publishing

```
host_kafka_publish(msg_ptr: i32, msg_len: i32) -> i32
```

Publishes a message to Kafka. Input is JSON: `{ "brokers": [...], "topic": "...", "key": "...", "value": "..." }`. Returns `0` on success, non-zero on failure.

**Capability name:** `kafka_publish`

```
host_nats_publish(msg_ptr: i32, msg_len: i32) -> i32
```

Publishes a message to NATS. Input is JSON: `{ "servers": [...], "subject": "...", "payload": "..." }`. Returns `0` on success, non-zero on failure.

**Capability name:** `nats_publish`

### 4.7 Clock

```
host_clock_now() -> i64
```

Returns the current monotonic time in milliseconds. Suitable for measuring durations, not wall-clock time.

**Capability name:** `clock_now`

### 4.8 Telemetry

```
host_metric_counter_inc(name_ptr: i32, name_len: i32, labels_ptr: i32, labels_len: i32, value: f64)
host_metric_histogram_observe(name_ptr: i32, name_len: i32, labels_ptr: i32, labels_len: i32, value: f64)
host_span_start(name_ptr: i32, name_len: i32)
host_span_end()
host_span_set_attribute(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32)
```

Metrics are auto-prefixed: `barbacane_plugin_<plugin_name>_<metric_name>`. Labels are JSON-encoded `{ "key": "value" }`.

**Capability name:** `telemetry`

---

## 5. Data Formats

All data crossing the WASM boundary is JSON-encoded UTF-8.

### 5.1 Request

```json
{
  "method": "GET",
  "path": "/users/123",
  "query": "include=orders",
  "headers": {
    "content-type": "application/json",
    "authorization": "Bearer eyJ..."
  },
  "body": "<base64-encoded bytes or null>",
  "client_ip": "192.168.1.1",
  "path_params": {
    "id": "123"
  }
}
```

| Field | Type | Description |
|-------|------|-------------|
| `method` | string | HTTP method (uppercase) |
| `path` | string | Request path (decoded) |
| `query` | string or null | Raw query string (without `?`) |
| `headers` | object | Header map (lowercase keys, last value wins for duplicates) |
| `body` | string or null | Base64-encoded request body, or `null` if no body |
| `client_ip` | string | Client IP address |
| `path_params` | object | Captured path parameters from routing |

### 5.2 Response

```json
{
  "status": 200,
  "headers": {
    "content-type": "application/json"
  },
  "body": "<base64-encoded bytes or null>"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `status` | integer | HTTP status code |
| `headers` | object | Response headers |
| `body` | string or null | Base64-encoded response body |

---

## 6. Plugin Lifecycle

### 6.1 Instance model

Each `(plugin name, config)` pair produces a separate WASM instance. If a spec references `rate-limit` globally with `{ quota: 100, window: 60 }` and on one route with `{ quota: 1000, window: 60 }`, two instances are created.

### 6.2 Memory limits

Each WASM instance is constrained:

| Limit | Default |
|-------|---------|
| Linear memory | 16 MB |
| Execution time per call | 100 ms (per `on_request` / `on_response` / `dispatch` call) |
| Stack size | 1 MB |

If a plugin exceeds execution time, the WASM runtime traps and the gateway returns `500`. If memory is exhausted, the runtime traps.

### 6.3 Concurrency

WASM instances are **not** shared across threads. The data plane maintains a pool of instances per plugin. Under load, instances are cloned from the AOT-compiled module (cheap — no recompilation).

### 6.4 Error handling

| Plugin behavior | Gateway response |
|----------------|-----------------|
| `on_request` returns `0` (continue) | Pass modified request to next middleware |
| `on_request` returns `1` (short-circuit) | Return response from output buffer |
| `on_request` traps (panic, timeout, OOM) | `500 Internal Server Error` |
| `dispatch` returns non-zero | `500 Internal Server Error` |
| `dispatch` traps | `500 Internal Server Error` |
| `on_response` traps | Log error, pass original (unmodified) response through |

The response phase is fault-tolerant: a plugin crash during `on_response` does not kill the response. The request phase is not: a crash during `on_request` or `dispatch` results in a `500`.

---

## 7. Plugin SDK (`barbacane-plugin-sdk`)

The Rust SDK is a crate that abstracts the raw WASM ABI.

### 7.1 Middleware example

```rust
use barbacane_plugin_sdk::prelude::*;

#[derive(Deserialize)]
struct Config {
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
fn on_request(req: &Request, config: &Config) -> Action<Request> {
    let key_value = if config.key.starts_with("context:") {
        barbacane::context::get(&config.key[8..]).unwrap_or_default()
    } else if config.key.starts_with("header:") {
        req.header(&config.key[7..]).unwrap_or_default()
    } else {
        req.client_ip.clone()
    };

    let remaining = get_remaining(&key_value, config.quota, config.window);

    if remaining == 0 {
        let reset_in = get_reset_seconds(&key_value, config.window);
        Action::ShortCircuit(
            Response::new(429)
                .header("content-type", "application/problem+json")
                .header("ratelimit-policy", &format!("\"{}\":q={};w={}", config.policy_name, config.quota, config.window))
                .header("ratelimit", &format!("\"{}\":r=0;t={}", config.policy_name, reset_in))
                .header("retry-after", &reset_in.to_string())
                .body(r#"{"type":"urn:barbacane:error:rate-limited","title":"Too Many Requests","status":429}"#)
        )
    } else {
        Action::Continue(req.clone())
    }
}

#[barbacane_middleware]
fn on_response(res: &Response, config: &Config) -> Response {
    let remaining = get_current_remaining(config);
    let reset_in = get_current_reset(config);
    res.clone()
        .header("ratelimit-policy", &format!("\"{}\":q={};w={}", config.policy_name, config.quota, config.window))
        .header("ratelimit", &format!("\"{}\":r={};t={}", config.policy_name, remaining, reset_in))
}
```

### 7.2 Dispatcher example

```rust
use barbacane_plugin_sdk::prelude::*;

#[derive(Deserialize)]
struct Config {
    url: String,
    timeout: Option<String>,
}

#[barbacane_dispatcher]
fn dispatch(req: &Request, config: &Config) -> Response {
    let upstream_req = HttpRequest {
        method: req.method.clone(),
        url: format!("{}{}", config.url, req.path),
        headers: req.headers.clone(),
        body: req.body.clone(),
    };

    match barbacane::http::call(&upstream_req) {
        Ok(res) => Response::new(res.status)
            .headers(res.headers)
            .body_bytes(res.body),
        Err(e) => {
            barbacane::log::error(&format!("upstream call failed: {}", e));
            Response::new(502)
                .header("content-type", "application/problem+json")
                .body(r#"{"type":"urn:barbacane:error:upstream-unavailable","title":"Bad Gateway","status":502}"#)
        }
    }
}
```

### 7.3 What the macros generate

`#[barbacane_middleware]` generates:
- `init` export: deserializes config JSON into the `Config` struct, stores in a `static`
- `on_request` export: deserializes request JSON, calls the user function, serializes the result, calls `host_set_output`
- `on_response` export: no-op pass-through (unless the user defines an `on_response` function)

`#[barbacane_dispatcher]` generates:
- `init` export: same as above
- `dispatch` export: deserializes request, calls user function, serializes response, calls `host_set_output`

---

## 8. Plugin Registration

### 8.1 CLI

```
barbacane-control plugin register [OPTIONS]

OPTIONS:
  --manifest <PATH>          Path to plugin.toml (required)
  --wasm <PATH>              Path to .wasm file (overrides plugin.toml wasm field)
  --registry <URL>           Plugin registry URL (default: from config)
```

### 8.2 Registration validation

The control plane validates:

1. `plugin.toml` conforms to the manifest schema (section 2.1)
2. `config-schema.json` is valid JSON Schema
3. The `.wasm` binary is valid WebAssembly
4. The `.wasm` binary exports the required functions for its declared type
5. The `.wasm` binary does not import host functions outside its declared capabilities
6. No plugin with the same `name@version` already exists (versions are immutable once registered)

### 8.3 Version resolution

In specs, plugins can be referenced as:

| Syntax | Resolution |
|--------|-----------|
| `name` | Latest registered version |
| `name@1.0.0` | Exact version |
| `name@^1.0.0` | Highest compatible version (semver range) |

Version resolution happens at compile time. The resolved version is recorded in the artifact manifest.
