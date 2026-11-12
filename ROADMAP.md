# Roadmap

Forward-looking priorities for Barbacane. See [CHANGELOG.md](CHANGELOG.md) for what has shipped.

Barbacane is the light, robust, zero-trust API gateway: the OpenAPI or AsyncAPI spec is the configuration, it compiles into a signed artifact, and plugins run sandboxed with only the capabilities they declare. 1.0 makes that surface a compatibility promise, so the roadmap favours fewer, finished features over a long list.

---

## Now

### 1.0.0-rc.1

The release candidate ([milestone](https://github.com/barbacane-dev/barbacane/milestone/1)). Done:

- [x] **Stability policy** ([#280](https://github.com/barbacane-dev/barbacane/issues/280)): what 1.0 guarantees (spec extensions, artifact format, plugin interface, control-plane API and CLI, environment variables, metrics, error codes), how deprecations work, and which versions get security fixes.
- [x] **Getting started and installation** ([#283](https://github.com/barbacane-dev/barbacane/issues/283)): from an empty directory to a running gateway with `barbacane dev`, and every install path (release binaries, images, source), each command run as written.
- [x] **Per-plugin resource limits** ([#274](https://github.com/barbacane-dev/barbacane/issues/274)): a `[limits]` table in `plugin.toml`, enforced per instance and covered by the artifact signature.
- [x] **Instance reuse** ([#273](https://github.com/barbacane-dev/barbacane/issues/273)): `init` once per plugin and config, each request in a fresh instance started from that state.
- [x] **YAML in safe Rust** ([#254](https://github.com/barbacane-dev/barbacane/issues/254)): serde-saphyr, held to libyaml by a differential test and nightly fuzzing.

Remaining: the findings of the pre-release review, issues [#321](https://github.com/barbacane-dev/barbacane/issues/321) to [#336](https://github.com/barbacane-dev/barbacane/issues/336) on the milestone.

---

## Next

After 1.0, prioritised roughly top to bottom:

- [ ] Official plugin limits: measure each official plugin's memory and fuel under realistic configs, and declare `[limits]` that fit
- [ ] Spec pointers in compile errors: a JSON Pointer (`#/paths/~1users/get`) on every error, and an E-code catalog with examples and remediation
- [ ] `barbacane plugin init`, with a plugin template repository
- [ ] `idempotency` middleware: `Idempotency-Key` through the cache capability
- [ ] Rollback: redeploy a project's previous artifact from the control plane and the CLI
- [ ] Integration guides for `http-log` (Datadog, Splunk, ELK) and the structured log format

---

## Later

Committed but not yet scheduled.

### Data plane

| Feature | Priority | Notes |
|---|---|---|
| gRPC support | P2 | Native gRPC proxying |
| Active health checks | P2 | Probe `http-upstream` pool endpoints, beside the passive circuit breakers |
| Certificate hot-reload | P2 | Reload TLS certificates without a restart |
| HTTP/3 support | P3 | QUIC-based ingress |
| HTTP/2 stream limits | P3 | Expose the stream limits, which are fixed |

### Control plane

| Feature | Priority | Notes |
|---|---|---|
| Audit log | P2 | Every spec, artifact and deployment change |
| RBAC | P2 | Roles on the control-plane API, beyond the admin token |
| Data plane groups | P2 | Deploy to a subset of a project's data planes |
| Health metrics collection | P2 | Aggregate request rates and resource use from data planes |
| Transparency log | P3 | Record artifact signatures in a transparency log (Sigstore) |

### Plugins

| Plugin | Type | Priority | Notes |
|---|---|---|---|
| `hmac-auth` | Middleware | P2 | Signature-based auth (AWS SigV4 style) |
| `grpc-web` | Middleware | P2 | gRPC-Web to gRPC translation |
| `mtls-auth` | Middleware | P3 | Client certificate authentication |
| `canary` | Middleware | P3 | Split traffic between operations or upstreams by percentage |
| `vault-auth` | Middleware | P3 | HashiCorp Vault integration |

### Secrets and integrations

| Feature | Priority | Notes |
|---|---|---|
| Vault secrets | P1 | `vault://` secret references |
| AWS Secrets Manager | P2 | `aws-sm://` |
| Kubernetes secrets | P2 | `k8s://` |
| Terraform provider | P2 | Control-plane resources as code, on the control plane's OpenAPI spec |

### Packaging and distribution

| Feature | Priority | Notes |
|---|---|---|
| Helm charts | P2 | Data plane and control plane |
| Homebrew formula | P3 | macOS |
| APT/RPM packages | P3 | Linux package managers |

### Engineering

| Item | Priority | Notes |
|---|---|---|
| Schema composition analysis | P2 | Interpret `allOf`/`oneOf`/`anyOf`/`discriminator` at compile time |
| Plugin SDK helpers | P2 | Constant-time compare, trusted-proxy client IP, RFC 9457 errors, CRLF-safe headers |
| Single-pass YAML reads | P2 | Bound alias expansion with the parser's own limits instead of a second pass |
| Deterministic builds | P2 | Verify that the same inputs compile to the same artifact in CI |
| Trace volume guidance | P2 | Sampling and volume at scale |
| OCI and SBOM | P3 | Carry the spec fingerprint in SBOMs and image labels |

---

## Someday / maybe

Ideas worth tracking but not committed. Items flagged **`[competitive]`** appear on competitors' feature matrices (see Competitive watch).

- **`[competitive]`** OpenAPI Overlay support: environment-specific config as overlays on one base spec
- **`[competitive]`** Developer portal generated from the compiled spec
- GraphQL proxying, SAML authentication

---

## Open questions

| Question | Context |
|---|---|
| Hot-reload semantics | What in-flight requests should see while an artifact reloads |
| Control plane scaling | Data planes per control plane, WebSocket limits |
| Library embedding | Whether to support embedding the data plane as a library, which would put more crates under the 1.0 promise |
| Nightly builds | Whether users want nightly binaries, or images only |

---

## Non-goals

| Item | Reason |
|---|---|
| AI gateway, MCP server | Removed for 1.0: a gateway that does less, and does it well |
| Web UI | The control plane is an API, with the CLI and the interactive reference at `/docs` |
| Native Gateway API controller | Complementary positioning: Barbacane routes from specs, not cluster resources |
| Version negotiation, cross-version transformation | The spec author's and the backend's concern |
| Fan-out / scatter-gather | One upstream per dispatch |
| Windows containers | No demand identified |

---

## Competitive watch

What competitors ship that we might copy. Signal only: absence from Barbacane is not a bug. Landscape refresh: **2026-04-20**.

### Protocol & routing

| Feature | Who | Barbacane status |
|---|---|---|
| gRPC proxying / `GRPCRoute` | Envoy Gateway (GA), Kong, Traefik | ➜ Later/P2 (gRPC support) |
| gRPC-Web ↔ gRPC translation | Envoy Gateway, Kong | ➜ Later/P2 (`grpc-web` middleware) |
| GraphQL Federation | Apollo, Kong | ➜ Someday/maybe (GraphQL proxying) |
| WebSocket proxy | Envoy Gateway, Kong, Tyk | ✅ `ws-upstream` (ADR-0026) |
| HTTP/3 (QUIC) ingress | Envoy, Cloudflare, NGINX | ➜ Later/P3 |
| Weighted / canary routing | Kong (plugin), Envoy (Gateway API native) | ✅ Weights across `http-upstream` endpoints (ADR-0035); percentage splits ➜ Later/P3 (`canary`) |

### Traffic management

| Feature | Who | Barbacane status |
|---|---|---|
| Sliding-window rate limiting | Kong (Rate Limiting Advanced, enterprise, for the advanced window), Tyk, Envoy local ratelimit | ✅ `rate-limit` |
| Multi-tier / layered rate limits | Kong, Tyk | ✅ via stacking (cf. `docs/guide/middlewares/traffic-control.md`) |
| Response caching | Kong, Tyk, Cloudflare | ✅ `cache` |
| Circuit breaker | Kong, Istio | ✅ Per endpoint in `http-upstream` pools (ADR-0035) |
| Idempotency | Some gateways via custom code | ➜ Next (`idempotency` middleware) |
| Request/response transformation | Kong, Tyk (core) | ✅ `request-transformer`, `response-transformer` |

### Authentication & security

| Feature | Who | Barbacane status |
|---|---|---|
| JWT / OIDC / OAuth2 | Kong, Tyk, Apigee, Zuplo | ✅ `jwt-auth`, `oidc-auth`, `oauth2-auth` |
| API keys | Kong, Tyk, Zuplo | ✅ `apikey-auth` |
| mTLS client auth | Kong (enterprise), Envoy | ➜ Later/P3 (`mtls-auth`) |
| Upstream mTLS | Kong, Envoy, Tyk | ✅ `http-upstream` `tls` |
| HMAC / SigV4-style auth | Kong, AWS API Gateway | ➜ Later/P2 (`hmac-auth`) |
| LDAP | Kong (CE), Tyk | ✅ `ldap-auth` on native host functions (ADR-0032) |
| CEL / inline policy | Kong (lua), Envoy (CEL), Barbacane | ✅ `cel` with routing mode |
| OPA integration | Kong, Envoy (ext-authz), Istio | ✅ `opa-authz` |
| IP restriction / bot detection | Kong, Cloudflare, AWS WAF | ✅ `ip-restriction`, `bot-detection` |

### Observability & developer experience

| Feature | Who | Barbacane status |
|---|---|---|
| OpenTelemetry export | Kong (native since 3.x), Envoy, Tyk Pump | ✅ OTLP + Prometheus + structured logs |
| **Self-service developer portal** | Kong, Tyk (integrated portal), Apigee, Zuplo (auto-generated from spec) | ➜ Someday/maybe |
| **Analytics dashboard** | Kong (Konnect), Tyk Dashboard, Apigee | Delegated to Prometheus and Grafana; no built-in dashboard planned |
| Spec linting | Stoplight Spectral, Redocly CLI, Vacuum, Barbacane | ✅ `vacuum:barbacane` ruleset |
| Local dev / hot-reload loop | Few competitors; most require external compose or operator setups | ✅ `barbacane dev` |

### Spec-driven gateways (closest philosophical cohort)

| Feature | Who | Barbacane status |
|---|---|---|
| OpenAPI-native config (spec = source of truth) | Zuplo, Bump.sh, Barbacane | ✅ Core identity |
| GitOps / PR-driven deploy | Zuplo (PR is the deploy) | ✅ Compile-to-artifact + control plane |
| **OpenAPI Overlay support** | Zuplo (env-specific overlays on one base spec) | ➜ Someday/maybe |
| Edge-deployed (300+ POPs) | Zuplo, Cloudflare | ⛔ Non-goal: Barbacane is self-hosted |

### Kubernetes & service mesh

| Feature | Who | Barbacane status |
|---|---|---|
| Gateway API controller (full conformance) | Envoy Gateway, Kong Gateway Operator, Cilium, Traefik | ⛔ Non-goal (complementary positioning); tracking as the ecosystem standardises |
| Multi-cluster routing | Kong, Traefik, Istio Ambient Multicluster (Beta) | ➜ Later (control-plane "data plane groups") |
| Sidecarless service mesh | Istio Ambient (GA since 1.24, Nov 2024) | ⛔ Out of scope |
| Service mesh (sidecar) | Istio, Linkerd | ⛔ Out of scope |
