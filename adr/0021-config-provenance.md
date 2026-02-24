# ADR-0021: Spec-to-Run configuration provenance and drift detection

**Status:** Proposed
**Date:** 2026-02-20

## Context

Barbacane’s architecture compiles human-readable configuration files (OpenAPI specs, routing rules) into an optimized binary artifact (`.bca`) which is then loaded by the Data Plane (`barbacane serve`). 

Currently, once a Data Plane is running, there is no cryptographic guarantee or programmatic way to verify exactly *which* specification it is executing. In highly regulated enterprise environments (Zero Trust, SOC2, PCI-DSS compliance), operators require a verifiable "Trust Chain". They need to answer:
1. Is the gateway running the exact configuration that was approved in Git?
2. Has the configuration drifted or been tampered with?

We need a standardized way to fingerprint configurations at build time and verify them at runtime.

## Decision

We will implement an end-to-end configuration provenance system. This will be broken down into four architectural pillars:

### 1. Build-Time Artifact Fingerprinting

The `barbacane compile` command will be updated to automatically calculate a cryptographic hash of **all inputs** that make up the artifact. To ensure full supply chain security, this hash will cover:
- All OpenAPI specifications and Barbacane YAML/JSON configuration files.
- **All referenced WASM plugin binaries** bundled into the build.

This will likely be implemented as a Merkle tree root or a hash of a sorted manifest of all input files to guarantee determinism.

**Embedded Metadata:** The resulting `.bca` file format will be updated to include a metadata header containing:
- `artifact_hash`: The combined SHA-256 hash of the input configuration files and WASM binaries. 
- `build_timestamp`: UTC timestamp of the compilation.
- Optional injected metadata via CLI flags (e.g., `--provenance-commit=a1b2c3d`, `--provenance-source=s3://bucket/config.zip`).

### 2. Runtime Provenance API (Data Plane)

The Data Plane (`barbacane`) will expose a new internal administration endpoint to query this metadata.
- **Endpoint:** `GET /_admin/provenance` 
- **Binding:** Available only on the dedicated admin port/interface (not exposed to the public internet alongside user traffic).
- **Response:** JSON payload containing the metadata extracted from the currently loaded `.bca` artifact's header.

### 3. Drift Detection (Control Plane)

The Control Plane (`barbacane-control`) will act as the source of truth for the *desired state*.
- A background worker will periodically poll the `/_admin/provenance` endpoint of all registered Data Plane nodes.
- If the `actual_hash` from the Data Plane does not match the `desired_hash` known to the Control Plane, the Control Plane will flag the node with a `ConfigurationDrift` status.
- This status will trigger native alerts (via logs, metrics, or webhooks) to notify operators.

### 4. OCI Image & SBOM Supply Chain Integration

When Barbacane artifacts are packaged into container images, the build tooling will extract the `artifact_hash` and provenance metadata to:
- Inject them as standard OCI image labels (e.g., `org.opencontainers.image.revision`).
- Include the configuration artifact and WASM plugins as verified components in the container's SBOM (Software Bill of Materials).

## Consequences

### Positive

- **Compliance & Auditability:** Provides cryptographic proof of what is running, satisfying strict audit requirements.
- **Observability:** Operators can instantly see if a deployment failed to roll out properly (e.g., half the fleet running the old config).
- **Security:** Detects unauthorized out-of-band changes or tampering with the `.bca` file or its embedded executable plugins directly on the server.

### Negative

- **Artifact Format Change:** This requires a breaking change to the internal `.bca` binary format to support a metadata header. We must ensure backward compatibility or bump the artifact version.
- **Admin Port Security:** Exposing the `/_admin/provenance` endpoint requires strict network binding (e.g., `127.0.0.1` or a dedicated internal network) to prevent exposing internal infrastructure details.
- **Control Plane Overhead:** The Control Plane now needs an active polling mechanism (worker loop) to monitor Data Plane nodes, increasing its CPU and network footprint slightly.

### Alternatives Considered

- **Hashing the `.bca` file directly instead of the input files:** Rejected. The compilation process might introduce non-deterministic byte ordering depending on the OS/Architecture where `barbacane compile` is run. Hashing the *source* files and binaries provides a true, reproducible representation of the user's intent.

## Related ADRs

- [ADR-0007: Control Plane / Data Plane Separation](0007-control-data-plane-separation.md) — Reaffirms the need for the control plane to actively monitor the isolated data planes.
- [ADR-0019: Packaging and Release Strategy](0019-packaging-and-release-strategy.md) — Aligns with the supply chain security goals (SBOMs, SLSA).
