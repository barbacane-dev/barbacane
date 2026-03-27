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

## MCP Server

```
POST /__barbacane/mcp
DELETE /__barbacane/mcp
```

JSON-RPC 2.0 endpoint for the Model Context Protocol. Only available when `x-barbacane-mcp: { enabled: true }` is set in the spec.

### Supported Methods

| JSON-RPC Method | Description |
|----------------|-------------|
| `initialize` | Handshake — returns server capabilities and session ID |
| `notifications/initialized` | Client acknowledgment (no response) |
| `tools/list` | List available MCP tools (generated from operations) |
| `tools/call` | Execute a tool (dispatches through middleware + dispatcher pipeline) |
| `ping` | Keepalive check |

### Session Management

- `initialize` returns a `Mcp-Session-Id` response header
- Subsequent requests should include `Mcp-Session-Id` in the request header
- `DELETE /__barbacane/mcp` with `Mcp-Session-Id` terminates the session
- Sessions expire after 30 minutes of inactivity

### Authentication

MCP tool calls route through the same middleware pipeline as regular HTTP requests. Auth headers (`Authorization`, `X-Api-Key`, etc.) from the MCP HTTP request are forwarded to the internal dispatch, so existing auth middleware (jwt-auth, apikey-auth, etc.) applies transparently.

### Example

```bash
# Initialize
curl -X POST http://localhost:8080/__barbacane/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'

# List tools
curl -X POST http://localhost:8080/__barbacane/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: <session-id>" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'

# Call a tool
curl -X POST http://localhost:8080/__barbacane/mcp \
  -H "Content-Type: application/json" \
  -H "Mcp-Session-Id: <session-id>" \
  -H "Authorization: Bearer <token>" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"createOrder","arguments":{"items":[{"id":"abc"}]}}}'
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

## Admin API (Dedicated Port)

Starting with v0.3.0, operational endpoints (metrics, provenance) are served on a dedicated admin port (default `127.0.0.1:8081`), separate from user traffic. This follows ADR-0022 and keeps operational data off the public-facing port.

Configure with `--admin-bind` (default: `127.0.0.1:8081`). Set to `off` to disable.

### Health Check (Admin)

```
GET /health
```

Returns the gateway health status with uptime.

```json
{
  "status": "healthy",
  "artifact_version": 2,
  "compiler_version": "0.2.1",
  "routes_count": 12,
  "uptime_secs": 3600
}
```

### Prometheus Metrics

```
GET /metrics
```

Returns gateway metrics in Prometheus text exposition format.

#### Response

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

#### Available Metrics

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

#### Usage

```bash
# Scrape metrics from admin port
curl http://localhost:8081/metrics
```

```yaml
# Prometheus scrape config
scrape_configs:
  - job_name: 'barbacane'
    static_configs:
      - targets: ['barbacane:8081']
    metrics_path: '/metrics'
```

### Provenance

```
GET /provenance
```

Returns full artifact provenance data: cryptographic fingerprint, build metadata, source specs, bundled plugins, and drift detection status.

#### Response

```json
{
  "artifact_hash": "sha256:a1b2c3d4e5f6...",
  "compiled_at": "2026-03-01T10:30:00Z",
  "compiler_version": "0.2.1",
  "artifact_version": 2,
  "provenance": {
    "commit": "abc123def456",
    "source": "ci/github-actions"
  },
  "source_specs": [
    { "file": "api.yaml", "sha256": "abc123...", "type": "openapi" }
  ],
  "plugins": [
    { "name": "rate-limit", "version": "1.0.0", "sha256": "789abc..." }
  ],
  "drift_detected": false
}
```

#### Fields

| Field | Type | Description |
|-------|------|-------------|
| `artifact_hash` | string | Combined SHA-256 fingerprint of all artifact inputs |
| `compiled_at` | string | ISO 8601 compilation timestamp |
| `compiler_version` | string | Barbacane compiler version |
| `provenance.commit` | string? | Git commit SHA (if provided at compile time) |
| `provenance.source` | string? | Build source identifier |
| `source_specs` | array | Source specifications with individual hashes |
| `plugins` | array | Bundled plugins with versions and hashes |
| `drift_detected` | boolean | `true` if control plane detected a hash mismatch |

#### Usage

```bash
# Check what's running
curl http://localhost:8081/provenance

# Verify artifact hash
curl -s http://localhost:8081/provenance | jq -r '.artifact_hash'

# Check for drift
curl -s http://localhost:8081/provenance | jq '.drift_detected'
```

---

## Security Considerations

The `/__barbacane/*` endpoints on the main traffic port (8080) serve **health checks** and **API specs** — both are typically safe to expose publicly. Health checks are standard for load balancers and Kubernetes probes. API specs are designed for API consumers (Swagger UI, client generation).

Operational endpoints (metrics, provenance) are served on the **admin port** (default `127.0.0.1:8081`), which binds to localhost only by default.

In production, consider:

1. **Keep admin port internal**: The default `127.0.0.1:8081` binding is already safe; if you change it to `0.0.0.0:8081`, ensure firewall rules restrict access
2. **Network segmentation**: Only expose port 8080 to your load balancer
3. **Spec access control**: If your API specs contain sensitive information, restrict `/__barbacane/specs` via reverse proxy

Example nginx configuration:

```nginx
location /__barbacane/specs {
    # Restrict spec access to internal network if needed
    allow 10.0.0.0/8;
    deny all;

    proxy_pass http://barbacane:8080;
}

location / {
    proxy_pass http://barbacane:8080;
}
```
