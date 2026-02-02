<p align="center">
  <img src="assets/img/barbacane-icon_black.png" alt="Barbacane" width="200">
</p>

<h1 align="center">Barbacane</h1>

<p align="center"><i>Your spec is your gateway.</i></p>

<p align="center">
  <a href="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml"><img src="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/tests-255%20passing-brightgreen" alt="Tests">
  <img src="https://img.shields.io/badge/rust-1.75%2B-orange" alt="Rust Version">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache%202.0-blue" alt="License"></a>
</p>

---

Barbacane is a spec-driven API gateway built in Rust. Point it at an OpenAPI or AsyncAPI spec and it becomes your gateway — routing, validation, authentication, and all. No proprietary config language, no drift between your spec and your infrastructure.

- **Spec as config** — Your OpenAPI 3.x or AsyncAPI 3.x specification is the single source of truth. No separate gateway DSL to maintain.
- **Fast and predictable** — Built on Rust, Tokio, and Hyper. No garbage collector, no latency surprises.
- **Secure by default** — Memory-safe runtime, TLS via Rustls, sandboxed WASM plugins, secrets never baked into artifacts.
- **Edge-ready** — Stateless data plane instances designed to run close to your users, with a separate control plane handling compilation and distribution.
- **Extensible** — Write plugins in any language that compiles to WebAssembly. They run in a sandbox, so a buggy plugin can't take down the gateway.
- **Observable** — Prometheus metrics, structured JSON logging, and distributed tracing with W3C Trace Context and OTLP export.

## Quick Start

```bash
# Clone and build
git clone https://github.com/barbacane-dev/barbacane.git
cd barbacane
cargo build --release

# Compile your OpenAPI spec
./target/release/barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Run the gateway
./target/release/barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

## Documentation

- [Getting Started](docs/guide/getting-started.md) — First steps with Barbacane
- [Spec Configuration](docs/guide/spec-configuration.md) — Configure routing and middleware
- [Middlewares](docs/guide/middlewares.md) — Authentication, rate limiting, caching
- [Dispatchers](docs/guide/dispatchers.md) — Route requests to backends
- [Plugin Development](docs/contributing/plugins.md) — Build custom WASM plugins
- [Architecture](docs/contributing/architecture.md) — System design overview

## Official Plugins

| Plugin | Type | Description |
|--------|------|-------------|
| `http-upstream` | Dispatcher | Reverse proxy to HTTP/HTTPS backends |
| `mock` | Dispatcher | Return static responses |
| `lambda` | Dispatcher | Invoke AWS Lambda functions |
| `jwt-auth` | Middleware | JWT token validation |
| `apikey-auth` | Middleware | API key authentication |
| `oauth2-auth` | Middleware | OAuth2 token introspection |
| `rate-limit` | Middleware | Sliding window rate limiting |
| `cache` | Middleware | Response caching |
| `cors` | Middleware | CORS header management |

## Performance

Benchmark results on Apple M4 (MacBook Air 16GB):

| Operation | Latency |
|-----------|---------|
| Route lookup (1000 routes) | ~83 ns |
| Request validation (full) | ~1.2 µs |
| Body validation (JSON) | ~458 ns |
| Router build (500 routes) | ~130 µs |

Run your own benchmarks:

```bash
cargo bench --workspace
```

## Project Status

Barbacane is under active development. See [MILESTONES.md](MILESTONES.md) for the roadmap and [CHANGELOG.md](CHANGELOG.md) for release history.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

Apache 2.0 — see [LICENSE](LICENSE) for details.
