# Barbacane Documentation

**Barbacane** is a spec-driven API gateway. Define your API using OpenAPI, add Barbacane extensions for routing and middleware, compile to an artifact, and deploy.

## Why Barbacane?

- **Spec as config** — Your OpenAPI 3.x or AsyncAPI 3.x specification is the single source of truth. No separate gateway DSL to maintain.
- **Compile-time safety** — Misconfigurations, ambiguous routes, and missing plugins are caught at compile time, not at 3 AM.
- **Fast and predictable** — Built on Rust, Tokio, and Hyper. No garbage collector, no latency surprises.
- **Secure by default** — Memory-safe runtime, TLS via Rustls, sandboxed WASM plugins, secrets never baked into artifacts.
- **Extensible** — Write plugins in any language that compiles to WebAssembly. They run in a sandbox, so a buggy plugin can't take down the gateway.
- **Observable** — Prometheus metrics, structured JSON logging, and distributed tracing with W3C Trace Context and OTLP export.

## Quick Start

### With Docker

```bash
# Compile your OpenAPI spec
docker run --rm -v $(pwd):/work ghcr.io/barbacane-dev/barbacane \
  compile --spec /work/api.yaml --manifest /work/barbacane.yaml --output /work/api.bca

# Run the gateway
docker run --rm -p 8080:8080 -v $(pwd)/api.bca:/config/api.bca \
  ghcr.io/barbacane-dev/barbacane serve --artifact /config/api.bca --listen 0.0.0.0:8080
```

### From source

```bash
git clone https://github.com/barbacane-dev/barbacane.git
cd barbacane && cargo build --release

# Compile your OpenAPI spec
./target/release/barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Run the gateway
./target/release/barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

## Documentation

### User Guide

- [Getting Started](guide/getting-started.md) - First steps with Barbacane
- [Spec Configuration](guide/spec-configuration.md) - Configure routing and middleware in your OpenAPI spec
- [Dispatchers](guide/dispatchers.md) - Route requests to backends
- [Middlewares](guide/middlewares.md) - Add authentication, rate limiting, and more
- [Secrets](guide/secrets.md) - Manage secrets in plugin configurations
- [Observability](guide/observability.md) - Metrics, logging, and distributed tracing
- [Control Plane](guide/control-plane.md) - REST API for spec and artifact management
- [Web UI](guide/web-ui.md) - Web-based management interface

### Reference

- [CLI Reference](reference/cli.md) - Command-line tools
- [Spec Extensions](reference/extensions.md) - Complete `x-barbacane-*` reference
- [Artifact Format](reference/artifact.md) - `.bca` file format
- [Reserved Endpoints](reference/endpoints.md) - `/__barbacane/*` endpoints

### Contributing

- [Architecture](contributing/architecture.md) - System design and crate structure
- [Development Guide](contributing/development.md) - Setting up and building
- [Plugin Development](contributing/plugins.md) - Creating WASM plugins

## Supported Spec Versions

| Format | Version | Status |
|--------|---------|--------|
| OpenAPI | 3.0.x | Supported |
| OpenAPI | 3.1.x | Supported |
| OpenAPI | 3.2.x | Supported |
| AsyncAPI | 3.0.x | Supported |

### AsyncAPI Support

Barbacane supports AsyncAPI 3.x for event-driven APIs. AsyncAPI `send` operations are accessible via HTTP POST requests, enabling a sync-to-async bridge pattern where HTTP clients can publish messages to Kafka or NATS brokers.

## License

Apache 2.0 - See [LICENSE](../LICENSE)
