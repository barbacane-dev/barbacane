# ADR-0009: Security Model

**Status:** Accepted
**Date:** 2026-01-28

## Context

As an API gateway, Barbacane is a security boundary. Every request entering the system passes through it. "Secure by design" is a founding principle, not a feature bolted on later.

Security concerns span multiple layers:

1. **Client authentication** — verifying who the caller is
2. **Authorization** — deciding what the caller can do
3. **Transport security** — TLS termination
4. **Secrets management** — where credentials, keys, and certs live
5. **Plugin isolation** — already addressed by WASM sandboxing (ADR-0006)

## Decision

### Authentication: Plugin-Based

Authentication is **not** built into the gateway core. It is implemented as middleware plugins (ADR-0006), consistent with the overall architecture.

Barbacane ships official auth plugins as reference implementations:

| Plugin | Mechanism | Use Case |
|--------|-----------|----------|
| `barbacane-auth-jwt` | JWT validation (RS256, ES256) | Stateless token auth |
| `barbacane-auth-apikey` | API key lookup | Simple service-to-service |
| `barbacane-auth-oauth2` | OAuth2 token introspection | Delegated auth to IdP |

These are WASM plugins like any other — they can be replaced, forked, or extended. Keeping auth out of the core means:

- Auth logic is auditable independently
- New auth mechanisms don't require gateway upgrades
- Users are not locked into Barbacane's auth opinions

Declared in specs via standard OpenAPI `securitySchemes` + middleware mapping:

```yaml
components:
  securitySchemes:
    BearerAuth:
      type: http
      scheme: bearer
      bearerFormat: JWT

x-barbacane-middlewares:
  - name: barbacane-auth-jwt
    config:
      issuer: https://auth.example.com
      audiences: [api.example.com]
      jwks_uri: https://auth.example.com/.well-known/jwks.json

paths:
  /users/{id}:
    get:
      security:
        - BearerAuth: []
```

### Authorization: Spec + OPA

Two layers of authorization:

#### Layer 1: Spec-level (coarse-grained)

OpenAPI `security` declarations define which security schemes apply to which routes. The gateway enforces these at the spec level — a route with `security: [BearerAuth]` rejects unauthenticated requests before any further processing.

#### Layer 2: OPA (fine-grained)

For attribute-based access control (ABAC), role-based rules, or cross-cutting policies, Barbacane integrates with **Open Policy Agent** via a dedicated middleware plugin:

```yaml
x-barbacane-middlewares:
  - name: barbacane-auth-jwt
    config:
      issuer: https://auth.example.com
  - name: barbacane-authz-opa
    config:
      policy: policies/api-access.rego
      input_mapping:
        user: context:auth.sub
        roles: context:auth.roles
        path: request:path
        method: request:method
```

OPA policies are compiled alongside specs in the CI/CD pipeline. They are bundled into the data plane artifact as WASM modules (OPA supports WASM compilation of Rego policies).

### Transport Security (TLS)

#### Client-facing (ingress)

| Concern | Approach |
|---------|----------|
| TLS termination | `rustls` — no OpenSSL dependency, memory-safe TLS |
| TLS versions | TLS 1.2 minimum, TLS 1.3 preferred |
| Cipher suites | Modern suites only, no legacy (no CBC, no RC4) |
| Certificate source | Loaded at startup from vault or filesystem |

Using `rustls` over OpenSSL is a deliberate choice: OpenSSL has a long history of CVEs (Heartbleed, etc.) rooted in C memory safety issues. A Rust-native TLS stack aligns with "secure by design."

#### Upstream (egress)

With data planes deployed at the edge, traffic to upstream services may cross the public internet. Barbacane enforces **TLS mandatory, mTLS optional** for all upstream connections:

| Policy | Behavior |
|--------|----------|
| **TLS mandatory** (default) | All upstream connections use TLS. Plain HTTP to upstreams is rejected. |
| **mTLS optional** | Mutual TLS is configurable per-dispatch for zero-trust environments. |
| **Development mode** | Plain HTTP allowed only when explicitly enabled via `--allow-plaintext-upstream` flag. This flag is refused in production builds. |

Configured per-dispatch in the spec:

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: https://user-service.internal:443
    timeout: 5s
    tls:
      # mTLS: present client certificate to upstream
      client_cert: vault://certs/edge-to-cloud
      client_key: vault://certs/edge-to-cloud-key
      # Optionally pin the upstream CA
      ca: vault://certs/internal-ca
```

When no `tls` block is specified, standard TLS is used with system CA roots. The key point: **there is no way to configure a plain `http://` upstream URL in production** — the compiler rejects it.

### Secrets Management

**No secrets in artifacts. No secrets in specs. No secrets in Git.**

Secrets are fetched at data plane startup from an external vault:

```
Data plane starts → Connects to vault → Fetches secrets → Initializes plugins → Ready
```

| Secret type | Storage | Fetched when |
|-------------|---------|-------------|
| Ingress TLS certificates | Vault / filesystem mount | Startup |
| Upstream mTLS client certs | Vault, referenced in dispatch config | Startup |
| Upstream CA pins | Vault, referenced in dispatch config | Startup |
| JWT signing keys (JWKS) | Fetched from IdP at runtime | Startup + periodic refresh |
| API keys database | Vault or external store | Startup + periodic refresh |
| Plugin configs with credentials | Vault, referenced by ID | Startup |

Supported vault backends:

| Backend | Priority |
|---------|----------|
| HashiCorp Vault | Primary |
| AWS Secrets Manager | Secondary |
| Kubernetes Secrets | Secondary (mounted as files) |
| Environment variables | Development only |

Specs reference secrets by ID, never by value:

```yaml
x-barbacane-middlewares:
  - name: barbacane-auth-jwt
    config:
      jwks_uri: https://auth.example.com/.well-known/jwks.json
      # No keys stored here — fetched from jwks_uri at runtime
  - name: barbacane-auth-apikey
    config:
      store: vault://secrets/api-keys  # reference, not value
```

### Security Defaults

Barbacane applies secure defaults that must be explicitly overridden:

| Default | Rationale |
|---------|-----------|
| Strict spec validation (ADR-0004) | Reject anything not in spec |
| TLS mandatory to upstreams | No plaintext over untrusted networks |
| No CORS | Must be explicitly enabled via middleware |
| No wildcard routes | Every route must be declared in the spec |
| Request size limits | Configurable, enforced before parsing |
| Header count limits | Prevent header-based DoS |
| Timeout enforcement | No request runs forever |

## Consequences

- **Easier:** Consistent plugin model (auth is not special), OPA provides powerful policy language, rustls eliminates a class of TLS vulnerabilities, vault integration prevents secret leakage
- **Harder:** Auth plugins must be carefully audited (security-critical WASM), vault dependency adds operational complexity, OPA policies add a learning curve
- **Tradeoff:** Auth as plugin means no "zero-config" auth — users must explicitly configure it. This is intentional: implicit security creates false confidence
