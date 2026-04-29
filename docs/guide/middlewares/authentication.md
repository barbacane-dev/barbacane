# Authentication Middlewares

All authentication middlewares set the standard [consumer identity headers](index.md#consumer-identity-headers) — `x-auth-consumer` and `x-auth-consumer-groups` — so downstream authorization plugins (notably [`acl`](authorization.md#acl)) don't need to know which auth plugin produced them.

- [`jwt-auth`](#jwt-auth) — JWT Bearer tokens with RS256/HS256 signatures
- [`apikey-auth`](#apikey-auth) — API keys from header or query parameter
- [`oauth2-auth`](#oauth2-auth) — Bearer tokens via RFC 7662 token introspection
- [`oidc-auth`](#oidc-auth) — OpenID Connect discovery + JWKS
- [`basic-auth`](#basic-auth) — HTTP Basic per RFC 7617

---

## jwt-auth

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

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `issuer` | string | - | Expected `iss` claim. Tokens not matching are rejected |
| `audience` | string | - | Expected `aud` claim. Tokens not matching are rejected |
| `clock_skew_seconds` | integer | `60` | Tolerance in seconds for `exp`/`nbf` validation |
| `groups_claim` | string | - | Claim name to extract consumer groups from (e.g., `"roles"`, `"groups"`). Value is set as `x-auth-consumer-groups` |
| `skip_signature_validation` | boolean | `false` | Skip cryptographic signature check. Required until JWKS support is implemented |

### Context headers

Sets headers for downstream:
- `x-auth-consumer` — Consumer identifier (from `sub` claim)
- `x-auth-consumer-groups` — Comma-separated groups (from `groups_claim`, if configured)
- `x-auth-sub` — Subject (user ID)
- `x-auth-claims` — Full JWT claims as JSON

---

## apikey-auth

Validates API keys from header or query parameter.

```yaml
x-barbacane-middlewares:
  - name: apikey-auth
    config:
      key_location: header        # or "query"
      header_name: X-API-Key      # when key_location is "header"
      query_param: api_key        # when key_location is "query"
      keys:
        - key: "env://API_KEY_PRODUCTION"
          id: key-001
          name: Production Key
          scopes: ["read", "write"]
        - key: sk_test_xyz789
          id: key-002
          name: Test Key
          scopes: ["read"]
```

The `key` field supports secret references (`env://`, `file://`) which are resolved at gateway startup. See [Secrets](../secrets.md) for details.

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `key_location` | string | `header` | Where to find key (`header` or `query`) |
| `header_name` | string | `X-API-Key` | Header name (when `key_location: header`) |
| `query_param` | string | `api_key` | Query param name (when `key_location: query`) |
| `keys` | array | `[]` | List of API key entries with metadata |

### Context headers

Sets headers for downstream:
- `x-auth-consumer` — Consumer identifier (from key `id`)
- `x-auth-consumer-groups` — Comma-separated groups (from key `scopes`)
- `x-auth-key-id` — Key identifier
- `x-auth-key-name` — Key human-readable name
- `x-auth-key-scopes` — Comma-separated scopes

---

## oauth2-auth

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

The `client_secret` uses a secret reference (`env://`) which is resolved at gateway startup. See [Secrets](../secrets.md) for details.

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `introspection_endpoint` | string | **required** | RFC 7662 introspection URL |
| `client_id` | string | **required** | Client ID for introspection auth |
| `client_secret` | string | **required** | Client secret for introspection auth |
| `required_scopes` | string | - | Space-separated required scopes |
| `timeout` | float | `5.0` | Introspection request timeout (seconds) |

### Context headers

Sets headers for downstream:
- `x-auth-consumer` — Consumer identifier (from `sub`, fallback to `username`)
- `x-auth-consumer-groups` — Comma-separated groups (from `scope`)
- `x-auth-sub` — Subject
- `x-auth-scope` — Token scopes
- `x-auth-client-id` — Client ID
- `x-auth-username` — Username (if present)
- `x-auth-claims` — Full introspection response as JSON

### Error responses

- `401 Unauthorized` — Missing token, invalid token, or inactive token
- `403 Forbidden` — Token lacks required scopes

Includes RFC 6750 `WWW-Authenticate` header with error details.

---

## oidc-auth

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
      allow_query_token: false  # RFC 6750 §2.3 query param fallback
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `issuer_url` | string | **required** | OIDC issuer URL (e.g., `https://accounts.google.com`) |
| `audience` | string | - | Expected `aud` claim. If set, tokens must match |
| `required_scopes` | string | - | Space-separated required scopes |
| `issuer_override` | string | - | Override expected `iss` claim (for split-network setups like Docker) |
| `clock_skew_seconds` | integer | `60` | Clock skew tolerance for `exp`/`nbf` validation |
| `jwks_refresh_seconds` | integer | `300` | How often to refresh JWKS keys (seconds) |
| `timeout` | float | `5.0` | HTTP timeout for discovery and JWKS calls (seconds) |
| `allow_query_token` | boolean | `false` | Allow token extraction from the `access_token` query parameter ([RFC 6750 §2.3](https://datatracker.ietf.org/doc/html/rfc6750#section-2.3)). Use with caution — tokens in URLs risk leaking via logs and referer headers. |

### How it works

1. Extracts the Bearer token from the `Authorization` header (or from the `access_token` query parameter if `allow_query_token` is enabled and no header is present)
2. Parses the JWT header to determine the signing algorithm and key ID (`kid`)
3. Fetches `{issuer_url}/.well-known/openid-configuration` (cached)
4. Fetches the JWKS endpoint from the discovery document (cached with TTL)
5. Finds the matching public key by `kid` (or `kty`/`use` fallback)
6. Verifies the signature using `host_verify_signature` (RS256/RS384/RS512, ES256/ES384)
7. Validates claims: `iss`, `aud`, `exp`, `nbf`
8. Checks required scopes (if configured)

### Context headers

Sets headers for downstream:
- `x-auth-consumer` — Consumer identifier (from `sub` claim)
- `x-auth-consumer-groups` — Comma-separated groups (from `scope`, space→comma)
- `x-auth-sub` — Subject (user ID)
- `x-auth-scope` — Token scopes
- `x-auth-claims` — Full JWT payload as JSON

### Error responses

- `401 Unauthorized` — Missing token, invalid token, expired token, bad signature, unknown issuer
- `403 Forbidden` — Token lacks required scopes

Includes RFC 6750 `WWW-Authenticate` header with error details.

---

## basic-auth

Validates credentials from the `Authorization: Basic` header per RFC 7617. Useful for internal APIs, admin endpoints, or simple services that don't need a full identity provider.

```yaml
x-barbacane-middlewares:
  - name: basic-auth
    config:
      realm: "My API"
      strip_credentials: true
      credentials:
        - username: admin
          password: "env://ADMIN_PASSWORD"
          roles: ["admin", "editor"]
        - username: readonly
          password: "env://READONLY_PASSWORD"
          roles: ["viewer"]
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `realm` | string | `api` | Authentication realm shown in `WWW-Authenticate` challenge |
| `strip_credentials` | boolean | `true` | Remove `Authorization` header before forwarding to upstream |
| `credentials` | array | `[]` | List of credential entries |

Each credential entry:

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `username` | string | **required** | Username for this credential |
| `password` | string | **required** | Password for this user (supports secret references) |
| `roles` | array | `[]` | Optional roles for authorization |

### Context headers

Sets headers for downstream:
- `x-auth-consumer` — Consumer identifier (username)
- `x-auth-consumer-groups` — Comma-separated groups (from `roles`)
- `x-auth-user` — Authenticated username
- `x-auth-roles` — Comma-separated roles (only set if the user has roles)

### Error responses

Returns `401 Unauthorized` with `WWW-Authenticate: Basic realm="<realm>"` and Problem JSON:

```json
{
  "type": "urn:barbacane:error:authentication-failed",
  "title": "Authentication failed",
  "status": 401,
  "detail": "Invalid username or password"
}
```
