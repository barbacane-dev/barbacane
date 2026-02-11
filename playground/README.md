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

### Bookings (Requires OIDC Auth)

The booking endpoints are protected by OpenID Connect authentication. A mock OAuth2 server issues signed JWT tokens that the gateway validates with full cryptographic verification (OIDC Discovery + JWKS).

```bash
# Get an access token from the mock OIDC provider
TOKEN=$(curl -s -X POST http://localhost:9099/barbacane/token \
  -d "grant_type=client_credentials&scope=openid" \
  | jq -r '.access_token')

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

# Without a token → 401 Unauthorized
curl http://localhost:8080/bookings
```

The OIDC flow:
1. Gateway fetches `http://mock-oauth:8080/barbacane/.well-known/openid-configuration`
2. Discovers the JWKS endpoint and fetches signing keys
3. Validates the JWT signature (RS256) against the provider's public key
4. Checks `iss`, `aud`, `exp` claims

### Events (NATS Dispatch)

Event endpoints publish messages to NATS subjects and return `202 Accepted`. The gateway validates the request payload against the AsyncAPI schema before publishing.

```bash
# Publish a train delay event
curl -X POST http://localhost:8080/events/trips/delayed \
  -H "Content-Type: application/json" \
  -d '{
    "event_type": "trip.delayed",
    "trip_id": "f08d2c3e-8d6f-7f5f-2c1f-4e5f6a7b8c9d",
    "delay_minutes": 15,
    "reason": "weather",
    "timestamp": "2025-03-15T10:30:00Z"
  }'
# Returns: {"status":"accepted","subject":"trains.trips.delayed"}

# Publish a booking confirmation event
curl -X POST http://localhost:8080/events/bookings/confirmed \
  -H "Content-Type: application/json" \
  -d '{
    "event_type": "booking.confirmed",
    "booking_id": "d4e5f6a7-b8c9-0d1e-2f3a-4b5c6d7e8f9a",
    "reference": "BOOK-123",
    "passenger_count": 2,
    "trip_id": "f08d2c3e-8d6f-7f5f-2c1f-4e5f6a7b8c9d",
    "timestamp": "2025-03-15T10:30:00Z"
  }'

# Publish a payment succeeded event
curl -X POST http://localhost:8080/events/payments/succeeded \
  -H "Content-Type: application/json" \
  -d '{
    "event_type": "payment.succeeded",
    "payment_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "booking_id": "d4e5f6a7-b8c9-0d1e-2f3a-4b5c6d7e8f9a",
    "amount": 49.99,
    "currency": "EUR",
    "method": "card",
    "timestamp": "2025-03-15T10:30:00Z"
  }'

# Check NATS server stats
curl http://localhost:8222/varz
```

Available event subjects: `trains.trips.delayed`, `trains.trips.cancelled`, `trains.stations.platform-changed`, `trains.bookings.confirmed`, `trains.bookings.cancelled`, `trains.payments.succeeded`, `trains.payments.failed`.

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
