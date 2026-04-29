# Observability Middlewares

- [`correlation-id`](#correlation-id) — request tracing ID propagation
- [`http-log`](#http-log) — structured log shipping to an HTTP endpoint

---

## correlation-id

Propagates or generates correlation IDs (UUID v7) for distributed tracing. The correlation ID is passed to upstream services and included in responses.

```yaml
x-barbacane-middlewares:
  - name: correlation-id
    config:
      header_name: X-Correlation-ID
      generate_if_missing: true
      trust_incoming: true
      include_in_response: true
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `header_name` | string | `X-Correlation-ID` | Header name for the correlation ID |
| `generate_if_missing` | boolean | `true` | Generate new UUID v7 if not provided |
| `trust_incoming` | boolean | `true` | Trust and propagate incoming correlation IDs |
| `include_in_response` | boolean | `true` | Include correlation ID in response headers |

---

## http-log

Sends structured JSON log entries to an HTTP endpoint for centralized logging. Captures request metadata, response status, timing, and optional headers/body sizes. Compatible with Datadog, Splunk, ELK, or any HTTP log ingestion endpoint.

```yaml
x-barbacane-middlewares:
  - name: http-log
    config:
      endpoint: https://logs.example.com/ingest
      method: POST
      timeout_ms: 2000
      include_headers: false
      include_body: true
      custom_fields:
        service: my-api
        environment: production
```

### Configuration

| Property | Type | Default | Description |
|----------|------|---------|-------------|
| `endpoint` | string | **required** | URL to send log entries to |
| `method` | string | `POST` | HTTP method (`POST` or `PUT`) |
| `timeout_ms` | integer | `2000` | Timeout for the log HTTP call (100-10000 ms) |
| `content_type` | string | `application/json` | Content-Type header for the log request |
| `include_headers` | boolean | `false` | Include request and response headers in log entries |
| `include_body` | boolean | `false` | Include request and response body sizes in log entries |
| `custom_fields` | object | `{}` | Static key-value fields included in every log entry |

### Log entry format

Each log entry is a JSON object:

```json
{
  "timestamp_ms": 1706500000000,
  "duration_ms": 42,
  "correlation_id": "abc-123",
  "request": {
    "method": "POST",
    "path": "/users",
    "query": "page=1",
    "client_ip": "10.0.0.1",
    "headers": { "content-type": "application/json" },
    "body_size": 256
  },
  "response": {
    "status": 201,
    "headers": { "content-type": "application/json" },
    "body_size": 64
  },
  "service": "my-api",
  "environment": "production"
}
```

Optional fields (`correlation_id`, `headers`, `body_size`, `query`) are omitted when not available or not enabled.

### Behavior

- Runs in the **response phase** (after dispatch) to capture both request and response data
- Log delivery is **best-effort** — failures never affect the upstream response
- The `correlation_id` field is automatically populated if the `correlation-id` middleware runs earlier in the chain
- Custom fields are flattened into the top-level JSON object
