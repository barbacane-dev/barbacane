# ADR-0002: Rust as Core Implementation Language

**Status:** Accepted
**Date:** 2026-01-28

## Context

We are building an API gateway targeting edge/CDN deployment with a p99 latency budget of 1-5ms for gateway overhead. This constrains our language choice significantly:

- **Edge deployment** requires small binaries, minimal dependencies, and low memory footprint
- **Predictable latency** rules out garbage-collected runtimes (JVM, Go, Node.js) where GC pauses can spike p99
- **Security by design** is a core requirement — memory safety vulnerabilities are a leading cause of CVEs in network infrastructure

Alternatives considered:

| Language | Pros | Cons |
|----------|------|------|
| **Go** | Simple, fast compilation, good concurrency | GC pauses (1-10ms), larger binaries |
| **C/C++** | Maximum performance, mature ecosystem | Memory safety issues, slower development |
| **Java (GraalVM Native)** | Enterprise ecosystem, AOT compilation | Still larger footprint, less edge-friendly |
| **Zig** | Performance, safety | Immature ecosystem, limited libraries |

## Decision

We will use **Rust** as the primary implementation language for both data plane and control plane components.

Key enablers:
- **Tokio** for async runtime
- **Hyper** for HTTP primitives
- **Tower** for middleware composition
- **Serde** for serialization

## Consequences

- **Easier:** Achieving latency targets, edge deployment, memory safety guarantees, single-language codebase
- **Harder:** Hiring (smaller talent pool), steeper learning curve, longer initial development time
- **Mitigation:** Invest in good documentation, leverage WASM for plugins (allows other languages for extensions)
