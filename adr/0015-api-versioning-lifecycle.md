# ADR-0015: API Versioning & Lifecycle

**Status:** Accepted
**Date:** 2026-01-28

## Context

APIs evolve. Routes get added, modified, deprecated, and removed. In a spec-driven gateway, versioning and lifecycle are inherently tied to the spec itself — but a few questions need explicit answers:

- How are breaking changes handled?
- How do consumers know a route is deprecated?
- Can multiple versions of an API coexist?

## Decision

### Lean on OpenAPI — Don't Reinvent

OpenAPI already has mechanisms for versioning and deprecation. Barbacane enforces them rather than adding its own layer.

#### Versioning: Spec Author's Choice

Barbacane does **not** impose a versioning strategy. Common patterns, all supported:

| Strategy | How it works in the spec |
|----------|-------------------------|
| URL path | `/v1/users`, `/v2/users` — separate paths in spec |
| Header | `Accept: application/vnd.api.v2+json` — content negotiation |
| Separate specs | `user-api-v1.yaml`, `user-api-v2.yaml` — compiled into separate or same artifact |

This is a spec concern, not a gateway concern. Barbacane routes what the spec declares.

#### Deprecation: OpenAPI `deprecated` Field

OpenAPI supports `deprecated: true` on operations. Barbacane makes this observable:

```yaml
paths:
  /users/{id}/legacy-profile:
    get:
      deprecated: true
      summary: Use /users/{id}/profile instead
```

Gateway behavior for deprecated routes:

- **Request is still served** — deprecated does not mean removed
- **Response includes `Sunset` header** (RFC 8594) if configured
- **Metric emitted**: `barbacane_deprecated_route_requests_total`
- **Log emitted**: structured log with deprecation warning

```yaml
paths:
  /users/{id}/legacy-profile:
    get:
      deprecated: true
      x-sunset: "Sat, 01 Jun 2026 00:00:00 GMT"
```

Produces response header:
```
Sunset: Sat, 01 Jun 2026 00:00:00 GMT
```

#### Removal: Just Remove From Spec

When a route is removed from the spec:
- The compiler produces an artifact without that route
- Requests to the removed route get `404` (ADR-0012)
- No special "removed" status — absence is removal

### What Barbacane Does NOT Do

| Concern | Why not |
|---------|---------|
| Automatic version negotiation | Spec author's responsibility |
| Request transformation between versions | Gateway is not an orchestrator |
| Parallel version routing (v1 → service-a, v2 → service-b) | Handled by declaring different dispatchers per path |
| Semantic versioning enforcement | Out of scope — specs are versioned by Git |

## Consequences

- **Easier:** No new concepts to learn — uses standard OpenAPI fields, Git handles version history, deprecation is observable via metrics and headers
- **Harder:** No built-in migration tooling between spec versions
- **Tradeoff:** Minimal opinion — this ADR mostly says "use what OpenAPI gives you." That's intentional: Barbacane adds value by enforcing the spec, not by reinventing version management.
