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

### http

Proxies requests to an HTTP upstream backend.

```yaml
x-barbacane-dispatch:
  name: http
  config:
    upstream: my-backend
    path: /api/v2/resource
```

#### Configuration

| Property | Type | Required | Default | Description |
|----------|------|----------|---------|-------------|
| `upstream` | string | Yes | - | Upstream name (from server's `x-barbacane-upstream`) |
| `path` | string | No | Same as operation | Backend path template |
| `method` | string | No | Same as operation | Override HTTP method |
| `timeout` | duration | No | Upstream default | Request timeout |

#### Path Parameters

Path parameters from the OpenAPI spec are automatically substituted:

```yaml
# OpenAPI path
/users/{userId}/orders/{orderId}:
  get:
    x-barbacane-dispatch:
      name: http
      config:
        upstream: backend
        path: /api/users/{userId}/orders/{orderId}

# Request: GET /users/123/orders/456
# Backend: GET /api/users/123/orders/456
```

#### Path Rewriting

Map frontend paths to different backend paths:

```yaml
# Frontend: /v2/products
# Backend: /api/v1/catalog/products
/v2/products:
  get:
    x-barbacane-dispatch:
      name: http
      config:
        upstream: catalog-service
        path: /api/v1/catalog/products
```

#### Method Override

Useful for legacy backends:

```yaml
/resources/{id}:
  delete:
    x-barbacane-dispatch:
      name: http
      config:
        upstream: legacy-api
        path: /resources/{id}/delete
        method: POST  # Backend doesn't support DELETE
```

#### Timeout Override

Per-operation timeout:

```yaml
/reports/generate:
  post:
    x-barbacane-dispatch:
      name: http
      config:
        upstream: reports
        timeout: 120s  # Long-running operation
```

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

### Use Meaningful Upstreams

Name upstreams by service, not by URL:

```yaml
# Good
servers:
  - url: https://users.internal.example.com
    x-barbacane-upstream:
      name: user-service

# Bad
servers:
  - url: https://users.internal.example.com
    x-barbacane-upstream:
      name: https-users-internal  # Don't embed URL in name
```

### Set Appropriate Timeouts

- Fast endpoints (health, simple reads): 5-10s
- Normal operations: 30s (default)
- Long operations (reports, uploads): 60-120s

```yaml
x-barbacane-dispatch:
  name: http
  config:
    upstream: backend
    timeout: 10s  # Quick timeout for simple operation
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
      name: http
      config:
        upstream: user-service
        path: /api/users/{id}
```
