# Middlewares

Middlewares process requests before they reach dispatchers and can modify responses on the way back. They're used for cross-cutting concerns like authentication, rate limiting, and caching.

## Overview

Middlewares are configured with `x-barbacane-middlewares`:

```yaml
x-barbacane-middlewares:
  - name: <middleware-name>
    config:
      # middleware-specific config
```

## Middleware Chain

Middlewares execute in order:

```
Request  →  [Global MW 1]  →  [Global MW 2]  →  [Operation MW]  →  Dispatcher
                                                                        │
Response ←  [Global MW 1]  ←  [Global MW 2]  ←  [Operation MW]  ←───────┘
```

## Global vs Operation Middlewares

### Global Middlewares

Apply to all operations:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

# These apply to every operation
x-barbacane-middlewares:
  - name: request-id
    config:
      header: X-Request-ID
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]

paths:
  /users:
    get:
      # Inherits global middlewares
      x-barbacane-dispatch:
        name: http
        config:
          upstream: backend
```

### Operation Middlewares

Apply to specific operations (run after global):

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
        name: http
        config:
          upstream: backend
```

### Merging with Global Middlewares

When an operation declares its own middlewares, they are **merged** with the global chain:

- Global middlewares run first, in order
- If an operation middleware has the same name as a global one, the operation config **overrides** that global entry
- Non-overridden global middlewares are preserved

```yaml
# Global: rate-limit at 100/min + cors
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100
  - name: cors
    config:
      allow_origin: "*"

paths:
  /public/feed:
    get:
      # Override rate-limit, cors is still applied from globals
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            requests_per_minute: 1000
      # Resolved chain: cors (global) → rate-limit (operation override)
```

To explicitly disable all middlewares for an operation, use an empty array:

```yaml
paths:
  /internal/health:
    get:
      x-barbacane-middlewares: []  # No middlewares at all
```

---

## Authentication Middlewares

### jwt-auth

Validates JWT tokens with RS256/HS256 signatures.

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      secret: "your-hs256-secret"  # For HS256
      # OR
      public_key: |                 # For RS256
        -----BEGIN PUBLIC KEY-----
        ...
        -----END PUBLIC KEY-----
      issuer: https://auth.example.com
      audience: my-api
      required_claims:
        - sub
        - email
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `secret` | string | - | HS256 secret key |
| `public_key` | string | - | RS256 public key (PEM format) |
| `issuer` | string | - | Expected `iss` claim |
| `audience` | string | - | Expected `aud` claim |
| `required_claims` | array | `[]` | Claims that must be present |
| `leeway` | integer | `0` | Seconds of clock skew tolerance |

#### Context Headers

Sets headers for downstream:
- `x-auth-sub` - Subject (user ID)
- `x-auth-claims` - Full JWT claims as JSON

---

### apikey-auth

Validates API keys from header or query parameter.

```yaml
x-barbacane-middlewares:
  - name: apikey-auth
    config:
      key_location: header        # or "query"
      header_name: X-API-Key      # when key_location is "header"
      query_param: api_key        # when key_location is "query"
      keys:
        sk_live_abc123:
          id: key-001
          name: Production Key
          scopes: ["read", "write"]
        sk_test_xyz789:
          id: key-002
          name: Test Key
          scopes: ["read"]
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `key_location` | string | `header` | Where to find key (`header` or `query`) |
| `header_name` | string | `X-API-Key` | Header name (when `key_location: header`) |
| `query_param` | string | `api_key` | Query param name (when `key_location: query`) |
| `keys` | object | `{}` | Map of valid API keys to metadata |

#### Context Headers

Sets headers for downstream:
- `x-auth-key-id` - Key identifier
- `x-auth-key-name` - Key human-readable name
- `x-auth-key-scopes` - Comma-separated scopes

---

### oauth2-auth

Validates Bearer tokens via RFC 7662 token introspection.

```yaml
x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      introspection_endpoint: https://auth.example.com/oauth2/introspect
      client_id: my-api-client
      client_secret: "env://OAUTH2_CLIENT_SECRET"  # resolved at startup
      required_scopes: "read write"                 # space-separated
      timeout: 5.0                                  # seconds
```

The `client_secret` uses a secret reference (`env://`) which is resolved at gateway startup. See [Secrets](secrets.md) for details.

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `introspection_endpoint` | string | **required** | RFC 7662 introspection URL |
| `client_id` | string | **required** | Client ID for introspection auth |
| `client_secret` | string | **required** | Client secret for introspection auth |
| `required_scopes` | string | - | Space-separated required scopes |
| `timeout` | float | `5.0` | Introspection request timeout (seconds) |

#### Context Headers

Sets headers for downstream:
- `x-auth-sub` - Subject
- `x-auth-scope` - Token scopes
- `x-auth-client-id` - Client ID
- `x-auth-username` - Username (if present)
- `x-auth-claims` - Full introspection response as JSON

#### Error Responses

- `401 Unauthorized` - Missing token, invalid token, or inactive token
- `403 Forbidden` - Token lacks required scopes

Includes RFC 6750 `WWW-Authenticate` header with error details.

---

## Rate Limiting

### rate-limit

Limits request rate per client using a sliding window algorithm. Implements IETF draft-ietf-httpapi-ratelimit-headers.

```yaml
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 100
      window: 60
      policy_name: default
      partition_key: client_ip
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `quota` | integer | **required** | Maximum requests allowed in the window |
| `window` | integer | **required** | Window duration in seconds |
| `policy_name` | string | `default` | Policy name for `RateLimit-Policy` header |
| `partition_key` | string | `client_ip` | Rate limit key source |

#### Partition Key Sources

- `client_ip` - Client IP from `X-Forwarded-For` or `X-Real-IP`
- `header:<name>` - Header value (e.g., `header:X-API-Key`)
- `context:<key>` - Context value (e.g., `context:auth.sub`)
- Any static string - Same limit for all requests

#### Response Headers

On allowed requests:
- `X-RateLimit-Policy` - Policy name and configuration
- `X-RateLimit-Limit` - Maximum requests in window
- `X-RateLimit-Remaining` - Remaining requests
- `X-RateLimit-Reset` - Unix timestamp when window resets

On rate-limited requests (429):
- `RateLimit-Policy` - IETF draft header
- `RateLimit` - IETF draft combined header
- `Retry-After` - Seconds until retry is allowed

---

## CORS

### cors

Handles Cross-Origin Resource Sharing per the Fetch specification. Processes preflight OPTIONS requests and adds CORS headers to responses.

```yaml
x-barbacane-middlewares:
  - name: cors
    config:
      allowed_origins:
        - https://app.example.com
        - https://admin.example.com
      allowed_methods:
        - GET
        - POST
        - PUT
        - DELETE
      allowed_headers:
        - Authorization
        - Content-Type
      expose_headers:
        - X-Request-ID
      max_age: 86400
      allow_credentials: false
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allowed_origins` | array | `[]` | Allowed origins (`["*"]` for any, or specific origins) |
| `allowed_methods` | array | `["GET", "POST"]` | Allowed HTTP methods |
| `allowed_headers` | array | `[]` | Allowed request headers (beyond simple headers) |
| `expose_headers` | array | `[]` | Headers exposed to browser JavaScript |
| `max_age` | integer | `3600` | Preflight cache time (seconds) |
| `allow_credentials` | boolean | `false` | Allow credentials (cookies, auth headers) |

#### Origin Patterns

Origins can be:
- Exact match: `https://app.example.com`
- Wildcard subdomain: `*.example.com` (matches `sub.example.com`)
- Wildcard: `*` (only when `allow_credentials: false`)

#### Error Responses

- `403 Forbidden` - Origin not in allowed list
- `403 Forbidden` - Method not allowed (preflight)
- `403 Forbidden` - Headers not allowed (preflight)

#### Preflight Responses

Returns `204 No Content` with:
- `Access-Control-Allow-Origin`
- `Access-Control-Allow-Methods`
- `Access-Control-Allow-Headers`
- `Access-Control-Max-Age`
- `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`

---

## Request Tracing

### correlation-id

Propagates or generates correlation IDs (UUID v7) for distributed tracing. The correlation ID is passed to upstream services and included in responses.

```yaml
x-barbacane-middlewares:
  - name: correlation-id
    config:
      header_name: X-Correlation-ID
      generate_if_missing: true
      trust_incoming: true
      include_in_response: true
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `header_name` | string | `X-Correlation-ID` | Header name for the correlation ID |
| `generate_if_missing` | boolean | `true` | Generate new UUID v7 if not provided |
| `trust_incoming` | boolean | `true` | Trust and propagate incoming correlation IDs |
| `include_in_response` | boolean | `true` | Include correlation ID in response headers |

---

## Request Protection

### ip-restriction

Allows or denies requests based on client IP address or CIDR ranges. Supports both allowlist and denylist modes.

```yaml
x-barbacane-middlewares:
  - name: ip-restriction
    config:
      allow:
        - 10.0.0.0/8
        - 192.168.1.0/24
      deny:
        - 10.0.0.5
      message: "Access denied from your IP address"
      status: 403
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allow` | array | `[]` | Allowed IPs or CIDR ranges (allowlist mode) |
| `deny` | array | `[]` | Denied IPs or CIDR ranges (denylist mode) |
| `message` | string | `Access denied` | Custom error message for denied requests |
| `status` | integer | `403` | HTTP status code for denied requests |

#### Behavior

- If `deny` is configured, IPs in the list are blocked (denylist takes precedence)
- If `allow` is configured, only IPs in the list are permitted (allowlist mode)
- Client IP is extracted from `X-Forwarded-For`, `X-Real-IP`, or direct connection
- Supports both single IPs (`10.0.0.1`) and CIDR notation (`10.0.0.0/8`)

#### Error Response

Returns Problem JSON (RFC 7807):

```json
{
  "type": "urn:barbacane:error:ip-restricted",
  "title": "Forbidden",
  "status": 403,
  "detail": "Access denied",
  "client_ip": "203.0.113.50"
}
```

---

### request-size-limit

Rejects requests that exceed a configurable body size limit. Checks both `Content-Length` header and actual body size.

```yaml
x-barbacane-middlewares:
  - name: request-size-limit
    config:
      max_bytes: 1048576        # 1 MiB
      check_content_length: true
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `max_bytes` | integer | `1048576` | Maximum allowed request body size in bytes (default: 1 MiB) |
| `check_content_length` | boolean | `true` | Check `Content-Length` header for early rejection |

#### Error Response

Returns `413 Payload Too Large` with Problem JSON:

```json
{
  "type": "urn:barbacane:error:payload-too-large",
  "title": "Payload Too Large",
  "status": 413,
  "detail": "Request body size 2097152 bytes exceeds maximum allowed size of 1048576 bytes."
}
```

---

## Caching

### cache

Caches responses in memory with TTL support.

```yaml
x-barbacane-middlewares:
  - name: cache
    config:
      ttl: 300
      vary:
        - Accept-Language
        - Accept-Encoding
      methods:
        - GET
        - HEAD
      cacheable_status:
        - 200
        - 301
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `ttl` | integer | `300` | Cache duration (seconds) |
| `vary` | array | `[]` | Headers that vary cache key |
| `methods` | array | `["GET", "HEAD"]` | HTTP methods to cache |
| `cacheable_status` | array | `[200, 301]` | Status codes to cache |

#### Cache Key

Cache key is computed from:
- HTTP method
- Request path
- Vary header values (if configured)

#### Cache-Control Respect

The middleware respects `Cache-Control` response headers:
- `no-store` - Response not cached
- `no-cache` - Cache but revalidate
- `max-age=N` - Use specified TTL instead of config

---

## Planned Middlewares

The following middlewares are planned for future milestones:

### basic-auth

Username/password authentication using HTTP Basic Auth.

```yaml
x-barbacane-middlewares:
  - name: basic-auth
    config:
      realm: "My API"
      users:
        admin: "env://ADMIN_PASSWORD"
        readonly: "env://READONLY_PASSWORD"
```

### http-log

Sends request/response logs to an HTTP endpoint for centralized logging.

```yaml
x-barbacane-middlewares:
  - name: http-log
    config:
      endpoint: https://logs.example.com/ingest
      method: POST
      batch_size: 100
      flush_interval: 5
```

### idempotency

Ensures idempotent processing.

```yaml
x-barbacane-middlewares:
  - name: idempotency
    config:
      header: Idempotency-Key
      ttl: 86400
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `header` | string | `Idempotency-Key` | Header containing key |
| `ttl` | integer | 86400 | Key expiration (seconds) |

---

## Context Passing

Middlewares can set context for downstream components:

```yaml
# Auth middleware sets context:auth.sub
x-barbacane-middlewares:
  - name: auth-jwt
    config:
      required: true

# Rate limit uses auth context
  - name: rate-limit
    config:
      key: context:auth.sub  # Rate limit per user
```

---

## Middleware Development (Future)

See [Plugin Development](../contributing/plugins.md) for creating custom middlewares.

### Middleware Interface

```rust
trait Middleware {
    /// Initialize with configuration.
    fn init(config: Value) -> Result<Self, Error>;

    /// Process incoming request.
    async fn on_request(
        &self,
        ctx: &mut RequestContext,
    ) -> Result<MiddlewareAction, Error>;

    /// Process outgoing response.
    async fn on_response(
        &self,
        ctx: &mut ResponseContext,
    ) -> Result<(), Error>;
}

enum MiddlewareAction {
    Continue,           // Pass to next middleware
    Respond(Response),  // Short-circuit with response
}
```

---

## Best Practices

### Order Matters

Put middlewares in logical order:

```yaml
x-barbacane-middlewares:
  - name: correlation-id    # 1. Add tracing ID first
  - name: cors              # 2. Handle CORS early
  - name: ip-restriction    # 3. Block bad IPs immediately
  - name: request-size-limit # 4. Reject oversized requests
  - name: rate-limit        # 5. Rate limit before auth (cheaper)
  - name: auth-jwt          # 6. Authenticate
```

### Fail Fast

Put restrictive middlewares early to reject bad requests quickly:

```yaml
x-barbacane-middlewares:
  - name: ip-restriction      # Block banned IPs immediately
  - name: request-size-limit  # Reject large payloads early
  - name: rate-limit          # Reject over-limit immediately
  - name: auth-jwt            # Reject unauthorized before processing
```

### Use Global for Common Concerns

```yaml
# Global: apply to everything
x-barbacane-middlewares:
  - name: correlation-id
  - name: cors
  - name: request-size-limit
    config:
      max_bytes: 10485760  # 10 MiB global limit
  - name: rate-limit

paths:
  /public:
    get:
      # No additional middlewares needed

  /private:
    get:
      # Only add what's different
      x-barbacane-middlewares:
        - name: auth-jwt

  /upload:
    post:
      # Override size limit for uploads
      x-barbacane-middlewares:
        - name: request-size-limit
          config:
            max_bytes: 104857600  # 100 MiB for uploads
```
