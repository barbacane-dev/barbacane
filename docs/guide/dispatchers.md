# Dispatchers

Dispatchers handle how requests are processed and responses are generated. Every operation in your OpenAPI spec needs an `x-barbacane-dispatch` extension.

## Overview

```yaml
paths:
  /example:
    get:
      x-barbacane-dispatch:
        name: <dispatcher-name>
        config:
          # dispatcher-specific config
```

## Built-in Dispatchers

### mock

Returns static responses. Useful for health checks, stubs, and testing.

```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"ok"}'
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `status` | integer | 200 | HTTP status code |
| `body` | string | `""` | Response body |

#### Examples

**Simple health check:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"healthy","version":"1.0.0"}'
```

**Not found response:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 404
    body: '{"error":"resource not found"}'
```

**Empty success:**
```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 204
```

### http-upstream

Reverse proxy to an HTTP/HTTPS upstream backend. Supports connection pooling, circuit breakers, and automatic TLS.

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    path: "/api/v2/resource"
    timeout: 30.0
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `url` | string | Yes | - | Base URL of the upstream (must be HTTPS in production) |
| `path` | string | No | Same as operation path | Upstream path template with `{param}` substitution |
| `timeout` | number | No | 30.0 | Request timeout in seconds |

#### Path Parameters

Path parameters from the OpenAPI spec are automatically substituted in the `path` template:

```yaml
# OpenAPI path: /users/{userId}/orders/{orderId}
/users/{userId}/orders/{orderId}:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://backend.internal"
        path: "/api/users/{userId}/orders/{orderId}"

# Request: GET /users/123/orders/456
# Backend: GET https://backend.internal/api/users/123/orders/456
```

#### Path Rewriting

Map frontend paths to different backend paths:

```yaml
# Frontend: /v2/products
# Backend: https://catalog.internal/api/v1/catalog/products
/v2/products:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://catalog.internal"
        path: "/api/v1/catalog/products"
```

#### Wildcard Proxy

Proxy any path to upstream:

```yaml
/proxy/{path}:
  get:
    parameters:
      - name: path
        in: path
        required: true
        schema:
          type: string
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://httpbin.org"
        path: "/{path}"
        timeout: 10.0
```

#### Timeout Override

Per-operation timeout for long-running operations:

```yaml
/reports/generate:
  post:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://reports.internal"
        path: "/generate"
        timeout: 120.0  # 2 minutes for report generation
```

#### Error Handling

The dispatcher returns RFC 9457 error responses:

| Status | Condition |
|--------|-----------|
| 502 Bad Gateway | Connection failed or upstream returned invalid response |
| 503 Service Unavailable | Circuit breaker is open |
| 504 Gateway Timeout | Request exceeded configured timeout |

#### Security

- **HTTPS required in production**: `http://` URLs are rejected by the compiler (E1031)
- **Development mode**: Use `--allow-plaintext-upstream` flag to allow `http://` URLs
- **TLS**: Uses rustls with system CA roots by default

---

## Plugin Dispatchers (Future)

Custom dispatchers can be implemented as WASM plugins.

### Example: gRPC Dispatcher

```yaml
x-barbacane-dispatch:
  name: grpc
  config:
    upstream: grpc-backend
    service: users.UserService
    method: GetUser
```

### Example: GraphQL Dispatcher

```yaml
x-barbacane-dispatch:
  name: graphql
  config:
    upstream: graphql-backend
    operation: |
      query GetUser($id: ID!) {
        user(id: $id) {
          id
          name
          email
        }
      }
```

### Example: Lambda Dispatcher

```yaml
x-barbacane-dispatch:
  name: lambda
  config:
    function: my-function
    region: eu-west-1
```

---

## Dispatcher Development

See [Plugin Development](../contributing/plugins.md) for creating custom dispatchers.

### Dispatcher Interface

```rust
trait Dispatcher {
    /// Initialize with configuration.
    fn init(config: Value) -> Result<Self, Error>;

    /// Handle a request and produce a response.
    async fn dispatch(
        &self,
        ctx: &RequestContext,
    ) -> Result<Response, Error>;
}
```

---

## Best Practices

### Set Appropriate Timeouts

- Fast endpoints (health, simple reads): 5-10s
- Normal operations: 30s (default)
- Long operations (reports, uploads): 60-120s

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://backend.internal"
    timeout: 10.0  # Quick timeout for simple operation
```

### Mock for Development

Use mock dispatchers during API design:

```yaml
/users/{id}:
  get:
    x-barbacane-dispatch:
      name: mock
      config:
        status: 200
        body: '{"id":"123","name":"Test User"}'
```

Then switch to real backend:

```yaml
/users/{id}:
  get:
    x-barbacane-dispatch:
      name: http-upstream
      config:
        url: "https://user-service.internal"
        path: "/api/users/{id}"
```

### Use HTTPS in Production

The compiler rejects `http://` URLs by default (E1031). For development with local services:

```bash
# Compile allows http:// - the check happens at runtime
barbacane compile --spec api.yaml --output api.bca

# Serve with plaintext upstream allowed (dev only!)
barbacane serve --artifact api.bca --dev --allow-plaintext-upstream
```
