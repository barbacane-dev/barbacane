# ADR-0029: Remote Plugin Distribution via HTTPS

**Status:** Accepted
**Date:** 2026-03-17

## Context

Barbacane plugins are WASM modules declared in `barbacane.yaml` and resolved at compile time (ADR-0006, ADR-0011). Until now, only local `path:` sources were implemented — `url:` sources returned a hard error despite being part of the original schema.

This forced users to either vendor plugin `.wasm` files in their repository or build them from source locally. For teams consuming official or third-party plugins, this adds unnecessary friction: they must download files manually, track versions by hand, and cannot point their manifest at a canonical release URL.

The existing GitHub release workflow already publishes platform binaries and container images but not plugin `.wasm` files. Adding plugin assets to releases would complete the distribution story — users could reference plugins by URL and get reproducible, version-pinned builds.

## Decision

### URL Plugin Sources with HTTPS Download

The compiler resolves `url:` plugin sources by downloading `.wasm` files over HTTPS at compile time. Downloads are cached locally to avoid redundant network requests.

```yaml
# barbacane.yaml
plugins:
  jwt-auth:
    url: https://github.com/barbacane-dev/barbacane/releases/download/v0.5.0/jwt-auth.wasm
    sha256: abc123def456...  # optional integrity check
  mock:
    path: ./plugins/mock.wasm  # local sources still work
```

### Integrity Verification

An optional `sha256` field on URL sources enables checksum verification. When provided, the compiler computes SHA-256 of the downloaded bytes and rejects mismatches. This defends against CDN corruption and supply-chain tampering without requiring a separate lockfile.

### Local File Cache

Downloaded plugins are cached at `~/.barbacane/cache/plugins/<sha256-of-url>/` containing:
- `plugin.wasm` — the WASM binary
- `plugin.toml` — plugin metadata (if available from remote)
- `metadata.json` — cache metadata (URL, checksum, download timestamp)

Cache behavior:
- If `sha256` is specified, the cache validates against it — mismatches trigger re-download
- If `sha256` is not specified, cached files are used if present
- `--no-cache` flag on `barbacane compile` bypasses the cache entirely (no read, no write)

### Plugin Metadata Resolution

The compiler attempts to fetch `plugin.toml` from the remote source to extract version, type, and capability metadata. Two URL conventions are tried:
1. `<name>.plugin.toml` — sibling file (GitHub release assets are flat)
2. `plugin.toml` — directory convention (self-hosted / structured URLs)

If neither is found, the plugin resolves without metadata (defaults apply).

### CI/CD: Plugin Assets in GitHub Releases

The release workflow (`release.yml`) now includes a `build-plugins` job that:
1. Builds all plugins in `plugins/` to `wasm32-unknown-unknown`
2. Uploads `<name>.wasm` + `<name>.plugin.toml` as release assets
3. Generates `plugin-checksums.txt` with SHA-256 for every `.wasm`

### What we are NOT doing

- **No OCI registry for plugins** — GitHub Releases provides versioned URLs, checksums, and CDN distribution for free. OCI adds complexity (ORAS tooling, registry auth) without solving a real problem at current scale. ADR-0027 already covers OCI for `.bca` artifacts if the need arises.
- **No plugin registry service** — discovery and versioning are handled by GitHub releases and manifest URLs. A registry may be added when a third-party plugin ecosystem emerges.
- **No lockfile** — the `sha256` field in `barbacane.yaml` serves the same purpose with less tooling overhead. A `barbacane lock` command may be added later to auto-populate checksums.

## Consequences

### Easier

- **Zero-friction plugin consumption** — users reference a URL instead of building from source
- **Reproducible builds** — pinning `sha256` ensures identical plugin bytes across environments
- **Official plugin distribution** — every release ships pre-built `.wasm` files with checksums
- **Caching** — repeated compiles don't re-download plugins

### Harder

- **Network dependency at compile time** — first compile requires internet access (cached afterward)
- **Cache management** — users must manually clear `~/.barbacane/cache/` if it grows; no automatic eviction yet
- **URL stability** — if a release is deleted or a URL changes, cached copies still work but new environments fail

## Related ADRs

- [ADR-0006: WASM Plugin Architecture](0006-wasm-plugin-architecture.md) — plugin model and manifest schema
- [ADR-0011: Spec Compilation Model](0011-spec-compilation-model.md) — compile-time plugin resolution
- [ADR-0027: OCI Artifact Distribution](0027-oci-artifact-distribution.md) — OCI distribution for `.bca` artifacts (not plugins)
