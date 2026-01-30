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

## M5 — Plugin Manifest System

Implement the `barbacane.yaml` manifest for explicit plugin configuration (ADR-0006). No "magic" built-in plugins — everything must be declared.

**Specs:** ADR-0006

### Manifest Parser
- [ ] `barbacane.yaml` schema definition — `plugins` section with name → source mapping
- [ ] Plugin source types — `path` (local file), `url` (HTTPS remote)
- [ ] Manifest parser — load and validate `barbacane.yaml`
- [ ] Plugin resolver — fetch from path or URL, validate `.wasm` format

### Compiler Integration
- [ ] `--manifest` CLI flag — path to manifest file (default: `./barbacane.yaml`)
- [ ] Plugin reference extraction — collect all plugin names from spec (`x-barbacane-dispatch`, `x-barbacane-middlewares`)
- [ ] Validation E1040 — plugin used in spec but not declared in manifest
- [ ] Artifact bundling — copy resolved `.wasm` files into `plugins/` directory of `.bca`
- [ ] Manifest embedding — include resolved manifest in artifact for reproducibility

### Data Plane
- [ ] Remove embedded plugins — no more `include_bytes!` in binary
- [ ] Load plugins from artifact — read `.wasm` from `plugins/` directory in `.bca`
- [ ] Bare binary validation — fail if spec uses plugin not in artifact

### CLI & Templates
- [ ] `barbacane init --template basic` — create project with `barbacane.yaml`, `plugins/`, example spec
- [ ] `barbacane init --template minimal` — create minimal project skeleton
- [ ] Plugin download — fetch official plugins from release URLs

### Testing
- [ ] Update all test fixtures — add `barbacane.yaml` to each fixture directory
- [ ] Integration tests — compile with manifest, verify plugin resolution
- [ ] Error tests — E1040 for undeclared plugins

---

## M6a — TLS & JWT Auth

HTTPS termination and JWT authentication — the most common production security setup.

**Specs:** SPEC-004 (partial)

### TLS Termination
- [x] TLS termination — rustls ingress, cert/key from file paths
- [x] TLS settings — TLS 1.2 min, 1.3 preferred, modern cipher suites
- [x] ALPN — HTTP/1.1 and HTTP/2 negotiation
- [x] `--tls-cert` and `--tls-key` CLI flags
- [ ] `--tls-config` for advanced settings (min version, cipher suites)

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
- [ ] Security defaults — strict validation enabled by default
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

## M7 — Rate Limiting & Caching

Built-in rate limiting aligned with draft-ietf-httpapi-ratelimit-headers, and response caching.

**Specs:** SPEC-001 (section 3.3, 3.4), SPEC-002 (section 4.9)

- [ ] `rate-limit` middleware plugin — token bucket / sliding window implementation
- [ ] IETF draft alignment — `quota`, `window`, `quota_unit`, `policy_name` config
- [ ] `RateLimit-Policy` response header — on every response
- [ ] `RateLimit` response header — remaining quota and reset time
- [ ] `Retry-After` header — on 429 responses
- [ ] Partition key support — `client_ip`, `header:<name>`, `context:<key>`
- [ ] `x-barbacane-ratelimit` sugar — compiler transforms into middleware chain entry
- [ ] Compiler validation — E1012 (missing quota/window), E1013 (invalid quota_unit)
- [ ] `cache` middleware plugin — in-memory response caching
- [ ] Cache key — path + method + `vary` headers
- [ ] `x-barbacane-cache` sugar — compiler transforms into middleware chain entry
- [ ] Integration tests — rate limiting (allow, block, reset), cache hit/miss

---

## M8 — Observability

Metrics, traces, structured logs, and OpenTelemetry export.

**Specs:** SPEC-005

- [ ] Structured logging — JSON to stdout, timestamp/level/target/trace_id/span_id/request_id
- [ ] Log events — startup, artifact_loaded, request_completed, validation_failure, wasm_trap, etc.
- [ ] `--log-level` flag
- [ ] Request metrics — `barbacane_requests_total`, `barbacane_request_duration_seconds`, sizes
- [ ] Connection metrics — `barbacane_active_connections`, `barbacane_connections_total`
- [ ] Validation metrics — `barbacane_validation_failures_total`
- [ ] Middleware metrics — `barbacane_middleware_duration_seconds`, `barbacane_middleware_short_circuits_total`
- [ ] Dispatch metrics — `barbacane_dispatch_duration_seconds`, `barbacane_dispatch_errors_total`
- [ ] WASM metrics — `barbacane_wasm_execution_duration_seconds`, `barbacane_wasm_traps_total`
- [ ] Deprecation metrics — `barbacane_deprecated_route_requests_total`
- [ ] SLO metrics — `barbacane_slo_violation_total` (when `latency_slo` configured)
- [ ] Prometheus endpoint — `GET /__barbacane/metrics`, text exposition format
- [ ] Histogram buckets — duration and size bucket definitions
- [ ] Distributed tracing — W3C Trace Context (`traceparent` / `tracestate`) propagation
- [ ] Span tree — `barbacane.request` → routing → validation → middleware → dispatch → response
- [ ] Span attributes — method, route, status, API name, artifact hash
- [ ] Trace sampling — global + per-spec + per-operation `trace_sampling` config
- [ ] OTLP export — gRPC/HTTP push to OpenTelemetry Collector
- [ ] `--otlp-endpoint` flag
- [ ] Plugin telemetry host functions — `host_metric_counter_inc`, `host_metric_histogram_observe`, `host_span_start/end/set_attribute`
- [ ] Plugin metric auto-prefix — `barbacane_plugin_<name>_<metric>`
- [ ] `x-barbacane-observability` extension — trace_sampling, detailed_validation_logs, latency_slo
- [ ] Fire-and-forget — telemetry export never blocks request processing
- [ ] Integration tests — metrics scrape, trace propagation, log correlation

---

## M9 — Control Plane

The management layer — REST API, database, spec/artifact/plugin lifecycle.

**Specs:** SPEC-006

- [ ] PostgreSQL schema — specs, spec_revisions, plugins, artifacts, artifact_specs, compilations
- [ ] Database migrations — setup and versioned migrations
- [ ] `barbacane-control serve` — REST API server
- [ ] `POST /specs` — upload and validate spec
- [ ] `GET /specs` — list specs
- [ ] `GET /specs/{id}` — get spec metadata + content
- [ ] `PUT /specs/{id}` — replace spec (new revision)
- [ ] `DELETE /specs/{id}` — delete spec and artifacts
- [ ] `GET /specs/{id}/history` — list spec revisions
- [ ] `POST /specs/{id}/compile` — async compilation
- [ ] `GET /compilations/{id}` — poll compilation status
- [ ] `GET /artifacts` — list artifacts
- [ ] `GET /artifacts/{id}` — artifact metadata + manifest
- [ ] `GET /artifacts/{id}/download` — download `.bca` file
- [ ] `DELETE /artifacts/{id}` — delete artifact
- [ ] `POST /plugins` — register plugin (manifest + wasm + schema)
- [ ] `GET /plugins` — list plugins (filter by type/name)
- [ ] `GET /plugins/{name}` — list plugin versions
- [ ] `GET /plugins/{name}/{version}` — plugin metadata + config schema
- [ ] `DELETE /plugins/{name}/{version}` — delete plugin (409 if referenced)
- [ ] `GET /health` — database connectivity check
- [ ] API versioning — `Accept: application/vnd.barbacane.v1+json`
- [ ] Error responses — RFC 9457 for all API errors
- [ ] CLI: `barbacane-control spec upload/list/show/delete/history`
- [ ] CLI: `barbacane-control artifact list/download/inspect`
- [ ] CLI: `barbacane-control plugin register/list/show/delete`
- [ ] Remote compilation — `barbacane-control compile --spec-id`
- [ ] Integration tests — full API lifecycle (upload → compile → download → inspect)

---

## M10 — AsyncAPI & Event Dispatch

Event-driven API support — AsyncAPI parsing, Kafka and NATS dispatchers.

**Specs:** SPEC-001 (section 2.1), SPEC-003 (section 4.6)

- [ ] AsyncAPI 3.x parser — read channels, servers, messages, bindings
- [ ] Channel routing — topic-to-handler mapping in `routes.fb`
- [ ] Message schema validation — AsyncAPI message schemas
- [ ] Host function: `host_kafka_publish` — publish to Kafka topic
- [ ] `kafka` dispatcher plugin — brokers, topic, key, ack-response config
- [ ] Host function: `host_nats_publish` — publish to NATS subject
- [ ] `nats` dispatcher plugin — servers, subject config
- [ ] Sync-to-async bridge — HTTP request in, broker publish out, 202 ack response
- [ ] Integration tests — AsyncAPI compilation, Kafka/NATS dispatch (with mock brokers)

---

## M11 — Production Readiness

Performance, testing infrastructure, lifecycle features, and hardening.

**Specs:** SPEC-002 (sections 6, 7), SPEC-007

- [ ] Graceful shutdown — SIGTERM handling, drain in-flight requests (30s), force-close
- [ ] HTTP/2 — ALPN negotiation, max concurrent streams, window size, frame size
- [ ] HTTP keep-alive — idle timeout (60s)
- [ ] `X-Request-Id` header — UUID v4 on every response
- [ ] `X-Trace-Id` header — trace ID on every response
- [ ] `Server` header — `barbacane/<version>`, strip upstream `Server`
- [ ] API lifecycle — `deprecated: true` support, `x-barbacane-sunset` header
- [ ] `barbacane-test` crate — `TestGateway`, `PluginHarness`, `SpecBuilder`, `RequestBuilder`
- [ ] Fixture specs — minimal, full-crud, async-kafka, multi-spec, invalid-* specs
- [ ] Performance benchmarks — criterion suite (routing, validation, WASM, full pipeline)
- [ ] CI/CD pipeline — fmt, clippy, test, bench, build, integration test
- [ ] Benchmark regression check — fail CI on >10% regression
- [ ] Artifact checksum verification — SHA-256 check at data plane startup (exit code 11)
- [ ] Startup exit codes — 10–15 for each failure category
- [ ] Multiple specs in one artifact — `barbacane-control compile --specs a.yaml b.yaml`
- [ ] Routing conflict detection — E1010 across specs
