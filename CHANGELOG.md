# Changelog

All notable changes to Barbacane are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### Response Transformer
- `response-transformer` middleware plugin — declarative response transformations before client delivery
  - Status code mapping: configurable mapping table (e.g., 200 → 201, 400 → 403)
  - Header transformations: add, set (if absent), remove, rename
  - JSON body transformations using JSON Pointer (RFC 6901): add, remove, rename fields

#### S3 Dispatcher
- `s3` dispatcher plugin — proxy requests to AWS S3 or any S3-compatible endpoint (MinIO, RustFS, Ceph) with AWS Signature Version 4 signing
- Virtual-hosted style (`{bucket}.s3.{region}.amazonaws.com`) and path-style URLs (`force_path_style`)
- Custom endpoint support for S3-compatible storage (always uses path-style)
- Single-bucket routes via `bucket` config field (e.g., `/assets/{key+}` CDN pattern)
- Multi-bucket routing via path parameters (`bucket_param`, `key_param`)
- Temporary credential support: `session_token` for STS / AssumeRole / IRSA
- `barbacane-sigv4` promoted from `plugins/sigv4` to a workspace crate (`crates/barbacane-sigv4`) for reuse by future plugins

#### Bot Detection Plugin

- `bot-detection` middleware plugin — block requests from known bots and scrapers by User-Agent pattern matching
  - `deny` list: block any UA containing the given substring (case-insensitive)
  - `allow` list: explicitly allow trusted crawlers (e.g. Googlebot), overrides deny
  - `block_empty_ua`: optionally reject requests with no User-Agent header
  - Configurable `status` and `message` for blocked responses
  - Returns `application/problem+json` with type `urn:barbacane:error:bot-detected`
  - 17 unit tests

#### Wildcard Path Parameters
- `{param+}` greedy path parameter syntax — captures all remaining segments including slashes
- Useful for S3 key routing (`/files/{bucket}/{key+}`), CDN paths, and any route with slash-separated sub-paths
- Enforced constraints: wildcard must be the last segment, at most one per path
- Precedence: static segments > regular params > wildcard param
- Wildcard values arrive in plugins as plain strings via `path_params`

#### Playground: S3 Object Storage
- Added RustFS (S3-compatible) service to the playground Docker Compose stack
- `/storage/{bucket}/{key+}` — OIDC-protected multi-bucket S3 proxy
- `/assets/{key+}` — public rate-limited CDN backed by `s3://assets`
- Added `playground.http` with ready-to-run requests for all playground endpoints

#### Web UI Improvements (Batch 1)
- Reusable components: `EmptyState`, `SearchInput`, `Breadcrumb`, `DropZone`
- `useDebounce` hook and shared time formatting utilities (`formatDate`, `formatRelativeTime`)
- Search and filtering on specs, plugins, and projects pages
- Breadcrumb navigation across all pages
- Drag-and-drop spec upload zones (empty state and persistent)
- Responsive sidebar with mobile close button
- On-demand spec compliance re-checking via `GET /specs/{id}/compliance`
- Compliance check button on spec cards (global and project pages)
- Build logs viewer with structured log display and level filtering
- Data plane health indicators with auto-refresh intervals

#### Web UI Improvements (Batch 2)
- Error boundaries: `RouteErrorBoundary` with React Router `errorElement` at root and project layout levels
- Confirmation dialogs: `ConfirmDialog` component and `useConfirm` hook replacing browser `confirm()` across 12 call sites
- Spec editor: `CodeBlock` component with `shiki` syntax highlighting for YAML/JSON in spec viewers
- Operations page: middleware chain preview showing resolved chain with correct merge semantics (inherited vs operation-level)
- Operations page: undo/redo support in edit dialogs (`useHistory` hook, keyboard shortcuts)
- E2E tests: Playwright setup with smoke navigation and spec workflow tests using API mocking via `page.route()`

#### CI
- UI unit tests job (Node 22, TypeScript build, vitest)
- UI E2E tests job (Playwright with Chromium, report upload on failure)

#### `$ref` Resolution in Spec Parser
- Local `$ref` pointers (`#/components/schemas/*`, `#/components/parameters/*`, etc.) are now resolved and inlined at parse time
- Applies to OpenAPI parameter schemas, request body schemas, and AsyncAPI message payloads
- Circular references produce a parse error (E1004) instead of causing infinite loops
- Unresolved references produce E1003 at parse time with a clear pointer to the missing component
- Users no longer need to pre-flatten specs with external tools before uploading

### Fixed

- Short-circuit middleware responses now correctly run `on_response` for preceding middlewares in reverse order via `execute_on_response_partial`
- `GET /projects/{id}/data-planes` now returns 404 when the project does not exist (previously returned 200 with an empty array)
- Invalid plugin configs in playground specs: `correlation-id` used `header` instead of `header_name`, `cache` used unsupported `stale_if_error` property

#### Request Transformer
- `request-transformer` middleware plugin — declarative request transformations before upstream dispatch
  - Header transformations: add, set (if absent), remove, rename
  - Query parameter transformations: add, remove, rename
  - Path rewriting: strip prefix, add prefix, regex replace with capture groups
  - JSON body transformations using JSON Pointer (RFC 6901): add, remove, rename fields
  - Variable interpolation: `$client_ip`, `$header.*`, `$query.*`, `$path.*`, `context:*`
  - Snapshot-based interpolation: variables resolve against the original request, unaffected by prior transforms
  - Lazy-compiled regex for path replace (compiled once, reused across requests)

## [0.1.2] - 2026-02-14

### Added

#### OIDC Authentication
- `oidc-auth` middleware plugin — OpenID Connect authentication with automatic JWKS discovery
  - OIDC Discovery (`/.well-known/openid-configuration`) and JWKS endpoint fetching via `host_http_call`
  - JWT parsing with base64url-encoded header, payload, and signature
  - Algorithm validation (RS256/RS384/RS512, ES256/ES384; rejects `none` and HMAC)
  - Claims validation: `iss`, `aud`, `exp`, `nbf` with configurable clock skew
  - Scope enforcement via `required_scopes` config
  - JWKS key caching with configurable refresh interval
  - Key lookup by `kid` with fallback to `kty`/`use` matching
  - RFC 6750 `WWW-Authenticate` error responses
  - Auth context headers: `x-auth-sub`, `x-auth-scope`, `x-auth-claims`
  - `issuer_override` config option for split-network environments (e.g., Docker)

#### CEL Policy Evaluation
- `cel` middleware plugin — inline expression-based access control via [CEL](https://cel.dev/)
  - Pre-compiled expressions for microsecond-latency evaluation
  - Full request context: method, path, headers, query, body, client IP, path params
  - Auth integration: `request.consumer` and `request.claims` from upstream auth plugins
  - CEL standard library: `startsWith`, `endsWith`, `contains`, `exists`, `has`, `in`, `matches`
  - Problem+json error responses (RFC 9457)

#### ACL & Consumer Headers
- `acl` middleware plugin — group and consumer-based access control
  - Allow/deny lists for consumer groups
  - Configurable hide-groups behaviour
- Standardized `x-auth-consumer` and `x-auth-consumer-groups` headers across all 5 auth plugins (`basic-auth`, `jwt-auth`, `oidc-auth`, `oauth2-auth`, `apikey-auth`)
- `groups_claim` config option added to `jwt-auth` for JWT-based group extraction

#### OPA Authorization
- `opa-authz` middleware plugin — policy-based access control via Open Policy Agent
  - Calls OPA Data API via `host_http_call` (POST to configurable endpoint)
  - Sends request context as OPA input: method, path, query, headers, client IP
  - Optional inclusion of auth claims from upstream auth plugins (`include_claims`)
  - Optional inclusion of request body (`include_body`)
  - Configurable deny message and timeout
  - 403 Forbidden with problem+json when policy denies, 503 when OPA unreachable

#### Plugins
- `basic-auth` middleware plugin — HTTP Basic authentication with credential validation
- `http-log` middleware plugin — HTTP logging with configurable endpoint and payload
- Unit tests for all 17 plugins (321 tests) with dedicated CI job

#### Host Functions
- `host_verify_signature` — cryptographic signature verification using `ring`
  - Supports RSA (RS256, RS384, RS512) and ECDSA (ES256, ES384)
  - JWK-based public key input with DER/uncompressed point construction
  - Returns 1 (valid), 0 (invalid), -1 (error)
  - Registered as `verify_signature` capability

### Changed
- Consolidated workspace from 11 crates to 8: merged `barbacane-router`, `barbacane-validator`, and `barbacane-spec-parser` into their parent crates
- Replaced all production `unwrap()`/`panic!()` with proper error handling (`expect()` with reason or `?` propagation)
- Switched to `parking_lot::Mutex`/`RwLock` for lock primitives (no poisoning)
- Enforced workspace-wide clippy lints: `unwrap_used = "warn"`, `panic = "warn"`
- Narrowed CI clippy to `--lib --bins` (test code may use `unwrap`)

### Removed
- `barbacane-router` crate (merged into `barbacane`)
- `barbacane-validator` crate (merged into `barbacane`)
- `barbacane-spec-parser` crate (merged into `barbacane-compiler`)

## [0.1.1] - 2026-02-10

### Added

#### AsyncAPI & Event Dispatch (M10)
- AsyncAPI 3.x parser with channels, operations, messages, and protocol bindings
- Channel parameters with templated addresses (e.g., `notifications/{userId}`)
- Operation actions: `send` (gateway publishes) and `receive` (gateway subscribes)
- Protocol bindings extraction for Kafka, NATS, MQTT, AMQP, WebSocket
- Message schema validation for AsyncAPI payloads
- Host functions: `host_kafka_publish`, `host_nats_publish`
- `kafka` dispatcher plugin with `brokers` config, topic routing, key expressions, and header forwarding
- `nats` dispatcher plugin with `url` config and subject routing
- Sync-to-async bridge: HTTP request in, broker publish out, 202 ack response
- `KafkaPublisher` — real Kafka publishing via `rskafka` (pure-Rust, no C deps) with connection caching and dedicated runtime
- `NatsPublisher` — real NATS publishing via `async-nats` with connection caching and dedicated runtime
- Integration tests for NATS and Kafka dispatchers (spec compilation, broker-unavailable 502, payload validation)

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
- Updated Dispatchers guide with Kafka `brokers` and NATS `url` configuration
- Updated Extensions reference with Kafka and NATS dispatcher schemas
- Updated CLI Reference with `seed-plugins` command documentation
- Added Interactive API Documentation section (Scalar at `/api/docs`)

#### API Spec Endpoint
- New `/__barbacane/specs` endpoint replacing `/__barbacane/openapi`
- Merged spec endpoints: `/__barbacane/specs/openapi` and `/__barbacane/specs/asyncapi`
- Format selection via `?format=json` or `?format=yaml` query parameter
- Type-aware index response separating OpenAPI and AsyncAPI specs
- Internal `x-barbacane-*` extensions stripped from served specs

#### Plugins
- New `observability` middleware plugin for per-operation observability:
  - Latency SLO monitoring with `latency_slo_ms` config
  - Detailed request/response logging with `detailed_request_logs` and `detailed_response_logs`
  - Custom latency histogram emission with `emit_latency_histogram`
  - Emits `barbacane_plugin_observability_slo_violation` counter when SLO exceeded

#### Other
- HTTP/2 support with automatic protocol detection via ALPN
- API lifecycle support with `deprecated` flag and `x-sunset` extension (RFC 8594)
- `Deprecation` and `Sunset` response headers for deprecated routes
- Fixture-based test specs for comprehensive integration testing
- Deprecation metrics (`barbacane_deprecated_route_requests_total`)

### Changed
- Improved test fixtures with more comprehensive scenarios
- Improved foreign key error handling in control plane API
- Plugin deletion now returns "resource is in use" error when referenced by projects
- Refactored compiler: extracted shared `compile_inner` core, eliminating ~380 lines of duplication across `compile_with_options`, `compile_with_manifest`, and `compile_with_plugins`

### Fixed
- CORS plugin now includes JSON Schema (`config-schema.json`) for UI configuration
- Plugin deletion errors now display user-friendly messages in the UI
- Global middlewares are now merged with operation-level middlewares instead of being overridden; operation middlewares override globals by name while preserving non-overridden globals
- `compile_with_plugins` now enforces the plaintext HTTP URL check (E1031), previously missing from this code path

### Removed
- `MessageBroker` trait, `BrokerRegistry`, and placeholder `KafkaBroker`/`NatsBroker` implementations — replaced by concrete `KafkaPublisher` and `NatsPublisher` with real broker connections
- `x-barbacane-observability` extension (dead code - was parsed but never used at runtime)
  - Per-operation observability should be achieved via the middleware plugin system
  - Global trace sampling remains configurable via `--trace-sampling` CLI flag

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

[Unreleased]: https://github.com/barbacane/barbacane/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/barbacane/barbacane/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/barbacane/barbacane/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/barbacane/barbacane/releases/tag/v0.1.0
