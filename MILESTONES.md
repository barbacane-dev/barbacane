# Sprints & Roadmap

Sprint-based planning for Barbacane development.

See [BACKLOG.md](BACKLOG.md) for the full prioritized backlog.

---

## Current Sprint

### Sprint 14 — Packaging & Release Pipeline
**Goal:** Ship the first official release with pre-built binaries and container images.
**Spec:** [ADR-0019](adr/0019-packaging-and-release-strategy.md)

#### Release Automation
- [ ] GitHub Actions release workflow — triggered on `vX.Y.Z` tags
- [ ] Version bump validation — CI checks version in `Cargo.toml` matches tag
- [ ] Changelog validation — CI checks `CHANGELOG.md` has entry for version

#### Binary Builds
- [ ] Linux x86_64 (gnu) binary
- [ ] Linux aarch64 (gnu) binary
- [ ] Linux x86_64 (musl) binary
- [ ] Linux aarch64 (musl) binary
- [ ] macOS x86_64 binary
- [ ] macOS aarch64 binary
- [ ] SHA256 checksums file
- [ ] GitHub Release creation

#### Container Images
- [ ] `Dockerfile` for data plane — multi-stage build, distroless base
- [ ] `Dockerfile` for control plane — includes UI assets
- [ ] Multi-arch builds — linux/amd64 + linux/arm64
- [ ] Push to ghcr.io
- [ ] Image tagging — `latest`, `x.y.z`, `x.y`, `x`

#### Documentation
- [ ] Installation guide update
- [ ] Getting started update

---

## Upcoming Sprints

### Sprint 15 — Core Plugins (Transformers)
**Goal:** Request/response transformation — the most requested missing feature.

- [ ] `request-transformer` plugin — modify headers, query params, body
- [ ] `response-transformer` plugin — modify response headers/body
- [ ] `correlation-id` plugin — propagate/generate X-Correlation-ID
- [ ] Documentation for transformation plugins

### Sprint 16 — Security Plugins
**Goal:** IP restriction and basic auth for common security patterns.

- [ ] `ip-restriction` plugin — allow/deny by IP/CIDR
- [ ] `basic-auth` plugin — username/password authentication
- [ ] `acl` plugin — access control lists after authentication
- [ ] Security plugins documentation

### Sprint 17 — Observability & Logging
**Goal:** External log shipping for observability integrations.

- [ ] `http-log` plugin — send logs to HTTP endpoint
- [ ] `tcp-log` plugin — send logs to TCP endpoint
- [ ] Structured log format documentation
- [ ] Integration guides (Datadog, Splunk, ELK)

### Sprint 18 — Developer Experience
**Goal:** Make local development faster and easier.

- [ ] `barbacane dev` — local dev server with file watching
- [ ] `barbacane plugin init` — scaffold new plugin projects
- [ ] JWKS fetch for jwt-auth (deferred from M6a)
- [ ] Improved error messages

---

## Backlog (Unprioritized)

See [BACKLOG.md](BACKLOG.md) for the complete prioritized backlog including:

- Additional plugins (bot-detection, redirect, request-termination, etc.)
- Data plane features (hot-reload, HTTP/3, health metrics)
- Control plane features (rollback, audit log, RBAC)
- Integrations (Terraform, Vault, AWS Secrets Manager)

---

## Release History

### v0.1.0 (Pre-release) — Foundation

Completed milestones that established the core platform:

<details>
<summary><strong>M1 — Compile and Route</strong></summary>

The minimum viable loop: parse an OpenAPI spec, compile it into an artifact, load it in the data plane, and route requests.

- OpenAPI 3.x parser with `x-barbacane-*` extensions
- Routing trie with static/param segments
- `.bca` artifact format
- `barbacane compile` and `barbacane serve` CLI
- Mock dispatcher, health endpoint, 404/405 responses
</details>

<details>
<summary><strong>M2 — Request Validation</strong></summary>

The gateway enforces the spec. Requests that don't conform are rejected.

- JSON Schema compilation and validation
- Path, query, header, and body validation
- RFC 9457 error responses
- Development mode with verbose errors
- Request size limits
</details>

<details>
<summary><strong>M3 — WASM Plugin System</strong></summary>

The extensibility layer with sandboxed WASM execution.

- wasmtime integration with AOT compilation
- Plugin manifest (`plugin.toml`) and config schema
- Host functions: logging, context, clock, secrets
- Middleware chain execution (request/response)
- `barbacane-plugin-sdk` and proc macros
</details>

<details>
<summary><strong>M4 — Built-in Dispatchers</strong></summary>

HTTP proxying and serverless dispatch.

- `http-upstream` dispatcher with connection pooling
- Circuit breaker, timeouts, upstream TLS/mTLS
- `mock` dispatcher (WASM plugin)
- `lambda` dispatcher for AWS Lambda
</details>

<details>
<summary><strong>M5 — Plugin Manifest System</strong></summary>

Explicit plugin configuration via `barbacane.yaml`.

- Plugin source types: `path` and `url`
- Plugin resolution and artifact bundling
- `barbacane init` with templates
</details>

<details>
<summary><strong>M6a — TLS & JWT Auth</strong></summary>

HTTPS termination and JWT authentication.

- TLS termination with rustls (TLS 1.2/1.3)
- `jwt-auth` middleware plugin
- Claims validation and context propagation
</details>

<details>
<summary><strong>M6b — API Key & OAuth2 Auth</strong></summary>

Additional authentication methods.

- `apikey-auth` middleware plugin
- `oauth2-auth` middleware plugin (RFC 7662 introspection)
</details>

<details>
<summary><strong>M6c — Secrets Management</strong></summary>

Secret references for sensitive configuration.

- `env://` and `file://` secret schemes
- `host_get_secret` host function for plugins
</details>

<details>
<summary><strong>M7 — Rate Limiting & Caching</strong></summary>

Traffic control plugins.

- `rate-limit` middleware with IETF draft headers
- `cache` middleware with vary-aware caching
</details>

<details>
<summary><strong>M8 — Observability</strong></summary>

Metrics, traces, and structured logs.

- Prometheus metrics endpoint
- W3C Trace Context support
- OTLP export to OpenTelemetry Collector
- Plugin telemetry host functions
</details>

<details>
<summary><strong>M9 — Control Plane</strong></summary>

Management layer with REST API and database.

- PostgreSQL schema and migrations
- Specs, artifacts, plugins, compilations APIs
- Background compilation worker
</details>

<details>
<summary><strong>M10 — AsyncAPI & Event Dispatch</strong></summary>

Event-driven API support.

- AsyncAPI 3.x parser
- `kafka` and `nats` dispatchers
- Sync-to-async bridge (HTTP → message queue)
</details>

<details>
<summary><strong>M11 — Production Readiness</strong></summary>

Hardening and testing infrastructure.

- Graceful shutdown, HTTP/2, keep-alive
- `barbacane-test` crate
- CI/CD pipeline, benchmarks
</details>

<details>
<summary><strong>M12 — Data Plane Connection</strong></summary>

Connected mode for fleet management.

- WebSocket connection between data plane and control plane
- API key authentication
- Deploy tab in control plane UI
</details>

---

## Sprint Conventions

**Story Format:**
```
- [ ] Short description — additional context if needed
```

**Completion Criteria:**
- All stories checked off
- Tests passing
- Documentation updated
- PR merged to main
