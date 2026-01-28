# ADR-0005: Proxy Framework — Hyper/Tower Stack

**Status:** Accepted
**Date:** 2026-01-28

## Context

Barbacane needs a core HTTP proxy layer. Two credible options exist in the Rust ecosystem:

### Option A: Pingora (Cloudflare)

Cloudflare's open-source proxy framework, handling ~1 trillion requests/day in production.

| Pros | Cons |
|------|------|
| Battle-tested at extreme scale | Large, opinionated dependency |
| Built-in connection pooling, load balancing, TLS | Tied to Cloudflare's release cycle and design decisions |
| Proxy-specific abstractions (filter phases) | Open-sourced in 2024, community still maturing |
| Less code to write for proxy fundamentals | May conflict with our spec-driven request lifecycle |

### Option B: Hyper + Tower (community standard)

The de facto Rust HTTP stack. Hyper handles HTTP protocol, Tower provides composable middleware.

| Pros | Cons |
|------|------|
| Maximum control over request lifecycle | Must build proxy features ourselves (pooling, LB) |
| Small, composable libraries | More code to write and maintain |
| Largest Rust community, most contributors | Edge cases in proxy behavior need manual handling |
| Tower middleware composes naturally with spec validation | — |

### Key consideration

Barbacane's core differentiator is **spec-driven request processing**. Every request passes through:

```
TLS → Parse → Route (from spec) → Validate (from spec) → Auth (from spec) → Proxy → Validate response
```

This means we need **deep control over every phase of the request lifecycle**. The validation and routing layers are not bolted on — they ARE the proxy logic.

## Decision

We will use **Hyper + Tower** as the foundation, not Pingora.

The core stack:

| Layer | Crate | Role |
|-------|-------|------|
| Async runtime | `tokio` | Event loop, I/O, timers |
| HTTP protocol | `hyper` | HTTP/1.1 and HTTP/2 parsing |
| TLS | `rustls` | TLS termination (no OpenSSL dependency) |
| Middleware | `tower` | Service composition, timeouts, rate limiting |
| Routing | Custom | Generated from OpenAPI specs at compile time |
| Validation | Custom | Generated from JSON Schema at compile time |
| Connection pool | Custom or `hyper-util` | Upstream connection management |

### Why not Pingora

Pingora is built for **programmable proxying** — intercepting and modifying traffic between clients and backends. Barbacane is built for **spec enforcement** — the proxy behavior is a consequence of the spec, not the other way around.

Using Pingora would mean fitting our spec-driven model into Pingora's filter-phase model, adding an abstraction layer that fights our architecture rather than serving it.

### HTTP/3 (QUIC)

Not required for initial release. HTTP/1.1 + HTTP/2 via Hyper covers the primary use cases. HTTP/3 can be added later via `quinn` (Rust QUIC implementation) without architectural changes.

### gRPC

Out of scope. Barbacane focuses on REST (OpenAPI) and event-driven (AsyncAPI) APIs.

## Consequences

- **Easier:** Full control over request lifecycle, natural integration with spec-driven validation, smaller binary, no large external dependency
- **Harder:** Must implement connection pooling, load balancing, and upstream health checks ourselves
- **Mitigated by:** `hyper-util` and `tower` crates provide building blocks for these features — we compose rather than build from scratch
