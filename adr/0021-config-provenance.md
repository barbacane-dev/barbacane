# ADR-0021: Spec-to-Run configuration provenance and drift detection

**Status:** Proposed
**Date:** 2026-02-20

## Context

Barbacane’s architecture compiles human-readable configuration files (OpenAPI specs, routing rules) into an optimized binary artifact (`.bca`) which is then loaded by the Data Plane (`barbacane serve`). 

Currently, once a Data Plane is running, there is no cryptographic guarantee or programmatic way to verify exactly *which* specification it is executing. In highly regulated enterprise environments (Zero Trust, SOC2, PCI-DSS compliance), operators require a verifiable "Trust Chain". They need to answer:
1. Is the gateway running the exact configuration that was approved in Git?
2. Has the configuration drifted or been tampered with?

We need a standardized way to fingerprint configurations at build time and verify them at runtime without disrupting the existing network topology.

## Decision

We will implement an end-to-end configuration provenance system. This will be broken down into four architectural pillars:

### 1. Build-time artifact fingerprinting

The `barbacane compile` command will be updated to automatically calculate a cryptographic hash of **all inputs** that make up the artifact. To ensure full supply chain security, this hash will cover:
- All OpenAPI specifications and Barbacane YAML/JSON configuration files.
- **All referenced WASM plugin binaries** bundled into the build.

This will likely be implemented as a Merkle tree root or a hash of a sorted manifest of all input files to guarantee determinism.

**Embedded metadata:** The resulting `.bca` file format will be updated to include a metadata header containing:
- `artifact_hash`: The combined SHA-256 hash of the input configuration files and WASM binaries. 
- `build_timestamp`: UTC timestamp of the compilation.
- Optional injected metadata via CLI flags (e.g., `--provenance-commit=a1b2c3d`, `--provenance-source=s3://bucket/config.zip`).

### 2. Local provenance API (Data Plane)

The Data Plane (`barbacane`) will expose a local administration endpoint to query this metadata for local observability and debugging.
- **Endpoint:** `GET /_admin/provenance` 
- **Response:** JSON payload containing the metadata extracted from the currently loaded `.bca` artifact's header.

**Dependency note:** Barbacane currently binds a single port for user traffic. Introducing a dedicated admin listener (with its own CLI flags, TLS config, and safe bind addresses) is a non-trivial prerequisite. The implementation of this endpoint will be deferred until the underlying admin interface is designed in a separate ADR (e.g., *ADR-0022: Admin API Listener*).

### 3. Drift detection via WebSocket (Control Plane)

To respect the existing network topology (where Data Planes sit behind NATs/firewalls), drift detection will **not** rely on the Control Plane polling the Data Planes via HTTP. Instead, it will piggyback on the existing Data Plane-to-Control Plane WebSocket channel.

- **Telemetry push:** When a Data Plane connects to `ws://control-plane/ws/data-plane`, it will include its currently loaded `artifact_hash` in the initial connection payload, and in all subsequent periodic heartbeat messages.
- **Verification:** The Control Plane acts as the source of truth. Upon receiving the heartbeat, it will compare the reported `artifact_hash` against the `desired_hash` for that specific node or cluster.
- **Alerting:** If the hashes do not match, the Control Plane will flag the node with a `ConfigurationDrift` status, triggering native alerts (via logs, metrics, or webhooks).

### 4. OCI image & SBOM supply chain integration

When Barbacane artifacts are packaged into container images, the build tooling will extract the `artifact_hash` and provenance metadata to:
- Inject them as standard OCI image labels (e.g., `org.opencontainers.image.revision`).
- Include the configuration artifact and WASM plugins as verified components in the container's SBOM (Software Bill of Materials).

## Consequences

### Positive

- **Conformity & auditability:** Provides cryptographic proof of what is running, satisfying strict audit requirements.
- **Network friendly:** Using the existing WebSocket channel for telemetry avoids the need for a reverse connectivity path from the Control Plane to the Data Planes.
- **Observability:** Operators can instantly see if a deployment failed to roll out properly.
- **Security:** Detects unauthorized out-of-band changes or tampering with the `.bca` file or its embedded executable plugins directly on the server.

### Negative

- **Artifact format change:** This requires a breaking change to the internal `.bca` binary format to support a metadata header. We must ensure backward compatibility or bump the artifact version.
- **Prerequisite required:** The local `/_admin/provenance` HTTP endpoint cannot be shipped until the Admin API listener architecture (ADR-0022) is finalized.

### Alternatives considered

- **Control Plane HTTP Polling:** Initially considered having the Control Plane poll the Data Planes. Rejected because the Control Plane has no way to initiate HTTP requests back to Data Plane instances, which would break deployments where Data Planes are isolated behind firewalls.
- **Hashing the `.bca` file directly:** Rejected. The compilation process might introduce non-deterministic byte ordering depending on the OS/Architecture where `barbacane compile` is run. Hashing the *source* files and binaries provides a true, reproducible representation of the user's intent.

## Related ADRs

- [ADR-0007: Control Plane / Data Plane Separation](0007-control-data-plane-separation.md) — Aligns with the push-model telemetry from isolated Data Planes to the central Control Plane.
- [ADR-0019: Packaging and Release Strategy](0019-packaging-and-release-strategy.md) — Aligns with the supply chain security goals (SBOMs, SLSA).
- **[Pending] ADR-0025: Admin API listener** — Required prerequisite for exposing the local HTTP `/_admin/provenance` endpoint.