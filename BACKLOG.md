# Product Backlog

Prioritized list of features, plugins, and improvements for future sprints.

Items are tagged with their source ADR or SPEC for traceability.

---

## Plugin Backlog

### P0 — High Value, Commonly Used

| Plugin | Type | Description | Source |
|--------|------|-------------|--------|
| `request-transformer` | Middleware | Modify headers, query params, body before upstream | Competitive analysis |
| `response-transformer` | Middleware | Modify response headers/body before client | Competitive analysis |
| ~~`ip-restriction`~~ | ~~Middleware~~ | ~~Allow/deny by IP or CIDR range~~ | **DONE** |
| `basic-auth` | Middleware | Username/password authentication | Competitive analysis |
| `http-log` | Middleware | Send request/response logs to HTTP endpoint | Competitive analysis |
| ~~`correlation-id`~~ | ~~Middleware~~ | ~~Propagate/generate X-Correlation-ID header~~ | **DONE** |

### P1 — Important for Production

| Plugin | Type | Description | Source |
|--------|------|-------------|--------|
| ~~`observability`~~ | ~~Middleware~~ | ~~Trace sampling, detailed validation logs, latency SLO monitoring~~ | **DONE** |
| `acl` | Middleware | Access control by consumer/group after auth | Competitive analysis |
| ~~`request-size-limit`~~ | ~~Middleware~~ | ~~Reject requests exceeding size (per-route)~~ | **DONE** |
| `bot-detection` | Middleware | Block known bots by User-Agent patterns | Competitive analysis |
| `redirect` | Middleware | URL redirections (301/302) | Competitive analysis |

### P2 — Nice to Have

| Plugin | Type | Description | Source |
|--------|------|-------------|--------|
| `opa-authz` | Middleware | OPA policy evaluation with WASM-compiled Rego | ADR-0009, SPEC-004 |
| `hmac-auth` | Middleware | Signature-based auth (AWS SigV4 style) | Competitive analysis |
| `ldap-auth` | Middleware | LDAP/Active Directory authentication | Competitive analysis |
| `grpc-web` | Middleware | gRPC-Web to gRPC translation | ADR-0004 |
| `websocket` | Dispatcher | WebSocket proxy support | SPEC-001 |

### P3 — Specialized / Enterprise

| Plugin | Type | Description | Source |
|--------|------|-------------|--------|
| `mtls-auth` | Middleware | Client certificate authentication | SPEC-004 |
| `canary` | Middleware | Traffic splitting by percentage | Competitive analysis |
| `graphql-proxy` | Dispatcher | GraphQL-specific routing and caching | Competitive analysis |
| `saml-auth` | Middleware | SAML authentication | Competitive analysis |
| `vault-auth` | Middleware | HashiCorp Vault integration for auth | ADR-0009 |

---

## Feature Backlog

### Data Plane

| Feature | Description | Priority | Source |
|---------|-------------|----------|--------|
| ~~Hot-reload~~ | ~~Download and swap artifact at runtime without restart~~ | ~~P0~~ | **DONE** |
| ~~CORS auto-preflight~~ | ~~Automatic OPTIONS response for CORS preflight requests~~ | ~~P1~~ | **DONE** |
| ~~Per-middleware timing metrics~~ | ~~Record execution duration per middleware in Prometheus~~ | ~~P1~~ | **DONE** |
| HTTP/3 support | QUIC-based HTTP/3 ingress via `quinn` crate | P3 | ADR-0004 |
| gRPC support | Native gRPC proxying | P2 | ADR-0004 |
| Response streaming | Stream large responses without buffering | P2 | — |
| Connection pooling tuning | Configurable pool sizes and health checks | P2 | ADR-0005 |
| Certificate hot-reload | Reload TLS certs without restart (currently requires rolling restart) | P2 | SPEC-004 |

### Control Plane

| Feature | Description | Priority | Source |
|---------|-------------|----------|--------|
| Rollback support | One-click rollback to previous artifact version | P1 | — |
| Data plane groups | Deploy to specific subsets of data planes | P2 | ADR-0007 |
| Audit log | Track all spec/artifact/deployment changes | P2 | — |
| RBAC | Role-based access control for control plane API | P2 | — |
| Plugin registry | Central registry for discovering and versioning plugins | P2 | ADR-0006 |
| Multi-tenancy | Organization/team isolation with SNI-based routing | P3 | ADR-0013 |
| Health metrics collection | Aggregate CPU, memory, request rates from data planes | P2 | ADR-0007 |

### Developer Experience

| Feature | Description | Priority | Source |
|---------|-------------|----------|--------|
| ~~Playground environment~~ | ~~Docker Compose with Prometheus, Grafana, Loki, WireMock~~ | ~~P1~~ | **DONE** |
| `barbacane dev` | Local development server with file watching | P1 | ADR-0014 |
| `barbacane plugin init` | Scaffold new plugin projects from template | P1 | ADR-0017 |
| Plugin template repo | `barbacane-plugin-template` repository with minimal scaffolding | P1 | ADR-0017 |
| VS Code extension | Spec editing with validation and autocomplete | P2 | — |
| OpenAPI diff | Show changes between spec versions | P2 | — |
| Improved error messages | More actionable validation and compilation errors | P2 | ADR-0012 |
| Compile-time error catalog | Document all E-codes with examples and remediation | P2 | Tech review |
| Extension documentation | Complete `x-barbacane-*` extension reference (ratelimit, cache, sunset) | P1 | Tech review |
| Middleware ordering guide | Best practices for middleware execution order | P2 | Tech review |

### Integrations

| Feature | Description | Priority | Source |
|---------|-------------|----------|--------|
| Vault secrets | `vault://` secret reference scheme | P1 | ADR-0009, SPEC-004 |
| AWS Secrets Manager | `aws-sm://` secret reference scheme | P2 | SPEC-004 |
| Kubernetes secrets | `k8s://` secret reference scheme | P2 | SPEC-004 |
| Terraform provider | Infrastructure-as-code for control plane resources | P2 | — |
| ArgoCD integration | GitOps deployment patterns documentation | P2 | — |

### Packaging & Distribution

| Feature | Description | Priority | Source |
|---------|-------------|----------|--------|
| Helm charts | Kubernetes deployment charts for data/control plane | P2 | ADR-0018, ADR-0019 |
| Docker Hub | Publish images to Docker Hub (in addition to ghcr.io) | P3 | ADR-0019 |
| Homebrew formula | macOS package manager support | P3 | ADR-0019 |
| APT/RPM packages | Linux package manager support | P3 | ADR-0019 |

---

## Technical Debt

### Compile-Time Safety Gaps

| Item | Description | Priority | Status |
|------|-------------|----------|--------|
| ~~Ambiguous route detection~~ | E1050: Detect overlapping path templates | P0 | **DONE** |
| ~~Schema complexity limits~~ | E1051/E1052: Depth (32) and property (256) limits | P0 | **DONE** |
| ~~Circular `$ref` detection~~ | E1053: Detect circular JSON Schema references | P0 | **DONE** |
| ~~Move E1011 to compile~~ | E1011: Missing middleware name validation | P1 | **DONE** |
| ~~Move E1015 to compile~~ | ~~Move unknown extension warning from `validate` to `compile`~~ | ~~P1~~ | **DONE** |
| ~~Path template syntax validation~~ | E1054: Validate braces, param names, duplicates | P2 | **DONE** |
| ~~Duplicate operationId detection~~ | E1055: Detect non-unique operationId | P2 | **DONE** |
| Spec pointers in errors | Add JSON Pointer (e.g., `#/paths/~1users/get`) to all compile errors | P2 | Tech review |
| ~~Deterministic artifact builds~~ | ~~Sort plugin/spec/route collections before serialization~~ | ~~P2~~ | **DONE** |

**Test cases added:**
- `compile_detects_ambiguous_routes` ✓
- `compile_detects_invalid_path_template_*` (3 tests) ✓
- `compile_detects_schema_too_deep` ✓
- `compile_detects_schema_too_complex` ✓
- `compile_detects_duplicate_operation_ids` ✓
- `compile_detects_missing_middleware_name` ✓
- `compile_detects_missing_global_middleware_name` ✓
- `validate_path_template_*` (2 unit tests) ✓
- `normalize_path_template_works` ✓

### Other Technical Debt

| Item | Description | Priority | Source |
|------|-------------|----------|--------|
| JWKS fetch | Load JWT public keys from `jwks_uri` | P1 | M6a deferred |
| OPA WASM compilation | Define OPA version, compilation flags, error handling | P1 | SPEC-004 |
| Auth plugin auditing | Security review process for auth plugins (security-critical WASM) | P1 | ADR-0009 |
| Trace volume guidance | Documentation for managing trace volume at scale | P1 | ADR-0010 |
| Integration tests | Full control plane API lifecycle tests with PostgreSQL | P2 | M9 deferred |
| Compile safety CI | Add fitness functions: deterministic build verification, fuzz testing for compiler | P2 | Tech review |
| CLI subcommands | `barbacane-control spec/artifact/plugin` REST-based commands | P2 | M9 deferred |
| E1032 validation | Warn on OpenAPI security scheme without matching auth middleware | P2 | M6c deferred |
| HTTP/2 stream config | Expose configuration for stream limits (currently fixed) | P3 | SPEC-002 |
| Connection idle timeout | Make configurable (currently 60s hard-coded) | P3 | SPEC-002 |

---

## Open Questions

Unresolved decisions requiring follow-up:

| Question | Context | Source |
|----------|---------|--------|
| Hot-reload semantics | How to handle in-flight requests during artifact reload? | ADR-0007 |
| Control plane scaling | How many data planes per control plane? WebSocket limits? | ADR-0007 |
| Plugin registry design | Trigger for implementing? Discovery and versioning model? | ADR-0006 |
| SNI router implementation | Build it, reuse existing (Istio/Envoy), or external? | ADR-0013 |
| Gateway API documentation | Which implementations to prioritize (Envoy, Istio, Cilium)? | ADR-0018 |
| Library embedding API | If users want to embed Barbacane, expose more internal crates? | ADR-0019 |
| Nightly build demand | User signal for binary nightlies vs container-only? | ADR-0019 |

---

## Explicitly Out of Scope

Decisions made to NOT implement:

| Item | Reason | Source |
|------|--------|--------|
| Automatic version negotiation | Deferred to spec authors; gateway routes, doesn't negotiate | ADR-0015 |
| Request transformation between API versions | Not a gateway concern; backend responsibility | ADR-0015 |
| Native Gateway API controller | Complementary positioning chosen (Option C) | ADR-0018 |
| Fan-out / scatter-gather | Not supported; single upstream per dispatch | ADR-0008 |
| Windows containers | No demand identified | ADR-0019 |

---

## Competitive Features to Monitor

Features from competitors that may become important:

| Feature | Competitors | Notes |
|---------|-------------|-------|
| AI Gateway | Kong AI Proxy, APISIX AI plugins | LLM request/response handling, token counting |
| Service Mesh integration | Istio, Linkerd | Sidecar mode for mesh environments |
| Multi-cluster routing | Kong, Traefik | Route across Kubernetes clusters |
| API Analytics | Kong, Tyk | Built-in analytics dashboard |
| Developer Portal | Kong, Tyk, Apigee | Self-service documentation and key management |
| GraphQL Federation | Apollo, Kong | Federated GraphQL gateway |
