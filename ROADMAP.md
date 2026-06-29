# Roadmap

Forward-looking priorities for Barbacane. See [CHANGELOG.md](CHANGELOG.md) for what's shipped.

---

## Now

Actively being worked on:

### Security hardening (in progress)

A security review drove a hardening pass. Landed on `security/hardening-and-test-framework`:

- [x] **Control-plane authentication** — admin bearer token (`BARBACANE_CONTROL_ADMIN_TOKEN`) required on all routes except `/health` and the data-plane WebSocket; server fails closed without it. CORS tightened to an allowlist (`BARBACANE_CONTROL_ALLOWED_ORIGINS`).
- [x] **Artifact integrity & signing** — `.bca` now carries an Ed25519 signature over its content hash; the data plane recomputes every plugin/spec/route checksum and verifies the signature against a pinned key (`BARBACANE_TRUSTED_PUBKEY`) on load. Sign at compile with `BARBACANE_SIGNING_KEY`.
- [x] **MCP session enforcement** — non-`initialize` requests without a valid session are rejected (no more header-omission bypass).
- [x] **Plugin egress SSRF guard** — the WASM host HTTP client rejects loopback/link-local/private/metadata targets and no longer follows redirects (override with `BARBACANE_ALLOW_INTERNAL_EGRESS`).
- [x] **`jwt-auth` real verification** — signatures verified via the host `verify_signature` capability (inline JWK); the test-only skip flag is ignored in production builds.
- [x] **Secret confinement** — `file://` secret references confined to `BARBACANE_SECRETS_DIR`.
- [x] **Security test framework** — adversarial integration suite (`crates/barbacane-test/tests/security/`) + `cargo-fuzz` targets (`fuzz/`). See `docs/contributing/security-testing.md`.

In flight:

- [ ] **WASM capability enforcement** — capability map corrected (`cache`/`rate_limit`/`http_stream`); per-plugin default-deny linker + `validate_imports` gate pending the plugin-manifest migration (most official `plugin.toml`s use a legacy capability dialect) and the wasm/Docker integration run.

---

## Next

Ready to pick up, prioritized roughly top-to-bottom:

- [ ] `barbacane plugin init` — scaffold new plugin projects from a template
- [ ] Plugin template repo — `barbacane-plugin-template` with minimal scaffolding
- [ ] `tcp-log` plugin — send structured logs to a TCP endpoint
- [ ] Integration guides — Datadog / Splunk / ELK recipes for `http-log`
- [ ] Structured log format documentation
- [ ] Getting-started refresh — align with `barbacane dev` and `specs/` folder layout
- [ ] Installation guide update
- [ ] Improved compiler error messages — more actionable validation / compilation errors with spec JSON Pointers

---

## Later

Committed but not yet scheduled. Grouped by concern.

### Data plane

| Feature | Priority | Notes |
|---|---|---|
| gRPC support | P2 | Native gRPC proxying |
| Connection pooling tuning | P2 | Configurable pool sizes and health checks |
| Certificate hot-reload | P2 | Reload TLS certs without restart |
| HTTP/3 support | P3 | QUIC-based ingress via `quinn` |
| HTTP/2 stream config | P3 | Expose stream limits (currently fixed) |
| Connection idle timeout | P3 | Currently 60s hard-coded |

### Control plane

| Feature | Priority | Notes |
|---|---|---|
| Rollback support | P1 | One-click rollback to previous artifact version |
| Audit log | P2 | Track all spec/artifact/deployment changes |
| RBAC | P2 | Role-based access control for control plane API |
| Plugin registry | P2 | Central registry for discovering and versioning plugins |
| Data plane groups | P2 | Deploy to specific subsets of data planes |
| Artifact signing | ✅ | Ed25519 signing + verify-on-load shipped (security hardening); remaining: key distribution/rotation tooling, optional Sigstore/transparency-log |
| Health metrics collection | P2 | Aggregate CPU, memory, request rates from data planes |
| Multi-tenancy | P3 | Organization/team isolation with SNI-based routing |

### Plugins

| Plugin | Type | Priority | Notes |
|---|---|---|---|
| `idempotency` | Middleware | P1 | `Idempotency-Key` header via cache capability |
| `hmac-auth` | Middleware | P2 | Signature-based auth (AWS SigV4 style) |
| `grpc-web` | Middleware | P2 | gRPC-Web ↔ gRPC translation |
| `mtls-auth` | Middleware | P3 | Client certificate authentication |
| `canary` | Middleware | P3 | Traffic splitting by percentage |
| `graphql-proxy` | Dispatcher | P3 | GraphQL-specific routing and caching |
| `saml-auth` | Middleware | P3 | SAML (most SSO covered by `oidc-auth`) |
| `vault-auth` | Middleware | P3 | HashiCorp Vault integration |

### Developer experience

| Feature | Priority | Notes |
|---|---|---|
| MCP compile-time validation | P1 | Warn on missing `operationId`; embed MCP tool manifest in `.bca` (ADR-0025) |
| VS Code extension | P2 | Spec editing with validation and autocomplete |
| OpenAPI diff | P2 | Show changes between spec versions |
| Compile-time error catalog | P2 | Document all E-codes with examples and remediation |
| Middleware ordering guide | P2 | Best practices for chain order |

### Integrations

| Feature | Priority | Notes |
|---|---|---|
| Vault secrets | P1 | `vault://` secret reference scheme |
| AWS Secrets Manager | P2 | `aws-sm://` scheme |
| Kubernetes secrets | P2 | `k8s://` scheme |
| Terraform provider | P2 | IaC for control plane resources |
| ArgoCD integration | P2 | GitOps deployment patterns documentation |

### Packaging & distribution

| Feature | Priority | Notes |
|---|---|---|
| Helm charts | P2 | Kubernetes deployment charts for data/control plane |
| Docker Hub | P3 | Publish images (in addition to ghcr.io) |
| Homebrew formula | P3 | macOS package manager |
| APT/RPM packages | P3 | Linux package managers |

### Security & supply-chain provenance

The first three rungs of the trusted spec-to-run pipeline are shipped (artifact fingerprinting, provenance admin endpoint, drift detection via heartbeat). Remaining:

- [ ] **OCI / SBOM integration** — surface the spec fingerprint in SBOMs and container labels when packaging the data plane as an OCI image.

### Technical debt

| Item | Priority | Notes |
|---|---|---|
| Spec pointers in errors | P2 | Add JSON Pointer (e.g., `#/paths/~1users/get`) to all compile errors |
| Schema composition analysis | P2 | Interpret `allOf`/`oneOf`/`anyOf`/`discriminator` at compile time instead of treating as opaque JSON |
| E1032 validation | P2 | Warn on OpenAPI security scheme without matching auth middleware |
| OPA WASM compilation | P1 | Define OPA version, compilation flags, error handling |
| Auth plugin auditing | P1 | Security review process for auth plugins |
| Ingress timeouts & limits | P1 | Header-read/idle/handshake timeouts, connection cap, streaming body-size limit on the data plane (wire the parsed `--keepalive-timeout`) |
| Per-plugin secret & capability scope | P1 | Scope resolved secrets per plugin; complete the per-plugin default-deny host-function linker + migrate official `plugin.toml` capability dialects |
| Plugin SDK hardening helpers | P2 | Shared SDK helpers: constant-time secret compare, trusted-proxy client-IP extraction, RFC-9457 errors, CRLF-safe headers (removes per-plugin drift) |
| WASM execution budget | P2 | Epoch-interruption wall-clock deadline + host-imposed timeouts/body caps on blocking host calls |
| CI/supply-chain hardening | P2 | SHA-pin third-party Actions, scope job token permissions, `cargo deny check` (licenses/bans/sources) as a gate, SBOM + image signing |
| Control-plane container non-root | P2 | Run `barbacane-control` (nginx) as non-root (high port or capability) |
| Trace volume guidance | P1 | Documentation for managing trace volume at scale |
| Integration tests | P2 | Full control plane API lifecycle tests with PostgreSQL |
| Compile safety CI | P2 | Fitness functions: deterministic build verification, fuzz testing |
| CLI subcommands | P2 | `barbacane-control spec/artifact/plugin` REST-based commands |

---

## Someday / maybe

Ideas worth tracking but not committed. Items flagged **`[competitive]`** are on competitors' feature matrices (see Competitive watch).

- **`[competitive]`** Semantic caching for `ai-proxy` — embedding-based response dedup, vector-store-backed via `host_http_call` (Kong 3.8, Portkey)
- **`[competitive]`** Semantic routing — route by cosine similarity between prompt and per-target descriptions (Kong 3.8)
- **`[competitive]`** Hard-budget spend enforcement — extend `ai-cost-tracker` from "emit cost metric" to "reject when spend > cap per consumer/window" (Portkey, LiteLLM)
- **`[competitive]`** OpenAPI Overlay support — env-specific config via overlay spec so one base spec + dev/staging/prod overlays replace copies (Zuplo pattern)
- **`[competitive]`** Auto-generated developer portal — Scalar/Redocly-backed portal served by the control plane from the compiled spec
- Multi-modal AI — explicit vision/audio support beyond the OpenAI-compatible image URLs we already carry
- **Stateful Responses API** — `previous_response_id` + `GET /v1/responses/{id}` retrieval (ADR-0030 §2 deferred). Requires session-scoped storage (artifact-bound vs. external KV is the open call); the stateless build emits a `Warning: 299` header on `store: true` and increments `responses_store_downgrades_total` so operators can quantify demand before we commit to a backend
- **Responses SSE re-encoding** — rewrite the in-event `id` in the OpenAI passthrough's text/event-stream so synthetic `resp_<uuid-v7>` ids hold under streaming (ADR-0030 §2 "Streaming"). Anthropic translation for streamed Responses also lands here
- **`/v1/models` caching + single-flight** — v1 hits upstreams on every call; on hot deployments, cache the aggregated catalog with a short TTL and coalesce concurrent fetches. Endpoint is intentionally off the data-plane critical path, so this is a polish item not a correctness one

---

## Blocked

Waiting on external unblockers:

- **`ldap-auth` plugin** — blocked pending a pure-Rust, FFI-free LDAP client. HTTP bridge approach rejected (ADR-0028) as it reduces to existing auth plugins.

---

## Open questions

| Question | Context |
|---|---|
| Hot-reload semantics | How to handle in-flight requests during artifact reload? |
| Control plane scaling | How many data planes per control plane? WebSocket limits? |
| Plugin registry design | Trigger for implementing? Discovery and versioning model? |
| SNI router implementation | Build it, reuse (Istio/Envoy), or external? |
| Gateway API prioritisation | Which implementations to prioritise (Envoy, Istio, Cilium)? |
| Library embedding API | If users want to embed Barbacane, expose more internal crates? |
| Nightly build demand | User signal for binary nightlies vs container-only? |

---

## Non-goals

| Item | Reason |
|---|---|
| Automatic version negotiation | Deferred to spec authors; gateway routes, doesn't negotiate |
| Request transformation between API versions | Not a gateway concern; backend responsibility |
| Native Gateway API controller | Complementary positioning chosen (Option C) |
| Fan-out / scatter-gather | Not supported; single upstream per dispatch |
| Windows containers | No demand identified |

---

## Competitive watch

What competitors ship that we might copy. Barbacane is primarily an API gateway; the AI gateway is one feature category among many. Signal only — absence from Barbacane isn't a bug. Landscape refresh: **2026-04-20**.

### Protocol & routing

| Feature | Who | Barbacane status |
|---|---|---|
| gRPC proxying / `GRPCRoute` | Envoy Gateway (GA), Kong, Traefik | ➜ Later/P2 (gRPC support) |
| gRPC-Web ↔ gRPC translation | Envoy Gateway, Kong | ➜ Later/P2 (`grpc-web` middleware) |
| GraphQL Federation | Apollo, Kong | ➜ Later/P3 (`graphql-proxy` dispatcher) |
| WebSocket proxy | Envoy Gateway, Kong, Tyk | ✅ `ws-upstream` (ADR-0026) |
| HTTP/3 (QUIC) ingress | Envoy, Cloudflare, NGINX | ➜ Later/P3 |
| Weighted / canary routing | Kong (plugin), Envoy (Gateway API native) | ➜ Later/P3 (`canary` middleware) |

### Traffic management

| Feature | Who | Barbacane status |
|---|---|---|
| Sliding-window rate limiting | Kong (Rate Limiting Advanced — enterprise for advanced window), Tyk, Envoy local ratelimit | ✅ `rate-limit` |
| Multi-tier / layered rate limits | Kong, Tyk | ✅ via stacking (cf. `docs/guide/middlewares/traffic-control.md`) |
| Response caching | Kong, Tyk, Cloudflare | ✅ `cache` |
| Circuit breaker | Kong, Istio | Partially covered: `ai-proxy` has provider-level fallback on 5xx/timeout. No general circuit breaker; not planned as a separate middleware |
| Idempotency | Some gateways via custom code | ➜ Later/P1 (`idempotency` middleware) |
| Request/response transformation | Kong, Tyk (core) | ✅ `request-transformer`, `response-transformer` |

### Authentication & security

| Feature | Who | Barbacane status |
|---|---|---|
| JWT / OIDC / OAuth2 | Kong, Tyk, Apigee, Zuplo | ✅ `jwt-auth`, `oidc-auth`, `oauth2-auth` |
| API keys | Kong, Tyk, Zuplo | ✅ `apikey-auth` |
| mTLS client auth | Kong (enterprise), Envoy | ➜ Later/P3 (`mtls-auth`) |
| HMAC / SigV4-style auth | Kong, AWS API Gateway | ➜ Later/P2 (`hmac-auth`) |
| LDAP | Kong (CE), Tyk | ⛔ Blocked — see Blocked section |
| CEL / inline policy | Kong (lua), Envoy (CEL), Barbacane | ✅ `cel` with routing mode |
| OPA integration | Kong, Envoy (ext-authz), Istio | ✅ `opa-authz` |
| IP restriction / bot detection | Kong, Cloudflare, AWS WAF | ✅ `ip-restriction`, `bot-detection` |

### Observability & developer experience

| Feature | Who | Barbacane status |
|---|---|---|
| OpenTelemetry export | Kong (native since 3.x), Envoy, Tyk Pump | ✅ OTLP + Prometheus + structured logs |
| **Self-service developer portal** | Kong, Tyk (integrated portal), Apigee, Zuplo (auto-generated from spec) | ➜ Not planned — see Someday/maybe |
| **Analytics dashboard** | Kong (Konnect), Tyk Dashboard, Apigee | Delegated to Prometheus/Grafana — no built-in dashboard planned |
| Spec linting | Stoplight Spectral, Redocly CLI, Vacuum, Barbacane | ✅ `vacuum:barbacane` ruleset |
| Local dev / hot-reload loop | Few competitors — most require external compose/operator setups | ✅ `barbacane dev` |

### Spec-driven gateways (closest philosophical cohort)

| Feature | Who | Barbacane status |
|---|---|---|
| OpenAPI-native config (spec = source of truth) | Zuplo, Bump.sh, Barbacane | ✅ Core identity |
| GitOps / PR-driven deploy | Zuplo (PR is the deploy) | ✅ Compile-to-artifact + control plane |
| **OpenAPI Overlay support** | Zuplo (env-specific overlays on one base spec) | ➜ Someday/maybe |
| Edge-deployed (300+ POPs) | Zuplo, Cloudflare | ⛔ Non-goal — Barbacane is self-hosted |

### Kubernetes & service mesh

| Feature | Who | Barbacane status |
|---|---|---|
| Gateway API controller (full conformance) | Envoy Gateway, Kong Gateway Operator, Cilium, Traefik | ⛔ Non-goal (Option C — complementary positioning); tracking as ecosystem standardizes |
| Multi-cluster routing | Kong, Traefik, Istio Ambient Multicluster (Beta) | ➜ Later (control-plane "data plane groups") |
| Sidecarless service mesh | Istio Ambient (GA since 1.24, Nov 2024) | ⛔ Out of scope |
| Service mesh (sidecar) | Istio, Linkerd | ⛔ Out of scope |

### AI / LLM gateway (one category among the above)

Every serious gateway now ships AI features. Barbacane is competitive on the core (multi-provider routing, guardrails, rate limiting, cost tracking, MCP server); newer differentiators sit in the semantic layer and MCP governance.

| Capability | Who | Barbacane status |
|---|---|---|
| Multi-provider LLM proxy + fallback | Kong 3.8+, APISIX 3.15+, Portkey, LiteLLM, Cloudflare AI Gateway (Universal Endpoint), Zuplo | ✅ `ai-proxy` (ADR-0024, ADR-0030) — caller-owned `model`, glob `routes`, per-target `allow`/`deny` |
| Prompt + response guardrails | Portkey (60+), LiteLLM (built-in), Kong, Zuplo | ✅ `ai-prompt-guard`, `ai-response-guard` |
| Token-based rate limiting + spend metric | Kong (enterprise for token limits), Portkey, LiteLLM | ✅ `ai-token-limit`, `ai-cost-tracker` |
| OpenAI Responses API (stateless) | OpenAI native, Cloudflare AI Gateway, Portkey | ✅ `POST /v1/responses` (ADR-0030 §2) — synthetic `resp_<uuid-v7>` ids, Anthropic Messages translation, Ollama 400. Stateful (`previous_response_id`, `GET /v1/responses/{id}`) deferred |
| Aggregated `/v1/models` discovery | OpenAI clients, Continue.dev, LiteLLM | ✅ `GET /v1/models` (ADR-0030 §4) — aggregates every unique provider declared in `routes` / `targets` / flat, partial-failure response with `partial: true` + `warnings[]` |
| MCP server + MCP traffic governance | Kong (API→MCP conversion), APISIX (`mcp-bridge`), LiteLLM, Portkey, Zuplo | ✅ Native MCP server from spec (ADR-0025) |
| MCP authentication (OAuth 2.1 / PKCE) | Portkey | Covered via existing auth middlewares on `/__barbacane/mcp` (`oidc-auth` handles PKCE-authenticated JWTs; `apikey-auth` / `oauth2-auth` also apply) |
| Streaming Responses API (SSE event-stream rewrites) | OpenAI native, Cloudflare AI Gateway | Partial — non-streaming JSON path complete; SSE re-encoding for synthetic ids deferred (ADR-0030 §2 "Streaming") |
| Semantic cache / semantic routing | Kong 3.8 (Redis-backed), Portkey (enterprise) | ➜ Someday/maybe |
| Hard-budget spend enforcement | Portkey, LiteLLM (per-team budgets) | ➜ Someday/maybe |
| Agent-to-agent (A2A) governance | Kong 3.14, Istio Agentgateway (experimental) | Watch — new category, unclear if it stabilizes |
| K8s Gateway API Inference Extension (`InferencePool` / `InferenceModel`) | Envoy Gateway, kgateway, GKE Gateway, NGINX Gateway Fabric | Watch — complementary to our CEL-driven policy routing, not overlapping |
