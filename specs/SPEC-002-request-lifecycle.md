# SPEC-002: Data Plane — Request Lifecycle

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0004, ADR-0005, ADR-0012

---

## 1. Overview

The data plane (`barbacane`) is a stateless HTTP proxy that loads a compiled artifact (SPEC-001) at startup and processes requests through a fixed pipeline: route, validate, middleware chain, dispatch, respond. This spec defines every phase of that pipeline and the exact behavior at each step.

---

## 2. Startup Sequence

The data plane starts in a strict order. If any step fails, the process exits with a non-zero code — it never serves traffic in a degraded state.

```
1. Parse CLI flags
2. Load artifact (.bca)
3. Verify checksums (manifest vs files)
4. Memory-map FlatBuffers (routes.fb, schemas.fb, middleware-chains.fb)
5. AOT-compile WASM modules (plugins + policies)
6. Connect to vault, fetch secrets
7. Call init(config) on every plugin instance
8. Bind to listen address
9. Serve traffic
```

### 2.1 Startup errors

| Step | Failure | Exit code |
|------|---------|-----------|
| 2 | Artifact file not found or corrupt | `10` |
| 3 | Checksum mismatch | `11` |
| 5 | WASM module fails to compile | `12` |
| 6 | Vault unreachable or secret not found | `13` |
| 7 | Plugin `init` returns non-zero | `14` |
| 8 | Port already in use | `15` |

### 2.2 CLI flags

```
barbacane [OPTIONS]

OPTIONS:
  --artifact <PATH>              Path to .bca file (required)
  --listen <ADDR>                Listen address (default: 0.0.0.0:8080)
  --tls-cert <PATH>              TLS certificate path (or vault:// reference)
  --tls-key <PATH>               TLS private key path (or vault:// reference)
  --vault-addr <URL>             Vault server address
  --vault-token <TOKEN>          Vault authentication token
  --allow-plaintext-upstream     Allow http:// upstreams (development only, refused in production builds)
  --dev                          Enable development mode (verbose errors, detailed logs)
  --log-level <LEVEL>            Log level: error, warn, info, debug, trace (default: info)
  --otlp-endpoint <URL>          OpenTelemetry collector endpoint
```

---

## 3. Request Pipeline

Every incoming request passes through the following phases in order. Each phase either advances to the next or short-circuits with an error response.

```
Client
  │
  ▼
┌─────────────────┐
│  TLS Termination│  (if TLS configured)
└────────┬────────┘
         ▼
┌─────────────────┐
│   HTTP Parse    │  Hyper decodes HTTP/1.1 or HTTP/2
└────────┬────────┘
         ▼
┌─────────────────┐
│  Request Limits │  Size + header count checks
└────────┬────────┘
         ▼
┌─────────────────┐
│    Routing      │  Prefix-trie lookup
└────────┬────────┘
         ▼
┌─────────────────┐
│   Validation    │  Path params, query, headers, body
└────────┬────────┘
         ▼
┌─────────────────┐
│  Middleware Chain│  Ordered WASM plugin calls
└────────┬────────┘
         ▼
┌─────────────────┐
│    Dispatch     │  Forward to upstream / mock / broker
└────────┬────────┘
         ▼
┌─────────────────┐
│ Middleware Chain │  Response phase (reverse order)
│  (response)     │
└────────┬────────┘
         ▼
Client
```

---

## 4. Phase Details

### 4.1 TLS Termination

- Handled by `rustls`.
- TLS 1.2 minimum, TLS 1.3 preferred.
- If no TLS cert/key is configured, the data plane listens on plain HTTP (development only).
- ALPN negotiation for HTTP/2 support over TLS.

### 4.2 HTTP Parse

Hyper parses the raw bytes into an HTTP request. Malformed requests are rejected with a TCP close (no HTTP response — the request is not parseable).

### 4.3 Request Limits

Before any routing or validation, hard limits are enforced:

| Limit | Default | Configurable |
|-------|---------|-------------|
| Max request body size | 1 MB | Per-operation via `requestBody.x-barbacane-max-size` |
| Max header count | 100 | Global via `x-barbacane-limits.max_headers` |
| Max header size | 8 KB per header | Global via `x-barbacane-limits.max_header_size` |
| Max URI length | 8 KB | Global via `x-barbacane-limits.max_uri_length` |
| Request timeout | 30s | Per-operation via `x-barbacane-dispatch.config.timeout` |

Exceeding a limit returns:

| Limit exceeded | Status | `type` URN |
|---------------|--------|------------|
| Body size | `413` | `urn:barbacane:error:payload-too-large` |
| Header count/size | `431` | `urn:barbacane:error:header-too-large` |
| URI length | `414` | `urn:barbacane:error:uri-too-long` |
| Timeout | `408` | `urn:barbacane:error:request-timeout` |

### 4.4 Routing

The prefix trie from `routes.fb` is consulted:

1. **Path lookup:** Walk the trie segment by segment. Static segments match exactly; parameter segments capture the value.
2. **Method check:** At the terminal node, check if the request method is in the allowed set.

| Outcome | Status | `type` URN |
|---------|--------|------------|
| No path match | `404` | `urn:barbacane:error:route-not-found` |
| Path matches but method does not | `405` | `urn:barbacane:error:method-not-allowed` |
| Match found | Continue to validation | — |

On `405`, the response includes an `Allow` header listing the permitted methods.

### 4.5 Validation

Validation runs against the matched operation's compiled schemas. Order:

1. **Path parameters** — each captured parameter is validated against its schema (type, format, enum, etc.)
2. **Query parameters** — required params must be present; all present params are validated
3. **Headers** — required headers must be present; declared headers are validated
4. **Content-Type** — must match one of the operation's `requestBody.content` keys
5. **Request body** — validated against the JSON Schema for the matched content type

**Fail-fast:** validation stops at the first failure and returns `400`.

```json
{
  "type": "urn:barbacane:error:validation-failed",
  "title": "Validation Failed",
  "status": 400,
  "detail": "Request body does not conform to the expected schema.",
  "instance": "/users/123"
}
```

In development mode (`--dev`), the `errors` extension is included:

```json
{
  "type": "urn:barbacane:error:validation-failed",
  "title": "Validation Failed",
  "status": 400,
  "detail": "Request body does not conform to the expected schema.",
  "instance": "/users/123",
  "errors": [
    {
      "field": "/email",
      "reason": "missing_required_field",
      "expected": "string (format: email)"
    }
  ],
  "spec": "user-api.yaml",
  "operation": "createUser"
}
```

### 4.6 Middleware Chain (Request Phase)

The resolved middleware chain for the matched operation is executed in order. Each middleware's `on_request` is called with the (possibly modified) request.

A middleware can:
- **Continue** — pass the request (possibly modified) to the next middleware
- **Short-circuit** — return a response immediately (e.g., `401`, `403`, `429`)

If a middleware short-circuits, no further middlewares or dispatch runs. The response is returned directly (skipping the response phase of earlier middlewares too — the short-circuit response is final).

### 4.7 Dispatch

The operation's dispatcher plugin is called with the fully processed request. The dispatcher delivers the request to its target and returns a response.

| Dispatch outcome | Behavior |
|------------------|----------|
| Success | Response is returned to the middleware response phase |
| Upstream timeout | `504 Gateway Timeout` — `type`: `urn:barbacane:error:upstream-timeout` |
| Upstream connection refused | `502 Bad Gateway` — `type`: `urn:barbacane:error:upstream-unavailable` |
| Circuit breaker open | `503 Service Unavailable` — `type`: `urn:barbacane:error:circuit-open` |
| Plugin panic/trap | `500 Internal Server Error` — `type`: `urn:barbacane:error:internal-error` |

### 4.8 Middleware Chain (Response Phase)

After dispatch, the middleware chain runs in **reverse order**. Each middleware's `on_response` is called with the response.

Middlewares can modify the response (add headers, transform body). They cannot short-circuit during the response phase — every middleware gets a chance to process.

### 4.9 Response

The final response is sent to the client. The gateway adds these headers to every response:

| Header | Value | Always present |
|--------|-------|----------------|
| `X-Request-Id` | UUID v4 (generated at request start) | Yes |
| `X-Trace-Id` | W3C trace ID (from `traceparent`) | Yes |
| `Sunset` | RFC 8594 date (if `x-sunset` is set on the operation) | Conditional |
| `Server` | `barbacane/<version>` | Yes |
| `RateLimit-Policy` | Quota policy per [draft-ietf-httpapi-ratelimit-headers](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/) | If rate limiting is active on the operation |
| `RateLimit` | Remaining quota and reset time | If rate limiting is active on the operation |
| `Retry-After` | Seconds until quota resets (on `429` only) | If rate-limited |

Upstream `Server` headers are stripped and replaced.

#### Rate limit headers

When `x-barbacane-ratelimit` is configured on an operation, the rate-limit middleware emits standardized headers on every response:

```
RateLimit-Policy: "default";q=100;w=60
RateLimit: "default";r=73;t=45
```

On `429 Too Many Requests`:

```
RateLimit-Policy: "default";q=100;w=60
RateLimit: "default";r=0;t=12
Retry-After: 12
```

---

## 5. Error Response Format

All gateway-generated errors follow RFC 9457 (`application/problem+json`).

### 5.1 Common fields

| Field | Type | Description |
|-------|------|-------------|
| `type` | string (URN) | Error type identifier, always starts with `urn:barbacane:error:` |
| `title` | string | Human-readable summary |
| `status` | integer | HTTP status code |
| `detail` | string | Human-readable explanation for this specific occurrence |
| `instance` | string | Request path that triggered the error |

### 5.2 Error catalog

| `type` URN | Status | Title | Trigger |
|------------|--------|-------|---------|
| `urn:barbacane:error:validation-failed` | 400 | Validation Failed | Schema validation failure |
| `urn:barbacane:error:unauthorized` | 401 | Unauthorized | Auth middleware rejection |
| `urn:barbacane:error:forbidden` | 403 | Forbidden | Authorization policy denial |
| `urn:barbacane:error:route-not-found` | 404 | Not Found | No matching path in trie |
| `urn:barbacane:error:method-not-allowed` | 405 | Method Not Allowed | Path exists, method does not |
| `urn:barbacane:error:request-timeout` | 408 | Request Timeout | Client request exceeded timeout |
| `urn:barbacane:error:payload-too-large` | 413 | Payload Too Large | Body exceeds max size |
| `urn:barbacane:error:uri-too-long` | 414 | URI Too Long | URI exceeds max length |
| `urn:barbacane:error:rate-limited` | 429 | Too Many Requests | Rate limiter triggered |
| `urn:barbacane:error:header-too-large` | 431 | Header Too Large | Header count or size exceeded |
| `urn:barbacane:error:internal-error` | 500 | Internal Server Error | Plugin crash or misconfiguration |
| `urn:barbacane:error:upstream-unavailable` | 502 | Bad Gateway | Upstream refused connection |
| `urn:barbacane:error:circuit-open` | 503 | Service Unavailable | Circuit breaker is open |
| `urn:barbacane:error:upstream-timeout` | 504 | Gateway Timeout | Upstream did not respond in time |

### 5.3 Development mode extensions

In `--dev` mode, error responses include additional fields:

| Field | Type | Present on |
|-------|------|-----------|
| `errors` | array of `{field, reason, expected}` | Validation errors |
| `spec` | string | All errors |
| `operation` | string | All errors (after routing) |
| `middleware` | string | Auth/authz errors |
| `dispatcher` | string | Dispatch errors |

These fields are **never** present in production mode.

---

## 6. Connection Handling

### 6.1 HTTP keep-alive

HTTP/1.1 connections are kept alive by default. The data plane closes idle connections after **60 seconds**.

### 6.2 HTTP/2

HTTP/2 is supported via ALPN negotiation over TLS. Settings:

| Setting | Value |
|---------|-------|
| Max concurrent streams | 128 |
| Initial window size | 64 KB |
| Max frame size | 16 KB |
| Max header list size | 16 KB |

### 6.3 Graceful shutdown

On SIGTERM:

1. Stop accepting new connections
2. Wait for in-flight requests to complete (up to 30 seconds)
3. Force-close remaining connections
4. Exit with code `0`

---

## 7. Health Endpoint

The data plane exposes a health endpoint that is **not** derived from the spec and cannot be overridden:

```
GET /__barbacane/health
```

Response:

```json
{
  "status": "healthy",
  "artifact": "<sha256 of manifest>",
  "uptime_seconds": 3600
}
```

Returns `200` when the gateway is ready to serve traffic, `503` during startup or shutdown.

This endpoint:
- Bypasses the entire request pipeline (no routing, validation, middlewares)
- Is excluded from metrics and traces
- Does not require TLS even when TLS is configured (listens on a separate port if needed)

---

## 8. Metrics Endpoint

```
GET /__barbacane/metrics
```

Returns Prometheus exposition format. See SPEC-005 for the full metrics catalog.

This endpoint has the same bypass behavior as the health endpoint.
