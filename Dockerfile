# Barbacane Data Plane - Multi-stage build
# Produces a minimal, rootless container image

# Build stage - Rust 1.95+ required (MSRV; wasmtime 48 floor), Bookworm for glibc compat
FROM rust:1.95-slim-bookworm@sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082 AS builder

# Install build dependencies for aws-lc-rs
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Limit parallel jobs to avoid OOM in memory-constrained Docker environments
ENV CARGO_BUILD_JOBS=2

# Build the data plane binary
RUN cargo build --locked --release --package barbacane && \
    cp target/release/barbacane /usr/local/bin/barbacane

# Runtime stage - glibc, libgcc, libstdc++ and CA certificates, no shell and
# no OpenSSL (TLS is rustls with aws-lc-rs, linked in). The free tier publishes
# only `latest`: bump this digest by hand.
FROM cgr.dev/chainguard/glibc-dynamic@sha256:2c362104c7f7adc37e6bdc87451450ff3834871a6268d930815cbe561493f947

# OCI labels for GitHub Container Registry
LABEL org.opencontainers.image.source="https://github.com/barbacane-dev/barbacane"
LABEL org.opencontainers.image.description="Barbacane API Gateway - Data Plane"
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"

# Copy the binary from builder
COPY --from=builder /usr/local/bin/barbacane /barbacane

# Run as non-root user (UID 65532)
USER 65532:65532

# Expose default ports
# 8080 - HTTP
# 8443 - HTTPS
EXPOSE 8080 8443

ENTRYPOINT ["/barbacane"]
CMD ["serve", "--artifact", "/config/api.bca"]
