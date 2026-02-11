# syntax=docker/dockerfile:1
# Barbacane Data Plane - Multi-stage build
# Produces a minimal, rootless container image

# Build stage - Rust 1.85+ required for edition 2024 deps, Bookworm for glibc compat
FROM rust:1.93-slim-bookworm AS builder

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

# Build the data plane binary (cache cargo registry + target dir across builds)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --package barbacane && \
    cp target/release/barbacane /usr/local/bin/barbacane

# Runtime stage - distroless for minimal attack surface
FROM gcr.io/distroless/cc-debian12:nonroot

# OCI labels for GitHub Container Registry
LABEL org.opencontainers.image.source="https://github.com/barbacane-dev/barbacane"
LABEL org.opencontainers.image.description="Barbacane API Gateway - Data Plane"
LABEL org.opencontainers.image.licenses="Apache-2.0"

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
