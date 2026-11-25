# Caching Middlewares

- [`cache`](#cache) — in-memory response caching with TTL

---

## cache

Caches responses in memory with TTL support.

```yaml
x-barbacane-middlewares:
  - name: cache
    config:
      ttl: 300
      vary:
        - Accept-Language
        - Accept-Encoding
      methods:
        - GET
        - HEAD
      cacheable_status:
        - 200
        - 301
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `ttl` | integer | `300` | Cache duration (seconds) |
| `vary` | array | `[]` | Headers that vary cache key |
| `methods` | array | `["GET", "HEAD"]` | HTTP methods to cache |
| `cacheable_status` | array | `[200, 301, 404]` | Status codes to cache |

### Cache key

The cache key is computed from:
- HTTP method
- Request path and query string
- Values of the headers listed in `vary`

### Storage rules

A response is stored only when its status is in `cacheable_status`, and never when its `Cache-Control` carries `no-store`, `private` or `no-cache` (the cache does not revalidate), or when it sets a cookie (`Set-Cookie`).

A response to a request carrying `Authorization` is stored only when it explicitly allows a shared cache to reuse it, through `public`, `s-maxage` or `must-revalidate` ([RFC 9111 §3.5](https://www.rfc-editor.org/rfc/rfc9111#section-3.5)), or when `vary` lists `authorization`, which keys each entry to its credential. Without either, one user's response could be served to another.

The cache is shared by every route of the gateway and holds at most 10,000 entries and 256 MiB, counting keys, headers and bodies; a response over 8 MiB is not stored. Past either bound, expired entries are evicted first, then those closest to expiry.

Entries expire after `ttl` seconds. `max-age` in the response is not interpreted.
