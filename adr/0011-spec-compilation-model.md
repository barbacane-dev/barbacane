# ADR-0011: Spec Compilation Model

**Status:** Accepted
**Date:** 2026-01-28

## Context

Barbacane's core differentiator is that OpenAPI/AsyncAPI specs are the only configuration input (ADR-0004). But parsing YAML specs at runtime on every request is unacceptable for a gateway targeting < 5ms p99 latency at the edge.

The solution is a **compilation step**: specs are processed ahead of time into an optimized artifact that the data plane loads at startup. This is the "spec compiler" referenced in ADR-0004 and ADR-0007.

## Decision

### Compiler Responsibilities

The spec compiler (`barbacane-control compile`) takes OpenAPI/AsyncAPI specs as input and produces a deployable artifact. It performs:

#### 1. Validation (fail fast)

| Check | Fails if... |
|-------|-------------|
| Spec validity | Spec doesn't conform to OpenAPI 3.x / AsyncAPI 3.x |
| Extension validity | `x-barbacane-*` extensions don't match expected schemas |
| Plugin existence | Referenced middleware or dispatcher plugin is not registered |
| Plugin config compatibility | Plugin config doesn't match the plugin's declared config schema |
| Plugin version constraints | Plugin version conflicts or unsatisfied constraints |
| Security | `http://` upstream URLs in production mode |
| Completeness | Routes without a dispatcher, security schemes without middleware mapping |

If any check fails, **compilation is rejected**. No partial artifacts.

#### 2. Optimization

| Input | Compiled to |
|-------|-------------|
| `paths` (OpenAPI) | Prefix-trie routing table (FlatBuffers) |
| `channels` (AsyncAPI) | Topic-to-handler mapping (FlatBuffers) |
| `schemas` / `parameters` | Precompiled JSON Schema validators (FlatBuffers) |
| `securitySchemes` | Route-to-auth-requirement mapping |
| `x-barbacane-middlewares` | Ordered middleware chain per route (global + overrides resolved) |
| `x-barbacane-dispatch` | Dispatcher assignment per route |

The routing table uses a prefix trie, not linear scan. Path parameters like `/users/{id}/orders/{orderId}` are compiled into trie nodes with typed capture slots.

#### 3. Bundling

All dependencies are resolved and bundled into the artifact:

- WASM plugin modules (middlewares + dispatchers)
- Compiled OPA policies (as WASM)
- Optimized routing and validation data

### Artifact Format (`.bca`)

The compiled artifact is a self-contained archive (`.bca` — Barbacane Compiled Artifact):

```
artifact.bca
├── manifest.json              # human-readable metadata
├── routes.fb                  # FlatBuffers: routing trie
├── schemas.fb                 # FlatBuffers: precompiled validators
├── middleware-chains.fb       # FlatBuffers: resolved middleware chains
├── plugins/
│   ├── jwt-auth.wasm
│   ├── rate-limit.wasm
│   └── http-upstream.wasm
└── policies/
    └── api-access.wasm
```

#### manifest.json

```json
{
  "version": "1.0.0",
  "compiled_at": "2026-01-28T14:30:00Z",
  "compiler_version": "0.1.0",
  "source_specs": [
    {
      "file": "user-api.yaml",
      "sha256": "abc123...",
      "type": "openapi",
      "version": "3.1.0"
    }
  ],
  "plugins": [
    {
      "name": "jwt-auth",
      "version": "1.2.0",
      "sha256": "def456...",
      "type": "middleware"
    },
    {
      "name": "http-upstream",
      "version": "1.0.0",
      "sha256": "ghi789...",
      "type": "dispatcher"
    }
  ],
  "routes_count": 42,
  "checksums": {
    "routes.fb": "sha256:...",
    "schemas.fb": "sha256:..."
  }
}
```

The manifest is intentionally human-readable: operators can inspect what's deployed without specialized tooling.

#### Why FlatBuffers

| Concern | FlatBuffers | JSON | MessagePack |
|---------|-------------|------|-------------|
| Deserialization | Zero-copy (read directly from buffer) | Parse into memory | Decode into memory |
| Startup time | Instant (mmap the file) | Slow for large schemas | Moderate |
| Schema evolution | Forward/backward compatible | No schema | No schema |
| Memory | No extra allocation | Full copy in heap | Full copy in heap |

At startup, the data plane memory-maps `routes.fb` and `schemas.fb` — no parsing, no allocation, instant readiness.

### Data Plane Startup Sequence

```
1. Load artifact.bca
2. Verify checksums (manifest vs actual files)
3. Memory-map FlatBuffers files (routes, schemas, middleware chains)
4. AOT-compile WASM modules (plugins + policies)
5. Fetch secrets from vault
6. Initialize plugins with config + secrets
7. Bind to port → Ready
```

### Compilation Pipeline (CI/CD)

```
┌──────────┐     ┌────────────────┐     ┌────────────┐     ┌──────────┐
│ Git push │────▶│ barbacane-control │──▶│  artifact   │────▶│  Deploy  │
│ (specs)  │     │    compile       │    │   .bca     │     │  (edge)  │
└──────────┘     └────────────────┘     └────────────┘     └──────────┘
                        │
                  Validates:                Stores in:
                  - spec conformance        - Object storage
                  - plugin compatibility    - Container registry
                  - security rules          - Artifact registry
                  - completeness
```

## Consequences

- **Easier:** Zero parsing at runtime, instant startup, full validation at build time (shift-left), inspectable artifacts, deterministic deployments
- **Harder:** Compilation adds a step to the deployment pipeline, FlatBuffers schemas must be maintained, artifact format versioning over time
- **Tradeoff:** Changes require recompilation and redeployment — no runtime config updates. This is intentional: the compilation step IS the safety net.
