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

### Dispatcher: `kafka`

Publish messages to Apache Kafka topics.

```yaml
x-barbacane-dispatch:
  name: kafka
  config:
    brokers: string   # Required. Comma-separated broker addresses
    topic: string     # Required. Kafka topic
```

### Dispatcher: `nats`

Publish messages to NATS subjects.

```yaml
x-barbacane-dispatch:
  name: nats
  config:
    url: string       # Required. NATS server URL (e.g. "nats://localhost:4222")
    subject: string   # Required. NATS subject
```

### Dispatcher: `s3`

Proxy requests to AWS S3 or any S3-compatible endpoint with AWS Signature Version 4 signing.

```yaml
x-barbacane-dispatch:
  name: s3
  config:
    access_key_id: string      # Required. AWS access key ID
    secret_access_key: string  # Required. AWS secret access key
    region: string             # Required. AWS region (e.g. "us-east-1")
    session_token: string      # Optional. STS/AssumeRole session token
    endpoint: string           # Optional. Custom S3-compatible endpoint (e.g. "https://minio.internal:9000")
                               #           Always uses path-style URLs when set.
    force_path_style: boolean  # Optional. Use path-style URLs (default: false)
    bucket: string             # Optional. Hard-coded bucket; ignores bucket_param when set
    bucket_param: string       # Optional. Path param name for bucket (default: "bucket")
    key_param: string          # Optional. Path param name for object key (default: "key")
    timeout: number            # Optional. Timeout in seconds (default: 30.0)
```

**URL styles:**
- **Virtual-hosted** (default): `{bucket}.s3.{region}.amazonaws.com/{key}`
- **Path-style** (`force_path_style: true`): `s3.{region}.amazonaws.com/{bucket}/{key}`
- **Custom endpoint**: `{endpoint}/{bucket}/{key}` (always path-style)

**Multi-segment keys** require `{key+}` (wildcard) in the route and `allowReserved: true` on the parameter:

```yaml
paths:
  /storage/{bucket}/{key+}:
    get:
      parameters:
        - { name: bucket, in: path, required: true, schema: { type: string } }
        - { name: key, in: path, required: true, allowReserved: true, schema: { type: string } }
      x-barbacane-dispatch:
        name: s3
        config:
          region: us-east-1
          access_key_id: env://AWS_ACCESS_KEY_ID
          secret_access_key: env://AWS_SECRET_ACCESS_KEY
```

**Single-bucket CDN** (hard-coded bucket, public route):

```yaml
paths:
  /assets/{key+}:
    get:
      parameters:
        - { name: key, in: path, required: true, allowReserved: true, schema: { type: string } }
      x-barbacane-dispatch:
        name: s3
        config:
          bucket: my-assets
          region: eu-west-1
          access_key_id: env://AWS_ACCESS_KEY_ID
          secret_access_key: env://AWS_SECRET_ACCESS_KEY
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

**Wildcard proxy** (multi-segment path capture with `{param+}`):
```yaml
paths:
  /proxy/{path+}:
    get:
      parameters:
        - name: path
          in: path
          required: true
          allowReserved: true  # value may contain unencoded '/'
          schema:
            type: string
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://backend.internal"
          path: "/{path}"
          timeout: 10.0
```

A request to `/proxy/api/v2/users/123` captures `path=api/v2/users/123` and forwards to `https://backend.internal/api/v2/users/123`. See [Path Parameters](../guide/spec-configuration.md#path-parameters) for details.

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

### Middleware Merging

Operation middlewares are merged with the global chain. Global middlewares not overridden by name are preserved. When an operation defines a middleware with the same name as a global one, the operation config overrides the global config for that entry. An empty array (`x-barbacane-middlewares: []`) disables all middlewares for that operation.

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
      quota: 100
      window: 60
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
        - name: jwt-auth
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
      quota: 100
      window: 60

paths:
  /high-traffic:
    get:
      # Override: 1000 req/min for this endpoint
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 1000
            window: 60
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
```

---

## Available Middlewares

See [Middlewares Guide](../guide/middlewares.md) for all available middleware plugins and their configuration options.

---

## Validation Errors

| Code | Message | Cause |
|------|---------|-------|
| E1010 | Routing conflict | Same path+method in multiple specs |
| E1020 | Missing dispatch | Operation has no `x-barbacane-dispatch` |
