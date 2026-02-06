<p align="center">
  <img src="assets/img/barbacane-icon_black.png" alt="Barbacane" width="200">
</p>

<h1 align="center">Barbacane</h1>

<p align="center"><i>Your spec is your gateway.</i></p>

<p align="center">
  <a href="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml"><img src="https://github.com/barbacane-dev/barbacane/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://docs.barbacane.dev"><img src="https://img.shields.io/badge/docs-docs.barbacane.dev-blue" alt="Documentation"></a>
  <img src="https://img.shields.io/badge/tests-332%20passing-brightgreen" alt="Tests">
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

Full documentation is available at **[docs.barbacane.dev](https://docs.barbacane.dev)**.

- [Getting Started](https://docs.barbacane.dev/guide/getting-started.html) — First steps with Barbacane
- [Spec Configuration](https://docs.barbacane.dev/guide/spec-configuration.html) — Configure routing and middleware
- [Middlewares](https://docs.barbacane.dev/guide/middlewares.html) — Authentication, rate limiting, caching
- [Dispatchers](https://docs.barbacane.dev/guide/dispatchers.html) — Route requests to backends
- [Control Plane](https://docs.barbacane.dev/guide/control-plane.html) — REST API for spec and artifact management
- [Web UI](https://docs.barbacane.dev/guide/web-ui.html) — Web-based management interface
- [Plugin Development](https://docs.barbacane.dev/contributing/plugins.html) — Build custom WASM plugins
- [Development Guide](https://docs.barbacane.dev/contributing/development.html) — Setup and contribute

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

## Trademark

Barbacane is a trademark. The software is open source; the brand is not. See [TRADEMARKS.md](TRADEMARKS.md) for usage guidelines.
