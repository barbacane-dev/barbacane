# WASM plugin development

This guide explains how to create WASM plugins for Barbacane.

## Overview

Barbacane plugins are WebAssembly (WASM) modules that extend gateway functionality. There are two types:

| Type | Purpose | Exports |
|------|---------|---------|
| **Middleware** | Process requests/responses in a chain | `init`, `on_request`, `on_response` |
| **Dispatcher** | Handle requests and generate responses | `init`, `dispatch` |

## Prerequisites

- Rust stable with `wasm32-unknown-unknown` target
- `barbacane-plugin-sdk` crate

```bash
# Add the WASM target
rustup target add wasm32-unknown-unknown
```

## Quick Start

### 1. Create a New Plugin

```bash
cargo new --lib my-plugin
cd my-plugin
```

### 2. Configure Cargo.toml

```toml
[package]
name = "my-plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
barbacane-plugin-sdk = { path = "../path/to/barbacane/crates/barbacane-plugin-sdk" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

### 3. Write the Plugin

**Middleware example:**

```rust
use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

#[barbacane_middleware]
#[derive(Deserialize)]
pub struct MyMiddleware {
    // Configuration fields from the spec
    header_name: String,
    header_value: String,
}

impl MyMiddleware {
    pub fn on_request(&mut self, req: Request) -> Action {
        // Add a header to the request
        let mut req = req;
        req.headers.insert(
            self.header_name.clone(),
            self.header_value.clone(),
        );
        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        // Pass through unchanged
        resp
    }
}
```

**Dispatcher example:**

```rust
use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;

#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct MyDispatcher {
    status: u16,
    body: String,
}

impl MyDispatcher {
    pub fn dispatch(&mut self, _req: Request) -> Response {
        Response::text(self.status, Default::default(), &self.body)
    }
}
```

### 4. Create plugin.toml

```toml
[plugin]
name = "my-plugin"
version = "0.1.0"
type = "middleware"  # or "dispatcher"
description = "My custom plugin"
wasm = "my_plugin.wasm"

[capabilities]
host_functions = ["log"]
```

### 5. Create config-schema.json

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "required": ["header_name", "header_value"],
  "properties": {
    "header_name": {
      "type": "string",
      "description": "Header name to add"
    },
    "header_value": {
      "type": "string",
      "description": "Header value to set"
    }
  }
}
```

Mark any field that carries a secret (API key, password, token, client secret)
with `"writeOnly": true` — the standard JSON Schema keyword for sensitive
values:

```json
"api_key": {
  "type": "string",
  "writeOnly": true,
  "description": "Provider API key. Use a secret reference (env://VAR or file://...)."
}
```

`barbacane compile` then warns (**E1070**) — and the vacuum linter flags — when
such a field is set to a plaintext literal instead of an `env://` / `file://`
reference, so credentials are never baked into the compiled artifact. The
detection is nested-aware (arrays and maps of objects). Do **not** mark
non-secret fields (e.g. a message-key expression) `writeOnly`.

After changing `config-schema.json`, regenerate the vacuum ruleset validators
(`node docs/rulesets/generate.mjs`) and run `docs/rulesets/tests/run-tests.sh`.

### 6. Build

```bash
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/my_plugin.wasm .
```

## Plugin SDK Types

### Request

```rust
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,      // binary-safe, travels via side-channel
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}
```

Helper methods: `body_str() -> Option<&str>`, `body_string() -> Option<String>`, `set_body_text(&str)`.

### Response

```rust
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,      // binary-safe, travels via side-channel
}
```

Helper methods: `body_str() -> Option<&str>`, `set_body_text(&str)`, `Response::text(status, headers, &str)`.

> **Note:** Bodies travel as raw bytes via side-channel host functions (`host_body_read`/`host_body_set`),
> not embedded in JSON. The proc macros handle this transparently — plugin authors just read and write
> `request.body` / `response.body` as `Option<Vec<u8>>`. This design (matching proxy-wasm and http-wasm)
> avoids the ~3.65× memory overhead of base64 encoding, allowing 10MB+ bodies within the default 16MB
> WASM memory limit.

### Action (Middleware only)

```rust
pub enum Action {
    /// Continue to next middleware/dispatcher with (possibly modified) request
    Continue(Request),
    /// Short-circuit the chain and return this response immediately
    Respond(Response),
}
```

## Host Functions

Plugins can call host functions to access gateway capabilities. Declare required capabilities in `plugin.toml`:

The SDK wraps the most common host functions so you don't hand-roll the FFI:
`barbacane_plugin_sdk::log`, `::http`, `::errors::ProblemDetails`, and `::jwt`.
Each has a native (non-wasm) stub so your plugin still compiles and unit-tests
off-target. You still declare the underlying capability in `plugin.toml`.

### Logging

```toml
[capabilities]
host_functions = ["log"]
```

```rust
use barbacane_plugin_sdk::log;

log::info("Processing request");
log::warn("rate limit exceeded");
log::error("something went wrong");
// or an explicit level: log::log(log::LEVEL_DEBUG, "verbose detail");
```

### HTTP Calls (Dispatcher only)

```toml
[capabilities]
host_functions = ["http_call"]
```

```rust
use barbacane_plugin_sdk::http;

let req = http::HttpRequest::new("GET", "https://api.example.com")
    .header("accept", "application/json")
    .timeout_ms(5000);

// The optional request body is passed separately (sent via the side-channel):
match http::call(&req, None) {
    Ok(resp) => {
        let _status = resp.status;
        let _body = resp.body_str(); // Option<&str>
    }
    Err(e) => { /* http::HttpError: Unreachable / Empty / ReadFailed / ... */ }
}
```

### Error responses (RFC 9457 problem+json)

Build consistent `application/problem+json` error responses with the shared builder:

```rust
use barbacane_plugin_sdk::errors::ProblemDetails;

return Action::ShortCircuit(
    ProblemDetails::new(403, "urn:barbacane:error:forbidden", "Forbidden")
        .detail("Access denied")
        .with("consumer", "alice") // optional extension members
        .into_response(),
);
```

### JWT parsing (auth plugins)

```rust
use barbacane_plugin_sdk::jwt;

if let Some(token) = jwt::bearer_token(auth_header) {
    // Decode claims for inspection — signature is verified separately
    // (host `verify_signature` capability), not by this helper.
    let claims: MyClaims = jwt::decode_claims_unverified(token)?;
    if claims.aud.contains("my-api") { /* jwt::Audience: string or array */ }
}
```

### Context, clock, secrets (host imports)

These host functions do not (yet) have SDK wrappers — declare the capability and
import the function directly. See any official plugin (e.g. `correlation-id` for
context, `oidc-auth` for secrets) for the exact `extern "C"` binding pattern.

```toml
[capabilities]
host_functions = ["context_get", "context_set", "clock_now", "get_secret"]
```

Secrets are resolved at gateway startup from `env://` / `file://` references, so
`get_secret` returns the resolved value (never a plaintext literal baked into the
artifact).

## Using Plugins in Specs

### Declare in barbacane.yaml

Plugins can be sourced from a local path or a remote URL:

```yaml
plugins:
  # Local path (development)
  my-plugin:
    path: ./plugins/my_plugin.wasm

  # Remote URL (production, CI/CD)
  jwt-auth:
    url: https://github.com/barbacane-dev/barbacane/releases/download/v0.5.2/jwt-auth.wasm
    sha256: abc123...  # optional integrity check
```

Remote plugins are downloaded at compile time and cached at `~/.barbacane/cache/plugins/`. Use `--no-cache` to bypass the cache entirely (re-download without caching).

### Use in OpenAPI spec

**As middleware:**

```yaml
paths:
  /users:
    get:
      x-barbacane-middlewares:
        - name: my-plugin
          config:
            header_name: "X-Custom"
            header_value: "hello"
```

**As dispatcher:**

```yaml
paths:
  /mock:
    get:
      x-barbacane-dispatch:
        name: my-plugin
        config:
          status: 200
          body: '{"message": "Hello"}'
```

## Testing Plugins

### Unit Testing

Test your plugin logic directly:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adds_header() {
        let mut plugin = MyMiddleware {
            header_name: "X-Test".to_string(),
            header_value: "value".to_string(),
        };

        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: Default::default(),
            ..Default::default()
        };

        let action = plugin.on_request(req);
        match action {
            Action::Continue(req) => {
                assert_eq!(req.headers.get("X-Test"), Some(&"value".to_string()));
            }
            _ => panic!("Expected Continue"),
        }
    }
}
```

### Integration Testing

Use fixture specs with `barbacane-test`:

```rust
use barbacane_test::TestGateway;

#[tokio::test]
async fn test_my_plugin() {
    let gw = TestGateway::from_spec("tests/fixtures/my-plugin-test.yaml")
        .await
        .unwrap();

    let resp = gw.get("/test").await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("X-Test"), Some("value"));
}
```

## Official Plugins

Barbacane includes these official plugins in the `plugins/` directory:

| Plugin | Type | Description |
|--------|------|-------------|
| `mock` | Dispatcher | Return static responses |
| `http-upstream` | Dispatcher | Reverse proxy to HTTP backends |
| `lambda` | Dispatcher | Invoke AWS Lambda functions |
| `kafka` | Dispatcher | Publish messages to Kafka |
| `nats` | Dispatcher | Publish messages to NATS |
| `s3` | Dispatcher | S3 / S3-compatible object storage proxy with SigV4 signing |
| `jwt-auth` | Middleware | JWT token validation |
| `apikey-auth` | Middleware | API key authentication |
| `oauth2-auth` | Middleware | OAuth2 token introspection |
| `rate-limit` | Middleware | Sliding window rate limiting |
| `cache` | Middleware | Response caching |
| `cors` | Middleware | CORS header management |
| `correlation-id` | Middleware | Request correlation ID propagation |
| `request-size-limit` | Middleware | Request body size limits |
| `ip-restriction` | Middleware | IP allowlist/blocklist |
| `bot-detection` | Middleware | Block bots by User-Agent pattern |
| `observability` | Middleware | SLO monitoring and detailed logging |

Use these as references for your own plugins.

## Best Practices

1. **Keep plugins focused** - One plugin, one responsibility
2. **Validate configuration** - Use JSON Schema to catch config errors at compile time
3. **Handle errors gracefully** - Return appropriate error responses, don't panic
4. **Document capabilities** - Only declare host functions you actually use
5. **Test thoroughly** - Unit test logic, integration test with the gateway
6. **Use semantic versioning** - Follow semver for plugin versions

## Resource Limits

Plugins run in a sandboxed WASM environment with these limits:

| Resource | Limit |
|----------|-------|
| Linear memory | max(16 MB, max_body_size + 4 MB) |
| Stack size | 1 MB |
| Execution time | 100 ms |

WASM memory scales automatically based on the configured `max_body_size`. Exceeding these limits results in a trap (500 error for request phase, fault-tolerant for response phase).

## Troubleshooting

### Plugin not found

Ensure the plugin is declared in `barbacane.yaml` and the WASM file exists at the specified path.

### Config validation failed

Check that your plugin's configuration in the OpenAPI spec matches the JSON Schema in `config-schema.json`.

### WASM trap

Your plugin exceeded resource limits or panicked. Check logs for details. Common causes:
- Infinite loops
- Large memory allocations
- Unhandled errors causing panic

### Unknown capability

You're using a host function not declared in `plugin.toml`. Add it to `capabilities.host_functions`.

## Distributing Plugins

### GitHub Releases

The recommended way to distribute plugins is as GitHub release assets. Upload both the `.wasm` binary and `plugin.toml` alongside your release:

```
my-plugin.wasm
my-plugin.plugin.toml
```

Generate checksums for integrity verification:

```bash
sha256sum my-plugin.wasm > checksums.txt
```

Users reference your plugin by URL in their `barbacane.yaml`:

```yaml
plugins:
  my-plugin:
    url: https://github.com/your-org/my-plugin/releases/download/v1.0.0/my-plugin.wasm
    sha256: <from checksums.txt>
```

### Official Plugins

All official Barbacane plugins are published as release assets on every tagged release. Pre-built `.wasm` files and checksums (`plugin-checksums.txt`) are available at:

```
https://github.com/barbacane-dev/barbacane/releases/download/v<VERSION>/<plugin-name>.wasm
```

### Plugin Metadata Discovery

When resolving a `url:` source, the compiler attempts to fetch `plugin.toml` from sibling URLs to extract version and type metadata:
1. `<name>.plugin.toml` (same directory as the `.wasm`)
2. `plugin.toml` (parent directory)

If neither is found, the plugin still works but without version/type metadata in the artifact manifest.
