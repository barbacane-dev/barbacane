# ADR-0012: Error Model

**Status:** Accepted
**Date:** 2026-01-28

## Context

The gateway generates its own errors in several situations:

- Request validation failures (bad schema, missing parameters)
- Authentication/authorization rejections
- Rate limiting
- Dispatch failures (upstream timeout, circuit breaker open)
- Internal errors (plugin crash, misconfiguration)

These errors are distinct from upstream errors (which are proxied as-is). The gateway needs a consistent, standards-based error format that is secure by default — no leaking of internal details to API consumers in production.

## Decision

### RFC 9457 (Problem Details for HTTP APIs)

All gateway-generated errors use the **RFC 9457** format (`application/problem+json`). This is an IETF standard adopted by major APIs (GitHub, Stripe, Azure).

```json
{
  "type": "urn:barbacane:error:validation-failed",
  "title": "Validation Failed",
  "status": 400,
  "detail": "Request body does not conform to the expected schema.",
  "instance": "/users/123"
}
```

Upstream responses are **never modified** — the gateway only wraps its own errors in this format.

### Error Categories

| Category | Status | Type URI suffix | When |
|----------|--------|-----------------|------|
| Validation failed | 400 | `/validation-failed` | Request doesn't match OpenAPI spec |
| Unauthorized | 401 | `/unauthorized` | Auth middleware rejects request |
| Forbidden | 403 | `/forbidden` | OPA policy denies access |
| Not found | 404 | `/route-not-found` | No matching route in spec |
| Method not allowed | 405 | `/method-not-allowed` | Route exists but not for this method |
| Rate limited | 429 | `/rate-limited` | Rate limit exceeded |
| Upstream timeout | 504 | `/upstream-timeout` | Dispatcher timed out |
| Circuit open | 503 | `/circuit-open` | Circuit breaker is open |
| Internal error | 500 | `/internal-error` | Plugin crash, misconfiguration |

### Error Detail Levels

#### Production mode (default)

Errors expose **minimal detail** — enough for the consumer to understand what went wrong, but no internals:

```json
{
  "type": "urn:barbacane:error:validation-failed",
  "title": "Validation Failed",
  "status": 400,
  "detail": "Request body does not conform to the expected schema.",
  "instance": "/users/123"
}
```

No field paths, no schema details, no plugin names, no stack traces.

#### Development mode

Errors include **full diagnostic detail** via an `extensions` block:

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

Dev mode is activated by the `--dev` flag on the data plane binary. This flag is refused in production builds (same pattern as `--allow-plaintext-upstream` in ADR-0009).

### Validation Behavior: Fail Fast

Validation stops at the **first failure** and returns immediately. Rationale:

- Lower latency — no need to validate the full request body when the first field already fails
- Simpler error response — one clear error rather than a list
- Consistent with the "strict" philosophy (ADR-0004) — any violation is a rejection

```
Request body arrives
  → Validate content-type     → fail? → 400
  → Validate required fields  → fail? → 400
  → Validate field schema     → fail? → 400
  → Continue to middlewares
```

### Correlation

Every error response includes the request's trace ID (from W3C Trace Context, ADR-0010) in a response header:

```
HTTP/1.1 400 Bad Request
Content-Type: application/problem+json
X-Trace-Id: abc123def456
```

This allows operators to correlate a consumer's error report with the full trace in the observability backend.

### What the Gateway Does NOT Do

- **No error transformation** — upstream errors pass through unmodified
- **No custom error pages** — the gateway returns JSON, not HTML
- **No error aggregation** — each request gets its own error response (aggregation is an observability concern, ADR-0010)

## Consequences

- **Easier:** Standard error format (RFC 9457) means consumers can use generic error handling, trace correlation simplifies debugging, fail-fast keeps latency predictable
- **Harder:** Consumers who want all validation errors at once must fix and retry iteratively
- **Tradeoff:** Minimal errors in production may frustrate developers during integration — mitigated by dev mode and trace ID correlation for operator-assisted debugging
