# Spec Extensions Reference

Complete reference for all `x-barbacane-*` OpenAPI extensions.

## Summary

| Extension | Location | Required | Purpose |
|-----------|----------|----------|---------|
| [`x-barbacane-dispatch`](#x-barbacane-dispatch) | Operation | Yes | Route to dispatcher |
| [`x-barbacane-middlewares`](#x-barbacane-middlewares) | Root / Operation | No | Apply middleware chain |

---

## x-barbacane-dispatch

Specifies how to handle a request for an operation.

### Location

Operation object (`get`, `post`, `put`, `delete`, `patch`, `options`, `head`).

### Schema

```yaml
x-barbacane-dispatch:
  name: string    # Required. Dispatcher name
  config: object  # Optional. Dispatcher-specific configuration
```

### Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Name of the dispatcher (e.g., `mock`, `http`) |
| `config` | object | No | Configuration passed to the dispatcher |

### Dispatcher: `mock`

Returns static responses.

```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: integer   # HTTP status (default: 200)
    body: string      # Response body (default: "")
```

### Dispatcher: `http-upstream`

Reverse proxy to HTTP/HTTPS backend.

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: string       # Required. Base URL (HTTPS required in production)
    path: string      # Optional. Upstream path template (default: operation path)
    timeout: number   # Optional. Timeout in seconds (default: 30.0)
```

### Examples

**Mock response:**
```yaml
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"ok"}'
```

**HTTP upstream proxy:**
```yaml
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://user-service.internal"
          path: "/api/v2/users/{id}"
```

**Wildcard proxy:**
```yaml
paths:
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
          url: "https://backend.internal"
          path: "/{path}"
          timeout: 10.0
```

### Secret References

Config values can reference secrets instead of hardcoding sensitive data. Secrets are resolved at gateway startup.

| Scheme | Example | Description |
|--------|---------|-------------|
| `env://` | `env://API_KEY` | Read from environment variable |
| `file://` | `file:///etc/secrets/key` | Read from file (content trimmed) |

**Example with secret reference:**
```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    headers:
      Authorization: "Bearer env://UPSTREAM_API_KEY"
```

If a secret cannot be resolved, the gateway fails to start with exit code 13.

See [Secrets Guide](../guide/secrets.md) for full documentation.

---

## x-barbacane-middlewares

Defines a middleware chain.

### Location

- **Root level**: Applies to all operations (global)
- **Operation level**: Applies to specific operation (after global)

### Schema

```yaml
x-barbacane-middlewares:
  - name: string    # Required. Middleware name
    config: object  # Optional. Middleware-specific configuration
```

### Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Name of the middleware plugin |
| `config` | object | No | Configuration passed to the middleware |

### Middleware Override

When an operation defines a middleware with the same name as a global one, the operation config overrides the global config for that middleware.

### Examples

**Global middlewares:**
```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

x-barbacane-middlewares:
  - name: request-id
    config:
      header: X-Request-ID
  - name: rate-limit
    config:
      requests_per_minute: 100
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]

paths:
  /users:
    get:
      # Inherits all global middlewares
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

**Operation-specific middlewares:**
```yaml
paths:
  /admin:
    get:
      x-barbacane-middlewares:
        - name: auth-jwt
          config:
            required: true
            scopes: ["admin:read"]
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

**Override global config:**
```yaml
# Global: 100 req/min
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100

paths:
  /high-traffic:
    get:
      # Override: 1000 req/min for this endpoint
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            requests_per_minute: 1000
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

---

## Common Middleware Configurations

### auth-jwt

```yaml
- name: auth-jwt
  config:
    required: true
    header: Authorization
    scheme: Bearer
    issuer: https://auth.example.com
    audience: my-api
    scopes: ["read"]
```

### rate-limit

```yaml
- name: rate-limit
  config:
    quota: 100             # Maximum requests allowed in window
    window: 60             # Window duration in seconds
    policy_name: "default" # Optional: name for RateLimit-Policy header
    partition_key: "client_ip" # Options: "client_ip", "header:<name>", "context:<key>"
```

Returns IETF draft-ietf-httpapi-ratelimit-headers compliant headers:
- `RateLimit-Policy`: Policy description (e.g., `default;q=100;w=60`)
- `RateLimit`: Current limit status
- `Retry-After`: Seconds until quota reset (only on 429)

### cors

```yaml
- name: cors
  config:
    allowed_origins: ["https://app.example.com"]
    allowed_methods: ["GET", "POST", "PUT", "DELETE"]
    allowed_headers: ["Authorization", "Content-Type"]
    max_age: 86400
```

### cache

```yaml
- name: cache
  config:
    ttl: 300                  # TTL in seconds (default: 300)
    vary: ["Accept-Language"] # Headers that differentiate cache entries
    methods: ["GET", "HEAD"]  # Cacheable methods (default: GET, HEAD)
    cacheable_status: [200, 301, 404] # Cacheable status codes
```

Adds `X-Cache` header to responses:
- `HIT`: Response served from cache
- `MISS`: Response not in cache (will be cached if cacheable)

### request-id

```yaml
- name: request-id
  config:
    header: X-Request-ID
    generate_if_missing: true
```

### idempotency

```yaml
- name: idempotency
  config:
    header: Idempotency-Key
    ttl: 86400
```

### observability

Per-operation observability middleware for SLO monitoring, detailed logging, and custom metrics.

```yaml
- name: observability
  config:
    latency_slo_ms: 200           # Emit SLO violation metric if exceeded
    detailed_request_logs: true   # Log request details (method, path, headers, body size)
    detailed_response_logs: true  # Log response details (status, duration, body size)
    emit_latency_histogram: true  # Emit per-operation latency histogram
```

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `latency_slo_ms` | integer | - | Latency threshold in ms; emits `barbacane_plugin_observability_slo_violation` counter when exceeded |
| `detailed_request_logs` | boolean | `false` | Log incoming request details |
| `detailed_response_logs` | boolean | `false` | Log outgoing response details including duration |
| `emit_latency_histogram` | boolean | `false` | Emit `barbacane_plugin_observability_latency_ms` histogram |

---

## Validation Errors

| Code | Message | Cause |
|------|---------|-------|
| E1010 | Routing conflict | Same path+method in multiple specs |
| E1020 | Missing dispatch | Operation has no `x-barbacane-dispatch` |

---

## Complete Example

```yaml
openapi: "3.1.0"
info:
  title: Complete Example API
  version: "1.0.0"

x-barbacane-middlewares:
  - name: request-id
    config:
      header: X-Request-ID
  - name: cors
    config:
      allowed_origins: ["*"]
  - name: rate-limit
    config:
      quota: 100
      window: 60
      partition_key: "client_ip"

paths:
  /health:
    get:
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"healthy"}'
      responses:
        "200":
          description: OK

  /users:
    get:
      operationId: listUsers
      x-barbacane-middlewares:
        - name: cache
          config:
            ttl: 60
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users
      responses:
        "200":
          description: User list

  /users/{id}:
    get:
      operationId: getUser
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users/{id}
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
            format: uuid
      responses:
        "200":
          description: User details

  /admin/users:
    get:
      operationId: adminListUsers
      x-barbacane-middlewares:
        - name: auth-jwt
          config:
            required: true
            scopes: ["admin:read"]
        - name: rate-limit
          config:
            quota: 50
            window: 60
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/admin/users
      responses:
        "200":
          description: Admin user list
```
