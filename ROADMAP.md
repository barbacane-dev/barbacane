# Roadmap

Prioritized roadmap for Barbacane development.

See [CHANGELOG.md](CHANGELOG.md) for release history.

---

## Current Focus

What's actively being worked on:

- [ ] `request-transformer` plugin — modify headers, query params, body before upstream
- [ ] `response-transformer` plugin — modify response headers/body before client
- [ ] Documentation for transformation plugins

---

## Up Next

Near-term items ready to be picked up:

- [ ] `tcp-log` plugin — send logs to TCP endpoint
- [ ] Security plugins documentation
- [ ] Structured log format documentation
- [ ] Integration guides (Datadog, Splunk, ELK)
- [ ] `barbacane dev` — local dev server with file watching
- [ ] `barbacane plugin init` — scaffold new plugin projects
- [ ] Improved error messages
- [ ] Installation guide update
- [ ] Getting started update

---

## Plugin Backlog

### P0 — High Value, Commonly Used

| Plugin | Type | Description |
|--------|------|-------------|
| ~~`request-transformer`~~ | Middleware | Modify headers, query params, body before upstream — **in progress** |
| ~~`response-transformer`~~ | Middleware | Modify response headers/body before client — **in progress** |
| ~~`ip-restriction`~~ | ~~Middleware~~ | ~~Allow/deny by IP or CIDR range~~ — **done** |
| ~~`basic-auth`~~ | ~~Middleware~~ | ~~Username/password authentication~~ — **done** |
| ~~`http-log`~~ | ~~Middleware~~ | ~~Send request/response logs to HTTP endpoint~~ — **done** |
| ~~`correlation-id`~~ | ~~Middleware~~ | ~~Propagate/generate X-Correlation-ID header~~ — **done** |

### P1 — Important for Production

| Plugin | Type | Description |
|--------|------|-------------|
| ~~`opa-authz`~~ | ~~Middleware~~ | ~~OPA policy evaluation via REST API (`host_http_call`)~~ — **done** |
| `bot-detection` | Middleware | Block known bots by User-Agent patterns |
| `redirect` | Middleware | URL redirections (301/302) |
| ~~`observability`~~ | ~~Middleware~~ | ~~Trace sampling, detailed validation logs, latency SLO monitoring~~ — **done** |
| ~~`acl`~~ | ~~Middleware~~ | ~~Access control by consumer/group after auth~~ — **done** |
| ~~`request-size-limit`~~ | ~~Middleware~~ | ~~Reject requests exceeding size (per-route)~~ — **done** |
| ~~`oidc-auth`~~ | ~~Middleware~~ | ~~OpenID Connect discovery + JWKS validation~~ — **done** |

### P2 — Nice to Have

| Plugin | Type | Description |
|--------|------|-------------|
| `ldap-auth` | Middleware | LDAP/Active Directory authentication (requires LDAP host functions or HTTP proxy) |
| `hmac-auth` | Middleware | Signature-based auth (AWS SigV4 style) |
| `grpc-web` | Middleware | gRPC-Web to gRPC translation |
| `websocket` | Dispatcher | WebSocket proxy support |

### P3 — Specialized / Enterprise

| Plugin | Type | Description |
|--------|------|-------------|
| `mtls-auth` | Middleware | Client certificate authentication |
| `canary` | Middleware | Traffic splitting by percentage |
| `graphql-proxy` | Dispatcher | GraphQL-specific routing and caching |
| `saml-auth` | Middleware | SAML authentication (most enterprise SSO covered by `oidc-auth`) |
| `vault-auth` | Middleware | HashiCorp Vault integration for auth |

---

## Feature Backlog

### Data Plane

| Feature | Description | Priority |
|---------|-------------|----------|
| HTTP/3 support | QUIC-based HTTP/3 ingress via `quinn` crate | P3 |
| gRPC support | Native gRPC proxying | P2 |
| Response streaming | Stream large responses without buffering | P2 |
| Connection pooling tuning | Configurable pool sizes and health checks | P2 |
| Certificate hot-reload | Reload TLS certs without restart | P2 |
| ~~Hot-reload~~ | ~~Download and swap artifact at runtime without restart~~ — **done** |
| ~~CORS auto-preflight~~ | ~~Automatic OPTIONS response for CORS preflight requests~~ — **done** |
| ~~Per-middleware timing metrics~~ | ~~Record execution duration per middleware in Prometheus~~ — **done** |

### Control Plane

| Feature | Description | Priority |
|---------|-------------|----------|
| Rollback support | One-click rollback to previous artifact version | P1 |
| Data plane groups | Deploy to specific subsets of data planes | P2 |
| Audit log | Track all spec/artifact/deployment changes | P2 |
| RBAC | Role-based access control for control plane API | P2 |
| Plugin registry | Central registry for discovering and versioning plugins | P2 |
| Multi-tenancy | Organization/team isolation with SNI-based routing | P3 |
| Health metrics collection | Aggregate CPU, memory, request rates from data planes | P2 |

### Developer Experience

| Feature | Description | Priority |
|---------|-------------|----------|
| `barbacane dev` | Local development server with file watching | P1 |
| `barbacane plugin init` | Scaffold new plugin projects from template | P1 |
| Plugin template repo | `barbacane-plugin-template` repository with minimal scaffolding | P1 |
| VS Code extension | Spec editing with validation and autocomplete | P2 |
| OpenAPI diff | Show changes between spec versions | P2 |
| Improved error messages | More actionable validation and compilation errors | P2 |
| Compile-time error catalog | Document all E-codes with examples and remediation | P2 |
| Extension documentation | Complete `x-barbacane-*` extension reference (ratelimit, cache, sunset) | P1 |
| Middleware ordering guide | Best practices for middleware execution order | P2 |
| ~~Playground environment~~ | ~~Docker Compose with Prometheus, Grafana, Loki, WireMock~~ — **done** |

### Integrations

| Feature | Description | Priority |
|---------|-------------|----------|
| Vault secrets | `vault://` secret reference scheme | P1 |
| AWS Secrets Manager | `aws-sm://` secret reference scheme | P2 |
| Kubernetes secrets | `k8s://` secret reference scheme | P2 |
| Terraform provider | Infrastructure-as-code for control plane resources | P2 |
| ArgoCD integration | GitOps deployment patterns documentation | P2 |

### Packaging & Distribution

| Feature | Description | Priority |
|---------|-------------|----------|
| Helm charts | Kubernetes deployment charts for data/control plane | P2 |
| Docker Hub | Publish images to Docker Hub (in addition to ghcr.io) | P3 |
| Homebrew formula | macOS package manager support | P3 |
| APT/RPM packages | Linux package manager support | P3 |

---

## Technical Debt

### Compile-Time Safety Gaps

| Item | Description | Priority | Status |
|------|-------------|----------|--------|
| Spec pointers in errors | Add JSON Pointer (e.g., `#/paths/~1users/get`) to all compile errors | P2 | Open |
| ~~Ambiguous route detection~~ | ~~E1050: Detect overlapping path templates~~ | ~~P0~~ | **Done** |
| ~~Schema complexity limits~~ | ~~E1051/E1052: Depth (32) and property (256) limits~~ | ~~P0~~ | **Done** |
| ~~Circular `$ref` detection~~ | ~~E1053: Detect circular JSON Schema references~~ | ~~P0~~ | **Done** |
| ~~Move E1011 to compile~~ | ~~E1011: Missing middleware name validation~~ | ~~P1~~ | **Done** |
| ~~Move E1015 to compile~~ | ~~Move unknown extension warning from `validate` to `compile`~~ | ~~P1~~ | **Done** |
| ~~Path template syntax validation~~ | ~~E1054: Validate braces, param names, duplicates~~ | ~~P2~~ | **Done** |
| ~~Duplicate operationId detection~~ | ~~E1055: Detect non-unique operationId~~ | ~~P2~~ | **Done** |
| ~~Deterministic artifact builds~~ | ~~Sort plugin/spec/route collections before serialization~~ | ~~P2~~ | **Done** |

### Other Technical Debt

| Item | Description | Priority |
|------|-------------|----------|
| `$ref` resolution in parser | Resolve local `#/components/*` refs at parse time instead of storing raw `$ref` values; currently users must pre-flatten specs | P1 |
| Schema composition analysis | Interpret `allOf`/`oneOf`/`anyOf`/`discriminator` at compile time instead of treating them as opaque JSON (runtime validation via `jsonschema` still works) | P2 |
| E1032 validation | Warn on OpenAPI security scheme without matching auth middleware | P2 |
| OPA WASM compilation | Define OPA version, compilation flags, error handling | P1 |
| Auth plugin auditing | Security review process for auth plugins (security-critical WASM) | P1 |
| Trace volume guidance | Documentation for managing trace volume at scale | P1 |
| Integration tests | Full control plane API lifecycle tests with PostgreSQL | P2 |
| Compile safety CI | Add fitness functions: deterministic build verification, fuzz testing for compiler | P2 |
| CLI subcommands | `barbacane-control spec/artifact/plugin` REST-based commands | P2 |
| HTTP/2 stream config | Expose configuration for stream limits (currently fixed) | P3 |
| Connection idle timeout | Make configurable (currently 60s hard-coded) | P3 |
| ~~JWKS fetch~~ | ~~Load JWT public keys from `jwks_uri`~~ — **done** |

---

## Open Questions

| Question | Context |
|----------|---------|
| Hot-reload semantics | How to handle in-flight requests during artifact reload? |
| Control plane scaling | How many data planes per control plane? WebSocket limits? |
| Plugin registry design | Trigger for implementing? Discovery and versioning model? |
| SNI router implementation | Build it, reuse existing (Istio/Envoy), or external? |
| Gateway API documentation | Which implementations to prioritize (Envoy, Istio, Cilium)? |
| Library embedding API | If users want to embed Barbacane, expose more internal crates? |
| Nightly build demand | User signal for binary nightlies vs container-only? |

---

## Out of Scope

| Item | Reason |
|------|--------|
| Automatic version negotiation | Deferred to spec authors; gateway routes, doesn't negotiate |
| Request transformation between API versions | Not a gateway concern; backend responsibility |
| Native Gateway API controller | Complementary positioning chosen (Option C) |
| Fan-out / scatter-gather | Not supported; single upstream per dispatch |
| Windows containers | No demand identified |

---

## Competitive Features to Monitor

| Feature | Competitors | Notes |
|---------|-------------|-------|
| AI Gateway | Kong AI Proxy, APISIX AI plugins | LLM request/response handling, token counting |
| Service Mesh integration | Istio, Linkerd | Sidecar mode for mesh environments |
| Multi-cluster routing | Kong, Traefik | Route across Kubernetes clusters |
| API Analytics | Kong, Tyk | Built-in analytics dashboard |
| Developer Portal | Kong, Tyk, Apigee | Self-service documentation and key management |
| GraphQL Federation | Apollo, Kong | Federated GraphQL gateway |
