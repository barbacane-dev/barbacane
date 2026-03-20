# redirect

HTTP redirect middleware plugin for Barbacane API gateway.

Redirects requests based on configurable path rules. Supports exact path matching, prefix matching with path rewriting, configurable status codes (301/302/307/308), and query string preservation.

## Configuration

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

### Properties

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `status_code` | integer | `302` | Default HTTP status code for redirects (301, 302, 307, 308) |
| `preserve_query` | boolean | `true` | Append the original query string to the redirect target |
| `rules` | array | **required** | Redirect rules evaluated in order; first match wins |

### Rule Properties

| Property | Type | Description |
|----------|------|-------------|
| `path` | string | Exact path to match. Mutually exclusive with `prefix` |
| `prefix` | string | Path prefix to match. The matched prefix is stripped and the remainder is appended to `target` |
| `target` | string | **Required.** Redirect target URL or path |
| `status_code` | integer | Override the top-level `status_code` for this rule |

If neither `path` nor `prefix` is set, the rule matches all requests (catch-all).

## Examples

### Domain migration (permanent)

```yaml
- name: redirect
  config:
    status_code: 301
    rules:
      - target: https://new-domain.com
```

### API versioning

```yaml
- name: redirect
  config:
    rules:
      - prefix: /api/v1
        target: /api/v2
        status_code: 301
```

Redirects `/api/v1/users?page=2` to `/api/v2/users?page=2`.

### Multiple redirect rules

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

## Behavior

- Rules are evaluated in order. The first matching rule wins.
- Query strings are preserved by default. Set `preserve_query: false` to strip them.
- The middleware short-circuits the request chain — matched requests never reach downstream middlewares or the dispatcher.
- The response body contains a human-readable status description (e.g., "Moved Permanently").

## Building

```bash
cd plugins/redirect
cargo build --target wasm32-unknown-unknown --release
```

## Testing

```bash
cd plugins/redirect
cargo test
```
