# Traffic Control Middlewares

Plugins that decide whether a request makes it to the dispatcher at all — rate limits, CORS, IP allow/deny, bot patterns, payload size caps.

- [`rate-limit`](#rate-limit) — sliding-window request rate limiting
- [`cors`](#cors) — Cross-Origin Resource Sharing
- [`ip-restriction`](#ip-restriction) — allow/deny by IP or CIDR
- [`bot-detection`](#bot-detection) — User-Agent-based blocking
- [`request-size-limit`](#request-size-limit) — body-size cap

---

## rate-limit

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `quota` | integer | **required** | Maximum requests allowed in the window |
| `window` | integer | **required** | Window duration in seconds |
| `policy_name` | string | `default` | Policy name for `RateLimit-Policy` header and the rate-limit bucket-key prefix |
| `partition_key` | string | `client_ip` | Rate limit key source |

### Partition key sources

- `client_ip` — Client IP from `X-Forwarded-For` or `X-Real-IP`
- `header:<name>` — Header value (e.g., `header:X-API-Key`)
- `context:<key>` — Context value set by an upstream middleware (e.g., `context:auth.sub`)
- Any static string — same limit for all requests sharing that string

### Response headers

On allowed requests:
- `X-RateLimit-Policy` — Policy name and configuration
- `X-RateLimit-Limit` — Maximum requests in window
- `X-RateLimit-Remaining` — Remaining requests
- `X-RateLimit-Reset` — Unix timestamp when window resets

On rate-limited requests (429):
- `RateLimit-Policy` — IETF draft header
- `RateLimit` — IETF draft combined header
- `Retry-After` — Seconds until retry is allowed

### Layered rate limits (stacking)

Stack multiple instances with **distinct `policy_name`**s to enforce layered limits — for example, a per-IP burst cap *and* a per-user daily budget:

```yaml
x-barbacane-middlewares:
  - name: rate-limit
    config:
      policy_name: per-ip-burst
      quota: 100
      window: 60
      partition_key: client_ip
  - name: rate-limit
    config:
      policy_name: per-user-daily
      quota: 10000
      window: 86400
      partition_key: "context:auth.sub"
```

`policy_name` is also the bucket-key prefix. If two stacked instances share a `policy_name`, they share the bucket — only the tighter of the two will be effective. Always override `policy_name` when stacking.

---

## cors

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allowed_origins` | array | `[]` | Allowed origins (`["*"]` for any, or specific origins) |
| `allowed_methods` | array | `["GET", "POST"]` | Allowed HTTP methods |
| `allowed_headers` | array | `[]` | Allowed request headers (beyond simple headers) |
| `expose_headers` | array | `[]` | Headers exposed to browser JavaScript |
| `max_age` | integer | `3600` | Preflight cache time (seconds) |
| `allow_credentials` | boolean | `false` | Allow credentials (cookies, auth headers) |

### Origin patterns

Origins can be:
- Exact match: `https://app.example.com`
- Wildcard subdomain: `*.example.com` (matches `sub.example.com`)
- Wildcard: `*` (only when `allow_credentials: false`)

### Error responses

- `403 Forbidden` — Origin not in allowed list
- `403 Forbidden` — Method not allowed (preflight)
- `403 Forbidden` — Headers not allowed (preflight)

### Preflight responses

Returns `204 No Content` with:
- `Access-Control-Allow-Origin`
- `Access-Control-Allow-Methods`
- `Access-Control-Allow-Headers`
- `Access-Control-Max-Age`
- `Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers`

---

## ip-restriction

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allow` | array | `[]` | Allowed IPs or CIDR ranges (allowlist mode) |
| `deny` | array | `[]` | Denied IPs or CIDR ranges (denylist mode) |
| `message` | string | `Access denied` | Custom error message for denied requests |
| `status` | integer | `403` | HTTP status code for denied requests |

### Behavior

- If `deny` is configured, IPs in the list are blocked (denylist takes precedence)
- If `allow` is configured, only IPs in the list are permitted (allowlist mode)
- Client IP is extracted from `X-Forwarded-For`, `X-Real-IP`, or direct connection
- Supports both single IPs (`10.0.0.1`) and CIDR notation (`10.0.0.0/8`)

### Error response

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

## bot-detection

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `deny` | array | `[]` | User-Agent substrings to block (case-insensitive substring match) |
| `allow` | array | `[]` | User-Agent substrings that override the deny list (trusted crawlers) |
| `block_empty_ua` | boolean | `false` | Block requests with no `User-Agent` header |
| `message` | string | `Access denied` | Custom error message for blocked requests |
| `status` | integer | `403` | HTTP status code for blocked requests |

### Behavior

- Matching is **case-insensitive substring**: `"bot"` matches `"AhrefsBot"`, `"DotBot"`, etc.
- The **allow list takes precedence** over deny: a UA matching both allow and deny is allowed through
- Missing `User-Agent` is permitted by default; set `block_empty_ua: true` to block it
- Both `deny` and `allow` are empty by default — the plugin is a no-op unless configured

### Error response

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

## request-size-limit

Rejects requests that exceed a configurable body size limit. Checks both `Content-Length` header and actual body size.

```yaml
x-barbacane-middlewares:
  - name: request-size-limit
    config:
      max_bytes: 1048576        # 1 MiB
      check_content_length: true
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `max_bytes` | integer | `1048576` | Maximum allowed request body size in bytes (default: 1 MiB) |
| `check_content_length` | boolean | `true` | Check `Content-Length` header for early rejection |

### Error response

Returns `413 Payload Too Large` with Problem JSON:

```json
{
  "type": "urn:barbacane:error:payload-too-large",
  "title": "Payload Too Large",
  "status": 413,
  "detail": "Request body size 2097152 bytes exceeds maximum allowed size of 1048576 bytes."
}
```
