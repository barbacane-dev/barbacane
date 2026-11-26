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
{"duration_ms":12,"level":"INFO","message":"request completed","method":"GET","path":"/users","request_id":"8a085784-a4de-4233-8f00-59641fcbdb30","status":200,"target":"barbacane::access","timestamp":"2026-01-15T10:30:00.123456Z","trace_id":"4bf92f3577b34da6a3ce929d0e0e4736"}
```

Each line is one JSON object with `timestamp`, `level`, `target`, `message` and the event's own fields at the top level. A line logged while a request is handled also carries the request's `request_id` and `trace_id` (the ids the response's `x-request-id` and `x-trace-id` give), and `upstream_endpoint` once a dispatch has used a pool endpoint.

Each request ends with a `request completed` line at `info`, under the target `barbacane::access`: `method`, `path`, `status` and `duration_ms` (for a streamed response, until its headers are sent). `RUST_LOG=info,barbacane::access=off` leaves it out.

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

Prometheus metrics are exposed on the dedicated admin API port (default `127.0.0.1:8081`):

```bash
curl http://localhost:8081/metrics
```

### Available Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `barbacane_requests_total` | counter | Total requests by method, path, status, api (the spec of the matched operation) |
| `barbacane_request_duration_seconds` | histogram | Request latency; for a streamed response, until its last byte is sent |
| `barbacane_request_size_bytes` | histogram | Request body size |
| `barbacane_response_size_bytes` | histogram | Response body size; for a streamed response, the bytes sent |
| `barbacane_active_connections` | gauge | Current open connections |
| `barbacane_connections_total` | counter | Total connections accepted |
| `barbacane_validation_failures_total` | counter | Validation errors by reason |
| `barbacane_middleware_duration_seconds` | histogram | Middleware execution time |
| `barbacane_dispatch_duration_seconds` | histogram | Dispatcher execution time, by dispatcher |
| `barbacane_dispatch_errors_total` | counter | Dispatches that failed (trap, fuel, deadline), by dispatcher |
| `barbacane_wasm_execution_duration_seconds` | histogram | Time of each plugin call, by plugin and function (`init`, `on_request`, `on_response`, `dispatch`) |
| `barbacane_wasm_traps_total` | counter | Plugin calls that trapped, ran out of fuel or passed their deadline |
| `barbacane_deprecated_route_requests_total` | counter | Requests to deprecated operations, by method, path, api |
| `barbacane_wasm_instances` | gauge | Plugin instances alive now |
| `barbacane_wasm_instances_unpooled_total` | counter | Plugin instances allocated per request because the [instance pool](../reference/cli.md#plugin-instance-pool) was full |
| `barbacane_upstream_endpoint_requests_total` | counter | Attempts sent to an `endpoints` pool member, by endpoint and outcome (`success`, `failure`) |
| `barbacane_upstream_endpoint_retries_total` | counter | Attempts sent to an endpoint after another one failed, by endpoint |
| `barbacane_upstream_endpoint_circuit_state` | gauge | An endpoint's circuit breaker: 0 closed, 1 open, 2 half-open |

An endpoint label is `scheme://host:port`, never the full URL, so it carries no user information or query; its values are the endpoints the artifact lists. The request records the endpoint a dispatch used as `upstream_endpoint`, a field of its log lines from then on, and breaker transitions are logged with it.

Labels stay bounded whatever clients send. `path` is the route template, or one label for requests that match no route. `method` is `GET`, `HEAD`, `POST`, `PUT`, `DELETE`, `CONNECT`, `OPTIONS`, `TRACE`, `PATCH` or `QUERY`, and `OTHER` for any other method, including one declared under OpenAPI 3.2 `additionalOperations`.

### Plugin metrics

A plugin with the `telemetry` capability emits counters and histograms. Each metric is its own family, `barbacane_plugin_<plugin>_<metric>`, with the plugin name's `-` written `_`: the `observability` middleware's `slo_violation` counter is `barbacane_plugin_observability_slo_violation_total`. The labels the plugin sends are the series' labels.

- A metric name is `[a-zA-Z_][a-zA-Z0-9_]*`; a label name is the same and does not start with `__`; a label value is a string, number or boolean. A histogram's buckets run from 1 ms to 10 s, for observations in seconds.
- Each plugin has at most 1,000 series. A family belongs to the plugin metric that created it: another plugin's metric with the same exported name, or the same metric with the other type, is refused.
- Every refused write is counted in `barbacane_plugin_metrics_dropped_total{plugin,reason}`, `reason` being `oversize`, `cardinality`, `non_finite`, `invalid_name`, `invalid_labels`, `name_conflict` or `type_conflict`.

### Prometheus Integration

Configure Prometheus to scrape metrics:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'barbacane'
    static_configs:
      - targets: ['barbacane:8081']
    metrics_path: '/metrics'
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

Every request is an OpenTelemetry span, exported over OTLP when the gateway has a collector to send it to, and its trace context reaches the plugins and the upstream.

### Enable OTLP Export

```bash
barbacane serve --artifact api.bca \
  --otlp-endpoint http://otel-collector:4317
```

Or use the environment variables:

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317 barbacane serve --artifact api.bca
```

Spans go over OTLP/gRPC by default. `--otlp-protocol http/protobuf` (or `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`) sends them over OTLP/HTTP, to the endpoint with `/v1/traces` after it, typically on port 4318. They leave in batches; the OpenTelemetry `OTEL_BSP_*` variables (`OTEL_BSP_SCHEDULE_DELAY`, `OTEL_BSP_MAX_QUEUE_SIZE`, ...) tune the batching. Spans still buffered are exported when the gateway shuts down. Without an endpoint no span is exported, and requests still carry trace context.

### Sampling

`--trace-sampling` is the share of the traces the gateway starts that are recorded, from `0.0` to `1.0` (default). A request whose `traceparent` says its trace is sampled, or not, keeps that decision, so a trace is recorded whole or not at all.

### Trace Context Propagation

- A request's `traceparent` and `tracestate` (W3C Trace Context) name the parent of its span; without a valid `traceparent` the request starts a trace. The response's `x-trace-id` is the trace id.
- Plugins and upstreams receive a `traceparent` and `tracestate` that name the request's span as their parent, in place of the client's.
- They receive `x-request-id` holding the request id the gateway logs and answers with: the client's when it is well-formed, a generated one otherwise.

### Span Structure

```
GET /orders/{id}                        server span: the method and the route
├── barbacane.middleware.jwt-auth.request
├── barbacane.middleware.rate-limit.request
├── barbacane.dispatch.http-upstream
├── barbacane.plugin.<plugin>.<name>    spans a plugin opens (telemetry capability)
├── barbacane.middleware.rate-limit.response
└── barbacane.middleware.jwt-auth.response
```

A request that matches no route keeps the method alone as its name. The dispatch span of a streamed response ends when the stream does.

### Span Attributes

| Span | Attributes |
|------|------------|
| server | `http.request.method`, `url.path`, `http.route`, `http.response.status_code`, `network.protocol.version`, `client.address`, `barbacane.request_id`, `barbacane.operation_id`; an error status for a `5xx` answer |
| middleware | `barbacane.middleware`, `barbacane.middleware.short_circuit` when it answered the request; an error status when it failed |
| dispatch | `barbacane.dispatcher`; an error status when the dispatcher failed |
| plugin | what the plugin sets with `host_span_set_attribute` |

The resource carries `service.name` (`barbacane`) and `service.version`.

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

## Production Setup

A typical production observability stack:

```bash
barbacane serve --artifact api.bca \
  --log-format json \
  --log-level info \
  --otlp-endpoint http://otel-collector:4317
```

Combined with:
- **Prometheus** scraping `/metrics` on the admin port (default 8081)
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
```

## What's Next?

- [CLI Reference](../reference/cli.md) - All command-line options
- [Reserved Endpoints](../reference/endpoints.md) - Full metrics endpoint documentation
- [Spec Extensions](../reference/extensions.md) - Barbacane spec extensions reference
