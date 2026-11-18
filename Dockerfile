# Barbacane Data Plane - Multi-stage build
# Produces a minimal, rootless container image: a static binary on scratch.

# Build stage - Rust 1.95+ required (MSRV; wasmtime 48 floor). Alpine's
# toolchain targets musl, so the binary links no shared library.
FROM rust:1.95-alpine3.23@sha256:606fd313a0f49743ee2a7bd49a0914bab7deedb12791f3a846a34a4711db7ed2 AS builder

# C toolchain for aws-lc-rs, zstd, lz4 and mimalloc; CA certificates for the
# runtime image.
RUN apk add --no-cache build-base cmake perl linux-headers ca-certificates

# `cargo auditable` records the binary's crates in it, where image scanners
# read them: the runtime image has no package database.
RUN cargo install --locked cargo-auditable@0.7.7

WORKDIR /build

# Copy manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Limit parallel jobs to avoid OOM in memory-constrained Docker environments
ENV CARGO_BUILD_JOBS=2

# Build the data plane binary; fail if it links a shared library.
RUN cargo auditable build --locked --release --package barbacane && \
    cp target/release/barbacane /barbacane && \
    if readelf -d /barbacane | grep -q NEEDED; then \
        echo "barbacane links shared libraries" >&2; exit 1; \
    fi

# The runtime image's files: CA certificates, the users, /tmp, and a home for
# the non-root user.
RUN mkdir -p /rootfs/etc/ssl/certs /rootfs/tmp /home-nonroot && \
    cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/ && \
    printf 'root:x:0:0:root:/root:/sbin/nologin\nnonroot:x:65532:65532:nonroot:/home/nonroot:/sbin/nologin\n' \
        > /rootfs/etc/passwd && \
    printf 'root:x:0:\nnonroot:x:65532:\n' > /rootfs/etc/group && \
    chmod 1777 /rootfs/tmp

# The binary alone, for `docker build --target binary --output type=local,dest=<dir>`.
FROM scratch AS binary
COPY --from=builder /barbacane /barbacane

# Runtime stage - the static binary, CA certificates and a non-root user: no
# shell, no libc, no package database.
FROM scratch

# OCI labels for GitHub Container Registry
LABEL org.opencontainers.image.source="https://github.com/barbacane-dev/barbacane"
LABEL org.opencontainers.image.description="Barbacane API Gateway - Data Plane"
LABEL org.opencontainers.image.licenses="AGPL-3.0-only"

COPY --from=builder /rootfs/ /
COPY --from=builder --chown=65532:65532 /home-nonroot/ /home/nonroot/
COPY --from=builder /barbacane /barbacane

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
