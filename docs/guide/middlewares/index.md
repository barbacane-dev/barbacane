# Middlewares

Middlewares process requests before they reach dispatchers and can modify responses on the way back. They handle cross-cutting concerns like authentication, rate limiting, transformation, and caching.

This guide splits middlewares by concern:

- [Authentication](authentication.md) — `jwt-auth`, `apikey-auth`, `oauth2-auth`, `oidc-auth`, `basic-auth`
- [Authorization](authorization.md) — `acl`, `opa-authz`, `cel`
- [Traffic Control](traffic-control.md) — `rate-limit`, `cors`, `ip-restriction`, `bot-detection`, `request-size-limit`
- [Observability](observability.md) — `correlation-id`, `http-log`
- [Transformation](transformation.md) — `request-transformer`, `response-transformer`, `redirect`
- [Caching](caching.md) — `cache`
- [AI Gateway](ai-gateway.md) — `ai-prompt-guard`, `ai-token-limit`, `ai-cost-tracker`, `ai-response-guard`

---

## Declaring middlewares

Middlewares are declared with the `x-barbacane-middlewares` extension — either at the root of a spec (global) or on a single operation:

```yaml
x-barbacane-middlewares:
  - name: <middleware-name>
    config:
      # middleware-specific config
```

## The chain

Middlewares execute in list order on the request path and in reverse on the response path:

```
Request  →  [MW 1]  →  [MW 2]  →  [MW 3]  →  Dispatcher
                                                  │
Response ←  [MW 1]  ←  [MW 2]  ←  [MW 3]  ←──────┘
```

Each entry in the list is an independent plugin instance with its own config and its own runtime state. Barbacane places no uniqueness constraint on the list — a plugin may appear any number of times.

## Stacking

Any middleware can appear multiple times in a chain. Each entry is executed independently; there is no name-based deduplication, no "second entry wins" — every entry runs, in the order you wrote it.

Patterns that rely on stacking:

- **`cel` with `on_match.set_context`** — one entry per routing rule. Each writes context keys that downstream plugins read. See [Policy-driven routing](authorization.md#policy-driven-routing-cel-stacking).
- **`ai-token-limit` with distinct `policy_name`** — multiple windows (per-minute, per-hour). See [Stacking multiple windows](ai-gateway.md#stacking-multiple-windows).
- **`rate-limit` with distinct `partition_key`** — layered limits (per-IP, per-user, per-tenant). See [Layered rate limits](traffic-control.md#layered-rate-limits-stacking).

Stacking is the primary composition mechanism. If a plugin's feature set feels constrained, stacking another instance is usually the answer before reaching for config complexity.

## Global vs operation merge

Global middlewares apply to every operation. Operations can add their own middlewares; the two lists are merged:

```yaml
x-barbacane-middlewares:
  - name: correlation-id
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]

paths:
  /admin/users:
    get:
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: "https://auth.example.com"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.internal"
# Resolved chain: correlation-id → cors → jwt-auth
```

**Name-based override.** When an operation entry has the same `name` as an entry in the global chain, **all** global entries with that name are dropped and the operation entries are appended in their declared order.

```yaml
# Global: rate-limit at 100/min + cors
x-barbacane-middlewares:
  - name: rate-limit
    config: { quota: 100, window: 60 }
  - name: cors
    config: { allow_origin: "*" }

paths:
  /public/feed:
    get:
      x-barbacane-middlewares:
        - name: rate-limit
          config: { quota: 1000, window: 60 }
      # Resolved chain: cors (global) → rate-limit (operation — replaced global)
```

**Consequence for stacked plugins.** A stack of `cel` entries at global level is replaced entirely if the operation declares *any* `cel` entry. To keep a global stack and add to it, re-declare the full stack at the operation level. (In practice, stack at one level.)

**Disabling all middlewares.** Use an empty array to opt a single operation out of the global chain:

```yaml
paths:
  /internal/health:
    get:
      x-barbacane-middlewares: []  # Empty chain, globals ignored
```

---

## Consumer identity headers

All authentication middlewares set two standard headers on successful authentication, in addition to their plugin-specific headers:

| Header | Description | Example |
|--------|-------------|---------|
| `x-auth-consumer` | Canonical consumer identifier | `"alice"`, `"user-123"` |
| `x-auth-consumer-groups` | Comma-separated group/role memberships | `"admin,editor"`, `"read"` |

These standard headers enable downstream middlewares (like [`acl`](authorization.md#acl)) to enforce authorization without coupling to a specific auth plugin.

| Plugin | `x-auth-consumer` source | `x-auth-consumer-groups` source |
|--------|--------------------------|----------------------------------|
| `basic-auth` | username | `roles` array |
| `jwt-auth` | `sub` claim | configurable via `groups_claim` |
| `oidc-auth` | `sub` claim | `scope` claim (space→comma) |
| `oauth2-auth` | `sub` claim (fallback: `username`) | `scope` claim (space→comma) |
| `apikey-auth` | `id` field | `scopes` array |

---

## Context passing

Middlewares can write and read a per-request key-value context. The chain's order defines visibility: a value set by middleware *N* is visible to every downstream middleware and to the dispatcher, and — after dispatch — to every middleware in the on_response chain.

```yaml
x-barbacane-middlewares:
  - name: jwt-auth          # writes context:auth.sub
    config: { issuer: "https://auth.example.com" }
  - name: rate-limit        # reads context:auth.sub
    config:
      quota: 100
      window: 60
      partition_key: "context:auth.sub"
```

The dispatcher may also write context keys (e.g. `ai-proxy` writes `ai.prompt_tokens` after calling the LLM) that flow into the on_response chain — see [AI Gateway](ai-gateway.md) for the full map.

---

## Best practices

### Order matters

Put middlewares in logical order:

```yaml
x-barbacane-middlewares:
  - name: correlation-id       # 1. Add tracing ID first
  - name: http-log             # 2. Log all requests (captures full lifecycle)
  - name: cors                 # 3. Handle CORS early
  - name: ip-restriction       # 4. Block bad IPs immediately
  - name: request-size-limit   # 5. Reject oversized requests
  - name: rate-limit           # 6. Rate limit before auth (cheaper)
  - name: oidc-auth            # 7. Authenticate
  - name: acl                  # 8. Authorize (after auth sets consumer headers)
  - name: request-transformer  # 9. Transform request before dispatch
  - name: response-transformer # 10. Transform response (runs first on the return)
```

### Fail fast

Put restrictive middlewares early to reject bad requests before spending work on them:

```yaml
x-barbacane-middlewares:
  - name: ip-restriction      # Block banned IPs immediately
  - name: request-size-limit  # Reject large payloads early
  - name: rate-limit          # Reject over-limit immediately
  - name: jwt-auth            # Reject unauthenticated before processing
```

### Use global for common concerns

Set shared middlewares once at the root and only add operation-level entries for exceptions:

```yaml
x-barbacane-middlewares:
  - name: correlation-id
  - name: cors
  - name: request-size-limit
    config:
      max_bytes: 10485760  # 10 MiB default
  - name: rate-limit
    config: { quota: 100, window: 60 }

paths:
  /upload:
    post:
      # Override only the size limit for uploads. CORS, correlation-id,
      # rate-limit still apply from global.
      x-barbacane-middlewares:
        - name: request-size-limit
          config:
            max_bytes: 104857600  # 100 MiB
```

Remember: if the operation entry's `name` matches a global entry, the entire matching global group is replaced. If the global has a stack of a given plugin and the operation overrides one of them, move the full stack to the operation level.

---

## Planned middlewares

### idempotency

Ensures idempotent processing via `Idempotency-Key` header. Not yet shipped.

```yaml
x-barbacane-middlewares:
  - name: idempotency
    config:
      header: Idempotency-Key
      ttl: 86400
```

See [ROADMAP.md](../../../ROADMAP.md) for scheduling.
