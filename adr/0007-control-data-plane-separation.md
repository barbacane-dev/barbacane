# ADR-0007: Control Plane / Data Plane Separation

**Status:** Accepted
**Date:** 2026-01-28

## Context

API gateways operate at two distinct levels:

- **Data plane:** The hot path — receives requests, validates, routes, proxies. Must be fast, small, and reliable.
- **Control plane:** The management layer — ingests specs, compiles config, provides admin visibility.

These have fundamentally different requirements:

| Concern | Data Plane | Control Plane |
|---------|-----------|--------------|
| Latency | p99 < 5ms | Not critical |
| Binary size | Minimal (edge deployment) | Can be larger |
| Dependencies | As few as possible | Database, storage, etc. |
| State | Stateless (loaded config only) | Stateful (specs, history, status) |
| Scaling | Horizontal, many instances | Few instances, or single |

## Decision

### Separate Processes

The control plane and data plane are **separate binaries** deployed independently.

```
┌─────────────────────────────────────────────────────────────────┐
│                        Control Plane                            │
│                     (barbacane-control)                          │
│                                                                 │
│  ┌──────────┐  ┌───────────┐  ┌──────────┐  ┌──────────────┐   │
│  │ Admin API│  │   Spec    │  │ Artifact │  │   Config DB  │   │
│  │  (REST)  │  │ Compiler  │  │  Store   │  │  (Postgres)  │   │
│  └──────────┘  └───────────┘  └──────────┘  └──────────────┘   │
│                                                                 │
└────────────────────────┬────────────────────────────────────────┘
                         │
                    CI/CD pushes
                   compiled artifacts
                         │
        ┌────────────────┼────────────────┐
        ▼                ▼                ▼
┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│  Data Plane  │ │  Data Plane  │ │  Data Plane  │
│  (barbacane) │ │  (barbacane) │ │  (barbacane) │
│              │ │              │ │              │
│  Edge Node 1 │ │  Edge Node 2 │ │  Edge Node N │
└──────────────┘ └──────────────┘ └──────────────┘
```

### Data Plane (`barbacane`)

- Single static binary, no runtime dependencies
- Loads compiled spec artifact at startup
- Stateless — all behavior derived from the artifact
- No connection to control plane at runtime
- Exposes health/metrics endpoints only

### Control Plane (`barbacane-control`)

- Separate binary with richer dependencies (Postgres, object storage)
- Ingests OpenAPI/AsyncAPI specs
- Validates and compiles specs into optimized data plane artifacts
- Exposes REST admin API for:
  - Spec management (CRUD, versioning, history)
  - Deployment status across data plane instances
  - Metrics aggregation and dashboards
  - Extension/plugin management

### Coordination Model: GitOps Only

Data plane instances do **not** coordinate with each other or with the control plane at runtime.

```
Spec change → Git push → CI pipeline → Control plane compiles → Artifact stored → CD deploys to edge
```

- No service mesh, no runtime config distribution
- All instances are identical — same artifact, same behavior
- Rollback = deploy previous artifact
- Canary/blue-green at the deployment layer, not the gateway layer

### Admin API

The control plane exposes a REST API (itself described by an OpenAPI spec):

- `POST /specs` — submit a new OpenAPI/AsyncAPI spec
- `GET /specs` — list managed specs
- `POST /specs/{id}/compile` — compile spec into data plane artifact
- `GET /artifacts` — list compiled artifacts
- `GET /status` — deployment status across data plane instances
- `GET /metrics` — aggregated gateway metrics

A CLI tool (`barbacane-cli`) wraps this API for terminal workflows.

## Consequences

- **Easier:** Data plane stays minimal and edge-friendly, independent scaling, simple mental model, rollback is trivial
- **Harder:** No real-time config updates (by design), metrics aggregation requires separate pipeline
- **Tradeoff:** Deployment latency for config changes (spec change → CI/CD → deploy), acceptable for production use given the security and reliability benefits
