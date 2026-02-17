# ADR-0020: Request/Response Transformers

**Status:** Proposed
**Date:** 2026-02-11
**Authors:** Nicolas, Nathan

## Context

API gateways commonly need to modify requests before they reach the upstream and responses before they return to the client. Typical use cases:

- **Header injection** — add `X-Request-Id`, `X-Forwarded-For`, or internal routing headers before dispatch
- **Header cleanup** — strip sensitive upstream headers (`Server`, `X-Powered-By`) from responses
- **Query parameter manipulation** — add API version params, remove internal-only params
- **Path rewriting** — map public paths to internal service paths (e.g., `/api/v2/users` → `/users`)
- **Body transformation** — reshape JSON payloads between client and upstream formats

Barbacane's middleware interface (ADR-0006, ADR-0016) already supports request/response modification via `on_request` / `on_response`. What's missing is a **declarative, config-driven transformer plugin** so users don't need to write custom WASM plugins for common transformations.

This is a P0 backlog item, sourced from competitive analysis (Kong, Envoy, Tyk, APISIX all provide transformer plugins).

### Why two plugins, not one

The middleware chain runs in order for requests and in **reverse** for responses (SPEC-002):

```
Request  → [M1] → [M2] → [M3] → [Dispatcher]
Response ← [M3] ← [M2] ← [M1] ← [Response]
```

A single `transformer` plugin handling both phases would lock its request and response transformations to the **same chain position**. Placing it first means request transforms run early (good for path rewriting) but response transforms run last (bad if you need to strip upstream headers before other middlewares see them). Placing it last gives the opposite problem.

With two separate plugins, users control ordering independently:

```yaml
x-barbacane-middlewares:
  - name: request-transformer    # early: rewrite path before auth
    config:
      path: { strip_prefix: /api/v2 }
  - name: jwt-auth
  - name: request-transformer    # late: inject headers using auth context
    config:
      headers:
        add: { X-User-Id: "context:auth.sub" }
  - name: rate-limit
  - name: response-transformer   # in reverse order, runs first on response phase
    config:
      headers:
        remove: [Server, X-Powered-By]
```

This also allows `request-transformer` to appear **multiple times** at different positions in the chain — a common real-world need (path rewrite early, context-based header injection after auth).

### Design decisions summary

1. **Two plugins** — `request-transformer` and `response-transformer` for independent chain positioning (see rationale above)
2. **Variable interpolation** — Supported from v1. Uses `context:<key>` (existing convention) plus `$client_ip`, `$path.<name>`, `$header.<name>`, `$query.<name>`
3. **Body transformations** — Field-level operations using **JSON Pointer** (RFC 6901) addressing. One pointer = one location = natural fit for add/remove/replace. Implementation via `serde_json::pointer_mut()` (already available) + `jsonptr` crate for assign/delete
4. **Path rewriting** — Included in `request-transformer` (it's a request mutation like headers/query)
5. **Ordering** — The existing middleware chain ordering is sufficient. Two separate plugins + multi-instance support gives users full control over positioning

## Decision

### Two Separate Plugins

Barbacane provides two distinct middleware plugins: **`request-transformer`** and **`response-transformer`**. This decouples request-phase and response-phase transformation ordering in the middleware chain (see rationale in Context above).

Both plugins can appear **multiple times** in the same chain with different configs, enabling patterns like early path rewriting + late header injection.

### `request-transformer` Capabilities

#### Headers

```yaml
headers:
  add: {}        # Add or overwrite
  set: {}        # Add only if not already present
  remove: []     # Remove by name
  rename: {}     # old-name → new-name
```

#### Query Parameters

```yaml
querystring:
  add: {}
  remove: []
  rename: {}
```

#### Path Rewriting

```yaml
path:
  # Option A: prefix strip/add
  strip_prefix: /api/v2
  add_prefix: /internal
  # Option B: regex replace
  replace: { pattern: "...", replacement: "..." }
```

#### Body (JSON Pointer — RFC 6901)

Field-level operations on JSON bodies using JSON Pointer addressing. Each pointer targets exactly one location in the document.

```yaml
body:
  add:
    /metadata/gateway: barbacane         # add or overwrite a field
    /metadata/timestamp: "$clock_now"    # interpolation supported
  remove:
    - /internal_flags                    # remove a field
    - /debug
  rename:
    /userName: /user_name                # move value from one pointer to another
```

Implementation uses `serde_json::pointer_mut()` (already available via plugin SDK) for reads and overwrites, plus the `jsonptr` crate (RFC 6901, MIT/Apache-2.0, pure Rust) for assign and delete operations.

Non-JSON bodies are passed through unmodified. If a pointer targets a non-existent path, `add` creates intermediate objects; `remove` is a no-op.

### `response-transformer` Capabilities

#### Headers

```yaml
headers:
  add: {}
  set: {}
  remove: []
  rename: {}
```

#### Body (JSON Pointer — RFC 6901)

Same operations as `request-transformer`:

```yaml
body:
  add:
    /pagination/total: "100"
  remove:
    - /internal_metadata
    - /debug_info
```

### Variable Interpolation

Config values can reference dynamic request data using placeholders. Interpolation is supported in header values, query parameter values, and body `add` values.

| Variable | Source | Example | Resolution |
|----------|--------|---------|------------|
| `$client_ip` | Connection | `$client_ip` | `192.168.1.1` |
| `$path.<name>` | Route parameter | `$path.id` | `123` |
| `$header.<name>` | Request header | `$header.host` | `api.example.com` |
| `$query.<name>` | Query parameter | `$query.page` | `2` |
| `context:<key>` | Middleware context | `context:auth.sub` | `user-123` |

The `context:<key>` syntax is consistent with the existing convention used by `rate-limit` (ADR-0016) and other plugins.

If a variable cannot be resolved (missing header, unset context key), the value is left as an empty string. This is a conscious choice over failing the request — transformations are best-effort enrichment, not validation.

### Spec Integration Examples

#### Global header cleanup

```yaml
x-barbacane-middlewares:
  - name: request-transformer
    config:
      headers:
        add:
          X-Gateway: barbacane
  - name: jwt-auth
    config:
      issuer: https://auth.example.com
  - name: response-transformer
    config:
      headers:
        remove: [Server, X-Powered-By]
```

#### Path rewrite + context-based header injection

```yaml
paths:
  /api/v2/users/{id}:
    get:
      x-barbacane-middlewares:
        - name: request-transformer        # early: rewrite before auth
          config:
            path:
              strip_prefix: /api/v2
        - name: jwt-auth
          config:
            issuer: https://auth.example.com
        - name: request-transformer        # late: inject after auth sets context
          config:
            headers:
              add:
                X-User-Id: "context:auth.sub"
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: http://user-service:3000
```

#### Body enrichment + response cleanup

```yaml
paths:
  /orders:
    post:
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: https://auth.example.com
        - name: request-transformer
          config:
            body:
              add:
                /metadata/tenant_id: "context:tenant_id"
                /metadata/requested_by: "context:auth.sub"
        - name: response-transformer
          config:
            body:
              remove:
                - /internal_flags
                - /debug_trace
            headers:
              remove: [X-Powered-By]
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: http://order-service:8080
```

## Consequences

- **Easier:** Common transformations (header injection, path rewrite, field stripping) are declarative — no custom WASM plugin needed. Two-plugin design gives full control over chain ordering. JSON Pointer (RFC 6901) is a well-known standard with minimal implementation cost. Variable interpolation enables context-aware transformations (auth → header propagation) without coupling plugins to each other.
- **Harder:** Two plugins to manage instead of one (but the ordering benefit justifies this). JSON Pointer doesn't support wildcards or array filters — users needing complex body reshaping should write a custom WASM plugin.
- **Trade-offs:** Empty-string fallback on unresolvable variables is forgiving but may mask config errors. The compiler could optionally warn on variables that reference unused context keys. Body transforms only work on JSON (`application/json`) — non-JSON bodies pass through unmodified.
- **Related:** ADR-0006 (WASM plugins), ADR-0008 (dispatch interface), ADR-0016 (plugin contract), SPEC-002 (request lifecycle / middleware chain ordering)
