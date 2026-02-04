# ADR-0019: Packaging and Release Strategy

**Status:** Proposed
**Date:** 2026-02-04

## Context

Barbacane is approaching a state where we need to define how users obtain and deploy the software. Currently:

- **Two binaries:** `barbacane` (data plane) and `barbacane-control` (control plane)
- **Version:** 0.1.0 (pre-release)
- **Installation:** Source only (`cargo build --release`)
- **No published artifacts:** No Docker images, no pre-built binaries, no crates.io packages
- **CI exists:** Tests, lints, benchmarks — but no release automation

Users currently must clone the repo and build from source. This limits adoption and creates friction for evaluation.

### Goals

1. **Easy evaluation:** Users should be able to try Barbacane without building from source
2. **Production deployment:** Clear path from evaluation to production
3. **Platform coverage:** Support common deployment targets (Linux x86_64, ARM64, macOS, containers)
4. **Reproducibility:** Releases should be verifiable and reproducible
5. **Maintainability:** Release process should be automated and low-friction

## Decision

### Release Artifacts

We will publish the following artifacts for each release:

| Artifact | Registry | Purpose |
|----------|----------|---------|
| Pre-built binaries | GitHub Releases | Direct download for servers, CI/CD |
| Container images | GitHub Container Registry (ghcr.io) | Kubernetes, Docker deployments |
| Rust crates | crates.io | Embedding, library usage, `cargo install` |
| WASM plugins | GitHub Releases | Official plugin distribution |

### Platform Matrix

**Binaries:**

| OS | Architecture | Priority | Notes |
|----|--------------|----------|-------|
| Linux | x86_64 (gnu) | P0 | Primary server target |
| Linux | aarch64 (gnu) | P0 | ARM servers, Graviton, Ampere |
| Linux | x86_64 (musl) | P1 | Alpine, static linking (via `cross-rs`) |
| Linux | aarch64 (musl) | P1 | Alpine ARM (via `cross-rs`) |
| macOS | x86_64 | P1 | Intel Macs (development) |
| macOS | aarch64 | P1 | Apple Silicon (development) |
| Windows | x86_64 | P2 | Development only, not recommended for production |

**TLS stack:** Barbacane uses `rustls` with the `aws-lc-rs` crypto backend — no OpenSSL dependency. This simplifies cross-compilation but musl targets still require `cross-rs` for the C dependencies in `aws-lc-rs`.

**Container images:**

| Base | Architecture | Tags |
|------|--------------|------|
| Debian slim | linux/amd64, linux/arm64 | `latest`, `x.y.z`, `x.y`, `x` |
| Alpine | linux/amd64, linux/arm64 | `x.y.z-alpine`, `alpine` |
| Distroless | linux/amd64, linux/arm64 | `x.y.z-distroless`, `distroless` |

### Container Strategy

**Two images:**

1. **`ghcr.io/barbacane-dev/barbacane`** — Data plane only
   - Minimal image (distroless or Alpine-based)
   - Single binary, no shell
   - Intended for production gateway deployments

2. **`ghcr.io/barbacane-dev/barbacane-control`** — Control plane
   - Includes web UI assets
   - Requires external PostgreSQL
   - Intended for management deployments

**Dockerfile approach:**

```dockerfile
# Data plane - multi-stage build, rootless
# Use -bookworm suffix to match glibc in distroless cc-debian12
FROM rust:1.93-slim-bookworm AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --package barbacane

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /build/target/release/barbacane /barbacane
USER nonroot:nonroot
ENTRYPOINT ["/barbacane"]
CMD ["serve", "--artifact", "/config/api.bca"]
```

**Security:** All container images run as non-root user (`nonroot:nonroot`, UID 65532) for defense in depth.

**NOT providing:**
- All-in-one images (control + data plane) — violates separation principle (ADR-0007)
- Images with embedded PostgreSQL — use external database
- Helm charts (initially) — defer until patterns stabilize

### Version Strategy

**Semantic Versioning (SemVer):**
- `MAJOR.MINOR.PATCH` format
- Pre-release: `0.x.y` (current)
- Stable: `1.0.0` and beyond

**Single workspace version:**
- All crates share the same version number
- Simplifies compatibility guarantees
- Version defined in workspace `Cargo.toml`

**Breaking changes:**
- Before 1.0: Breaking changes allowed in minor versions
- After 1.0: Breaking changes require major version bump

### Release Cadence

**Release types:**

| Type | Trigger | Version Bump | Artifacts |
|------|---------|--------------|-----------|
| Stable | Git tag `vX.Y.Z` | Manual | All |
| Pre-release | Git tag `vX.Y.Z-beta.N` | Manual | All (marked pre-release) |
| Nightly | Daily (optional) | None | Container images only (`nightly` tag) |

**Release process:**

1. Update `CHANGELOG.md` with release notes
2. Bump version in `Cargo.toml`
3. Create PR, merge to main
4. Tag release: `git tag v0.2.0 && git push --tags`
5. GitHub Actions builds and publishes all artifacts
6. Create GitHub Release with changelog and artifact links

### crates.io Publication

**All workspace crates are published** to satisfy Cargo's requirement that path dependencies must be available on crates.io.

| Crate | Purpose | API Stability |
|-------|---------|---------------|
| `barbacane` | Data plane CLI | Stable (SemVer) |
| `barbacane-control` | Control plane CLI | Stable (SemVer) |
| `barbacane-plugin-sdk` | Plugin development | Stable (SemVer) |
| `barbacane-plugin-macros` | Plugin proc macros | Stable (SemVer) |
| `barbacane-compiler` | Spec compilation | Internal |
| `barbacane-router` | Routing engine | Internal |
| `barbacane-validator` | Request validation | Internal |
| `barbacane-spec-parser` | OpenAPI/AsyncAPI parsing | Internal |
| `barbacane-wasm` | WASM runtime | Internal |
| `barbacane-telemetry` | Observability | Internal |
| `barbacane-test` | Test utilities | Internal |

**Internal crates disclaimer:** Crates marked "Internal" are implementation details of Barbacane and are **not subject to SemVer guarantees**. Their APIs may change without notice in any release. Do not depend on them directly.

### Signing and Verification

**Binary releases:**
- SHA256 checksums for all artifacts (`barbacane-v0.2.0-checksums.txt`)
- GPG signatures (optional, adds complexity)
- GitHub Releases provides HTTPS + tag verification

**Container images:**
- Signed with Sigstore/cosign
- SBOM (Software Bill of Materials) attached
- Provenance attestation via SLSA

**Verification example:**
```bash
# Verify checksum
sha256sum -c barbacane-v0.2.0-checksums.txt

# Verify container signature
cosign verify ghcr.io/barbacane-dev/barbacane:0.2.0
```

### GitHub Actions Workflow

```yaml
name: Release
on:
  push:
    tags: ['v*']

jobs:
  build-binaries:
    strategy:
      matrix:
        include:
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            cross: false
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
            cross: true
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
            cross: true  # Required for aws-lc-rs C dependencies
          - target: aarch64-unknown-linux-musl
            os: ubuntu-latest
            cross: true
          - target: x86_64-apple-darwin
            os: macos-latest
            cross: false
          - target: aarch64-apple-darwin
            os: macos-latest
            cross: false
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - name: Install cross
        if: matrix.cross
        run: cargo install cross --git https://github.com/cross-rs/cross
      - name: Build with cargo
        if: ${{ !matrix.cross }}
        run: cargo build --release --target ${{ matrix.target }}
      - name: Build with cross
        if: matrix.cross
        run: cross build --release --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: barbacane-${{ matrix.target }}
          path: target/${{ matrix.target }}/release/barbacane

  build-containers:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/setup-buildx-action@v3
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v5
        with:
          push: true
          platforms: linux/amd64,linux/arm64
          tags: ghcr.io/barbacane-dev/barbacane:${{ github.ref_name }}

  publish-crates:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      # Publish in dependency order (leaves first, roots last)
      - run: cargo publish -p barbacane-plugin-macros
      - run: cargo publish -p barbacane-plugin-sdk
      - run: cargo publish -p barbacane-telemetry
      - run: cargo publish -p barbacane-spec-parser
      - run: cargo publish -p barbacane-router
      - run: cargo publish -p barbacane-validator
      - run: cargo publish -p barbacane-wasm
      - run: cargo publish -p barbacane-compiler
      - run: cargo publish -p barbacane
      - run: cargo publish -p barbacane-control
    env:
      CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}

  create-release:
    needs: [build-binaries, build-containers, publish-crates]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
      - run: sha256sum barbacane-*/barbacane* > checksums.txt
      - uses: softprops/action-gh-release@v1
        with:
          files: |
            barbacane-*/barbacane*
            checksums.txt
          generate_release_notes: true
```

### What We're NOT Doing

- **Homebrew formula:** Defer until stable release and user demand
- **APT/RPM packages:** High maintenance, limited benefit over binaries
- **Helm charts:** Defer until deployment patterns stabilize (per ADR-0018)
- **Windows containers:** No demand, Linux containers cover all use cases
- **Nightly binaries:** Only nightly container images (storage costs)

### Installation Methods Summary

**For evaluation:**
```bash
# Container (quickest)
docker run -v ./api.bca:/config/api.bca ghcr.io/barbacane-dev/barbacane

# Binary download
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-x86_64-unknown-linux-gnu
chmod +x barbacane-x86_64-unknown-linux-gnu
./barbacane-x86_64-unknown-linux-gnu serve --artifact api.bca
```

**For development:**
```bash
# Via cargo
cargo install barbacane

# From source
git clone https://github.com/barbacane-dev/barbacane
cd barbacane && cargo build --release
```

**For production (Kubernetes):**
```yaml
containers:
  - name: barbacane
    image: ghcr.io/barbacane-dev/barbacane:1.0.0
    args: ["serve", "--artifact", "/config/api.bca"]
```

## Consequences

### Easier

- **Adoption:** Users can evaluate without building from source
- **CI/CD integration:** Pre-built binaries work in any pipeline
- **Container deployments:** Standard container workflow
- **Version pinning:** Specific versions available forever

### Harder

- **Release overhead:** Each release requires building for multiple platforms
- **Storage costs:** GitHub Releases and container registry usage
- **Signing infrastructure:** Cosign keys and rotation

### Trade-offs

- **ghcr.io over Docker Hub:** Better GitHub integration, no rate limits, but less discoverable
- **No Helm charts:** Reduces maintenance, but users must write their own K8s manifests
- **Single workspace version:** Simpler, but all crates bump together even for small changes

## Open Questions

To revisit after initial release:

1. **Container registry:** ghcr.io vs Docker Hub? Current choice (ghcr.io) favors GitHub integration and no rate limits, but Docker Hub offers better discoverability. May publish to both eventually.

2. **Nightly builds:** Currently minimal (container images only). Evaluate whether binary nightlies are worth the CI and storage cost based on user demand.

3. **Helm charts:** Deferred per ADR-0018. Revisit once Kubernetes deployment patterns stabilize and user demand emerges.

## Related ADRs

- [ADR-0007: Control Plane / Data Plane Separation](0007-control-data-plane-separation.md) — Separate binaries, separate images
- [ADR-0018: Kubernetes Gateway API Compatibility](0018-kubernetes-gateway-api-compatibility.md) — No Helm charts initially

## References

- [Rust Release Checklist](https://rust-lang.github.io/api-guidelines/documentation.html)
- [Sigstore Cosign](https://docs.sigstore.dev/cosign/overview/)
- [GitHub Container Registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)
- [Semantic Versioning](https://semver.org/)
