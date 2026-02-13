# Product Vision

## What is Barbacane?

Barbacane is a spec-driven API gateway. Your OpenAPI or AsyncAPI specification **is** your gateway configuration. No separate routing rules, no middleware YAML, no config drift — the spec you design is the spec the gateway enforces.

**Your spec is your gateway.**

## The Problem

API gateways today require maintaining two sources of truth:

1. The API specification (OpenAPI) — documentation, client SDK generation, design reviews
2. The gateway configuration — routing rules, validation, auth, rate limiting

These inevitably diverge. The spec says one thing, the gateway does another. Teams waste time reconciling differences. Bugs slip through. Security policies are inconsistent.

## The Solution

Barbacane eliminates this gap by using the spec as the single source of truth:

- **Write your OpenAPI spec** with `x-barbacane-*` extensions for gateway behavior
- **Compile it** into an optimized binary artifact
- **Deploy the artifact** to the data plane
- **Every request** is validated against your spec — automatically

If the spec says a field is required, the gateway enforces it. If the spec says an endpoint is rate-limited, it is. No extra configuration needed.

## Target Users

### Platform Teams

Teams building internal developer platforms who want:
- Consistent API standards across services
- Automatic enforcement of validation and security policies
- A single artifact to deploy and version

### API Product Companies

Organizations exposing APIs as products who need:
- Strict contract enforcement with external consumers
- Rate limiting and auth that matches the published spec
- Audit trails and compliance

### Enterprises Modernizing

Large organizations moving from legacy gateways who want:
- Spec-first development workflow
- Gradual migration path (Barbacane can proxy to existing backends)
- Modern tooling without vendor lock-in

## Core Principles

1. **Spec is the source of truth** — No shadow configuration. What's in the spec is what runs.

2. **Compile-time safety** — Most errors caught before deployment. Invalid schemas, missing dispatchers, conflicting routes — all caught at compile time.

3. **Zero runtime surprises** — If it compiles, it runs. Validation is strict and predictable.

4. **Extensible via WASM** — Custom logic via sandboxed WebAssembly plugins. Safe, portable, fast.

5. **Observable by default** — Structured logs, Prometheus metrics, OpenTelemetry traces. All built in.

## Business Model

Barbacane is fully open source under the Apache 2.0 license.

### How We Make Money

| Offering | Description |
|----------|-------------|
| **Consulting** | Architecture reviews, migration planning, custom plugin development, integration with existing infrastructure |
| **Pro Support** | SLA-backed support contracts, priority issue resolution, direct access to maintainers |
| **Training** | Workshops on spec-driven API design, Barbacane deployment, WASM plugin development |

### What Stays Free

Everything in this repository:
- The full gateway (data plane and control plane)
- All built-in plugins (auth, rate limiting, caching, etc.)
- The plugin SDK
- Documentation and specifications

There is no "enterprise edition" with gated features. The open source version is the complete product.

## Roadmap

See [ROADMAP.md](ROADMAP.md) for the prioritized roadmap.

## Get Involved

- Star the repo
- Try it out and report issues
- Contribute code (see [CONTRIBUTING.md](CONTRIBUTING.md))
- Share feedback on what would make Barbacane useful for your use case
