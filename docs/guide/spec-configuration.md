# Spec Configuration

Barbacane extends OpenAPI with custom `x-barbacane-*` extensions. These tell the gateway how to route requests, apply middleware, and connect to backends.

## Extension Overview

| Extension | Location | Purpose |
|-----------|----------|---------|
| `x-barbacane-upstream` | Server object | Define named backend connections |
| `x-barbacane-dispatch` | Operation | Route request to a dispatcher |
| `x-barbacane-middlewares` | Root or Operation | Apply middleware chain |

## Upstreams

Define backend connections on the `servers` array:

```yaml
servers:
  - url: https://api.example.com
    description: Production API
    x-barbacane-upstream:
      name: main-backend
      timeout: 30s
      retries: 3

  - url: https://auth.example.com
    description: Auth Service
    x-barbacane-upstream:
      name: auth-service
      timeout: 10s
```

### Upstream Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Unique identifier for this upstream |
| `timeout` | duration | No | Request timeout (default: 30s) |
| `retries` | integer | No | Number of retry attempts (default: 0) |

Upstreams are a design pattern for named backends. The `http-upstream` dispatcher uses the `url` config directly:

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"  # Direct URL to backend
    path: /api/resource
```

## Dispatchers

Every operation needs an `x-barbacane-dispatch` to tell Barbacane how to handle it:

```yaml
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/v2/users
```

### Dispatch Properties

| Property | Type | Required | Description |
|----------|------|----------|-------------|
| `name` | string | Yes | Dispatcher plugin name |
| `config` | object | No | Plugin-specific configuration |

### Built-in Dispatchers

#### `mock` - Return Static Responses

For health checks, stubs, or testing:

```yaml
x-barbacane-dispatch:
  name: mock
  config:
    status: 200
    body: '{"status":"ok","version":"1.0"}'
```

| Config | Type | Default | Description |
|--------|------|---------|-------------|
| `status` | integer | 200 | HTTP status code |
| `body` | string | "" | Response body |

#### `http-upstream` - Proxy to HTTP Backend

Forward requests to an upstream:

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    path: /api/users/{id}
    timeout: 10.0
```

| Config | Type | Default | Description |
|--------|------|---------|-------------|
| `url` | string | Required | Base URL of upstream (HTTPS required in production) |
| `path` | string | Same as operation | Backend path (supports `{param}` substitution) |
| `timeout` | number | 30.0 | Request timeout in seconds |

Path parameters from the OpenAPI spec are substituted automatically:

```yaml
# OpenAPI path: /users/{userId}/orders/{orderId}
# Request: GET /users/123/orders/456
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    path: /api/v2/users/{userId}/orders/{orderId}
    # Becomes: GET https://api.example.com/api/v2/users/123/orders/456
```

### Secret References in Config

Config values can reference secrets instead of hardcoding sensitive data:

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: "https://api.example.com"
    headers:
      Authorization: "Bearer env://UPSTREAM_API_KEY"
```

Supported formats:
- `env://VAR_NAME` - Read from environment variable
- `file:///path/to/secret` - Read from file

Secrets are resolved at gateway startup. If any secret is missing, the gateway fails with exit code 13. See [Secrets](secrets.md) for details.

## Middlewares

Middlewares process requests before dispatching and responses after.

### Global Middlewares

Apply to all operations:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]

paths:
  # ... all operations get these middlewares
```

### Operation Middlewares

Apply to a specific operation (runs after global middlewares):

```yaml
paths:
  /admin/users:
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

### Middleware Override

Operation middlewares can override global ones by name:

```yaml
# Global: rate limit 100/min
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100

paths:
  /public/stats:
    get:
      # Override: higher limit for this endpoint
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            requests_per_minute: 1000
```

### Middleware Chain Order

1. Global middlewares (in order defined)
2. Operation middlewares (in order defined)
3. Dispatch
4. Response middlewares (reverse order)

## Complete Example

```yaml
openapi: "3.1.0"
info:
  title: E-Commerce API
  version: "2.0.0"

servers:
  - url: https://api.shop.example.com
    x-barbacane-upstream:
      name: shop-backend
      timeout: 30s
      retries: 2

  - url: https://payments.example.com
    x-barbacane-upstream:
      name: payments
      timeout: 60s

# Global middlewares
x-barbacane-middlewares:
  - name: request-id
    config:
      header: X-Request-ID
  - name: cors
    config:
      allowed_origins: ["https://shop.example.com"]
  - name: rate-limit
    config:
      requests_per_minute: 200

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

  /products:
    get:
      operationId: listProducts
      x-barbacane-middlewares:
        - name: cache
          config:
            ttl: 300
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.shop.example.com"
          path: /api/products
      responses:
        "200":
          description: Product list

  /orders:
    post:
      operationId: createOrder
      x-barbacane-middlewares:
        - name: auth-jwt
          config:
            required: true
        - name: idempotency
          config:
            header: Idempotency-Key
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.shop.example.com"
          path: /api/orders
      responses:
        "201":
          description: Order created

  /orders/{orderId}/pay:
    post:
      operationId: payOrder
      x-barbacane-middlewares:
        - name: auth-jwt
          config:
            required: true
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://payments.example.com"  # Different backend!
          path: /process/{orderId}
          timeout: 45.0
      responses:
        "200":
          description: Payment processed
```

## API Lifecycle

Barbacane supports API lifecycle management through standard OpenAPI deprecation and the `x-barbacane-sunset` extension.

### Marking Operations as Deprecated

Use the standard OpenAPI `deprecated` field:

```yaml
paths:
  /v1/users:
    get:
      deprecated: true
      summary: List users (deprecated, use /v2/users)
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

When a client calls a deprecated endpoint, the response includes a `Deprecation: true` header per [draft-ietf-httpapi-deprecation-header](https://datatracker.ietf.org/doc/draft-ietf-httpapi-deprecation-header/).

### Setting a Sunset Date

Use `x-barbacane-sunset` to specify when an endpoint will be removed:

```yaml
paths:
  /v1/users:
    get:
      deprecated: true
      x-barbacane-sunset: "Sat, 31 Dec 2025 23:59:59 GMT"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

The sunset date must be in HTTP-date format (RFC 9110). When set, the response includes a `Sunset` header per [RFC 8594](https://datatracker.ietf.org/doc/html/rfc8594).

### Example Response Headers

```
HTTP/1.1 200 OK
Deprecation: true
Sunset: Sat, 31 Dec 2025 23:59:59 GMT
Content-Type: application/json
```

### Best Practices

1. **Mark deprecated first**: Set `deprecated: true` before setting a sunset date
2. **Give advance notice**: Set the sunset date at least 6 months in advance
3. **Update API docs**: Include migration instructions in the operation summary or description
4. **Monitor usage**: Track calls to deprecated endpoints via metrics

---

## Validation

The compiler validates your spec:

```bash
barbacane validate --spec api.yaml
```

Errors you might see:

| Error Code | Meaning |
|------------|---------|
| E1010 | Routing conflict (same path+method in multiple specs) |
| E1020 | Missing `x-barbacane-dispatch` on operation |
| E1031 | Plaintext `http://` upstream URL (use HTTPS or `--allow-plaintext-upstream`) |

## Next Steps

- [Dispatchers](dispatchers.md) - All dispatcher types and options
- [Middlewares](middlewares.md) - Available middleware plugins
- [CLI Reference](../reference/cli.md) - Full command options
