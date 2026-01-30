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

### Override Global Middleware

Operation middlewares can override global config by name:

```yaml
# Global: 100 requests/minute
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100

paths:
  /public/feed:
    get:
      # Override: 1000 requests/minute for this endpoint
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            requests_per_minute: 1000
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

## Planned Middlewares

The following middlewares are planned for future milestones:

### rate-limit

Limits request rate per client.

```yaml
x-barbacane-middlewares:
  - name: rate-limit
    config:
      requests_per_minute: 100
      burst: 20
      key: header:Authorization
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `requests_per_minute` | integer | 60 | Sustained rate limit |
| `burst` | integer | 10 | Burst allowance |
| `key` | string | `ip` | Rate limit key source |

#### Key Sources

- `ip` - Client IP address
- `header:<name>` - Header value (e.g., `header:X-API-Key`)
- `context:<key>` - Context value (e.g., `context:auth.sub`)

### cors

Handles Cross-Origin Resource Sharing.

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
      max_age: 86400
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allowed_origins` | array | `[]` | Allowed origins (`*` for any) |
| `allowed_methods` | array | `["GET", "POST"]` | Allowed HTTP methods |
| `allowed_headers` | array | `[]` | Allowed request headers |
| `expose_headers` | array | `[]` | Headers exposed to browser |
| `max_age` | integer | 3600 | Preflight cache time (seconds) |
| `allow_credentials` | boolean | `false` | Allow credentials |

### cache

Caches responses.

```yaml
x-barbacane-middlewares:
  - name: cache
    config:
      ttl: 300
      vary:
        - Accept-Language
        - Accept-Encoding
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `ttl` | integer | 60 | Cache duration (seconds) |
| `vary` | array | `[]` | Headers that vary cache |
| `stale_while_revalidate` | integer | 0 | Serve stale while refreshing |
| `stale_if_error` | integer | 0 | Serve stale on upstream error |

### request-id

Adds request ID for tracing.

```yaml
x-barbacane-middlewares:
  - name: request-id
    config:
      header: X-Request-ID
      generate_if_missing: true
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `header` | string | `X-Request-ID` | Header name |
| `generate_if_missing` | boolean | `true` | Generate UUID if not present |

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
  - name: request-id     # 1. Add tracing ID first
  - name: cors           # 2. Handle CORS early
  - name: rate-limit     # 3. Rate limit before auth (cheaper)
  - name: auth-jwt       # 4. Authenticate
```

### Fail Fast

Put restrictive middlewares early to reject bad requests quickly:

```yaml
x-barbacane-middlewares:
  - name: rate-limit     # Reject over-limit immediately
  - name: auth-jwt       # Reject unauthorized before processing
```

### Use Global for Common Concerns

```yaml
# Global: apply to everything
x-barbacane-middlewares:
  - name: request-id
  - name: cors
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
```
