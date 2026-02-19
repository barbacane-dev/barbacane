# Spec Configuration

Barbacane extends OpenAPI and AsyncAPI specs with custom `x-barbacane-*` extensions. These tell the gateway how to route requests, apply middleware, and connect to backends.

## Extension Overview

| Extension | Location | Purpose |
|-----------|----------|---------|
| `x-barbacane-dispatch` | Operation | Route request to a dispatcher (required) |
| `x-barbacane-middlewares` | Root or Operation | Apply middleware chain |

## Path Parameters

### Regular Parameters

Use `{paramName}` for single-segment parameters — the parameter captures exactly one path segment:

```yaml
paths:
  /users/{id}/orders/{orderId}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
        - name: orderId
          in: path
          required: true
          schema:
            type: string
```

### Wildcard Parameters

Use `{paramName+}` to capture all remaining path segments as a single value, including any `/` characters:

```yaml
paths:
  /files/{bucket}/{key+}:
    get:
      parameters:
        - name: bucket
          in: path
          required: true
          schema:
            type: string
        - name: key
          in: path
          required: true
          allowReserved: true   # tells client tooling not to percent-encode '/'
          schema:
            type: string
```

A `GET /files/my-bucket/docs/2024/report.pdf` request captures `bucket=my-bucket` and `key=docs/2024/report.pdf`.

**Rules:**
- The wildcard parameter must be the **last segment** of the path
- At most one wildcard parameter per path
- Parameter names use the same characters as regular params (alphanumeric and `_`)

**`allowReserved` note:** This is advisory metadata for client generators and documentation tools — it signals that the value may contain unencoded `/` characters. Barbacane does not parse or enforce it, but including it produces correct client SDKs.

---

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

### Middleware Merging

Operation middlewares are **merged** with global ones. If an operation middleware has the same name as a global one, the operation config overrides it. Non-overridden globals are preserved.

```yaml
# Global: rate limit 100/min + cors
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100
  - name: cors
    config:
      allow_origin: "*"

paths:
  /public/stats:
    get:
      # Override rate-limit; cors still applies from globals
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            requests_per_minute: 1000
      # Resolved chain: cors (global) → rate-limit (operation override)
```

Use an empty array to explicitly disable all middlewares for an operation:

```yaml
x-barbacane-middlewares: []  # No middlewares at all
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

## AsyncAPI Support

Barbacane supports AsyncAPI 3.x for event-driven APIs. AsyncAPI specs work similarly to OpenAPI, with channels and operations instead of paths and methods.

### Sync-to-Async Bridge Pattern

AsyncAPI `send` operations are accessible via HTTP POST requests. This enables clients to publish messages to Kafka or NATS through the gateway:

1. Client sends HTTP POST to the channel address
2. Gateway validates the message against the schema
3. Dispatcher publishes to Kafka/NATS
4. Gateway returns 202 Accepted

### Basic AsyncAPI Example

```yaml
asyncapi: "3.0.0"
info:
  title: User Events API
  version: "1.0.0"

channels:
  userEvents:
    address: /events/users
    messages:
      UserCreated:
        contentType: application/json
        payload:
          type: object
          required:
            - userId
            - email
          properties:
            userId:
              type: string
              format: uuid
            email:
              type: string
              format: email

operations:
  publishUserCreated:
    action: send
    channel:
      $ref: '#/channels/userEvents'
    x-barbacane-dispatch:
      name: kafka
      config:
        brokers: "kafka.internal:9092"
        topic: "user-events"
```

### Channel Parameters

AsyncAPI channels can have parameters (like path params in OpenAPI):

```yaml
channels:
  orderEvents:
    address: /events/orders/{orderId}
    parameters:
      orderId:
        schema:
          type: string
          format: uuid
    messages:
      OrderPlaced:
        payload:
          type: object
```

### Message Validation

Message payloads are validated against the schema before dispatch. Invalid messages receive a 400 response with validation details.

### HTTP Method Mapping

| AsyncAPI Action | HTTP Method |
|-----------------|-------------|
| `send` | POST |
| `receive` | GET |

### Middlewares

AsyncAPI operations support the same middleware extensions as OpenAPI:

```yaml
operations:
  publishEvent:
    action: send
    channel:
      $ref: '#/channels/events'
    x-barbacane-middlewares:
      - name: jwt-auth
        config:
          required: true
    x-barbacane-dispatch:
      name: kafka
      config:
        brokers: "kafka.internal:9092"
        topic: "events"
```

---

## API Lifecycle

Barbacane supports API lifecycle management through standard OpenAPI deprecation and the `x-sunset` extension.

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

Use `x-sunset` to specify when an endpoint will be removed (per [RFC 8594](https://datatracker.ietf.org/doc/html/rfc8594)):

```yaml
paths:
  /v1/users:
    get:
      deprecated: true
      x-sunset: "Sat, 31 Dec 2025 23:59:59 GMT"
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
| E1031 | Plaintext `http://` upstream URL (use HTTPS or `--allow-plaintext` at compile time) |
| E1054 | Invalid path template (unbalanced braces, empty param name, duplicate param, `{param+}` not last segment, multiple wildcards) |

## Next Steps

- [Dispatchers](dispatchers.md) - All dispatcher types and options
- [Middlewares](middlewares.md) - Available middleware plugins
- [CLI Reference](../reference/cli.md) - Full command options
