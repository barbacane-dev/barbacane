# SPEC-005: Observability

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0010

---

## 1. Overview

Barbacane emits metrics, traces, and structured logs using OpenTelemetry as the wire format. This spec defines every metric name, trace span, log field, and export configuration.

---

## 2. Architecture

```
Data Plane ──OTLP──▶ OTel Collector ──▶ Backend(s)
```

The data plane pushes telemetry to an OpenTelemetry Collector. It never talks directly to observability backends (Prometheus, Jaeger, Loki, etc.). The collector handles routing, sampling, and fan-out.

If the collector is unreachable, telemetry is silently dropped. Request processing is never blocked or degraded by telemetry failures.

---

## 3. Configuration

### 3.1 CLI flags

```
--otlp-endpoint <URL>       OTel Collector endpoint (default: http://localhost:4317)
--otlp-protocol <PROTO>     "grpc" | "http" (default: grpc)
--otlp-headers <K=V,...>    Additional headers for OTLP export (e.g. auth tokens)
```

### 3.2 Spec-level tuning

```yaml
x-barbacane-observability:
  trace_sampling: 0.1              # sample 10% of traces (default: 1.0)
  detailed_validation_logs: true   # log every validation failure detail (default: false)
  latency_slo: 50ms               # emit alert metric when p99 exceeds this
```

Placement: spec root (global) or operation level (override per route).

---

## 4. Metrics Catalog

All metrics use the `barbacane_` prefix. Labels never contain unbounded cardinality values (no full URLs, no request bodies).

### 4.1 Request metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_requests_total` | Counter | `method`, `path_template`, `status`, `api` | Total requests processed |
| `barbacane_request_duration_seconds` | Histogram | `method`, `path_template`, `status`, `api` | End-to-end request duration |
| `barbacane_request_size_bytes` | Histogram | `method`, `path_template`, `api` | Request body size |
| `barbacane_response_size_bytes` | Histogram | `method`, `path_template`, `api` | Response body size |

`path_template` uses the OpenAPI path with parameters (e.g. `/users/{id}`), not the actual path (`/users/123`). This bounds cardinality.

`api` is the `info.title` from the OpenAPI spec.

### 4.2 Connection metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_active_connections` | Gauge | — | Current open client connections |
| `barbacane_connections_total` | Counter | `protocol` | Total connections accepted (`h1`, `h2`) |

### 4.3 Validation metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_validation_failures_total` | Counter | `method`, `path_template`, `reason` | Validation rejection count |

`reason` values: `invalid_path_param`, `invalid_query_param`, `missing_required_param`, `invalid_body`, `invalid_content_type`, `missing_required_header`.

### 4.4 Middleware metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_middleware_duration_seconds` | Histogram | `middleware`, `phase` | Middleware execution time |
| `barbacane_middleware_short_circuits_total` | Counter | `middleware`, `status` | Middleware short-circuit count |

`phase`: `request` or `response`.

### 4.5 Dispatch metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_dispatch_duration_seconds` | Histogram | `dispatcher`, `upstream` | Dispatch call duration |
| `barbacane_dispatch_errors_total` | Counter | `dispatcher`, `upstream`, `reason` | Dispatch failures |

`reason` values: `timeout`, `connection_refused`, `circuit_open`, `plugin_error`.

`upstream` is the host portion of the upstream URL (e.g. `user-service:3000`), not the full URL.

### 4.6 WASM metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_wasm_execution_duration_seconds` | Histogram | `plugin`, `function` | WASM call duration |
| `barbacane_wasm_traps_total` | Counter | `plugin`, `function`, `reason` | WASM traps (panics, timeouts, OOM) |
| `barbacane_wasm_instances_active` | Gauge | `plugin` | Active WASM instances in pool |

`function`: `init`, `on_request`, `on_response`, `dispatch`.
`reason`: `panic`, `timeout`, `oom`.

### 4.7 Deprecation metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_deprecated_route_requests_total` | Counter | `method`, `path_template`, `api` | Requests to deprecated operations |

### 4.8 SLO metrics

When `x-barbacane-observability.latency_slo` is configured:

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_slo_violation_total` | Counter | `method`, `path_template`, `api` | Requests exceeding the latency SLO |

### 4.9 Plugin-emitted metrics

Plugins emit custom metrics via host functions (SPEC-003 section 4.8). These are auto-prefixed:

```
barbacane_plugin_<plugin_name>_<metric_name>
```

Example: plugin `jwt-auth` emits `tokens_validated` → `barbacane_plugin_jwt_auth_tokens_validated`.

### 4.10 Histogram buckets

All duration histograms use these bucket boundaries (seconds):

```
[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
```

All size histograms use these bucket boundaries (bytes):

```
[64, 256, 1024, 4096, 16384, 65536, 262144, 1048576, 4194304]
```

---

## 5. Traces

### 5.1 Trace context propagation

Barbacane propagates W3C Trace Context (`traceparent` / `tracestate` headers) as defined in the W3C Trace Context specification.

- If the incoming request has a `traceparent` header, the gateway joins that trace.
- If no `traceparent` is present, the gateway starts a new trace.
- The `traceparent` header is forwarded to upstreams with an updated `parent-id`.

### 5.2 Span structure

Every request produces the following span tree:

```
barbacane.request                           (root span)
├── barbacane.routing                       (trie lookup)
├── barbacane.validation                    (schema validation)
│   ├── barbacane.validation.path_params
│   ├── barbacane.validation.query_params
│   ├── barbacane.validation.headers
│   └── barbacane.validation.body
├── barbacane.middleware.<name>.request      (per middleware, request phase)
├── barbacane.dispatch.<name>               (dispatcher call)
│   └── barbacane.upstream_call             (actual HTTP/Kafka/NATS call)
└── barbacane.middleware.<name>.response     (per middleware, response phase, reverse order)
```

### 5.3 Span attributes

**`barbacane.request` (root):**

| Attribute | Type | Description |
|-----------|------|-------------|
| `http.method` | string | HTTP method |
| `http.url` | string | Full request URL |
| `http.route` | string | OpenAPI path template (e.g. `/users/{id}`) |
| `http.status_code` | int | Response status code |
| `http.request_content_length` | int | Request body size |
| `http.response_content_length` | int | Response body size |
| `net.peer.ip` | string | Client IP |
| `barbacane.api` | string | API name from spec `info.title` |
| `barbacane.artifact` | string | Artifact SHA-256 (first 12 chars) |

**`barbacane.validation` (on failure):**

| Attribute | Type | Description |
|-----------|------|-------------|
| `barbacane.validation.reason` | string | Failure reason code |
| `barbacane.validation.field` | string | Field that failed (dev mode only) |

**`barbacane.middleware.<name>.request`:**

| Attribute | Type | Description |
|-----------|------|-------------|
| `barbacane.middleware.action` | string | `continue` or `short_circuit` |
| `barbacane.middleware.short_circuit_status` | int | Status code (if short-circuited) |

**`barbacane.dispatch.<name>`:**

| Attribute | Type | Description |
|-----------|------|-------------|
| `barbacane.dispatch.upstream` | string | Upstream host |
| `barbacane.dispatch.status` | int | Upstream response status |

### 5.4 Sampling

Trace sampling is configurable:

- **Global default:** 100% (`trace_sampling: 1.0`)
- **Per-spec override:** `x-barbacane-observability.trace_sampling`
- **Per-operation override:** same field at operation level

Sampling decision is made at the root span and propagated to all child spans.

Health and metrics endpoint requests (`/__barbacane/*`) are never traced.

---

## 6. Logs

### 6.1 Format

All logs are structured JSON, emitted to stdout:

```json
{
  "timestamp": "2026-01-28T10:30:00.123Z",
  "level": "warn",
  "target": "barbacane::validation",
  "trace_id": "abc123def456",
  "span_id": "789012",
  "request_id": "550e8400-e29b-41d4-a716-446655440000",
  "message": "validation_failure",
  "fields": {
    "method": "POST",
    "path": "/users",
    "reason": "missing_required_field",
    "field": "email"
  }
}
```

### 6.2 Standard fields

Every log entry includes:

| Field | Type | Description |
|-------|------|-------------|
| `timestamp` | string | ISO-8601 UTC with milliseconds |
| `level` | string | `error`, `warn`, `info`, `debug`, `trace` |
| `target` | string | Rust module path (e.g. `barbacane::routing`) |
| `trace_id` | string | W3C trace ID (if in request context) |
| `span_id` | string | Current span ID |
| `request_id` | string | UUID v4 (matches `X-Request-Id` response header) |
| `message` | string | Log message |
| `fields` | object | Structured key-value data |

### 6.3 Log events

| Event | Level | When |
|-------|-------|------|
| `startup` | `info` | Data plane starts |
| `artifact_loaded` | `info` | Artifact loaded and verified |
| `plugin_initialized` | `info` | Plugin `init` completed |
| `listening` | `info` | Server bound to address |
| `shutdown` | `info` | Graceful shutdown started |
| `request_completed` | `info` | Request fully processed (includes method, path, status, duration) |
| `validation_failure` | `warn` | Request rejected by validation |
| `middleware_short_circuit` | `warn` | Middleware short-circuited (includes plugin name, status) |
| `dispatch_error` | `error` | Dispatch failed (includes dispatcher, reason) |
| `wasm_trap` | `error` | WASM plugin trapped (includes plugin, function, reason) |
| `secret_refresh_failed` | `warn` | Secret periodic refresh failed (retaining previous value) |
| `otlp_export_failed` | `warn` | Telemetry export to collector failed |

### 6.4 Log levels

| Level | What gets logged |
|-------|-----------------|
| `error` | Failures that affect request processing (plugin crashes, dispatch failures) |
| `warn` | Degraded conditions (validation rejections, secret refresh failures, telemetry export failures) |
| `info` | Normal operation events (startup, shutdown, requests) |
| `debug` | Detailed request processing (routing decisions, middleware chain steps) |
| `trace` | WASM-level detail (host function calls, memory operations) |

Default: `info`. Configurable via `--log-level`.

### 6.5 Plugin logs

Logs emitted by plugins via the `host_log` host function are output with:

```json
{
  "target": "barbacane::plugin::jwt-auth",
  "message": "<plugin message>",
  ...
}
```

Plugin logs inherit the request's `trace_id`, `span_id`, and `request_id`.

---

## 7. Prometheus Endpoint

The data plane exposes a Prometheus-compatible metrics scrape endpoint:

```
GET /__barbacane/metrics
```

Format: Prometheus text exposition format (OpenMetrics compatible).

This endpoint is always available, regardless of whether OTLP export is configured. It bypasses the request pipeline (no routing, validation, or middlewares).
