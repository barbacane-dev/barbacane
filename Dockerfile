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

# Build the data plane binary
RUN cargo build --release --package barbacane

# Runtime stage - distroless for minimal attack surface
FROM gcr.io/distroless/cc-debian12:nonroot

# Copy the binary from builder
COPY --from=builder /build/target/release/barbacane /barbacane

# Run as non-root user (UID 65532)
USER nonroot:nonroot

# Expose default ports
# 8080 - HTTP
# 8443 - HTTPS
EXPOSE 8080 8443

ENTRYPOINT ["/barbacane"]
CMD ["serve", "--artifact", "/config/api.bca"]
