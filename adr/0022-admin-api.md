# ADR-022: Dedicated listening server for the admin API

**Status:** Proposed
**Date:** 2026-02-25

## Context

Currently, the Barbacane data plane (`barbacane serve`) only opens a single network port, which is intended to receive public user traffic. 

However, we have an increasing need to expose internal endpoints for observability and maintenance. For example:
- Health checks and readiness probes.
- Prometheus metrics (`/metrics`).
- Configuration provenance data (`/_admin/provenance`, see ADR-0024).
- Profiling or debugging tools (pprof).

Exposing these sensitive routes on the same port as public traffic represents a major security risk. Even with strict routing rules, a simple configuration error could expose metrics or internal gateway data to the internet.

## Decision

We will introduce a dedicated listening server (a separate listener) solely for administration and observability requests on the data plane.

1. **Default port and interface:** The admin API will listen on `127.0.0.1:8081` by default. Restricting it to `localhost` ensures it is not accidentally exposed during local tests or basic deployments.

2. **CLI configuration:** We will add a new explicit parameter to configure this interface. For example: `--admin-bind 0.0.0.0:8081`. 

3. **Strict routing separation:**
   The main router (which processes user traffic based on OpenAPI specs) will completely ignore administration routes. The admin server will have its own minimalist, hardcoded router, independent of the dynamic configuration lifecycle.

## Consequences

### Positive

- **Security by default:** Internal APIs are physically isolated (at the network socket level) from public traffic. No OpenAPI specification error can accidentally expose `/metrics` or `/_admin/provenance`.
- **Foundation for the future:** This unblocks the implementation of essential features (like the provenance verification in ADR-0024) without compromising security.
- **Kubernetes best practice:** This allows operators to configure their probes (liveness/readiness) and metrics scrapers on a distinct internal port, which is the industry standard (similar to Envoy or Traefik).

### Negative

- **Deployment complexity:** Users deploying Barbacane in containers (Docker/Kubernetes) will need to remember to expose an additional port if they want to access metrics from outside the container.
- **Resources:** Running a second HTTP server consumes slightly more memory and system resources, although the impact is negligible in Rust.

## Alternatives considered

- **IP-based filtering on the main port:** Rejected. Filtering requests to `/metrics` by checking the source IP on the public port is fragile, complex to maintain, and vulnerable to IP spoofing or upstream proxy misconfigurations.
- **Dynamic admin configuration via the spec:** Rejected. The admin API must remain available even if the user-provided specification is invalid or corrupted. It must therefore be configured via the CLI or environment variables, not via the `.bca` file.