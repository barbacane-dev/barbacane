# Barbacane Playground

A complete demonstration environment for the Barbacane API Gateway featuring a realistic Train Travel API, full observability stack (logs, metrics, traces), control plane UI, and mock backend services.

## Quick Start

```bash
cd playground

# Start all services (specs are compiled automatically)
docker compose up -d

# Wait for services to be healthy (about 60 seconds for first build)
docker compose ps

# Test the gateway
curl http://localhost:8080/stations
```

That's it! The API specs are compiled automatically before the gateway starts.

## Services

| Service | URL | Description |
|---------|-----|-------------|
| **Gateway** | http://localhost:8080 | Barbacane API Gateway (Data Plane) |
| **Control Plane** | http://localhost:3001 | Web UI for managing APIs |
| **Grafana** | http://localhost:3000 | Dashboards (admin/admin) |
| **Prometheus** | http://localhost:9090 | Metrics |
| **WireMock** | http://localhost:8081/__admin | Mock backend admin |

## API Endpoints

### Stations (Public, Cached)

```bash
# List all stations
curl http://localhost:8080/stations

# Get station details
curl http://localhost:8080/stations/efdbb9d1-02c2-4bc3-afb7-6788d8782b1e

# Get live departures
curl http://localhost:8080/stations/efdbb9d1-02c2-4bc3-afb7-6788d8782b1e/departures
```

### Trips (Public)

```bash
# Search for trips
curl "http://localhost:8080/trips?origin=efdbb9d1-02c2-4bc3-afb7-6788d8782b1e&destination=b2e783e1-c824-4d63-b37a-d8ee6c0b95da&departure_date=2025-03-15"

# Get trip details
curl http://localhost:8080/trips/f08d2c3e-8d6f-7f5f-2c1f-4e5f6a7b8c9d

# Get real-time status
curl http://localhost:8080/trips/f08d2c3e-8d6f-7f5f-2c1f-4e5f6a7b8c9d/realtime
```

### Bookings (Requires JWT)

The booking endpoints require JWT authentication. For demo purposes, signature validation is disabled.

```bash
# Create a demo JWT (paste this into jwt.io to customize)
# Header: {"alg":"RS256","typ":"JWT"}
# Payload: {"sub":"user123","iss":"demo","aud":"trains","exp":1893456000}
TOKEN="eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyMTIzIiwiaXNzIjoiZGVtbyIsImF1ZCI6InRyYWlucyIsImV4cCI6MTg5MzQ1NjAwMH0.demo"

# List bookings
curl -H "Authorization: Bearer $TOKEN" http://localhost:8080/bookings

# Get booking details
curl -H "Authorization: Bearer $TOKEN" http://localhost:8080/bookings/d4e5f6a7-b8c9-0d1e-2f3a-4b5c6d7e8f9a

# Create a booking
curl -X POST http://localhost:8080/bookings \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "trip_id": "f08d2c3e-8d6f-7f5f-2c1f-4e5f6a7b8c9d",
    "passengers": [{"name": "John Doe", "email": "john@example.com"}]
  }'
```

## Feature Demonstrations

### Rate Limiting

The API is rate limited to 60 requests per minute per API key.

```bash
# Trigger rate limiting (run in a loop)
for i in {1..100}; do
  curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8080/stations
done
# You'll start seeing 429 responses after ~60 requests
```

### Caching

Station data is cached for 5 minutes. Check the response headers:

```bash
curl -v http://localhost:8080/stations 2>&1 | grep -i cache
```

### SLO Monitoring

The `/stations` endpoint has a 500ms SLO. Violations are logged and emit metrics.

```bash
# Check metrics for SLO violations
curl http://localhost:8080/__barbacane/metrics | grep slo
```

### Request Validation

```bash
# Invalid country code (should be 2 uppercase letters)
curl "http://localhost:8080/stations?country=invalid"
# Returns 400 Bad Request

# Invalid passenger count
curl "http://localhost:8080/trips?origin=...&destination=...&departure_date=2025-03-15&passengers=100"
# Returns 400 Bad Request (max is 9)
```

### CORS

CORS is configured globally. Test with:

```bash
curl -X OPTIONS http://localhost:8080/stations \
  -H "Origin: https://example.com" \
  -H "Access-Control-Request-Method: GET" \
  -v 2>&1 | grep -i "access-control"
```

## Observability

### Grafana Dashboards

Open http://localhost:3000 (login: admin/admin)

The pre-configured **Barbacane API Gateway** dashboard shows:
- Request rate and latency
- Error rates
- Middleware performance
- Active connections
- Gateway logs

### Metrics

View raw Prometheus metrics:

```bash
curl http://localhost:8080/__barbacane/metrics
```

Key metrics:
- `barbacane_requests_total` - Request count by status, method, path
- `barbacane_request_duration_seconds` - Request latency histogram
- `barbacane_middleware_duration_seconds` - Per-middleware latency
- `barbacane_active_connections` - Current connection count

### Logs

View gateway logs in Grafana (Explore > Loki) or directly:

```bash
docker compose logs -f barbacane
```

Logs are collected by Grafana Alloy and shipped to Loki.

### Traces

Distributed tracing is available in Grafana (Explore > Tempo).

## Control Plane

The Control Plane UI at http://localhost:3001 provides:
- API specification management
- Gateway configuration
- Plugin management

The Control Plane API is available at http://localhost:9091.

## WireMock Admin

View and manage mock stubs:

```bash
# List all stubs
curl http://localhost:8081/__admin/mappings

# View request log
curl http://localhost:8081/__admin/requests
```

## Development

### Modifying the API Spec

Edit `specs/train-travel-api.yaml` and restart to recompile:

```bash
docker compose up -d --force-recreate compiler barbacane
```

### Rebuilding from Source

```bash
# Rebuild all images
docker compose build

# Restart services
docker compose up -d
```

### Modifying Mock Responses

Edit files in `wiremock/mappings/` and restart WireMock:

```bash
docker compose restart wiremock
```

### Adding New Endpoints

1. Add the endpoint to `specs/train-travel-api.yaml`
2. Add a corresponding WireMock stub in `wiremock/mappings/`
3. Restart: `docker compose up -d --force-recreate compiler barbacane`

## Cleanup

```bash
# Stop all services
docker compose down

# Remove volumes (reset all data)
docker compose down -v
```

## Architecture

```
                    ┌──────────────────────────────────────────────────────────────┐
                    │                       Docker Network                          │
                    │                                                               │
┌──────────┐        │  ┌───────────┐      ┌───────────┐      ┌───────────┐         │
│  Client  │───────────│ Barbacane │──────│  WireMock │      │ Prometheus│         │
└──────────┘        │  │  :8080    │      │   :8081   │      │   :9090   │         │
                    │  └─────┬─────┘      └───────────┘      └─────┬─────┘         │
                    │        │                                      │               │
                    │        │ logs                         scrape  │               │
                    │        ▼                                      │               │
                    │  ┌───────────┐      ┌───────────┐             │               │
                    │  │   Alloy   │──────│   Loki    │             │               │
                    │  └───────────┘      │   :3100   │             │               │
                    │                     └─────┬─────┘             │               │
                    │        ┌──────────────────┼───────────────────┘               │
                    │        │                  │                                   │
                    │        ▼                  ▼                                   │
                    │  ┌─────────────────────────────┐      ┌───────────┐          │
                    │  │          Grafana            │◄─────│   Tempo   │          │
                    │  │           :3000             │      │   :3200   │          │
                    │  └─────────────────────────────┘      └───────────┘          │
                    │                                              ▲                │
                    │  ┌───────────────┐                          │ OTLP           │
                    │  │ Control Plane │                          │                │
                    │  │     :3001     │              ┌───────────┴───────────┐    │
                    │  └───────┬───────┘              │       Barbacane       │    │
                    │          │                      │        (traces)       │    │
                    │          ▼                      └───────────────────────┘    │
                    │  ┌───────────────┐                                           │
                    │  │   PostgreSQL  │                                           │
                    │  └───────────────┘                                           │
                    └──────────────────────────────────────────────────────────────┘
```
