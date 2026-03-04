# FIPS 140-3 Compliance

FIPS 140-3 is a US federal standard for cryptographic modules. Government agencies and their contractors are required to use FIPS-validated cryptography. Many large enterprises — especially in finance, healthcare, and defense — enforce FIPS compliance as a baseline security requirement.

Barbacane uses [rustls](https://github.com/rustls/rustls) with the [aws-lc-rs](https://github.com/aws/aws-lc-rs) cryptographic backend — no OpenSSL dependency. AWS-LC has received [FIPS 140-3 Level 1 certification](https://aws.amazon.com/blogs/security/aws-lc-is-now-fips-140-3-certified/) from NIST, making it straightforward to run Barbacane in FIPS-compliant mode.

## How It Works

By default, Barbacane links against `aws-lc-sys` (the non-FIPS build of AWS-LC). Enabling the `fips` feature switches the entire TLS stack to `aws-lc-fips-sys`, which is the FIPS-validated module.

When FIPS mode is enabled:

- Only FIPS-approved cipher suites and key exchange algorithms are available
- TLS Extended Master Secret (EMS) is **required** for TLS 1.2 connections
- The aws-lc-fips-sys crate performs a power-on self-test at startup to verify cryptographic integrity

## Enabling FIPS Mode

### 1. Install Additional Build Dependencies

The FIPS build of AWS-LC requires **Go** in addition to the standard build tools:

| Tool | Standard Build | FIPS Build |
|------|---------------|------------|
| cmake | Required | Required |
| clang | Required | Required |
| Go 1.18+ | Not required | **Required** |

On Debian/Ubuntu:

```bash
apt-get install -y cmake clang golang
```

On macOS:

```bash
brew install cmake go
```

### 2. Build with the FIPS Feature Flag

The `barbacane` crate exposes a `fips` Cargo feature. Pass it at build time:

```bash
cargo build -p barbacane --release --features fips
```

This enables `rustls/fips`, which transitively pulls in `aws-lc-fips-sys` instead of `aws-lc-sys`. No source edits required.

### 3. Use the FIPS Crypto Provider

No code change is needed — the startup code in `main.rs` already installs the aws-lc-rs default provider:

```rust
let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
```

When built with the `fips` feature, this provider automatically uses the FIPS-validated module.

### 4. Verify FIPS Mode (Optional)

You can assert FIPS compliance at runtime using `ServerConfig::fips()`:

```rust
let tls_config = load_tls_config(&config)?;
assert!(tls_config.fips(), "TLS config is not FIPS-compliant");
```

In production, prefer a health-check or log message over panicking:

```rust
if !tls_config.fips() {
    tracing::warn!("TLS configuration is NOT FIPS-compliant");
}
```

### 5. Build

```bash
cargo build -p barbacane --release --features fips
```

The first FIPS build takes longer because `aws-lc-fips-sys` compiles AWS-LC from source with FIPS self-tests enabled.

## Docker

Update the Dockerfile to install Go for the FIPS build:

```dockerfile
FROM rust:1.93-slim-bookworm AS builder

# Build dependencies — Go required for aws-lc-fips-sys
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    golang \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
```

The runtime image (`distroless/cc-debian12`) does not need changes — the FIPS module is statically linked.

## FIPS Cipher Suites

When FIPS mode is active, rustls restricts the available cipher suites to FIPS-approved algorithms:

**TLS 1.3:**
- `TLS_AES_256_GCM_SHA384`
- `TLS_AES_128_GCM_SHA256`

**TLS 1.2:**
- `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384`
- `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256`
- `TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384`
- `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256`

> **Note:** ChaCha20-Poly1305 cipher suites are **not** available in FIPS mode, as ChaCha20 is not FIPS-approved.

## Platform Support

| Platform | FIPS Build | Notes |
|----------|-----------|-------|
| Linux (glibc) | Supported | Primary target |
| Linux (musl) | Supported | Requires `cross-rs` (same as non-FIPS) |
| macOS | Supported | Development only — not FIPS-certified |
| Windows | Supported | Static build used automatically |

> **Important:** FIPS 140-3 validation only covers the specific certified platforms. For production FIPS compliance, deploy on Linux. macOS and Windows builds use the same code but are not covered by the NIST certificate.

## Downstream Dependencies

The `reqwest` and `tokio-tungstenite` crates also use rustls for outbound TLS. Since they share the same workspace-level rustls dependency, enabling the `fips` feature applies to **all** TLS connections — both ingress and egress.

| Crate | TLS Usage | FIPS Coverage |
|-------|-----------|---------------|
| `barbacane` | Ingress TLS termination | Covered |
| `barbacane-wasm` | Outbound HTTP (plugin `host_http_call`) | Covered via `reqwest` |
| `barbacane-control` | WebSocket to data plane | Covered via `tokio-tungstenite` |

## References

- [rustls FIPS documentation](https://rustls.dev/docs/manual/_06_fips/index.html)
- [aws-lc-rs GitHub](https://github.com/aws/aws-lc-rs)
- [AWS-LC FIPS 140-3 certification announcement](https://aws.amazon.com/blogs/security/aws-lc-is-now-fips-140-3-certified/)
- [ADR-0019: Packaging and Release Strategy](../../adr/0019-packaging-and-release-strategy.md) — documents the aws-lc-rs backend choice
