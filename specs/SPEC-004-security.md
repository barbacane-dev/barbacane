# SPEC-004: Security

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0009

---

## 1. Overview

Barbacane is a security boundary. This spec defines TLS configuration, authentication and authorization plugin conventions, secrets management, and the security defaults enforced by the gateway.

---

## 2. TLS — Ingress (Client-Facing)

### 2.1 Configuration

TLS is configured via CLI flags at data plane startup:

```
--tls-cert <PATH>     PEM certificate file or vault:// reference
--tls-key <PATH>      PEM private key file or vault:// reference
```

If neither flag is provided, the data plane listens on plain HTTP. This is acceptable in development. In production builds, the binary logs a warning on startup if TLS is not configured.

### 2.2 TLS settings

| Setting | Value |
|---------|-------|
| Implementation | `rustls` (no OpenSSL) |
| Minimum version | TLS 1.2 |
| Preferred version | TLS 1.3 |
| TLS 1.3 cipher suites | `TLS_AES_256_GCM_SHA384`, `TLS_AES_128_GCM_SHA256`, `TLS_CHACHA20_POLY1305_SHA256` |
| TLS 1.2 cipher suites | `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384`, `TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384`, `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256`, `TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256` |
| ALPN | `h2`, `http/1.1` |
| Session resumption | Enabled (TLS 1.3 tickets) |
| OCSP stapling | Supported if the cert includes OCSP responder info |

No legacy cipher suites (CBC, RC4, 3DES). No RSA key exchange (forward secrecy required).

### 2.3 Certificate reload

The data plane does not hot-reload certificates. Certificate rotation requires a restart. In containerized environments, this is handled by rolling restarts.

---

## 3. TLS — Egress (Upstream)

### 3.1 Default: TLS mandatory

All upstream connections from dispatchers use TLS. The compiler rejects `http://` upstream URLs in production mode (SPEC-001 `E1031`).

### 3.2 Per-dispatch TLS config

```yaml
x-barbacane-dispatch:
  name: http-upstream
  config:
    url: https://user-service.internal:443
    tls:
      client_cert: vault://certs/edge-to-cloud
      client_key: vault://certs/edge-to-cloud-key
      ca: vault://certs/internal-ca
```

| Field | Type | Description |
|-------|------|-------------|
| `tls.client_cert` | string | Client certificate for mTLS (vault reference or file path) |
| `tls.client_key` | string | Client private key for mTLS |
| `tls.ca` | string | Custom CA to trust for this upstream (pinning) |

When no `tls` block is specified, standard TLS with system CA roots is used.

### 3.3 Development override

```
--allow-plaintext-upstream
```

Allows `http://` upstream URLs. This flag is **refused** by production builds (the binary exits with an error if the flag is present and the build is a release build).

---

## 4. Authentication

Authentication is implemented as middleware plugins (SPEC-003), not as a core gateway feature.

### 4.1 Mapping OpenAPI `securitySchemes` to middlewares

The spec author declares security schemes using standard OpenAPI and maps them to auth middleware plugins:

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

security:
  - BearerAuth: []
```

The compiler validates that every `securitySchemes` entry referenced in a `security` block has a corresponding auth middleware in the chain (SPEC-001 `E1032`).

### 4.2 Auth middleware output convention

Auth middlewares that successfully authenticate a request add headers for downstream use:

| Header | Description | Set by |
|--------|-------------|--------|
| `x-auth-sub` | Subject identifier (user ID) | jwt-auth, oauth2-auth |
| `x-auth-scope` | Space-separated scopes | oauth2-auth |
| `x-auth-claims` | Full token claims as JSON | jwt-auth, oauth2-auth |
| `x-auth-client-id` | OAuth2 client ID | oauth2-auth |
| `x-auth-username` | Username (if present) | oauth2-auth |
| `x-auth-key-id` | API key identifier | apikey-auth |
| `x-auth-key-name` | API key human-readable name | apikey-auth |
| `x-auth-key-scopes` | Comma-separated key scopes | apikey-auth |

Headers are removed from upstream requests to prevent spoofing.

### 4.3 Auth rejection behavior

When an auth middleware rejects a request:

| Situation | Status | `type` URN | Response header |
|-----------|--------|------------|-----------------|
| No credentials provided | `401` | `urn:barbacane:error:authentication-failed` | `WWW-Authenticate: Bearer realm="api"` |
| Invalid/expired token | `401` | `urn:barbacane:error:authentication-failed` | `WWW-Authenticate: Bearer error="invalid_token"` |
| Token valid but insufficient scope | `403` | `urn:barbacane:error:authorization-failed` | `WWW-Authenticate: Bearer error="insufficient_scope"` |

Response body follows RFC 7807 Problem Details format with `type`, `title`, `status`, and `detail` fields.

### 4.4 Built-in auth plugins

| Plugin | Config fields |
|--------|--------------|
| `jwt-auth` | `secret` (string, HS256), `public_key` (string, RS256 PEM), `issuer` (string), `audience` (string), `required_claims` (string[]), `leeway` (int, seconds) |
| `apikey-auth` | `key_location` (`header`/`query`), `header_name` (default `X-API-Key`), `query_param` (default `api_key`), `keys` (map of key→metadata) |
| `oauth2-auth` | `introspection_endpoint` (string, required), `client_id` (string, required), `client_secret` (string, required), `required_scopes` (string, space-separated), `timeout` (float, seconds) |

---

## 5. Authorization

### 5.1 Spec-level (coarse-grained)

OpenAPI `security` declarations on operations are enforced by the gateway. If an operation has `security: [BearerAuth: []]`, the request must pass through the JWT auth middleware before reaching dispatch. This is enforced structurally — the auth middleware is in the chain.

### 5.2 OPA-based (fine-grained)

The `barbacane-authz-opa` middleware evaluates Open Policy Agent policies.

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

**`input_mapping`** defines how request and context data is mapped to the OPA policy input:

| Prefix | Source |
|--------|--------|
| `context:` | Per-request context map (e.g. `context:auth.sub`) |
| `request:` | Request field (supported: `path`, `method`, `query`, `client_ip`) |
| `header:` | Request header value (e.g. `header:x-custom`) |

**OPA policy format:**

```rego
package barbacane.authz

default allow = false

allow {
    input.roles[_] == "admin"
}

allow {
    input.method == "GET"
    input.roles[_] == "reader"
}
```

The policy must define `barbacane.authz.allow` as a boolean. If `allow` is `false`, the middleware returns `403`.

**Policy compilation:** OPA `.rego` files are compiled to WASM by the control plane during artifact compilation and bundled in the `policies/` directory of the `.bca` artifact.

---

## 6. Secrets Management

### 6.1 Principle

No secrets in specs. No secrets in artifacts. No secrets in Git. Specs reference secrets by identifier; values are resolved at data plane startup.

### 6.2 Reference format

Secrets are referenced as URI strings:

| Scheme | Example | Backend |
|--------|---------|---------|
| `vault://` | `vault://secret/data/api-keys` | HashiCorp Vault |
| `aws-sm://` | `aws-sm://prod/api-key` | AWS Secrets Manager |
| `k8s://` | `k8s://namespace/secret-name/key` | Kubernetes Secrets (file mount) |
| `env://` | `env://API_KEY` | Environment variable (development only) |

### 6.3 Resolution sequence

At startup:

1. Parse all secret references from the loaded artifact config
2. Connect to the vault backend(s)
3. Fetch all secrets
4. Pass resolved values to plugins during `init`

If any secret cannot be resolved, the data plane refuses to start (exit code `13`).

### 6.4 Secret refresh

| Secret type | Refresh behavior |
|-------------|-----------------|
| TLS certificates | No refresh (requires restart) |
| JWKS (signing keys) | Periodic refresh (configurable interval, default 5 minutes) |
| API key store | Periodic refresh (configurable interval, default 1 minute) |
| Static secrets (passwords, tokens) | No refresh (requires restart) |

Refresh failures are logged and the previous value is retained. The gateway does not stop serving traffic on refresh failure.

### 6.5 Vault authentication

| Backend | Auth method |
|---------|-------------|
| HashiCorp Vault | Token (`--vault-token`), Kubernetes auth, AppRole |
| AWS Secrets Manager | IAM role (instance profile or ECS task role) |
| Kubernetes Secrets | Service account (automatic when running in-cluster) |

---

## 7. Security Defaults

These defaults are enforced unless explicitly overridden:

| Default | Behavior | Override |
|---------|----------|---------|
| Strict spec validation | Reject anything not declared in spec | Cannot be disabled |
| TLS mandatory to upstreams | `http://` upstreams rejected | `--allow-plaintext-upstream` (dev only) |
| No CORS | Cross-origin requests receive no CORS headers | Add CORS middleware plugin |
| No wildcard routes | Every route must be in the spec | Cannot be disabled |
| Request body size limit | 1 MB | `requestBody.x-barbacane-max-size` |
| Header count limit | 100 | `x-barbacane-limits.max_headers` |
| Header size limit | 8 KB per header | `x-barbacane-limits.max_header_size` |
| URI length limit | 8 KB | `x-barbacane-limits.max_uri_length` |
| Request timeout | 30 seconds | `x-barbacane-dispatch.config.timeout` |
| No `Server` header leaking | Upstream `Server` header replaced with `barbacane/<version>` | Cannot be disabled |
| Error detail suppression | Production errors contain no internal details | `--dev` flag |

---

## 8. WASM Plugin Security

### 8.1 Sandbox guarantees

WASM plugins execute in `wasmtime` with:

- No filesystem access
- No network access (except via granted host functions)
- No clock access (except via `clock_now` host function)
- No access to other plugins' memory
- Execution time limits (100ms per call)
- Memory limits (16 MB per instance)

### 8.2 Capability enforcement

A plugin's capabilities are declared in `plugin.toml` and verified at two stages:

1. **Registration time:** The control plane checks that the `.wasm` binary's imports match the declared capabilities
2. **Compile time:** The compiler checks that the plugin's capabilities are consistent with its type (e.g., a middleware should not need `kafka_publish`)

### 8.3 Plugin provenance

The artifact manifest records the SHA-256 hash of every plugin binary. At startup, the data plane verifies hashes before loading any WASM module. A hash mismatch causes startup failure (exit code `11`).
