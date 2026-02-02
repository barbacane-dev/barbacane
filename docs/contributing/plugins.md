# Plugin Development Guide

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
        Response {
            status: self.status,
            headers: Default::default(),
            body: Some(self.body.clone()),
        }
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

### 6. Build

```bash
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/my_plugin.wasm .
```

## Plugin SDK Types

### Request

```rust
pub struct Request {
    pub method: String,      // HTTP method (GET, POST, etc.)
    pub path: String,        // Request path
    pub query: Option<String>, // Query string
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}
```

### Response

```rust
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}
```

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

### Logging

```toml
[capabilities]
host_functions = ["log"]
```

```rust
use barbacane_plugin_sdk::host;

host::log("info", "Processing request");
host::log("error", "Something went wrong");
```

### Context (per-request key-value store)

```toml
[capabilities]
host_functions = ["context_get", "context_set"]
```

```rust
use barbacane_plugin_sdk::host;

// Set a value for downstream middleware/dispatcher
host::context_set("user_id", "12345");

// Get a value set by upstream middleware
if let Some(value) = host::context_get("auth_token") {
    // use value
}
```

### Clock

```toml
[capabilities]
host_functions = ["clock_now"]
```

```rust
use barbacane_plugin_sdk::host;

let timestamp_ms = host::clock_now();
```

### Secrets

```toml
[capabilities]
host_functions = ["get_secret"]
```

```rust
use barbacane_plugin_sdk::host;

// Secrets are resolved at gateway startup from env:// or file:// references
if let Some(api_key) = host::get_secret("api_key") {
    // use api_key
}
```

### HTTP Calls (Dispatcher only)

```toml
[capabilities]
host_functions = ["http_call"]
```

```rust
use barbacane_plugin_sdk::host;

let response = host::http_call(HttpRequest {
    method: "GET".to_string(),
    url: "https://api.example.com/data".to_string(),
    headers: Default::default(),
    body: None,
    timeout_ms: Some(5000),
})?;
```

## Using Plugins in Specs

### Declare in barbacane.yaml

```yaml
plugins:
  my-plugin:
    path: ./plugins/my_plugin.wasm
```

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
| `jwt-auth` | Middleware | JWT token validation |
| `apikey-auth` | Middleware | API key authentication |
| `oauth2-auth` | Middleware | OAuth2 token introspection |
| `rate-limit` | Middleware | Sliding window rate limiting |
| `cache` | Middleware | Response caching |
| `cors` | Middleware | CORS header management |

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
| Linear memory | 16 MB |
| Stack size | 1 MB |
| Execution time | 100 ms |

Exceeding these limits results in a trap (500 error for request phase, fault-tolerant for response phase).

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
