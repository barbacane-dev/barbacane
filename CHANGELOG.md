# Changelog

All notable changes to Barbacane are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### AsyncAPI & Event Dispatch (M10)
- AsyncAPI 3.x parser with channels, operations, messages, and protocol bindings
- Channel parameters with templated addresses (e.g., `notifications/{userId}`)
- Operation actions: `send` (gateway publishes) and `receive` (gateway subscribes)
- Protocol bindings extraction for Kafka, NATS, MQTT, AMQP, WebSocket
- Message schema validation for AsyncAPI payloads
- Host functions: `host_kafka_publish`, `host_nats_publish`
- `kafka` dispatcher plugin with topic routing, key expressions, and header forwarding
- `nats` dispatcher plugin with subject routing
- Sync-to-async bridge: HTTP request in, broker publish out, 202 ack response

#### Data Plane Connection (M12)
- WebSocket-based connection between data planes and control plane
- Connected mode for centralized fleet management (`--control-plane` flag)
- API key authentication for data plane connections (`--api-key` flag)
- Data plane registration and heartbeat protocol (30-second intervals)
- Artifact deployment notifications to connected data planes
- Deploy tab in UI showing connected data planes
- API key management (create, list, revoke) in Deploy tab
- One-click deployment to all connected data planes
- REST endpoints for data plane management:
  - `GET /projects/{id}/data-planes` — list connected data planes
  - `GET /projects/{id}/data-planes/{dpId}` — get data plane details
  - `DELETE /projects/{id}/data-planes/{dpId}` — disconnect data plane
  - `POST /projects/{id}/deploy` — deploy artifact to connected data planes
  - `POST /projects/{id}/api-keys` — create API key
  - `GET /projects/{id}/api-keys` — list API keys
  - `DELETE /projects/{id}/api-keys/{keyId}` — revoke API key
  - `WS /ws/data-plane` — WebSocket endpoint for data plane connections
- Graceful degradation: data planes continue serving if control plane unavailable
- Reconnection with exponential backoff (1s to 60s max)

#### Web UI Improvements
- Added JSON Schema for CORS plugin configuration
- Improved plugin deletion error handling with user-friendly messages
- Plugin configuration forms now auto-generate from JSON Schema
- Real-time validation of plugin configurations

#### Documentation
- New Web UI guide (`docs/guide/web-ui.md`)
- Updated Control Plane guide with Projects, Data Planes, Deploy sections
- Updated Development guide with Makefile targets and UI setup
- Updated Dispatchers guide with AsyncAPI support
- Updated CLI Reference with `seed-plugins` command documentation
- Added Interactive API Documentation section (Scalar at `/api/docs`)

#### Other
- HTTP/2 support with automatic protocol detection via ALPN
- API lifecycle support with `deprecated` flag and `x-barbacane-sunset` extension
- `Deprecation` and `Sunset` response headers for deprecated routes
- Fixture-based test specs for comprehensive integration testing
- SLO violation metrics (`barbacane_slo_violation_total`)
- Deprecation metrics (`barbacane_deprecated_route_requests_total`)

### Changed
- Improved test fixtures with more comprehensive scenarios
- Improved foreign key error handling in control plane API
- Plugin deletion now returns "resource is in use" error when referenced by projects

### Fixed
- CORS plugin now includes JSON Schema (`config-schema.json`) for UI configuration
- Plugin deletion errors now display user-friendly messages in the UI

## [0.1.0] - 2026-01-28

### Added

#### Core Gateway (M1)
- OpenAPI 3.x parser with `x-barbacane-*` extension support
- Prefix trie router with O(path length) lookups
- `.bca` artifact format (tar.gz with manifest, routes, specs, plugins)
- `barbacane compile` CLI command
- `barbacane validate` CLI command
- `barbacane serve` data plane binary
- Path parameter extraction and matching
- 404/405 responses with RFC 9457 format
- Health endpoint at `GET /__barbacane/health`
- Path normalization (trailing slashes, double slashes)

#### Request Validation (M2)
- JSON Schema validation for request bodies
- Path, query, and header parameter validation
- Content-Type enforcement
- Request limits (body size, header count/size, URI length)
- Format validation (date-time, email, uuid, uri, ipv4, ipv6)
- Development mode with verbose error details
- Compiler validation codes E1001-E1024

#### WASM Plugin System (M3)
- Wasmtime 28 integration with AOT compilation
- Instance pooling per (plugin, config) pair
- Plugin manifest (`plugin.toml`) format
- Config schema validation (`config-schema.json`)
- Middleware chain with request/response phases
- Short-circuit support for early responses
- Resource limits: 16 MB memory, 100ms timeout, 1 MB stack
- Host functions:
  - `host_log` - structured logging
  - `host_context_get/set` - per-request context
  - `host_clock_now` - monotonic time
  - `host_set_output` - plugin output
- Plugin SDK with `#[barbacane_middleware]` and `#[barbacane_dispatcher]` macros

#### Built-in Dispatchers (M4)
- `http-upstream` dispatcher - reverse proxy with path rewriting
- `mock` dispatcher - static responses from config
- `lambda` dispatcher - AWS Lambda invocation
- Connection pooling for upstream requests
- Circuit breaker with threshold/window config
- Upstream TLS with rustls
- Upstream mTLS support
- Host function: `host_http_call` / `host_http_read_result`
- Compiler check E1031 for plaintext HTTP upstreams

#### Plugin Manifest System (M5)
- `barbacane.yaml` project manifest
- Plugin sources: `path` (local file)
- Plugin reference extraction from specs
- Validation E1040 for undeclared plugins
- Artifact bundling of resolved WASM plugins
- Manifest embedding in artifacts

#### TLS & Authentication (M6a, M6b)
- TLS termination with rustls
- ALPN negotiation (HTTP/1.1, HTTP/2)
- `jwt-auth` middleware - RS256/ES256 JWT validation
- Claims validation (iss, aud, exp, nbf)
- `apikey-auth` middleware - API key from header/query
- `oauth2-auth` middleware - RFC 7662 token introspection
- Auth context headers (x-auth-sub, x-auth-claims, etc.)

#### Secrets Management (M6c)
- Secret references: `env://VAR_NAME`, `file:///path`
- Resolution at startup with exit code 13 on failure
- Host function: `host_get_secret` / `host_secret_read_result`

#### Rate Limiting & Caching (M7)
- `rate-limit` middleware - sliding window algorithm
- IETF draft alignment (RateLimit-Policy, RateLimit headers)
- Partition keys: client_ip, header, context
- `cache` middleware - in-memory response caching
- Cache key: path + method + vary headers

#### Observability (M8)
- Structured JSON logging via tracing
- Prometheus metrics endpoint at `/__barbacane/metrics`
- Request metrics: total, duration, sizes
- Connection metrics: active, total
- Validation failure metrics
- Middleware/dispatch duration metrics
- WASM execution metrics
- Distributed tracing with W3C Trace Context
- OTLP export to OpenTelemetry Collector
- Plugin telemetry host functions
- `x-barbacane-observability` extension

#### Control Plane (M9)
- `barbacane-control serve` REST API server
- PostgreSQL database with auto-migrations
- Specs API: upload, list, retrieve, delete, history
- Compilation API: async compilation with job queue
- Artifacts API: list, download, delete
- Plugins API: register, list, download, delete
- OpenAPI specification for control plane

#### Production Readiness (M11)
- Graceful shutdown with configurable timeout
- HTTP keep-alive with configurable idle timeout
- `X-Request-Id` header (UUID v4, propagates incoming)
- `X-Trace-Id` header (from traceparent or generated)
- `Server` header with version
- Artifact checksum verification (SHA-256)
- Startup exit codes 10-15 for failure categories
- Multiple specs in one artifact
- Routing conflict detection across specs
- `barbacane-test` crate with `TestGateway`
- CI/CD pipeline with fmt, clippy, audit, tests

### Infrastructure
- Cargo workspace with 14 crates
- 17 Architecture Decision Records (ADRs)
- 7 formal specifications
- Comprehensive documentation
- GitHub Actions CI

[Unreleased]: https://github.com/barbacane/barbacane/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/barbacane/barbacane/releases/tag/v0.1.0
