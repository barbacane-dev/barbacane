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
| **NATS** | nats://localhost:4222 | Message broker ([monitoring](http://localhost:8222)) |
| **Mock OAuth** | http://localhost:9099 | OIDC Provider ([discovery](http://localhost:9099/barbacane/.well-known/openid-configuration)) |
| **WireMock** | http://localhost:8081/__admin | Mock backend admin |
| **RustFS** | http://localhost:9000 | S3-compatible object storage ([console](http://localhost:9001)) |

## API Endpoints

All requests are in **[playground.http](playground.http)** — open it with the [JetBrains HTTP Client](https://www.jetbrains.com/help/idea/http-client-in-product-code-editor.html) or the [VS Code REST Client](https://marketplace.visualstudio.com/items?itemName=humao.rest-client) extension. The file handles the OIDC token exchange automatically.

| Endpoint group | Path | Auth |
|---|---|---|
| Stations | `GET /stations`, `/stations/{id}`, `/stations/{id}/departures` | Public |
| Trips | `GET /trips`, `/trips/{id}`, `/trips/{id}/realtime` | Public |
| Bookings | `GET/POST /bookings`, `GET /bookings/{id}` | OIDC |
| Events | `POST /events/trips/delayed`, `/events/bookings/confirmed`, `/events/payments/succeeded` | Public |
| S3 proxy | `PUT/GET/DELETE /storage/{bucket}/{key+}` | OIDC |
| Asset CDN | `GET /assets/{key+}` | Public (rate-limited) |

### Bookings — OIDC flow

Booking endpoints require a JWT issued by the mock OAuth2 server. The gateway validates the token via OIDC Discovery + JWKS (no shared secret, full RS256 verification):

1. Gateway fetches `http://mock-oauth:8080/barbacane/.well-known/openid-configuration`
2. Discovers the JWKS endpoint and fetches signing keys
3. Validates the JWT signature (RS256) against the provider's public key
4. Checks `iss`, `aud`, `exp` claims

### Events — NATS dispatch

Event endpoints publish to NATS and return `202 Accepted`. The gateway validates the request body against the AsyncAPI schema before publishing.

Available subjects: `trains.trips.delayed`, `trains.trips.cancelled`, `trains.stations.platform-changed`, `trains.bookings.confirmed`, `trains.bookings.cancelled`, `trains.payments.succeeded`, `trains.payments.failed`.

### S3 Object Storage Proxy (RustFS)

Full S3 proxy powered by the `s3` dispatcher and backed by [RustFS](https://rustfs.com) — an S3-compatible object storage written in Rust. Auth is handled by the gateway; S3 credentials never leave the server.

- `/storage/{bucket}/{key+}` — OIDC-protected multi-bucket proxy
- `/assets/{key+}` — Public rate-limited CDN (pre-seeded with `s3://assets/welcome.txt`)

RustFS console: http://localhost:9001 (credentials: `playground` / `playground-secret`)

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

View and manage mock stubs via http://localhost:8081/__admin (see `playground.http` for ready-to-run requests).

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
                    │  └──┬──┬──┬──┘      └───────────┘      └─────┬─────┘         │
                    │     │  │  │                                   │               │
                    │     │  │  │ OIDC/JWKS      ┌───────────┐     │               │
                    │     │  │  └────────────────▶│Mock OAuth │     │               │
                    │     │  │                    │   :9099   │     │               │
                    │     │  │ publish            └───────────┘     │               │
                    │     │  ▼                                scrape│               │
                    │     │ ┌───────────┐                          │               │
                    │     │ │   NATS    │                          │               │
                    │     │ │  :4222    │                          │               │
                    │     │ └───────────┘                          │               │
                    │     │ logs                                   │               │
                    │     ▼                                        │               │
                    │  ┌───────────┐      ┌───────────┐            │               │
                    │  │   Alloy   │──────│   Loki    │            │               │
                    │  └───────────┘      │   :3100   │            │               │
                    │                     └─────┬─────┘            │               │
                    │        ┌──────────────────┼──────────────────┘               │
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
