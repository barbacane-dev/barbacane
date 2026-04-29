<p align="center">
  <img src="assets/img/barbacane_logo_transparent_bg.png" alt="Barbacane" width="200">
</p>

<h1 align="center">Barbacane</h1>

<p align="center"><i>Your spec is your gateway.</i></p>

<p align="center">
  <a href="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml"><img src="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://docs.barbacane.dev"><img src="https://img.shields.io/badge/docs-docs.barbacane.dev-blue" alt="Documentation"></a>
  <img src="https://img.shields.io/badge/unit%20tests-517%20passing-brightgreen" alt="Unit Tests">
  <img src="https://img.shields.io/badge/plugin%20tests-777%20passing-brightgreen" alt="Plugin Tests">
  <img src="https://img.shields.io/badge/integration%20tests-275%20passing-brightgreen" alt="Integration Tests">
  <img src="https://img.shields.io/badge/cli%20tests-23%20passing-brightgreen" alt="CLI Tests">
  <img src="https://img.shields.io/badge/ui%20tests-44%20passing-brightgreen" alt="UI Tests">
  <img src="https://img.shields.io/badge/e2e%20tests-11%20passing-brightgreen" alt="E2E Tests">
  <img src="https://img.shields.io/badge/rust-1.75%2B-orange" alt="Rust Version">
  <a href="LICENSING.md"><img src="https://img.shields.io/badge/license-AGPLv3-blue" alt="License"></a>
</p>

---

Barbacane is a spec-driven API gateway built in Rust. Point it at an OpenAPI or AsyncAPI spec and it becomes your gateway — routing, validation, authentication, AI traffic, MCP, and all. No proprietary config language, no drift between your spec and your infrastructure.

- **Spec as config** — Your OpenAPI 3.x or AsyncAPI 3.x specification is the single source of truth. The compiler turns it into a sealed `.bca` artifact; no separate gateway DSL to maintain.
- **Fast and predictable** — Built on Rust, Tokio, and Hyper. No garbage collector, no latency surprises. Route lookup in ~83 ns, full request validation in ~1.2 µs.
- **Secure by default** — Memory-safe runtime, TLS via Rustls (FIPS-ready via aws-lc-rs), sandboxed WASM plugins, secrets resolved at runtime via `env://`, `file://`, and similar references — never baked into artifacts.
- **AI gateway built-in** — `ai-proxy` unifies OpenAI / Anthropic / Ollama with provider fallback, plus four dedicated middlewares for prompt guarding, response redaction, token-based rate limiting, and per-call cost tracking ([ADR-0024](adr/0024-ai-gateway-plugin.md)).
- **MCP from your spec** — Every operation in your OpenAPI spec is automatically exposed as a Model Context Protocol tool at `POST /__barbacane/mcp`, behind the same auth/rate-limit/validation chain ([ADR-0025](adr/0025-mcp-server.md)).
- **Edge-ready** — Stateless data plane instances designed to run close to your users, with a separate control plane handling compilation, artifact distribution, and hot-reload.
- **Extensible** — 33 official plugins; write your own in any language that compiles to WebAssembly. Plugins run in a sandbox, so a buggy plugin can't take down the gateway.
- **Observable** — Prometheus metrics, structured JSON logging, and distributed tracing with W3C Trace Context and OTLP export. Per-middleware timing comes for free.

## Quick Start

```bash
# Clone and build
git clone https://github.com/barbacane-dev/barbacane.git
cd barbacane
cargo build --release

# Initialize a project (scaffolds barbacane.yaml + specs/api.yaml)
./target/release/barbacane init my-api --fetch-plugins
cd my-api

# Start the dev server (compiles, serves, and hot-reloads on save)
../target/release/barbacane dev
```

For production, use the explicit compile-and-serve workflow:

```bash
barbacane compile -m barbacane.yaml -o api.bca
barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

### What configuration looks like

Routing, auth, rate limits, AI policy — all declared inline on the operation:

```yaml
paths:
  /v1/chat/completions:
    post:
      operationId: chatCompletions
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: "https://auth.example/"
            audience: ai-gateway
        - name: ai-prompt-guard
          config:
            default_profile: standard
            profiles:
              standard:
                max_messages: 50
                blocked_patterns: ["(?i)ignore previous instructions"]
        - name: ai-token-limit
          config:
            default_profile: standard
            partition_key: "header:x-auth-sub"
            profiles:
              standard: { quota: 100000, window: 60 }
        - name: ai-response-guard
          config:
            default_profile: default
            profiles:
              default:
                redact:
                  - pattern: '\b\d{3}-\d{2}-\d{4}\b'
                    replacement: '[SSN]'
        - name: ai-cost-tracker
          config:
            prices:
              openai/gpt-4o:             { prompt: 0.0025, completion: 0.01 }
              anthropic/claude-opus-4-6: { prompt: 0.015,  completion: 0.075 }
      x-barbacane-dispatch:
        name: ai-proxy
        config:
          default_target: primary
          targets:
            primary: { provider: openai, model: gpt-4o }
          fallback:
            - { provider: anthropic, model: claude-opus-4-6 }
```

The compiler validates the spec against each plugin's JSON schema (`vacuum:barbacane`) and seals everything into a single `.bca` artifact — including pinned plugin WASM. The data plane runs the artifact; nothing is fetched at request time.

## Documentation

Full documentation is available at **[docs.barbacane.dev](https://docs.barbacane.dev)**.

- [Getting Started](https://docs.barbacane.dev/guide/getting-started.html) — First steps with Barbacane
- [Spec Configuration](https://docs.barbacane.dev/guide/spec-configuration.html) — Configure routing and middleware via `x-barbacane-*` extensions
- [Dispatchers](https://docs.barbacane.dev/guide/dispatchers.html) — Route requests to HTTP, Lambda, S3, Kafka, NATS, LLMs, WebSocket backends
- **Middlewares** — grouped by concern:
  - [Authentication](https://docs.barbacane.dev/guide/middlewares/authentication.html) · [Authorization](https://docs.barbacane.dev/guide/middlewares/authorization.html) · [Traffic control](https://docs.barbacane.dev/guide/middlewares/traffic-control.html)
  - [Caching](https://docs.barbacane.dev/guide/middlewares/caching.html) · [Transformation](https://docs.barbacane.dev/guide/middlewares/transformation.html) · [Observability](https://docs.barbacane.dev/guide/middlewares/observability.html)
  - [AI Gateway](https://docs.barbacane.dev/guide/middlewares/ai-gateway.html) — prompt guarding, token limits, cost tracking, response redaction
- [MCP Server](https://docs.barbacane.dev/guide/mcp.html) — Expose your spec as a Model Context Protocol server
- [Control Plane](https://docs.barbacane.dev/guide/control-plane.html) · [Web UI](https://docs.barbacane.dev/guide/web-ui.html) — Manage specs, artifacts, and data planes
- [Secrets](https://docs.barbacane.dev/guide/secrets.html) · [Vacuum linting](https://docs.barbacane.dev/guide/vacuum.html) · [FIPS](https://docs.barbacane.dev/guide/fips.html)
- [Extensions reference](https://docs.barbacane.dev/reference/extensions.html) · [CLI reference](https://docs.barbacane.dev/reference/cli.html) · [Artifact format](https://docs.barbacane.dev/reference/artifact.html)
- [Plugin Development](https://docs.barbacane.dev/contributing/plugins.html) — Build custom WASM plugins
- [Development Guide](https://docs.barbacane.dev/contributing/development.html) — Setup and contribute

## Playground

Try Barbacane locally with the full-featured playground — now in its own repo:

```bash
git clone https://github.com/barbacane-dev/playground
cd playground
docker-compose up -d

# Gateway: http://localhost:8080
# Grafana: http://localhost:3000 (admin/admin)
# Control Plane: http://localhost:3001
```

The playground includes a Train Travel API demo with WireMock backend, full observability stack (Prometheus, Loki, Tempo, Grafana), and the control plane UI. See [barbacane-dev/playground](https://github.com/barbacane-dev/playground) for details.

## Official Plugins

33 production-ready plugins ship with Barbacane. They're built as WASM modules and run in a sandbox.

### Dispatchers — where the request goes

| Plugin | Description |
|--------|-------------|
| `http-upstream` | Reverse proxy to HTTP/HTTPS backends |
| `mock` | Return static responses with `{{placeholder}}` interpolation |
| `lambda` | Invoke AWS Lambda functions |
| `kafka` | Publish messages to Kafka |
| `nats` | Publish messages to NATS |
| `s3` | Proxy requests to AWS S3 / S3-compatible storage with SigV4 signing |
| `ai-proxy` | Unified LLM routing to OpenAI, Anthropic, and Ollama with provider fallback |
| `ws-upstream` | WebSocket transparent proxy with full middleware chain on upgrade |
| `fire-and-forget` | Forward request to upstream and return immediate static response |

### Middlewares — what happens on the way

| Concern | Plugins |
|---------|---------|
| **Authentication** | `jwt-auth`, `apikey-auth`, `basic-auth`, `oauth2-auth`, `oidc-auth` |
| **Authorization** | `acl`, `opa-authz`, `cel` (CEL policy + policy-driven routing) |
| **Traffic control** | `rate-limit` (sliding window), `request-size-limit`, `ip-restriction`, `bot-detection`, `redirect` |
| **Caching** | `cache` (response caching) |
| **Transformation** | `request-transformer`, `response-transformer`, `cors`, `correlation-id` |
| **Observability** | `observability` (SLO + detailed logging), `http-log` |
| **AI gateway** | `ai-prompt-guard`, `ai-token-limit`, `ai-cost-tracker`, `ai-response-guard` |

## Performance

Benchmark results on Apple M4 (MacBook Air 16GB):

**Routing & Validation**

| Operation | Latency |
|-----------|---------|
| Route lookup (1000 routes) | ~83 ns |
| Request validation (full) | ~1.2 µs |
| Body validation (JSON) | ~458 ns |
| Router build (500 routes) | ~130 µs |

**WASM Plugin Runtime**

| Operation | Latency |
|-----------|---------|
| Module compilation | ~210 µs |
| Instance creation | ~17 µs |
| Middleware chain (1 plugin) | ~261 µs |
| Middleware chain (3 plugins) | ~941 µs |
| Middleware chain (5 plugins) | ~1.32 ms |
| Memory write (1 KB) | ~14 ns |
| Memory write (100 KB) | ~1.4 µs |

**Serialization**

| Operation | Latency |
|-----------|---------|
| Request (minimal) | ~118 ns |
| Request (full, 1 KB body) | ~921 ns |
| Response (1 KB body) | ~417 ns |

**Spec Compilation**

| Operation | Latency |
|-----------|---------|
| Compile 10 operations | ~550 µs |
| Compile 50 operations | ~2.17 ms |
| Compile 100 operations | ~3.72 ms |

Run your own benchmarks:

```bash
cargo bench --workspace
```

## Project Status

Barbacane is under active development. See [ROADMAP.md](ROADMAP.md) for the roadmap and [CHANGELOG.md](CHANGELOG.md) for release history.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

Dual-licensed under AGPLv3 and a commercial license. See [LICENSING.md](LICENSING.md) for details.

## Trademark

Barbacane is a trademark. The software is open source; the brand is not. See [TRADEMARKS.md](TRADEMARKS.md) for usage guidelines.
