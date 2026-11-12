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

# Runtime stage - distroless for minimal attack surface
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

# OCI labels for GitHub Container Registry
LABEL org.opencontainers.image.source="https://github.com/barbacane-dev/barbacane"
LABEL org.opencontainers.image.description="Barbacane API Gateway - Data Plane"
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"

# Copy the binary from builder
COPY --from=builder /usr/local/bin/barbacane /barbacane

# Run as non-root user (UID 65532)
USER nonroot:nonroot

# Expose default ports
# 8080 - HTTP
# 8443 - HTTPS
EXPOSE 8080 8443

ENTRYPOINT ["/barbacane"]
CMD ["serve", "--artifact", "/config/api.bca"]
