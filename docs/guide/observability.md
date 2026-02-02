# Observability

Barbacane provides comprehensive observability features out of the box: structured logging, Prometheus metrics, and distributed tracing with OpenTelemetry support.

## Logging

Structured logs are written to stdout. Two formats are available:

### JSON Format (Default)

Production-ready structured JSON logs:

```bash
barbacane serve --artifact api.bca --log-format json
```

```json
{"timestamp":"2024-01-15T10:30:00Z","level":"INFO","target":"barbacane","message":"request completed","trace_id":"abc123","request_id":"def456","method":"GET","path":"/users","status":200,"duration_ms":12}
```

### Pretty Format (Development)

Human-readable format for local development:

```bash
barbacane serve --artifact api.bca --log-format pretty --log-level debug
```

```
2024-01-15T10:30:00Z INFO  barbacane > request completed method=GET path=/users status=200 duration_ms=12
```

### Log Levels

Control verbosity with `--log-level`:

| Level | Description |
|-------|-------------|
| `error` | Errors only |
| `warn` | Warnings and errors |
| `info` | Normal operation (default) |
| `debug` | Detailed debugging |
| `trace` | Very verbose tracing |

```bash
barbacane serve --artifact api.bca --log-level debug
```

Or use the `RUST_LOG` environment variable:

```bash
RUST_LOG=debug barbacane serve --artifact api.bca
```

## Metrics

Prometheus metrics are exposed at `/__barbacane/metrics`:

```bash
curl http://localhost:8080/__barbacane/metrics
```

### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `barbacane_requests_total` | counter | Total requests by method, path, status, api |
| `barbacane_request_duration_seconds` | histogram | Request latency |
| `barbacane_request_size_bytes` | histogram | Request body size |
| `barbacane_response_size_bytes` | histogram | Response body size |
| `barbacane_active_connections` | gauge | Current open connections |
| `barbacane_connections_total` | counter | Total connections accepted |
| `barbacane_validation_failures_total` | counter | Validation errors by reason |
| `barbacane_middleware_duration_seconds` | histogram | Middleware execution time |
| `barbacane_dispatch_duration_seconds` | histogram | Dispatcher execution time |
| `barbacane_wasm_execution_duration_seconds` | histogram | WASM plugin execution time |
| `barbacane_slo_violation_total` | counter | SLO violations (when configured) |

### Prometheus Integration

Configure Prometheus to scrape metrics:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'barbacane'
    static_configs:
      - targets: ['barbacane:8080']
    metrics_path: '/__barbacane/metrics'
    scrape_interval: 15s
```

### Example Queries

```promql
# Request rate
rate(barbacane_requests_total[5m])

# P99 latency
histogram_quantile(0.99, rate(barbacane_request_duration_seconds_bucket[5m]))

# Error rate
sum(rate(barbacane_requests_total{status=~"5.."}[5m])) / sum(rate(barbacane_requests_total[5m]))

# Active connections
barbacane_active_connections
```

## Distributed Tracing

Barbacane supports W3C Trace Context for distributed tracing and can export spans via OpenTelemetry Protocol (OTLP).

### Enable OTLP Export

```bash
barbacane serve --artifact api.bca \
  --otlp-endpoint http://otel-collector:4317
```

Or use the environment variable:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317 barbacane serve --artifact api.bca
```

### Trace Context Propagation

Barbacane automatically:
- Extracts `traceparent` and `tracestate` headers from incoming requests
- Generates a new trace ID if none provided
- Injects trace context into upstream requests

This enables end-to-end tracing across your entire service mesh.

### Span Structure

Each request creates a span tree:

```
barbacane.request (root span)
├── barbacane.routing
├── barbacane.validation
├── barbacane.middleware.jwt-auth.request
├── barbacane.middleware.rate-limit.request
├── barbacane.dispatch.http-upstream
├── barbacane.middleware.rate-limit.response
└── barbacane.middleware.jwt-auth.response
```

### Span Attributes

Spans include attributes like:
- `http.method`, `http.route`, `http.status_code`
- `barbacane.api`, `barbacane.operation_id`
- `barbacane.middleware`, `barbacane.dispatcher`

### Integration with Collectors

Works with any OpenTelemetry-compatible backend:

**Jaeger:**
```bash
barbacane serve --artifact api.bca --otlp-endpoint http://jaeger:4317
```

**Grafana Tempo:**
```bash
barbacane serve --artifact api.bca --otlp-endpoint http://tempo:4317
```

**Datadog (with collector):**
```yaml
# otel-collector-config.yaml
exporters:
  datadog:
    api:
      key: ${DD_API_KEY}
```

## Per-Operation Configuration

Use `x-barbacane-observability` to configure observability per operation:

```yaml
# Global defaults
x-barbacane-observability:
  trace_sampling: 0.1    # Sample 10% of traces

paths:
  /health:
    get:
      x-barbacane-observability:
        trace_sampling: 0.0  # Don't trace health checks
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"ok"}'

  /payments:
    post:
      x-barbacane-observability:
        trace_sampling: 1.0           # 100% for critical endpoints
        latency_slo_ms: 200           # SLO threshold
        detailed_validation_logs: true # Debug validation issues
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://payments.internal"
```

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `trace_sampling` | number | `1.0` | Sampling rate (0.0 = none, 1.0 = all) |
| `latency_slo_ms` | integer | - | Latency threshold; emits `barbacane_slo_violation_total` when exceeded |
| `detailed_validation_logs` | boolean | `false` | Include full validation error details in logs |

## Production Setup

A typical production observability stack:

```bash
barbacane serve --artifact api.bca \
  --log-format json \
  --log-level info \
  --otlp-endpoint http://otel-collector:4317
```

Combined with:
- **Prometheus** scraping `/__barbacane/metrics`
- **OpenTelemetry Collector** receiving OTLP traces
- **Grafana** for dashboards and alerting
- **Log aggregation** (Loki, Elasticsearch) ingesting stdout

### Example Alert Rules

```yaml
# prometheus-rules.yaml
groups:
  - name: barbacane
    rules:
      - alert: HighErrorRate
        expr: |
          sum(rate(barbacane_requests_total{status=~"5.."}[5m]))
          / sum(rate(barbacane_requests_total[5m])) > 0.01
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "High error rate detected"

      - alert: HighLatency
        expr: |
          histogram_quantile(0.99, rate(barbacane_request_duration_seconds_bucket[5m])) > 1
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "P99 latency exceeds 1 second"

      - alert: SLOViolations
        expr: rate(barbacane_slo_violation_total[5m]) > 0.01
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "SLO violations detected"
```

## What's Next?

- [CLI Reference](../reference/cli.md) - All command-line options
- [Reserved Endpoints](../reference/endpoints.md) - Full metrics endpoint documentation
- [Spec Extensions](../reference/extensions.md) - Complete `x-barbacane-observability` reference
