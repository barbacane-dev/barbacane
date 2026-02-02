# Milestones & Stories

Each milestone produces a testable increment. Stories are ordered within a milestone — later ones may depend on earlier ones.

---

## M1 — Compile and Route ✅

The minimum viable loop: parse an OpenAPI spec, compile it into an artifact, load it in the data plane, and route requests to a built-in mock response. No WASM, no validation, no auth — just prove the pipeline works end to end.

**Specs:** SPEC-001 (partial), SPEC-002 (partial)

- [x] Set up Rust workspace (`barbacane`, `barbacane-control`, `barbacane-plugin-sdk`)
- [x] OpenAPI 3.x parser — read YAML/JSON, extract `paths`, `servers`, `x-barbacane-*` extensions
- [x] Routing trie — compile `paths` into a prefix trie with static/param segments and method sets
- [x] Artifact format — produce a `.bca` archive with `manifest.json` and `routes.json`
- [x] `barbacane compile` CLI — accepts `--spec`, outputs `.bca`
- [x] Data plane binary — loads `.bca`, binds to port
- [x] Request routing — trie lookup, path parameter capture, method matching
- [x] Mock dispatcher — hardcoded in the data plane (not yet a plugin), returns the config body
- [x] `x-barbacane-dispatch` extraction — read dispatcher name + config from spec
- [x] 404 and 405 responses — `route-not-found`, `method-not-allowed` with RFC 9457 format
- [x] Health endpoint — `GET /__barbacane/health`
- [x] Path normalization — strip trailing slashes, collapse double slashes
- [x] Integration test — compile a fixture spec, boot a `TestGateway`, send requests, assert status codes

---

## M2 — Request Validation ✅

The gateway enforces the spec. Requests that don't conform are rejected before reaching any dispatcher.

**Specs:** SPEC-001 (section 8), SPEC-002 (section 4.5)

- [x] JSON Schema compilation — precompile schemas at gateway startup using `jsonschema` crate
- [x] Path parameter validation — type constraints (as strings with pattern validation)
- [x] Query parameter validation — required check, schema validation
- [x] Header validation — required headers, schema validation
- [x] Content-Type check — reject if not in `requestBody.content`
- [x] Request body validation — JSON Schema validation against matched content type
- [x] Fail-fast behavior — stop at first failure category, return 400
- [x] Error model — RFC 9457 responses with `urn:barbacane:error:*` URNs
- [x] Development mode — `--dev` flag, verbose error details (field, location, reason)
- [x] Request limits — max body size, header count/size, URI length
- [x] `format` validation — date-time, email, uuid, uri, ipv4, ipv6
- [x] `barbacane validate` CLI — quick spec validation without plugin resolution
- [x] Compiler validation — E1001–E1004 (spec validity), E1010–E1015 (extension validity)
- [x] Integration tests — validation acceptance/rejection for each constraint type

---

## M3 — WASM Plugin System ✅

The extensibility layer. Plugins are loaded as WASM modules with sandboxed execution, host functions, and context passing.

**Specs:** SPEC-003

### Runtime Core (`barbacane-wasm` crate)
- [x] wasmtime integration — load `.wasm` modules, AOT compile, instance pooling
- [x] Plugin manifest — parse `plugin.toml` (name, version, type, capabilities, wasm path)
- [x] Plugin config schema — load `config-schema.json`, validate spec config blocks against it (E1023)
- [x] WASM export contract — `init`, `on_request`, `on_response` for middlewares
- [x] WASM export contract — `dispatch` for dispatchers
- [x] Plugin instance model — separate instance per (name, config) pair
- [x] Memory limits — 16 MB linear memory, 100ms execution timeout, 1 MB stack
- [x] Error handling — traps produce 500, response-phase traps are fault-tolerant

### Host Functions
- [x] Host function: `host_set_output` — plugin writes results to host buffer
- [x] Host function: `host_log` — structured logging from plugins
- [x] Host function: `host_context_get` / `host_context_set` — per-request context map
- [x] Host function: `host_clock_now` — monotonic clock
- [x] Capability enforcement — reject imports not declared in `plugin.toml`

### Middleware Chain
- [x] Middleware chain execution — ordered `on_request` calls, reverse `on_response` calls
- [x] Short-circuit support — middleware returns 1, chain stops, response returned
- [x] Per-operation chain resolution — global chain + per-route overrides (replace, not merge)

### Plugin SDK (`barbacane-plugin-macros` crate)
- [x] `barbacane-plugin-sdk` crate — `Request`, `Response`, `Action` types, serde glue
- [x] `#[barbacane_middleware]` macro — generates init/on_request/on_response exports
- [x] `#[barbacane_dispatcher]` macro — generates init/dispatch exports

### CLI & Bundling
- [x] Plugin version resolution — `name`, `name@1.0.0`, `name@^1.0.0`
- [x] Artifact bundling — copy `.wasm` files into `plugins/` directory of `.bca`
- [ ] `barbacane-control plugin register` CLI — validate and store plugin in registry (deferred to M9)
- [ ] Compiler: plugin resolution — E1020–E1024 checks (deferred to M9)
- [ ] Integration tests — middleware chain with real WASM plugins (requires M4 for http-upstream)

---

## M4 — Built-in Dispatchers ✅

Move dispatchers from hardcoded to WASM plugins. Add real HTTP upstream proxying.

**Specs:** SPEC-002 (section 4.7), SPEC-004 (section 3)

### HTTP Client Infrastructure
- [x] Connection pooling — reuse connections to the same upstream host (reqwest)
- [x] Upstream TLS — rustls for egress, system CA roots by default
- [x] Circuit breaker — `threshold` and `window` config, 503 when open
- [x] Timeouts — per-dispatch `timeout` config
- [x] Dispatch error responses — 502, 503, 504 with RFC 9457 format

### Host Functions
- [x] Host function: `host_http_call` / `host_http_read_result` — outbound HTTP requests

### Dispatchers
- [x] `http-upstream` dispatcher — reverse proxy (built-in, uses `url`, `path`, `timeout` config)
- [x] `mock` dispatcher — static response from config (WASM plugin)
- [x] `lambda` dispatcher — invoke AWS Lambda via Lambda Function URLs (WASM plugin)

### Compiler & CLI
- [x] Compiler check E1031 — reject `http://` upstream URLs in production mode
- [x] `--allow-plaintext-upstream` flag — dev only

### Remaining
- [x] Upstream mTLS — `tls.client_cert`, `tls.client_key`, `tls.ca` config
- [x] Integration tests — upstream proxying (httpbin.org tests)

---

## M5 — Plugin Manifest System ✅

Implement the `barbacane.yaml` manifest for explicit plugin configuration (ADR-0006). No "magic" built-in plugins — everything must be declared.

**Specs:** ADR-0006

### Manifest Parser
- [x] `barbacane.yaml` schema definition — `plugins` section with name → source mapping
- [x] Plugin source types — `path` (local file), `url` (HTTPS remote)
- [x] Manifest parser — load and validate `barbacane.yaml`
- [x] Plugin resolver — fetch from path or URL, validate `.wasm` format

### Compiler Integration
- [x] `--manifest` CLI flag — path to manifest file (default: `./barbacane.yaml`)
- [x] Plugin reference extraction — collect all plugin names from spec (`x-barbacane-dispatch`, `x-barbacane-middlewares`)
- [x] Validation E1040 — plugin used in spec but not declared in manifest
- [x] Artifact bundling — copy resolved `.wasm` files into `plugins/` directory of `.bca`
- [x] Manifest embedding — include resolved manifest in artifact for reproducibility

### Data Plane
- [x] Remove embedded plugins — no more `include_bytes!` in binary
- [x] Load plugins from artifact — read `.wasm` from `plugins/` directory in `.bca`
- [x] Bare binary validation — fail if spec uses plugin not in artifact

### CLI & Templates
- [x] `barbacane init --template basic` — create project with `barbacane.yaml`, `plugins/`, example spec
- [x] `barbacane init --template minimal` — create minimal project skeleton
- [x] Plugin download — `barbacane init --fetch-plugins` fetches from GitHub releases

### Testing
- [x] Update all test fixtures — add `barbacane.yaml` to each fixture directory
- [x] Integration tests — compile with manifest, verify plugin resolution
- [x] Error tests — E1040 for undeclared plugins

---

## M6a — TLS & JWT Auth

HTTPS termination and JWT authentication — the most common production security setup.

**Specs:** SPEC-004 (partial)

### TLS Termination
- [x] TLS termination — rustls ingress, cert/key from file paths
- [x] TLS settings — TLS 1.2 min, 1.3 preferred, modern cipher suites
- [x] ALPN — HTTP/1.1 and HTTP/2 negotiation
- [x] `--tls-cert` and `--tls-key` CLI flags
- [x] `--tls-min-version` flag for minimum TLS version (1.2 or 1.3)

### JWT Authentication
- [x] `jwt-auth` middleware plugin — RS256/ES256 token validation (signature validation scaffolded)
- [ ] JWKS fetch — load public keys from `jwks_uri` (deferred)
- [ ] JWKS caching — configurable refresh interval, retain previous on failure (deferred)
- [x] Token extraction — `Authorization: Bearer` header
- [x] Claims validation — `iss`, `aud`, `exp`, `nbf` checks
- [x] Context output — `x-auth-sub`, `x-auth-claims` headers to downstream
- [x] Auth rejection — 401 with `WWW-Authenticate` header
- [x] Middleware chain execution — on_request/on_response chain implemented
- [x] `host_get_unix_timestamp` host function — for token expiration validation

### Integration
- [x] Auth context convention — `x-auth-*` headers for downstream
- [x] Security defaults — security headers enabled by default (X-Content-Type-Options, X-Frame-Options)
- [x] Integration tests — valid/invalid JWT, expired token, wrong audience, missing token, malformed token

---

## M6b — API Key & OAuth2 Auth

Additional authentication methods for diverse integration patterns.

**Specs:** SPEC-004 (partial)

### API Key Authentication
- [x] `apikey-auth` middleware plugin — API key validation
- [x] Key extraction — header (`X-API-Key`), query param, or custom location
- [x] Key store — in-memory map loaded from config
- [x] Context output — `x-auth-key-id`, `x-auth-key-name`, `x-auth-key-scopes` headers

### OAuth2 Token Introspection
- [x] `oauth2-auth` middleware plugin — token introspection (RFC 7662)
- [x] Introspection endpoint — configurable URL
- [x] Client credentials — `client_id`, `client_secret` for introspection request
- [x] Required scopes — optional scope validation (space-separated list)
- [x] Context headers — `x-auth-sub`, `x-auth-scope`, `x-auth-client-id`, `x-auth-username`, `x-auth-claims`
- [x] Auth rejection — 401 for invalid token, 403 for insufficient scope
- [ ] Token caching — cache active tokens to reduce introspection calls (future)

### Integration
- [x] Multiple auth methods — chain multiple auth middlewares (via middleware stacking)
- [x] Integration tests — API key validation (6 tests)
- [x] Integration tests — OAuth2 introspection (5 tests)

---

## M6c — OPA Authz & Secrets

Policy-based authorization and secrets management for enterprise deployments.

**Specs:** SPEC-004 (partial)

### OPA Authorization
- [ ] `opa-authz` middleware plugin — OPA policy evaluation (deferred)
- [ ] Policy format — WASM-compiled Rego policies (deferred)
- [ ] OPA input mapping — `input.request`, `input.context`, `input.headers` (deferred)
- [ ] Policy bundling — `.wasm` policies in `policies/` directory of artifact (deferred)
- [ ] Decision output — allow/deny based on policy result (deferred)
- [ ] Authz rejection — 403 with policy violation details (dev mode) (deferred)

### Secrets Management
- [x] Secret references — `env://VAR_NAME` for environment variables
- [x] Secret references — `file:///path/to/secret` for file-based secrets
- [x] Secret resolution at startup — fetch all, fail if any missing (exit code 13)
- [x] Host function: `host_get_secret` / `host_secret_read_result` — secret access from plugins
- [ ] Future: `vault://`, `aws-sm://`, `k8s://` references (deferred)

### Compiler Validation
- [ ] Compiler check E1032 — OpenAPI security scheme without matching auth middleware (deferred)
- [ ] Security audit mode — warn on common misconfigurations (deferred)

### Integration
- [x] Integration tests — secret resolution (env and file), missing secret error (exit code 13)
- [ ] Integration tests — OPA allow/deny (deferred)

---

## M7 — Rate Limiting & Caching ✅

Built-in rate limiting aligned with draft-ietf-httpapi-ratelimit-headers, and response caching.

**Specs:** SPEC-001 (section 3.3, 3.4), SPEC-002 (section 4.9)

- [x] `rate-limit` middleware plugin — sliding window implementation
- [x] IETF draft alignment — `quota`, `window`, `policy_name` config
- [x] `RateLimit-Policy` response header — on every response
- [x] `RateLimit` response header — remaining quota and reset time
- [x] `Retry-After` header — on 429 responses
- [x] Partition key support — `client_ip`, `header:<name>`, `context:<key>`
- [x] `cache` middleware plugin — in-memory response caching
- [x] Cache key — path + method + `vary` headers
- [x] Integration tests — rate limiting (allow, block, reset), cache hit/miss

---

## M8 — Observability ✅

Metrics, traces, structured logs, and OpenTelemetry export.

**Specs:** SPEC-005

### Telemetry Infrastructure (`barbacane-telemetry` crate)
- [x] Structured logging — JSON to stdout via tracing-subscriber
- [x] Log events — startup, artifact_loaded, request_completed, validation_failure, wasm_trap, etc.
- [x] `--log-level` and `--log-format` flags
- [x] Request metrics — `barbacane_requests_total`, `barbacane_request_duration_seconds`, sizes
- [x] Connection metrics — `barbacane_active_connections`, `barbacane_connections_total`
- [x] Validation metrics — `barbacane_validation_failures_total`
- [x] Middleware metrics — `barbacane_middleware_duration_seconds`, `barbacane_middleware_short_circuits_total`
- [x] Dispatch metrics — `barbacane_dispatch_duration_seconds`, `barbacane_dispatch_errors_total`
- [x] WASM metrics — `barbacane_wasm_execution_duration_seconds`, `barbacane_wasm_traps_total`
- [x] Deprecation metrics — `barbacane_deprecated_route_requests_total`
- [x] SLO metrics — `barbacane_slo_violation_total` (when `latency_slo` configured)
- [x] Prometheus endpoint — `GET /__barbacane/metrics`, text exposition format
- [x] Histogram buckets — duration and size bucket definitions
- [x] Distributed tracing — W3C Trace Context (`traceparent` / `tracestate`) extraction/injection
- [x] Span tree support — span names and attributes defined per ADR-0010
- [x] Trace sampling — configurable sampling rate
- [x] OTLP export — gRPC/HTTP push to OpenTelemetry Collector
- [x] `--otlp-endpoint` flag
- [x] Plugin telemetry host functions — `host_metric_counter_inc`, `host_metric_histogram_observe`, `host_span_start/end/set_attribute`
- [x] Plugin metric auto-prefix — `barbacane_plugin_<name>_<metric>`
- [x] `x-barbacane-observability` extension — trace_sampling, detailed_validation_logs, latency_slo_ms
- [x] Fire-and-forget — OTLP batch export with bounded queues

### Pipeline Integration
- [x] Request metrics recording — timing, sizes, status codes at each lifecycle point
- [x] Validation failure tracking — metrics with reason labels
- [x] Connection tracking — opened/closed connection metrics
- [x] Integration tests — Prometheus endpoint format, request counts, validation failures, 404s, connection tracking

---

## M9 — Control Plane ✅

The management layer — REST API, database, spec/artifact/plugin lifecycle.

**Specs:** SPEC-006

### Database & Migrations
- [x] PostgreSQL schema — specs, spec_revisions, plugins, artifacts, artifact_specs, compilations
- [x] Database migrations — setup and versioned migrations (auto-run on startup)

### REST API Server
- [x] `barbacane-control serve` — REST API server with Axum
- [x] `GET /health` — database connectivity check
- [x] Error responses — RFC 9457 for all API errors

### Specs API
- [x] `POST /specs` — upload and validate spec (multipart)
- [x] `GET /specs` — list specs (with type/name filters)
- [x] `GET /specs/{id}` — get spec metadata
- [x] `GET /specs/{id}/content` — download spec content (with revision query param)
- [x] `DELETE /specs/{id}` — delete spec and revisions
- [x] `GET /specs/{id}/history` — list spec revisions

### Compilation API
- [x] `POST /specs/{id}/compile` — async compilation (returns 202)
- [x] `GET /compilations/{id}` — poll compilation status
- [x] `GET /specs/{id}/compilations` — list compilations for spec
- [x] Background worker — async compilation with channel-based job queue

### Artifacts API
- [x] `GET /artifacts` — list artifacts
- [x] `GET /artifacts/{id}` — artifact metadata + manifest
- [x] `GET /artifacts/{id}/download` — download `.bca` file
- [x] `DELETE /artifacts/{id}` — delete artifact

### Plugins API
- [x] `POST /plugins` — register plugin (multipart: name, version, type, capabilities, schema, wasm)
- [x] `GET /plugins` — list plugins (filter by type/name)
- [x] `GET /plugins/{name}` — list plugin versions
- [x] `GET /plugins/{name}/{version}` — plugin metadata + config schema
- [x] `GET /plugins/{name}/{version}/download` — download WASM binary
- [x] `DELETE /plugins/{name}/{version}` — delete plugin

### Documentation
- [x] OpenAPI specification — `crates/barbacane-control/openapi.yaml`
- [x] Control Plane Guide — `docs/guide/control-plane.md`
- [x] CLI Reference — updated with `barbacane-control serve`

### Deferred
- [x] API versioning — `Content-Type: application/vnd.barbacane.v1+json` on all responses
- [ ] CLI subcommands — `barbacane-control spec/artifact/plugin` REST-based commands
- [ ] Integration tests — full API lifecycle (requires running PostgreSQL)

---

## M10 — AsyncAPI & Event Dispatch

Event-driven API support — AsyncAPI parsing, Kafka and NATS dispatchers.

**Specs:** SPEC-001 (section 2.1), SPEC-003 (section 4.6)

- [x] AsyncAPI 3.x parser — read channels, operations, messages, bindings
- [x] Message model — `Message` struct with payload, content type, protocol bindings
- [x] Channel parameters — templated addresses (e.g., `notifications/{userId}`)
- [x] Operation actions — `send` (gateway publishes) and `receive` (gateway subscribes)
- [x] Protocol bindings — Kafka, NATS, MQTT, AMQP, WebSocket binding extraction
- [x] Request body mapping — SEND operations create request body from message payload
- [x] Channel routing — AsyncAPI fields (messages, bindings) propagated to compiled operations
- [x] Message schema validation — AsyncAPI message payloads validated via request body schema
- [x] Host function: `host_kafka_publish` — publish to Kafka topic (mock broker for testing)
- [ ] `kafka` dispatcher plugin — brokers, topic, key, ack-response config
- [x] Host function: `host_nats_publish` — publish to NATS subject (mock broker for testing)
- [ ] `nats` dispatcher plugin — servers, subject config
- [ ] Sync-to-async bridge — HTTP request in, broker publish out, 202 ack response
- [ ] Integration tests — AsyncAPI compilation, Kafka/NATS dispatch (with mock brokers)

---

## M11 — Production Readiness ✅

Performance, testing infrastructure, lifecycle features, and hardening.

**Specs:** SPEC-002 (sections 6, 7), SPEC-007

### Server Hardening
- [x] Graceful shutdown — SIGTERM/SIGINT handling, drain in-flight requests with configurable timeout (--shutdown-timeout)
- [x] HTTP keep-alive — enabled with configurable idle timeout (--keepalive-timeout)
- [x] `X-Request-Id` header — UUID v4 on every response (propagates incoming header if present)
- [x] `X-Trace-Id` header — trace ID on every response (extracted from traceparent or generated)
- [x] `Server` header — `barbacane/<version>` on every response
- [x] HTTP/2 support — auto protocol detection via ALPN (HTTP/1.1 or HTTP/2)
- [x] API lifecycle — `deprecated: true` support, `x-barbacane-sunset` extension, `Deprecation` and `Sunset` response headers

### Testing Infrastructure
- [x] `barbacane-test` crate — `TestGateway`, `PluginHarness`, `SpecBuilder`, `RequestBuilder`
- [x] Fixture specs — minimal, full-crud, deprecated, multi-spec, invalid-* specs
- [x] Performance benchmarks — criterion suite (routing, validation)
- [x] Benchmark regression check — CI warns on performance regression

### CI/CD
- [x] CI/CD pipeline — fmt, clippy, audit, build, unit tests, integration tests (`.github/workflows/ci.yml`)

### Already Implemented (from previous milestones)
- [x] Artifact checksum verification — SHA-256 check at data plane startup (exit code 11)
- [x] Startup exit codes — 10–15 for each failure category (13 for secret resolution failure)
- [x] Multiple specs in one artifact — `barbacane compile --spec a.yaml --spec b.yaml`
- [x] Routing conflict detection — E1010 across specs
