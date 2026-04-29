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
| `cacheable_status` | array | `[200, 301]` | Status codes to cache |

### Cache key

Cache key is computed from:
- HTTP method
- Request path
- Vary header values (if configured)

### Cache-Control respect

The middleware respects `Cache-Control` response headers:
- `no-store` — Response not cached
- `no-cache` — Cache but revalidate
- `max-age=N` — Use specified TTL instead of config
