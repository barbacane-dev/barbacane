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
        name: http-upstream
        config:
          url: "https://api.example.com"
```

### Operation Middlewares

Apply to specific operations (run after global):

```yaml
paths:
  /admin/users:
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
      quota: 100
      window: 60
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
            quota: 1000
            window: 60
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

## Consumer Identity Headers

All authentication middlewares set two standard headers on successful authentication, in addition to their plugin-specific headers:

| Header | Description | Example |
|--------|-------------|---------|
| `x-auth-consumer` | Canonical consumer identifier | `"alice"`, `"user-123"` |
| `x-auth-consumer-groups` | Comma-separated group/role memberships | `"admin,editor"`, `"read"` |

These standard headers enable downstream middlewares (like [acl](#acl)) to enforce authorization without coupling to a specific auth plugin.

| Plugin | `x-auth-consumer` source | `x-auth-consumer-groups` source |
|--------|--------------------------|----------------------------------|
| `basic-auth` | username | `roles` array |
| `jwt-auth` | `sub` claim | configurable via `groups_claim` |
| `oidc-auth` | `sub` claim | `scope` claim (space→comma) |
| `oauth2-auth` | `sub` claim (fallback: `username`) | `scope` claim (space→comma) |
| `apikey-auth` | `id` field | `scopes` array |

---

## Authentication Middlewares

### jwt-auth

Validates JWT tokens with RS256/HS256 signatures.

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: "https://auth.example.com"  # Optional: validate iss claim
      audience: "my-api"                  # Optional: validate aud claim
      groups_claim: "roles"               # Optional: claim name for consumer groups
      skip_signature_validation: true     # Required until JWKS support is implemented
```

Accepted algorithms: RS256, RS384, RS512, ES256, ES384, ES512. HS256/HS512 and `none` are rejected.

**Note:** Cryptographic signature validation is not yet implemented. Set `skip_signature_validation: true` in production until JWKS support lands. Without it, all tokens are rejected with 401 at the signature step.

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `issuer` | string | - | Expected `iss` claim. Tokens not matching are rejected |
| `audience` | string | - | Expected `aud` claim. Tokens not matching are rejected |
| `clock_skew_seconds` | integer | `60` | Tolerance in seconds for `exp`/`nbf` validation |
| `groups_claim` | string | - | Claim name to extract consumer groups from (e.g., `"roles"`, `"groups"`). Value is set as `x-auth-consumer-groups` |
| `skip_signature_validation` | boolean | `false` | Skip cryptographic signature check. Required until JWKS support is implemented |

#### Context Headers

Sets headers for downstream:
- `x-auth-consumer` - Consumer identifier (from `sub` claim)
- `x-auth-consumer-groups` - Comma-separated groups (from `groups_claim`, if configured)
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
- `x-auth-consumer` - Consumer identifier (from key `id`)
- `x-auth-consumer-groups` - Comma-separated groups (from key `scopes`)
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
- `x-auth-consumer` - Consumer identifier (from `sub`, fallback to `username`)
- `x-auth-consumer-groups` - Comma-separated groups (from `scope`)
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

### oidc-auth

OpenID Connect authentication via OIDC Discovery and JWKS. Automatically fetches the provider's signing keys and validates JWT tokens with full cryptographic verification.

```yaml
x-barbacane-middlewares:
  - name: oidc-auth
    config:
      issuer_url: https://accounts.google.com
      audience: my-api-client-id
      required_scopes: "openid profile email"
      issuer_override: https://external.example.com  # optional
      clock_skew_seconds: 60
      jwks_refresh_seconds: 300
      timeout: 5.0
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `issuer_url` | string | **required** | OIDC issuer URL (e.g., `https://accounts.google.com`) |
| `audience` | string | - | Expected `aud` claim. If set, tokens must match |
| `required_scopes` | string | - | Space-separated required scopes |
| `issuer_override` | string | - | Override expected `iss` claim (for split-network setups like Docker) |
| `clock_skew_seconds` | integer | `60` | Clock skew tolerance for `exp`/`nbf` validation |
| `jwks_refresh_seconds` | integer | `300` | How often to refresh JWKS keys (seconds) |
| `timeout` | float | `5.0` | HTTP timeout for discovery and JWKS calls (seconds) |

#### How It Works

1. Extracts the Bearer token from the `Authorization` header
2. Parses the JWT header to determine the signing algorithm and key ID (`kid`)
3. Fetches `{issuer_url}/.well-known/openid-configuration` (cached)
4. Fetches the JWKS endpoint from the discovery document (cached with TTL)
5. Finds the matching public key by `kid` (or `kty`/`use` fallback)
6. Verifies the signature using `host_verify_signature` (RS256/RS384/RS512, ES256/ES384)
7. Validates claims: `iss`, `aud`, `exp`, `nbf`
8. Checks required scopes (if configured)

#### Context Headers

Sets headers for downstream:
- `x-auth-consumer` - Consumer identifier (from `sub` claim)
- `x-auth-consumer-groups` - Comma-separated groups (from `scope`, space→comma)
- `x-auth-sub` - Subject (user ID)
- `x-auth-scope` - Token scopes
- `x-auth-claims` - Full JWT payload as JSON

#### Error Responses

- `401 Unauthorized` - Missing token, invalid token, expired token, bad signature, unknown issuer
- `403 Forbidden` - Token lacks required scopes

Includes RFC 6750 `WWW-Authenticate` header with error details.

---

### basic-auth

Validates credentials from the `Authorization: Basic` header per RFC 7617. Useful for internal APIs, admin endpoints, or simple services that don't need a full identity provider.

```yaml
x-barbacane-middlewares:
  - name: basic-auth
    config:
      realm: "My API"
      strip_credentials: true
      credentials:
        admin:
          password: "env://ADMIN_PASSWORD"
          roles: ["admin", "editor"]
        readonly:
          password: "env://READONLY_PASSWORD"
          roles: ["viewer"]
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `realm` | string | `api` | Authentication realm shown in `WWW-Authenticate` challenge |
| `strip_credentials` | boolean | `true` | Remove `Authorization` header before forwarding to upstream |
| `credentials` | object | `{}` | Map of username to credential entry |

Each credential entry:

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `password` | string | **required** | Password for this user (supports secret references) |
| `roles` | array | `[]` | Optional roles for authorization |

#### Context Headers

Sets headers for downstream:
- `x-auth-consumer` - Consumer identifier (username)
- `x-auth-consumer-groups` - Comma-separated groups (from `roles`)
- `x-auth-user` - Authenticated username
- `x-auth-roles` - Comma-separated roles (only set if the user has roles)

#### Error Responses

Returns `401 Unauthorized` with `WWW-Authenticate: Basic realm="<realm>"` and Problem JSON:

```json
{
  "type": "urn:barbacane:error:authentication-failed",
  "title": "Authentication failed",
  "status": 401,
  "detail": "Invalid username or password"
}
```

---

## Authorization Middlewares

### acl

Enforces access control based on consumer identity and group membership. Reads the standard `x-auth-consumer` and `x-auth-consumer-groups` headers set by upstream auth plugins.

```yaml
x-barbacane-middlewares:
  - name: basic-auth
    config:
      realm: "my-api"
      credentials:
        admin:
          password: "env://ADMIN_PASSWORD"
          roles: ["admin", "editor"]
        viewer:
          password: "env://VIEWER_PASSWORD"
          roles: ["viewer"]
  - name: acl
    config:
      allow:
        - admin
      deny:
        - banned
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allow` | array | `[]` | Group names allowed access. If non-empty, consumer must belong to at least one |
| `deny` | array | `[]` | Group names denied access (takes precedence over `allow`) |
| `allow_consumers` | array | `[]` | Specific consumer IDs allowed (bypasses group checks) |
| `deny_consumers` | array | `[]` | Specific consumer IDs denied (highest precedence) |
| `consumer_groups` | object | `{}` | Static consumer-to-groups mapping, merged with `x-auth-consumer-groups` header |
| `message` | string | `Access denied by ACL policy` | Custom 403 error message |
| `hide_consumer_in_errors` | boolean | `false` | Suppress consumer identity in 403 error body |

#### Evaluation Order

1. Missing/empty `x-auth-consumer` header → **403**
2. `deny_consumers` match → **403**
3. `allow_consumers` match → **200** (bypasses group checks)
4. Resolve groups (merge `x-auth-consumer-groups` header + static `consumer_groups` config)
5. `deny` group match → **403** (takes precedence over allow)
6. `allow` non-empty + group match → **200**
7. `allow` non-empty + no group match → **403**
8. `allow` empty → **200** (only deny rules active)

#### Static Consumer Groups

You can supplement the groups from the auth plugin with static mappings:

```yaml
- name: acl
  config:
    allow:
      - premium
    consumer_groups:
      free_user:
        - premium    # Grant premium access to specific consumers
```

Groups from the `consumer_groups` config are merged with the `x-auth-consumer-groups` header (deduplicated).

#### Error Response

Returns `403 Forbidden` with Problem JSON (RFC 9457):

```json
{
  "type": "urn:barbacane:error:acl-denied",
  "title": "Forbidden",
  "status": 403,
  "detail": "Access denied by ACL policy",
  "consumer": "alice"
}
```

Set `hide_consumer_in_errors: true` to omit the `consumer` field.

### opa-authz

Policy-based access control via [Open Policy Agent](https://www.openpolicyagent.org/). Sends request context to an OPA REST API endpoint and enforces the boolean decision. Typically placed after an authentication middleware so that auth claims are available as OPA input.

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: "https://auth.example.com"
      skip_signature_validation: true
  - name: opa-authz
    config:
      opa_url: "http://opa:8181/v1/data/authz/allow"
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `opa_url` | string | *(required)* | OPA Data API endpoint URL (e.g., `http://opa:8181/v1/data/authz/allow`) |
| `timeout` | number | `5` | HTTP request timeout in seconds for OPA calls |
| `include_body` | boolean | `false` | Include the request body in the OPA input payload |
| `include_claims` | boolean | `true` | Include parsed `x-auth-claims` header (set by upstream auth plugins) in the OPA input |
| `deny_message` | string | `Authorization denied by policy` | Custom message returned in the 403 response body |

#### OPA Input Payload

The plugin POSTs the following JSON to your OPA endpoint:

```json
{
  "input": {
    "method": "GET",
    "path": "/admin/users",
    "query": "page=1",
    "headers": { "x-auth-consumer": "alice" },
    "client_ip": "10.0.0.1",
    "claims": { "sub": "alice", "roles": ["admin"] },
    "body": "..."
  }
}
```

- `claims` is included only when `include_claims` is `true` and the `x-auth-claims` header contains valid JSON (set by auth plugins like `jwt-auth`, `oauth2-auth`)
- `body` is included only when `include_body` is `true`

#### Decision Logic

The plugin expects OPA to return the standard Data API response:

```json
{ "result": true }
```

| OPA Response | Result |
|-------------|--------|
| `{"result": true}` | **200** — request continues |
| `{"result": false}` | **403** — access denied |
| `{}` (undefined document) | **403** — access denied |
| Non-boolean `result` | **403** — access denied |
| OPA unreachable or error | **503** — service unavailable |

#### Error Responses

**403 Forbidden** — OPA denies access:

```json
{
  "type": "urn:barbacane:error:opa-denied",
  "title": "Forbidden",
  "status": 403,
  "detail": "Authorization denied by policy"
}
```

**503 Service Unavailable** — OPA is unreachable or returns a non-200 status:

```json
{
  "type": "urn:barbacane:error:opa-unavailable",
  "title": "Service Unavailable",
  "status": 503,
  "detail": "OPA service unreachable"
}
```

#### Example OPA Policy

```rego
package authz

default allow := false

# Allow admins everywhere
allow if {
    input.claims.roles[_] == "admin"
}

# Allow GET on public paths
allow if {
    input.method == "GET"
    startswith(input.path, "/public/")
}
```

### cel

Inline policy evaluation using [CEL (Common Expression Language)](https://cel.dev/). Evaluates expressions directly in-process — no external service needed. CEL is the same language used by Envoy, Kubernetes, and Firebase for policy rules.

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: "https://auth.example.com"
  - name: cel
    config:
      expression: >
        'admin' in request.claims.roles
        || (request.method == 'GET' && request.path.startsWith('/public/'))
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `expression` | string | *(required)* | CEL expression that must evaluate to a boolean |
| `deny_message` | string | `Access denied by policy` | Custom message returned in the 403 response body |

#### Request Context

The expression has access to a `request` object with these fields:

| Variable | Type | Description |
|----------|------|-------------|
| `request.method` | string | HTTP method (`GET`, `POST`, etc.) |
| `request.path` | string | Request path (e.g., `/api/users`) |
| `request.query` | string | Query string (empty string if none) |
| `request.headers` | map | Request headers (e.g., `request.headers.authorization`) |
| `request.body` | string | Request body (empty string if none) |
| `request.client_ip` | string | Client IP address |
| `request.path_params` | map | Path parameters (e.g., `request.path_params.id`) |
| `request.consumer` | string | Consumer identity from `x-auth-consumer` header (empty if absent) |
| `request.claims` | map | Parsed JSON from `x-auth-claims` header (empty map if absent/invalid) |

#### CEL Features

CEL supports a rich expression language:

```cel
// String operations
request.path.startsWith('/api/')
request.path.endsWith('.json')
request.headers.host.contains('example')

// List operations
'admin' in request.claims.roles
request.claims.roles.exists(r, r == 'editor')

// Field presence
has(request.claims.email)

// Logical operators
request.method == 'GET' && request.consumer != ''
request.method in ['GET', 'HEAD', 'OPTIONS']
!(request.client_ip.startsWith('192.168.'))
```

#### Decision Logic

| Expression Result | HTTP Response |
|------------------|---------------|
| `true` | Request continues to next middleware/dispatcher |
| `false` | **403** Forbidden |
| Non-boolean | **500** Internal Server Error |
| Parse/evaluation error | **500** Internal Server Error |

#### Error Responses

**403 Forbidden** — expression evaluates to `false`:

```json
{
  "type": "urn:barbacane:error:cel-denied",
  "title": "Forbidden",
  "status": 403,
  "detail": "Access denied by policy"
}
```

**500 Internal Server Error** — invalid expression or non-boolean result:

```json
{
  "type": "urn:barbacane:error:cel-evaluation",
  "title": "Internal Server Error",
  "status": 500,
  "detail": "expression returned string, expected bool"
}
```

#### CEL vs OPA

| | `cel` | `opa-authz` |
|---|---|---|
| Deployment | Embedded (no sidecar) | External OPA server |
| Language | CEL | Rego |
| Latency | Microseconds (in-process) | HTTP round-trip |
| Best for | Inline route-level rules | Complex policy repos, audit trails |

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

### bot-detection

Blocks requests from known bots and scrapers by matching the `User-Agent` header against configurable deny patterns. An allow list lets trusted crawlers bypass the deny list.

```yaml
x-barbacane-middlewares:
  - name: bot-detection
    config:
      deny:
        - scrapy
        - ahrefsbot
        - semrushbot
        - mj12bot
        - dotbot
      allow:
        - Googlebot
        - Bingbot
      block_empty_ua: false
      message: "Automated access is not permitted"
      status: 403
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `deny` | array | `[]` | User-Agent substrings to block (case-insensitive substring match) |
| `allow` | array | `[]` | User-Agent substrings that override the deny list (trusted crawlers) |
| `block_empty_ua` | boolean | `false` | Block requests with no `User-Agent` header |
| `message` | string | `Access denied` | Custom error message for blocked requests |
| `status` | integer | `403` | HTTP status code for blocked requests |

#### Behavior

- Matching is **case-insensitive substring**: `"bot"` matches `"AhrefsBot"`, `"DotBot"`, etc.
- The **allow list takes precedence** over deny: a UA matching both allow and deny is allowed through
- Missing `User-Agent` is permitted by default; set `block_empty_ua: true` to block it
- Both `deny` and `allow` are empty by default — the plugin is a no-op unless configured

#### Error Response

Returns Problem JSON (RFC 7807):

```json
{
  "type": "urn:barbacane:error:bot-detected",
  "title": "Forbidden",
  "status": 403,
  "detail": "Access denied",
  "user_agent": "scrapy/2.11"
}
```

The `user_agent` field is omitted when the request had no `User-Agent` header.

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

## Logging

### http-log

Sends structured JSON log entries to an HTTP endpoint for centralized logging. Captures request metadata, response status, timing, and optional headers/body sizes. Compatible with Datadog, Splunk, ELK, or any HTTP log ingestion endpoint.

```yaml
x-barbacane-middlewares:
  - name: http-log
    config:
      endpoint: https://logs.example.com/ingest
      method: POST
      timeout_ms: 2000
      include_headers: false
      include_body: true
      custom_fields:
        service: my-api
        environment: production
```

#### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `endpoint` | string | **required** | URL to send log entries to |
| `method` | string | `POST` | HTTP method (`POST` or `PUT`) |
| `timeout_ms` | integer | `2000` | Timeout for the log HTTP call (100-10000 ms) |
| `content_type` | string | `application/json` | Content-Type header for the log request |
| `include_headers` | boolean | `false` | Include request and response headers in log entries |
| `include_body` | boolean | `false` | Include request and response body sizes in log entries |
| `custom_fields` | object | `{}` | Static key-value fields included in every log entry |

#### Log Entry Format

Each log entry is a JSON object:

```json
{
  "timestamp_ms": 1706500000000,
  "duration_ms": 42,
  "correlation_id": "abc-123",
  "request": {
    "method": "POST",
    "path": "/users",
    "query": "page=1",
    "client_ip": "10.0.0.1",
    "headers": { "content-type": "application/json" },
    "body_size": 256
  },
  "response": {
    "status": 201,
    "headers": { "content-type": "application/json" },
    "body_size": 64
  },
  "service": "my-api",
  "environment": "production"
}
```

Optional fields (`correlation_id`, `headers`, `body_size`, `query`) are omitted when not available or not enabled.

#### Behavior

- Runs in the **response phase** (after dispatch) to capture both request and response data
- Log delivery is **best-effort** — failures never affect the upstream response
- The `correlation_id` field is automatically populated if the `correlation-id` middleware runs earlier in the chain
- Custom fields are flattened into the top-level JSON object

---

## Request Transformation

### request-transformer

Declaratively modifies requests before they reach the dispatcher. Supports header, query parameter, path, and JSON body transformations with variable interpolation.

```yaml
x-barbacane-middlewares:
  - name: request-transformer
    config:
      headers:
        add:
          X-Gateway: "barbacane"
          X-Client-IP: "$client_ip"
        set:
          X-Request-Source: "external"
        remove:
          - Authorization
          - X-Internal-Token
        rename:
          X-Old-Name: X-New-Name
      querystring:
        add:
          gateway: "barbacane"
          userId: "$path.userId"
        remove:
          - internal_token
        rename:
          oldParam: newParam
      path:
        strip_prefix: "/api/v1"
        add_prefix: "/internal"
        replace:
          pattern: "/users/(\\w+)/orders"
          replacement: "/v2/orders/$1"
      body:
        add:
          /metadata/gateway: "barbacane"
          /userId: "$path.userId"
        remove:
          - /password
          - /internal_flags
        rename:
          /userName: /user_name
```

#### Configuration

##### headers

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite headers. Supports variable interpolation |
| `set` | object | `{}` | Add headers only if not already present. Supports variable interpolation |
| `remove` | array | `[]` | Remove headers by name (case-insensitive) |
| `rename` | object | `{}` | Rename headers (old-name to new-name) |

##### querystring

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite query parameters. Supports variable interpolation |
| `remove` | array | `[]` | Remove query parameters by name |
| `rename` | object | `{}` | Rename query parameters (old-name to new-name) |

##### path

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `strip_prefix` | string | - | Remove prefix from path (e.g., `/api/v2`) |
| `add_prefix` | string | - | Add prefix to path (e.g., `/internal`) |
| `replace.pattern` | string | - | Regex pattern to match in path |
| `replace.replacement` | string | - | Replacement string (supports regex capture groups) |

Path operations are applied in order: strip prefix, add prefix, regex replace.

##### body

JSON body transformations use [JSON Pointer (RFC 6901)](https://tools.ietf.org/html/rfc6901) paths.

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite JSON fields. Supports variable interpolation |
| `remove` | array | `[]` | Remove JSON fields by JSON Pointer path |
| `rename` | object | `{}` | Rename JSON fields (old-pointer to new-pointer) |

Body transformations only apply to requests with `application/json` content type. Non-JSON bodies pass through unchanged.

#### Variable Interpolation

Values in `add`, `set`, and body `add` support variable templates:

| Variable | Description | Example |
|----------|-------------|---------|
| `$client_ip` | Client IP address | `192.168.1.1` |
| `$header.<name>` | Request header value (case-insensitive) | `$header.host` |
| `$query.<name>` | Query parameter value | `$query.page` |
| `$path.<name>` | Path parameter value | `$path.userId` |
| `context:<key>` | Request context value (set by other middlewares) | `context:auth.sub` |

Variables always resolve against the **original** incoming request, regardless of transformations applied by earlier sections. This means a query parameter removed in `querystring.remove` is still available via `$query.<name>` in `body.add`.

If a variable cannot be resolved, it is replaced with an empty string.

#### Transformation Order

Transformations are applied in this order:

1. **Path** — strip prefix, add prefix, regex replace
2. **Headers** — add, set, remove, rename
3. **Query parameters** — add, remove, rename
4. **Body** — add, remove, rename

#### Use Cases

**Strip API version prefix:**
```yaml
- name: request-transformer
  config:
    path:
      strip_prefix: "/api/v2"
```

**Move query parameter to body (ADR-0020 showcase):**
```yaml
- name: request-transformer
  config:
    querystring:
      remove:
        - userId
    body:
      add:
        /userId: "$query.userId"
```

**Add gateway metadata to every request:**
```yaml
# Global middleware
x-barbacane-middlewares:
  - name: request-transformer
    config:
      headers:
        add:
          X-Gateway: "barbacane"
          X-Client-IP: "$client_ip"
```

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
      partition_key: context:auth.sub  # Rate limit per user
```

---

## Best Practices

### Order Matters

Put middlewares in logical order:

```yaml
x-barbacane-middlewares:
  - name: correlation-id       # 1. Add tracing ID first
  - name: http-log             # 2. Log all requests (captures full lifecycle)
  - name: cors                 # 3. Handle CORS early
  - name: ip-restriction       # 4. Block bad IPs immediately
  - name: request-size-limit   # 5. Reject oversized requests
  - name: rate-limit           # 6. Rate limit before auth (cheaper)
  - name: oidc-auth            # 7. Authenticate (OIDC/JWT)
  - name: basic-auth           # 8. Authenticate (fallback)
  - name: acl                  # 9. Authorize (after auth sets consumer headers)
  - name: request-transformer  # 10. Transform request before dispatch
```

### Fail Fast

Put restrictive middlewares early to reject bad requests quickly:

```yaml
x-barbacane-middlewares:
  - name: ip-restriction      # Block banned IPs immediately
  - name: request-size-limit  # Reject large payloads early
  - name: rate-limit          # Reject over-limit immediately
  - name: jwt-auth            # Reject unauthorized before processing
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
