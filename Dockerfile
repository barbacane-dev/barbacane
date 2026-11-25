# Barbacane Data Plane - Multi-stage build
# Produces a minimal, rootless container image: the binary on the runtime base
# of docker/runtime.apko.yaml (glibc, libgcc and CA certificates from Wolfi; no
# shell, no package manager).

# Runtime base - apko installs the Wolfi packages docker/runtime.apko.lock.json
# pins, for the target architecture, into an image of one layer; its files
# become /rootfs.
FROM --platform=$BUILDPLATFORM golang:1.27-bookworm@sha256:4fa7c968ba93f1635670522dbb01a2c78150443e1385746924566079f36570ea AS runtime
ARG TARGETARCH
RUN go install chainguard.dev/apko@v1.4.15
WORKDIR /runtime
COPY docker/runtime.apko.yaml docker/runtime.apko.lock.json ./
RUN apko build --arch "$TARGETARCH" --lockfile runtime.apko.lock.json --sbom=false \
        runtime.apko.yaml barbacane-runtime:build runtime.tar && \
    layer=$(tar -tf runtime.tar | grep '\.tar\.gz$') && \
    test "$(printf '%s\n' "$layer" | wc -l)" -eq 1 && \
    mkdir /rootfs && \
    tar -xOf runtime.tar "$layer" | tar -xzp --same-owner -C /rootfs

# Build stage - Rust 1.100.0, the toolchain CI builds and tests with. Debian 12,
# so the binary needs no newer glibc than the runtime base holds.
FROM rust:1.100.0-slim-bookworm@sha256:ac965c2299127add362cdcf165f19516a0ca90d893bf7bd04cf1a44de39cfca7 AS builder

# Install build dependencies for aws-lc-rs
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# `cargo auditable` records the binary's crates in it, where image scanners
# read them.
RUN cargo install --locked cargo-auditable@0.7.7

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Limit parallel jobs to avoid OOM in memory-constrained Docker environments
ENV CARGO_BUILD_JOBS=2

# Build the data plane binary
RUN cargo auditable build --locked --release --package barbacane && \
    cp target/release/barbacane /usr/local/bin/barbacane && \
    mkdir /artifacts

FROM scratch

# OCI labels for GitHub Container Registry
LABEL org.opencontainers.image.source="https://github.com/barbacane-dev/barbacane"
LABEL org.opencontainers.image.description="Barbacane API Gateway - Data Plane"
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"

COPY --from=runtime /rootfs/ /
COPY --from=runtime --chown=65532:65532 /rootfs/home/nonroot/ /home/nonroot/

# Copy the binary from builder
COPY --from=builder /usr/local/bin/barbacane /barbacane

# Where a data plane in connected mode keeps the artifacts it is deployed:
# owned by the user it runs as, which a named volume mounted here inherits.
COPY --from=builder --chown=65532:65532 /artifacts/ /var/lib/barbacane/
ENV BARBACANE_ARTIFACT_DIR=/var/lib/barbacane

# The compiler caches downloaded plugins under $HOME.
ENV HOME=/home/nonroot

# Run as non-root user (UID 65532)
USER 65532:65532

# Expose default ports
# 8080 - HTTP
# 8443 - HTTPS
EXPOSE 8080 8443

ENTRYPOINT ["/barbacane"]
CMD ["serve", "--artifact", "/config/api.bca"]
