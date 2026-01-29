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

## OpenAPI Spec

```
GET /__barbacane/openapi
```

Returns the embedded OpenAPI specification(s).

### Single Spec

When the artifact contains one spec, returns it directly:

```bash
curl http://localhost:8080/__barbacane/openapi
```

Response: The original YAML/JSON spec

Headers:
- `Content-Type: application/x-yaml` (for YAML files)
- `Content-Type: application/json` (for JSON files)

### Multiple Specs

When the artifact contains multiple specs, returns an index:

```bash
curl http://localhost:8080/__barbacane/openapi
```

```json
{
  "specs": [
    {
      "name": "users-api.yaml",
      "url": "/__barbacane/openapi/users-api.yaml"
    },
    {
      "name": "orders-api.yaml",
      "url": "/__barbacane/openapi/orders-api.yaml"
    }
  ],
  "count": 2
}
```

Then fetch individual specs:

```bash
curl http://localhost:8080/__barbacane/openapi/users-api.yaml
```

### Usage

```bash
# Swagger UI integration
# Point Swagger UI to: http://your-gateway/__barbacane/openapi

# Download spec for documentation
curl -o api.yaml http://localhost:8080/__barbacane/openapi

# API client generation
curl http://localhost:8080/__barbacane/openapi | \
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

## Future Endpoints

These endpoints are planned for future releases:

| Endpoint | Purpose |
|----------|---------|
| `/__barbacane/metrics` | Prometheus metrics |
| `/__barbacane/ready` | Readiness probe (after warm-up) |
| `/__barbacane/config` | Runtime configuration |
| `/__barbacane/routes` | Route table inspection |

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
