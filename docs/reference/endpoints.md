# Reserved Endpoints

Barbacane reserves the `/__barbacane/*` path prefix for gateway introspection and management endpoints. These are always available regardless of your spec configuration.

## Health Check

```
GET /__barbacane/health
```

Returns the gateway health status.

### Response

```json
{
  "status": "healthy",
  "artifact_version": 1,
  "compiler_version": "0.1.0",
  "routes_count": 12
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `status` | string | Always `"healthy"` if responding |
| `artifact_version` | integer | `.bca` format version |
| `compiler_version` | string | `barbacane` version that compiled the artifact |
| `routes_count` | integer | Number of routes loaded |

### Usage

```bash
# Kubernetes liveness probe
livenessProbe:
  httpGet:
    path: /__barbacane/health
    port: 8080
  initialDelaySeconds: 5
  periodSeconds: 10

# Load balancer health check
curl -f http://localhost:8080/__barbacane/health
```

---

## API Specs

```
GET /__barbacane/specs
```

Returns an index of all embedded API specifications (OpenAPI and AsyncAPI).

### Index Response

```bash
curl http://localhost:8080/__barbacane/specs
```

```json
{
  "openapi": {
    "specs": [
      { "name": "users-api.yaml", "url": "/__barbacane/specs/users-api.yaml" },
      { "name": "orders-api.yaml", "url": "/__barbacane/specs/orders-api.yaml" }
    ],
    "count": 2,
    "merged_url": "/__barbacane/specs/openapi"
  },
  "asyncapi": {
    "specs": [
      { "name": "events.yaml", "url": "/__barbacane/specs/events.yaml" }
    ],
    "count": 1,
    "merged_url": "/__barbacane/specs/asyncapi"
  }
}
```

### Merged Specs

Get all OpenAPI specs merged into one (for Swagger UI):

```bash
curl http://localhost:8080/__barbacane/specs/openapi
```

Get all AsyncAPI specs merged into one (for AsyncAPI Studio):

```bash
curl http://localhost:8080/__barbacane/specs/asyncapi
```

### Individual Specs

Fetch a specific spec by filename:

```bash
curl http://localhost:8080/__barbacane/specs/users-api.yaml
```

### Format Selection

Request specs in JSON or YAML format using the `format` query parameter:

```bash
# Get merged OpenAPI as JSON (for tools that prefer JSON)
curl "http://localhost:8080/__barbacane/specs/openapi?format=json"

# Get merged OpenAPI as YAML (default)
curl "http://localhost:8080/__barbacane/specs/openapi?format=yaml"
```

### Extension Stripping

All specs served via these endpoints have internal `x-barbacane-*` extensions stripped automatically. Only standard OpenAPI/AsyncAPI fields and the `x-sunset` extension (RFC 8594) are preserved.

### Usage

```bash
# Swagger UI integration (for OpenAPI specs)
# Point Swagger UI to: http://your-gateway/__barbacane/specs/openapi

# AsyncAPI Studio integration (for AsyncAPI specs)
# Point to: http://your-gateway/__barbacane/specs/asyncapi

# Download merged spec for documentation
curl -o api.yaml http://localhost:8080/__barbacane/specs/openapi

# API client generation
curl http://localhost:8080/__barbacane/specs/openapi | \
  openapi-generator generate -i /dev/stdin -g typescript-fetch -o ./client
```

---

## Path Reservation

The entire `/__barbacane/` prefix is reserved. Attempting to define operations under this path in your spec will result in undefined behavior (your routes may be shadowed by built-in endpoints).

**Don't do this:**

```yaml
paths:
  /__barbacane/custom:  # BAD: Reserved prefix
    get:
      ...
```

---

## Prometheus Metrics

```
GET /__barbacane/metrics
```

Returns gateway metrics in Prometheus text exposition format.

### Response

```
# HELP barbacane_requests_total Total number of HTTP requests processed
# TYPE barbacane_requests_total counter
barbacane_requests_total{method="GET",path="/users",status="200",api="users-api"} 42

# HELP barbacane_request_duration_seconds HTTP request duration in seconds
# TYPE barbacane_request_duration_seconds histogram
barbacane_request_duration_seconds_bucket{method="GET",path="/users",status="200",api="users-api",le="0.01"} 35
...

# HELP barbacane_active_connections Number of currently active connections
# TYPE barbacane_active_connections gauge
barbacane_active_connections 5
```

### Available Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_requests_total` | counter | method, path, status, api | Total requests processed |
| `barbacane_request_duration_seconds` | histogram | method, path, status, api | Request latency |
| `barbacane_request_size_bytes` | histogram | method, path, status, api | Request body size |
| `barbacane_response_size_bytes` | histogram | method, path, status, api | Response body size |
| `barbacane_active_connections` | gauge | - | Current open connections |
| `barbacane_connections_total` | counter | - | Total connections accepted |
| `barbacane_validation_failures_total` | counter | method, path, reason | Validation errors |
| `barbacane_middleware_duration_seconds` | histogram | middleware, phase | Middleware execution time |
| `barbacane_dispatch_duration_seconds` | histogram | dispatcher, upstream | Dispatcher execution time |
| `barbacane_wasm_execution_duration_seconds` | histogram | plugin, function | WASM plugin execution time |

### Usage

```bash
# Scrape metrics
curl http://localhost:8080/__barbacane/metrics
```

```yaml
# Prometheus scrape config
scrape_configs:
  - job_name: 'barbacane'
    static_configs:
      - targets: ['barbacane:8080']
    metrics_path: '/__barbacane/metrics'
```

---

## Security Considerations

Reserved endpoints are public by default. In production, consider:

1. **Network segmentation**: Only expose port 8080 to your load balancer
2. **Firewall rules**: Block `/__barbacane/*` from public access
3. **Reverse proxy**: Strip or restrict access to reserved paths

Example nginx configuration:

```nginx
location /__barbacane/ {
    # Only allow from internal network
    allow 10.0.0.0/8;
    deny all;

    proxy_pass http://barbacane:8080;
}

location / {
    proxy_pass http://barbacane:8080;
}
```
