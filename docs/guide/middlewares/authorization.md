# Authorization Middlewares

- [`acl`](#acl) — consumer/group-based allow-deny lists
- [`opa-authz`](#opa-authz) — policy-as-code via an external Open Policy Agent server
- [`cel`](#cel) — inline CEL expressions; also the engine behind policy-driven routing ([see below](#policy-driven-routing-cel-stacking))

---

## acl

Enforces access control based on consumer identity and group membership. Reads the standard `x-auth-consumer` and `x-auth-consumer-groups` headers set by upstream auth plugins.

```yaml
x-barbacane-middlewares:
  - name: basic-auth
    config:
      realm: "my-api"
      credentials:
        - username: admin
          password: "env://ADMIN_PASSWORD"
          roles: ["admin", "editor"]
        - username: viewer
          password: "env://VIEWER_PASSWORD"
          roles: ["viewer"]
  - name: acl
    config:
      allow:
        - admin
      deny:
        - banned
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `allow` | array | `[]` | Group names allowed access. If non-empty, consumer must belong to at least one |
| `deny` | array | `[]` | Group names denied access (takes precedence over `allow`) |
| `allow_consumers` | array | `[]` | Specific consumer IDs allowed (bypasses group checks) |
| `deny_consumers` | array | `[]` | Specific consumer IDs denied (highest precedence) |
| `consumer_groups` | object | `{}` | Static consumer-to-groups mapping, merged with `x-auth-consumer-groups` header |
| `message` | string | `Access denied by ACL policy` | Custom 403 error message |
| `hide_consumer_in_errors` | boolean | `false` | Suppress consumer identity in 403 error body |

### Evaluation order

1. Missing/empty `x-auth-consumer` header → **403**
2. `deny_consumers` match → **403**
3. `allow_consumers` match → **200** (bypasses group checks)
4. Resolve groups (merge `x-auth-consumer-groups` header + static `consumer_groups` config)
5. `deny` group match → **403** (takes precedence over allow)
6. `allow` non-empty + group match → **200**
7. `allow` non-empty + no group match → **403**
8. `allow` empty → **200** (only deny rules active)

### Static consumer groups

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

### Error response

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

---

## opa-authz

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `opa_url` | string | *(required)* | OPA Data API endpoint URL (e.g., `http://opa:8181/v1/data/authz/allow`) |
| `timeout` | number | `5` | HTTP request timeout in seconds for OPA calls |
| `include_body` | boolean | `false` | Include the request body in the OPA input payload |
| `include_claims` | boolean | `true` | Include parsed `x-auth-claims` header (set by upstream auth plugins) in the OPA input |
| `deny_message` | string | `Authorization denied by policy` | Custom message returned in the 403 response body |

### OPA input payload

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

### Decision logic

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

### Error responses

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

### Example OPA policy

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

---

## cel

Inline policy evaluation using [CEL (Common Expression Language)](https://cel.dev/). Evaluates expressions directly in-process — no external service needed. CEL is the same language used by Envoy, Kubernetes, and Firebase for policy rules.

Two modes:

- **Access-control mode** (default, no `on_match`): `true` → continue, `false` → **403**.
- **`on_match` mode** (`on_match` present): on `true`, take the configured actions — write context keys (`set_context`) and/or reject with a configured status + code (`deny`). On `false`, continue unchanged (no 403). Used to drive [policy-driven routing](#policy-driven-routing-cel-stacking) and per-tier resource gating (e.g. block `gpt-4*` for the free tier without standing up a separate target — see [example](#match-and-deny-per-tier-model-gating)).

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `expression` | string | *(required)* | CEL expression that must evaluate to a boolean |
| `deny_message` | string | `Access denied by policy` | Custom message returned in the 403 response (access-control mode only; ignored when `on_match` is set) |
| `on_match` | object | - | Enables match-and-act mode. Holds `set_context` (write context keys), `deny` (reject with a configured 4xx + code), or both. When both are present, `deny` wins — a denied request is not also written to context |

#### `on_match.set_context`

| Property | Type | Description |
|----------|------|-------------|
| *(map)* | `string → string` | Key-value pairs written into the request context when the expression is true. The `ai-proxy` dispatcher reads `ai.target`; the [AI gateway middlewares](ai-gateway.md) read `ai.policy` |

#### `on_match.deny`

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `status` | integer | `403` | HTTP status code. Must be 4xx — a `cel` denial returning 5xx would mask a policy decision as a server fault. Out-of-range values fall back to `403` |
| `code` | string | *(required)* | Machine-readable error code, `snake_case`. Becomes the URN suffix on the `type` field and the `code` field on the response body — the convention `ai-proxy` uses for `model_not_permitted` and similar |
| `message` | string | falls back to `code` | Human-readable `detail` message returned in the problem+json body |

### Request context

The expression has access to a `request` object with these fields:

| Variable | Type | Description |
|----------|------|-------------|
| `request.method` | string | HTTP method (`GET`, `POST`, etc.) |
| `request.path` | string | Request path (e.g., `/api/users`) |
| `request.query` | string | Query string (empty string if none) |
| `request.headers` | map | Request headers (e.g., `request.headers.authorization`) |
| `request.body` | string | Request body (empty string if none) |
| `request.body_json` | map | Request body parsed as JSON when `Content-Type` is `application/json` or `application/*+json`; empty map otherwise. A malformed body is logged and yields an empty map (a CEL plugin that 500'd on every malformed body would let an attacker take down every downstream policy with one bad byte). Use `has(...)` to test field presence safely |
| `request.client_ip` | string | Client IP address |
| `request.path_params` | map | Path parameters (e.g., `request.path_params.id`) |
| `request.consumer` | string | Consumer identity from `x-auth-consumer` header (empty if absent) |
| `request.claims` | map | Parsed JSON from `x-auth-claims` header (empty map if absent/invalid) |

### CEL features

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

### Decision logic

| Expression result | Access-control mode | `on_match` mode |
|------------------|-----|-----|
| `true` | Continue | Run configured actions: `set_context` writes keys, `deny` short-circuits with the configured 4xx + code; if both are set, `deny` wins |
| `false` | **403** Forbidden | Continue unchanged |
| Non-boolean | **500** Internal Server Error | **500** |
| Parse/evaluation error | **500** | **500** |

### Error responses

**403 Forbidden** — access-control mode, expression evaluates to `false`:

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

**Configured 4xx** — `on_match.deny` fires:

```json
{
  "type": "urn:barbacane:error:model_not_permitted",
  "title": "Forbidden",
  "status": 403,
  "code": "model_not_permitted",
  "detail": "gpt-4* is restricted to the premium tier"
}
```

The URN type, response `code`, and (optionally) `title` are derived from `on_match.deny.code` + `status`; `detail` is the configured `message` or, if absent, the `code` itself.

### Match-and-deny: per-tier model gating

The `on_match.deny` action turns `cel` into a fully programmable gate — useful when access depends on a combination of caller attributes *and* request body that the dispatcher's static `allow`/`deny` lists can't express. The canonical case is "block expensive models for the free tier":

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: "https://auth.example.com"
  - name: cel
    config:
      expression: >
        request.body_json.model.startsWith('gpt-4')
        && request.claims.tier != 'premium'
      on_match:
        deny:
          status: 403
          code: model_not_permitted_for_tier
          message: "gpt-4* is restricted to the premium tier"
```

The expression sees both the parsed JSON body and the JWT claims, so a single rule covers what would otherwise need two layers (a custom auth filter plus a separate routing config). Pair with [`ai-proxy`'s `routes`](../dispatchers.md#ai-proxy) for static catalog policy and reach for `cel` only when the rule depends on caller context.

### Policy-driven routing (cel stacking)

CEL in routing mode is the building block for declarative policy routing. **Stack one entry per rule** — each writes a distinct set of context keys. Downstream plugins (notably [`ai-proxy`](../dispatchers.md#ai-proxy) via `ai.target`, and all [AI Gateway](ai-gateway.md) middlewares via `ai.policy`) read the written keys to pick their active behavior.

```yaml
x-barbacane-middlewares:
  - name: cel
    config:
      expression: "request.claims.tier == 'premium'"
      on_match:
        set_context:
          ai.policy: premium
          ai.target: premium

  - name: cel
    config:
      expression: "'ai:premium' in request.claims.scopes"
      on_match:
        set_context:
          ai.policy: premium
          ai.target: premium

  - name: cel
    config:
      expression: "request.headers['x-ai-model-tier'] == 'best'"
      on_match:
        set_context:
          ai.policy: premium
          ai.target: premium
```

Each entry is evaluated in order. On a `true` match, the context keys are written (the last match wins when keys collide); on `false`, the entry is a no-op. No request is ever denied by a routing-mode cel — it's pure data-plane policy, not access control.

See [ADR-0024 §Policy-Driven Model Routing](../../../adr/0024-ai-gateway-plugin.md) for the full design.

### cel vs OPA

| | `cel` | `opa-authz` |
|---|---|---|
| Deployment | Embedded (no sidecar) | External OPA server |
| Language | CEL | Rego |
| Latency | Microseconds (in-process) | HTTP round-trip |
| Best for | Inline route-level rules, policy routing | Complex policy repos, audit trails |
