# ADR-0004: OpenAPI and AsyncAPI as Single Source of Truth

**Status:** Accepted
**Date:** 2026-01-28

## Context

Traditional API gateways require configuration in their own DSL or format, leading to:

- **Drift** between API specs and gateway config
- **Duplication** of route definitions, validation rules, security schemes
- **Vendor lock-in** to proprietary configuration formats

We want the gateway to be **spec-native**: OpenAPI 3.x for synchronous APIs, AsyncAPI 3.x for asynchronous/event-driven APIs. The specs are not documentation — they are the executable configuration.

## Decision

### Core Principle

**OpenAPI and AsyncAPI specifications are the only input for API configuration.** No proprietary gateway DSL.

### Configuration Flow (GitOps)

```
┌─────────────┐      ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
│  Git Repo   │──CI──│  Compiler   │─────▶│  Artifact   │─────▶│   Gateway   │
│  (specs)    │      │  (validate  │      │  (optimized │      │   (deploy)  │
│             │      │   + build)  │      │   config)   │      │             │
└─────────────┘      └─────────────┘      └─────────────┘      └─────────────┘
```

- Specs live in Git (versioned, auditable, reviewable)
- CI pipeline validates and compiles specs into optimized gateway config
- Compiled artifacts deployed to gateway instances
- No runtime spec parsing in hot path

### Validation Behavior

**Strict by default:** Requests not conforming to the OpenAPI spec are rejected with `400 Bad Request`. The gateway acts as a contract enforcer, not just a proxy.

- Path parameters: validated against schema
- Query parameters: validated against schema
- Request body: validated against JSON Schema
- Headers: required headers enforced
- Content-Type: must match spec's `requestBody.content`

### Async Protocol Support

AsyncAPI specs configure event-driven APIs with a **protocol-agnostic adapter layer**:

| Protocol | Priority | Use Case |
|----------|----------|----------|
| Kafka | Primary | Cloud-native event streaming |
| NATS | Primary | Lightweight, edge-friendly messaging |
| MQTT | Secondary | IoT, constrained devices |
| AMQP | Secondary | Enterprise integration |
| WebSocket | Secondary | Client-facing real-time |
| SSE | Secondary | Server push to browsers |

### What Specs Control

| Concern | OpenAPI | AsyncAPI |
|---------|---------|----------|
| Routing | `paths`, `servers` | `channels`, `servers` |
| Validation | `schemas`, `parameters` | `schemas`, `messages` |
| Security | `securitySchemes` | `securitySchemes` |
| Rate limiting | `x-barbacane-ratelimit` extension | `x-barbacane-ratelimit` extension |
| Dispatch | `servers`, `x-barbacane-dispatch` | `servers`, bindings |

### Gateway-Specific Extensions

Where OpenAPI/AsyncAPI lack expressiveness, we use `x-barbacane-*` vendor extensions:

```yaml
x-barbacane-ratelimit:
  quota: 100
  window: 60
  key: header:x-api-key

x-barbacane-cache:
  ttl: 60s
  vary: [Accept, Authorization]

x-barbacane-dispatch:
  name: http-upstream
  config:
    timeout: 5s
    retries: 2
    circuit-breaker:
      threshold: 5
      window: 30s
```

## Consequences

- **Easier:** Single source of truth, no config drift, API-first development, standard tooling (Swagger UI, Redoc, etc.)
- **Harder:** Must extend specs for gateway-specific features, compilation step adds deployment complexity
- **Tradeoff:** No hot-reload in production (by design — changes go through CI/CD)
