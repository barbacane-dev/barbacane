# Transformation Middlewares

Modify requests before dispatch, modify responses before return, or short-circuit to a different URL entirely.

- [`request-transformer`](#request-transformer) — declarative request-side edits
- [`response-transformer`](#response-transformer) — declarative response-side edits
- [`redirect`](#redirect) — rule-driven 3xx redirects

---

## request-transformer

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

### Configuration

#### headers

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite headers. Supports variable interpolation |
| `set` | object | `{}` | Add headers only if not already present. Supports variable interpolation |
| `remove` | array | `[]` | Remove headers by name (case-insensitive) |
| `rename` | object | `{}` | Rename headers (old-name to new-name) |

#### querystring

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite query parameters. Supports variable interpolation |
| `remove` | array | `[]` | Remove query parameters by name |
| `rename` | object | `{}` | Rename query parameters (old-name to new-name) |

#### path

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `strip_prefix` | string | - | Remove prefix from path (e.g., `/api/v2`) |
| `add_prefix` | string | - | Add prefix to path (e.g., `/internal`) |
| `replace.pattern` | string | - | Regex pattern to match in path |
| `replace.replacement` | string | - | Replacement string (supports regex capture groups) |

Path operations are applied in order: strip prefix, add prefix, regex replace.

#### body

JSON body transformations use [JSON Pointer (RFC 6901)](https://tools.ietf.org/html/rfc6901) paths.

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite JSON fields. Supports variable interpolation |
| `remove` | array | `[]` | Remove JSON fields by JSON Pointer path |
| `rename` | object | `{}` | Rename JSON fields (old-pointer to new-pointer) |

Body transformations only apply to requests with `application/json` content type. Non-JSON bodies pass through unchanged.

### Variable interpolation

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

### Transformation order

Transformations are applied in this order:

1. **Path** — strip prefix, add prefix, regex replace
2. **Headers** — add, set, remove, rename
3. **Query parameters** — add, remove, rename
4. **Body** — add, remove, rename

### Use cases

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
x-barbacane-middlewares:
  - name: request-transformer
    config:
      headers:
        add:
          X-Gateway: "barbacane"
          X-Client-IP: "$client_ip"
```

---

## response-transformer

Declaratively modifies responses before they return to the client. Supports status code mapping, header transformations, and JSON body transformations.

```yaml
x-barbacane-middlewares:
  - name: response-transformer
    config:
      status:
        200: 201
        400: 403
        500: 503
      headers:
        add:
          X-Gateway: "barbacane"
          X-Frame-Options: "DENY"
        set:
          X-Content-Type-Options: "nosniff"
        remove:
          - Server
          - X-Powered-By
        rename:
          X-Old-Name: X-New-Name
      body:
        add:
          /metadata/gateway: "barbacane"
        remove:
          - /internal_flags
          - /debug_info
        rename:
          /userName: /user_name
```

### Configuration

#### status

A mapping of upstream status codes to replacement status codes. Unmapped codes pass through unchanged.

```yaml
status:
  200: 201    # Created instead of OK
  400: 422    # Unprocessable Entity instead of Bad Request
  500: 503    # Service Unavailable instead of Internal Server Error
```

#### headers

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite response headers |
| `set` | object | `{}` | Add headers only if not already present in the response |
| `remove` | array | `[]` | Remove headers by name (case-insensitive) |
| `rename` | object | `{}` | Rename headers (old-name to new-name) |

#### body

JSON body transformations use [JSON Pointer (RFC 6901)](https://tools.ietf.org/html/rfc6901) paths.

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `add` | object | `{}` | Add or overwrite JSON fields |
| `remove` | array | `[]` | Remove JSON fields by JSON Pointer path |
| `rename` | object | `{}` | Rename JSON fields (old-pointer to new-pointer) |

Body transformations only apply to responses with JSON bodies. Non-JSON bodies pass through unchanged.

### Transformation order

Transformations are applied in this order:

1. **Status** — map status code
2. **Headers** — remove, rename, set, add
3. **Body** — remove, rename, add

### Use cases

**Strip upstream server headers:**
```yaml
- name: response-transformer
  config:
    headers:
      remove: [Server, X-Powered-By, X-AspNet-Version]
```

**Add security headers to all responses:**
```yaml
- name: response-transformer
  config:
    headers:
      add:
        X-Frame-Options: "DENY"
        X-Content-Type-Options: "nosniff"
        Strict-Transport-Security: "max-age=31536000"
```

**Clean up internal fields from response body:**
```yaml
- name: response-transformer
  config:
    body:
      remove:
        - /internal_metadata
        - /debug_trace
        - /password_hash
```

**Map status codes for API versioning:**
```yaml
- name: response-transformer
  config:
    status:
      200: 201
```

---

## redirect

Redirects requests based on configurable path rules. Supports exact path matching, prefix matching with path rewriting, configurable status codes (301/302/307/308), and query string preservation.

```yaml
x-barbacane-middlewares:
  - name: redirect
    config:
      status_code: 302
      preserve_query: true
      rules:
        - path: /old-page
          target: /new-page
          status_code: 301
        - prefix: /api/v1
          target: /api/v2
        - target: https://fallback.example.com
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `status_code` | integer | `302` | Default HTTP status code for redirects (301, 302, 307, 308) |
| `preserve_query` | boolean | `true` | Append the original query string to the redirect target |
| `rules` | array | **required** | Redirect rules evaluated in order; first match wins |

### Rule properties

| Property | Type | Description |
|----------|------|-------------|
| `path` | string | Exact path to match. Mutually exclusive with `prefix` |
| `prefix` | string | Path prefix to match. The matched prefix is stripped and the remainder is appended to `target` |
| `target` | string | **Required.** Redirect target URL or path |
| `status_code` | integer | Override the top-level `status_code` for this rule |

If neither `path` nor `prefix` is set, the rule matches all requests (catch-all).

### Matching behavior

- Rules are evaluated in order. The first matching rule wins.
- **Exact match** (`path`): redirects only when the request path equals the value exactly.
- **Prefix match** (`prefix`): strips the matched prefix and appends the remainder to `target`. For example, `prefix: /api/v1` with `target: /api/v2` redirects `/api/v1/users?page=2` to `/api/v2/users?page=2`.
- **Catch-all**: omit both `path` and `prefix` to redirect all requests hitting the route.

### Status codes

| Code | Meaning | Method preserved? |
|------|---------|-------------------|
| 301 | Moved Permanently | No (may change to GET) |
| 302 | Found | No (may change to GET) |
| 307 | Temporary Redirect | Yes |
| 308 | Permanent Redirect | Yes |

Use 307/308 when you need POST/PUT/DELETE requests to be retried with the same method.

### Use cases

**Domain migration:**
```yaml
- name: redirect
  config:
    status_code: 301
    rules:
      - target: https://new-domain.com
```

**API versioning:**
```yaml
- name: redirect
  config:
    rules:
      - prefix: /api/v1
        target: /api/v2
        status_code: 301
```

**Multiple redirects:**
```yaml
- name: redirect
  config:
    rules:
      - path: /blog
        target: https://blog.example.com
        status_code: 301
      - path: /docs
        target: https://docs.example.com
        status_code: 301
      - prefix: /old-api
        target: /api
```
