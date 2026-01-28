# ADR-0013: Multi-Tenancy — Single Tenant Per Instance

**Status:** Accepted
**Date:** 2026-01-28

## Context

Multi-tenancy in API gateways typically means serving multiple independent APIs or organizations from shared infrastructure. This adds complexity in routing, isolation, resource limits, and security boundaries.

Barbacane's initial scope does not require multi-tenancy. However, the architecture should not prevent it in the future.

## Decision

### Now: One Artifact, One Instance

Each data plane instance loads **exactly one compiled artifact** (ADR-0011) and serves the APIs defined within it.

```
Instance A  ←  artifact-user-api.bca
Instance B  ←  artifact-billing-api.bca
Instance C  ←  artifact-partner-api.bca
```

No shared-process multi-tenancy. This keeps the data plane simple, predictable, and free from noisy-neighbor issues.

### Future Direction: Separate Process, SNI Routing

When multi-tenancy becomes necessary, the model is **separate data plane instances per tenant**, fronted by SNI-based routing:

```
                          ┌─ api.tenant-a.com ──▶ Instance A (tenant-a.bca)
Client ──▶ [SNI Router] ─┤
                          └─ api.tenant-b.com ──▶ Instance B (tenant-b.bca)
```

- Each tenant gets its own process — full isolation (memory, CPU, secrets)
- Routing by hostname via TLS SNI (no header or path tricks)
- The SNI router is a lightweight L4 component, not Barbacane itself
- Scaling is per-tenant: high-traffic tenants get more instances

### What This Rules Out

| Approach | Why not |
|----------|---------|
| Shared process, config-separated tenants | Noisy neighbor risk, shared failure domain, complex secret isolation |
| Path-based tenancy (`/tenant-a/*`) | Pollutes API design, leaks tenancy into spec |
| Header-based tenancy (`X-Tenant-Id`) | Requires upstream cooperation, easy to spoof |

### Multiple APIs in One Artifact

A single artifact **can** contain multiple OpenAPI/AsyncAPI specs (e.g., a user API and a billing API managed by the same team). This is a single-tenant scenario with multiple specs, not multi-tenancy:

```yaml
# barbacane-control compile --specs user-api.yaml billing-api.yaml
```

Routing between specs within one artifact uses `servers` (hostnames or base paths) as defined in the OpenAPI specs themselves.

## Consequences

- **Easier:** Simple deployment model, no tenant isolation complexity, no resource quota management, no shared-state bugs
- **Harder:** Operating many small instances instead of fewer large ones — mitigated by containerization and orchestration (Kubernetes)
- **Future-proof:** The single-instance model naturally extends to per-tenant instances without architectural changes
