# ADR-0010: Observability Strategy

**Status:** Accepted
**Date:** 2026-01-28

## Context

With data planes deployed at the edge (ADR-0007), observability is critical but constrained:

- Edge nodes have limited storage and connectivity
- Observability must not degrade gateway latency
- WASM plugins need a way to emit telemetry without host access
- Operators need a unified view across distributed edge instances

## Decision

### OpenTelemetry as the Standard

All observability uses **OpenTelemetry (OTel)** as the wire format and SDK. No proprietary formats.

```
┌──────────────┐         ┌──────────────────┐         ┌──────────────┐
│  Data Plane  │──OTLP──▶│  OTel Collector  │────────▶│   Backends   │
│   (edge)     │         │  (sidecar or     │         │  Jaeger      │
│              │         │   regional)      │         │  Prometheus  │
└──────────────┘         └──────────────────┘         │  Loki / ...  │
                                                      └──────────────┘
```

### Three Pillars

#### 1. Metrics (core built-in)

The gateway core emits standard metrics without any plugin required:

| Metric | Type | Labels |
|--------|------|--------|
| `barbacane_requests_total` | Counter | `method`, `path`, `status`, `api` |
| `barbacane_request_duration_seconds` | Histogram | `method`, `path`, `status`, `api` |
| `barbacane_request_size_bytes` | Histogram | `method`, `path`, `api` |
| `barbacane_response_size_bytes` | Histogram | `method`, `path`, `api` |
| `barbacane_active_connections` | Gauge | `api` |
| `barbacane_validation_failures_total` | Counter | `method`, `path`, `reason` |
| `barbacane_dispatch_duration_seconds` | Histogram | `dispatcher`, `upstream` |
| `barbacane_middleware_duration_seconds` | Histogram | `middleware`, `phase` |
| `barbacane_wasm_execution_duration_seconds` | Histogram | `plugin`, `function` |

These are always collected — no configuration needed.

#### 2. Traces (core built-in)

Every request gets a distributed trace with spans for each processing phase:

```
[Request]
  ├── [TLS termination]
  ├── [Routing]
  ├── [Validation]
  ├── [Middleware: jwt-auth]
  ├── [Middleware: rate-limit]
  ├── [Dispatch: http-upstream]
  │     └── [Upstream call]
  ├── [Response validation]  (if enabled)
  └── [Response]
```

- W3C Trace Context (`traceparent` / `tracestate`) propagated to upstreams
- Trace sampling configurable to manage volume at scale

#### 3. Logs (structured)

All logs are structured JSON, emitted to stdout (12-factor app style):

```json
{
  "timestamp": "2026-01-28T10:30:00Z",
  "level": "warn",
  "trace_id": "abc123",
  "span_id": "def456",
  "message": "validation_failure",
  "path": "/users/123",
  "reason": "missing_required_field",
  "field": "email"
}
```

Logs are correlated with traces via `trace_id` and `span_id`.

### Plugin Telemetry

WASM plugins can emit custom telemetry via host functions:

| Host function | Purpose |
|---------------|---------|
| `metric_counter_inc(name, labels, value)` | Increment a counter |
| `metric_histogram_observe(name, labels, value)` | Record a histogram observation |
| `span_start(name)` | Start a child span |
| `span_end()` | End the current span |
| `span_set_attribute(key, value)` | Add metadata to current span |
| `log(level, message, fields)` | Emit a structured log |

Plugin telemetry is namespaced automatically: a plugin named `jwt-auth` emitting counter `tokens_validated` results in `barbacane_plugin_jwt_auth_tokens_validated`.

### Export

Data planes push telemetry to an **OpenTelemetry Collector** via OTLP (gRPC or HTTP):

- Collector runs as sidecar or regional aggregator
- Collector handles routing to backends (Jaeger, Prometheus, Loki, Datadog, etc.)
- Data plane never talks directly to observability backends
- If collector is unreachable, telemetry is dropped (never blocks request processing)

### Spec Integration

Observability can be tuned per-API via extensions:

```yaml
x-barbacane-observability:
  trace_sampling: 0.1        # sample 10% of traces
  detailed_validation_logs: true  # log every validation failure
  latency_slo: 50ms          # emit alert metric when exceeded
```

## Consequences

- **Easier:** Unified observability format (OTel), correlation across traces/metrics/logs, plugins get telemetry "for free" via host functions
- **Harder:** OTel Collector is an operational dependency, trace volume management at scale
- **Tradeoff:** Telemetry is fire-and-forget (dropped if collector is down) — acceptable for edge deployment where availability of the gateway matters more than completeness of telemetry
