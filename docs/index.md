# Barbacane Documentation

**Barbacane** is a spec-driven API gateway. Define your API using OpenAPI, add Barbacane extensions for routing and middleware, compile to an artifact, and deploy.

## Why Barbacane?

- **Spec-first**: Your OpenAPI spec is the source of truth
- **Compile-time validation**: Catch misconfigurations before deployment
- **Plugin architecture**: Extend with WASM plugins for auth, rate limiting, transforms
- **Observable**: Prometheus metrics, structured JSON logging, distributed tracing with OTLP export
- **European-made**: Built in Europe, hosted on EU infrastructure

## Quick Start

```bash
# Build from source (crates.io publishing planned for v1.0)
git clone https://github.com/barbacane/barbacane.git
cd barbacane && cargo build --release

# Add x-barbacane-dispatch to your OpenAPI spec
# Compile
./target/release/barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Run
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
| OpenAPI | 3.2.x | Supported (draft) |
| AsyncAPI | 3.0.x | Supported |

### AsyncAPI Support

Barbacane supports AsyncAPI 3.x for event-driven APIs. AsyncAPI `send` operations are accessible via HTTP POST requests, enabling a sync-to-async bridge pattern where HTTP clients can publish messages to Kafka or NATS brokers.

## License

Apache 2.0 - See [LICENSE](../LICENSE)
