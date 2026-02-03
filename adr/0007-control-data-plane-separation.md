# ADR-0007: Control Plane / Data Plane Separation

**Status:** Implemented
**Date:** 2026-01-28
**Updated:** 2026-02-02
**Implemented:** 2026-02-02 (M12)

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

### Data Plane (`barbacane`)

- Single static binary, no runtime dependencies
- Loads compiled spec artifact at startup
- Stateless — all behavior derived from the artifact
- Exposes health/metrics endpoints only
- **Two operating modes:** standalone or connected (see below)

### Control Plane (`barbacane-control`)

- Separate binary with richer dependencies (Postgres, object storage)
- Ingests OpenAPI/AsyncAPI specs
- Validates and compiles specs into optimized data plane artifacts
- Exposes REST admin API and WebSocket endpoint for connected data planes
- Web UI for project and gateway management

---

## Coordination Models

### Mode 1: Standalone (Default)

Data plane runs independently with no control plane connection. This is the simplest deployment model.

```
# Just run with an artifact
barbacane serve --artifact api.bca
```

- No runtime dependencies
- Artifact loaded at startup, hot-reload via file watch or SIGHUP
- Suitable for edge deployments, CI/CD pipelines, air-gapped environments
- Deployment via GitOps, Kubernetes, or any standard method

```
Spec change → Git push → CI pipeline → Control plane compiles → Artifact stored → CD deploys to edge

┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│  Data Plane  │ │  Data Plane  │ │  Data Plane  │
│  (barbacane) │ │  (barbacane) │ │  (barbacane) │
│              │ │              │ │              │
│  Standalone  │ │  Standalone  │ │  Standalone  │
└──────────────┘ └──────────────┘ └──────────────┘
      │                │                │
      └────────────────┴────────────────┘
                       │
              Artifacts deployed via
              CI/CD, K8s, file sync, etc.
```

### Mode 2: Connected (Optional)

Data plane connects to control plane for centralized management. Useful for:
- Real-time deployment visibility
- One-click artifact deployment from UI
- Centralized monitoring of gateway fleet

```
# Connect to control plane
barbacane serve --artifact api.bca \
                --control-plane http://control:8080 \
                --project-id <uuid> \
                --api-key <key>
```

Architecture:

```
┌─────────────────────────────────────────────────────────────────┐
│                        Control Plane                            │
│                     (barbacane-control)                         │
│                                                                 │
│  ┌──────────┐  ┌───────────┐  ┌──────────┐  ┌──────────────┐   │
│  │ Admin API│  │ WebSocket │  │ Artifact │  │   Config DB  │   │
│  │  (REST)  │  │  Server   │  │  Store   │  │  (Postgres)  │   │
│  └──────────┘  └───────────┘  └──────────┘  └──────────────┘   │
│                      ▲                                          │
└──────────────────────┼──────────────────────────────────────────┘
                       │
         Persistent WebSocket connections
         (heartbeat, status, artifact notifications)
                       │
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
┌──────────────┐ ┌──────────────┐ ┌──────────────┐
│  Data Plane  │ │  Data Plane  │ │  Data Plane  │
│  (barbacane) │ │  (barbacane) │ │  (barbacane) │
│              │ │              │ │              │
│  Connected   │ │  Connected   │ │  Connected   │
└──────────────┘ └──────────────┘ └──────────────┘
```

#### Connection Protocol

1. **Registration:** Data plane connects via WebSocket, authenticates with API key
2. **Heartbeat:** Regular ping/pong to detect connection health
3. **Status reporting:** Data plane reports current artifact version, health metrics
4. **Artifact notification:** Control plane notifies when new artifact is available
5. **Pull model:** Data plane pulls artifact from control plane (not pushed)

#### Control Plane Responsibilities

- Track connected data planes per project
- Store connection state: `data_planes` table with status, last_seen, artifact_id
- Broadcast artifact availability to connected data planes
- Expose UI showing connected gateways, their versions, health

#### Data Plane Behavior (Connected Mode)

- Establish WebSocket connection on startup
- Send heartbeat every 30 seconds
- On artifact notification: download new artifact, hot-reload
- Graceful degradation: if connection lost, continue serving with current artifact
- Reconnect with exponential backoff

---

## Admin API

The control plane exposes a REST API:

**Spec & Project Management:**
- `POST /projects` — create a new project
- `GET /projects` — list projects
- `POST /projects/{id}/specs` — upload spec to project
- `POST /specs/{id}/compile` — compile spec into artifact
- `GET /artifacts/{id}/download` — download compiled artifact

**Data Plane Management (Connected Mode):**
- `GET /projects/{id}/data-planes` — list connected data planes
- `GET /projects/{id}/data-planes/{dpId}` — get data plane details
- `DELETE /projects/{id}/data-planes/{dpId}` — disconnect a data plane
- `POST /projects/{id}/deploy` — trigger deployment to all connected data planes

**WebSocket:**
- `WS /ws/data-plane` — data plane connection endpoint

---

## Database Schema (Connected Mode)

```sql
CREATE TABLE data_planes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name            TEXT,                          -- Optional friendly name
    artifact_id     UUID REFERENCES artifacts(id), -- Currently deployed artifact
    status          TEXT NOT NULL DEFAULT 'offline', -- online, offline, deploying
    last_seen       TIMESTAMPTZ,
    connected_at    TIMESTAMPTZ,
    metadata        JSONB DEFAULT '{}',            -- Version, hostname, etc.
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_data_planes_project ON data_planes(project_id);
CREATE INDEX idx_data_planes_status ON data_planes(status);
```

---

## Consequences

### Standalone Mode
- **Pros:** Zero runtime dependencies, edge-friendly, works anywhere
- **Cons:** No visibility into running gateways from control plane

### Connected Mode
- **Pros:** Real-time visibility, one-click deployment, centralized fleet management
- **Cons:** Requires network connectivity to control plane, additional complexity

### Design Principles
- Standalone is always the default — connected mode is opt-in
- Data planes are always functional without control plane (graceful degradation)
- Control plane never pushes artifacts directly — data planes pull
- No service mesh, no sidecar — just a simple WebSocket connection

---

## Implementation Notes (M12)

The connected mode was fully implemented in Milestone 12:

**Control Plane (`barbacane-control`):**
- WebSocket endpoint at `/ws/data-plane`
- API key authentication with `bbk_` prefix and SHA-256 hashing
- `ConnectionManager` tracking active WebSocket connections via DashMap
- REST endpoints for data plane and API key management
- Deploy tab in UI showing connected data planes

**Data Plane (`barbacane`):**
- `--control-plane`, `--project-id`, `--api-key` CLI flags
- WebSocket client with exponential backoff reconnection (1s to 60s)
- 30-second heartbeat interval
- Artifact notification handling (hot-reload not yet implemented)

**Database:**
- `data_planes` table for connection tracking
- `api_keys` table with scopes, expiration, and revocation support
