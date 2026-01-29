# Artifact Format

Barbacane compiles OpenAPI specs into `.bca` (Barbacane Compiled Artifact) files. This document describes the artifact format.

## Overview

A `.bca` file is a gzip-compressed tar archive containing:

```
artifact.bca (tar.gz)
├── manifest.json       # Artifact metadata
├── routes.json         # Compiled routing table
├── specs/              # Embedded source specifications
│   ├── api.yaml
│   └── ...
└── plugins/            # Bundled WASM plugins (optional)
    ├── rate-limit.wasm
    └── ...
```

## File Structure

### manifest.json

Metadata about the artifact.

```json
{
  "barbacane_artifact_version": 1,
  "compiled_at": "2025-01-29T10:30:00Z",
  "compiler_version": "0.1.0",
  "source_specs": [
    {
      "file": "api.yaml",
      "sha256": "abc123...",
      "type": "openapi",
      "version": "3.1.0"
    }
  ],
  "bundled_plugins": [
    {
      "name": "rate-limit",
      "version": "1.0.0",
      "plugin_type": "middleware",
      "wasm_path": "plugins/rate-limit.wasm",
      "sha256": "789abc..."
    }
  ],
  "routes_count": 12,
  "checksums": {
    "routes.json": "sha256:def456..."
  }
}
```

#### Fields

| Field | Type | Description |
|-------|------|-------------|
| `barbacane_artifact_version` | integer | Format version (currently `1`) |
| `compiled_at` | string | ISO 8601 timestamp of compilation |
| `compiler_version` | string | Version of `barbacane` compiler |
| `source_specs` | array | List of source specifications |
| `bundled_plugins` | array | List of bundled WASM plugins (optional) |
| `routes_count` | integer | Number of compiled routes |
| `checksums` | object | SHA-256 checksums for integrity |

#### source_specs entry

| Field | Type | Description |
|-------|------|-------------|
| `file` | string | Original filename |
| `sha256` | string | Hash of source content |
| `type` | string | Spec type (`openapi` or `asyncapi`) |
| `version` | string | Spec version (e.g., `3.1.0`) |

#### bundled_plugins entry

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Plugin name (kebab-case) |
| `version` | string | Plugin version (semver) |
| `plugin_type` | string | Plugin type (`middleware` or `dispatcher`) |
| `wasm_path` | string | Path to WASM file within artifact |
| `sha256` | string | SHA-256 hash of WASM file |

### routes.json

Compiled operations with routing information.

```json
{
  "operations": [
    {
      "index": 0,
      "path": "/users",
      "method": "GET",
      "operation_id": "listUsers",
      "dispatch": {
        "name": "http",
        "config": {
          "upstream": "backend",
          "path": "/api/users"
        }
      }
    },
    {
      "index": 1,
      "path": "/users/{id}",
      "method": "GET",
      "operation_id": "getUser",
      "dispatch": {
        "name": "http",
        "config": {
          "upstream": "backend",
          "path": "/api/users/{id}"
        }
      }
    }
  ]
}
```

#### operation entry

| Field | Type | Description |
|-------|------|-------------|
| `index` | integer | Unique operation index |
| `path` | string | OpenAPI path template |
| `method` | string | HTTP method (uppercase) |
| `operation_id` | string | Operation ID (optional) |
| `dispatch` | object | Dispatcher configuration |

### specs/

Directory containing the original source specifications. These are embedded for:

- Serving via `/__barbacane/openapi` endpoint
- Documentation and debugging
- Audit trail

Files retain their original names.

## Version History

| Version | Changes |
|---------|---------|
| 1 | Initial format |

## Inspecting Artifacts

### List Contents

```bash
tar -tzf artifact.bca
```

Output:
```
manifest.json
routes.json
specs/
specs/api.yaml
plugins/
plugins/rate-limit.wasm
```

### Extract and View

```bash
# Extract
tar -xzf artifact.bca -C ./extracted

# View manifest
cat extracted/manifest.json | jq .

# View routes
cat extracted/routes.json | jq '.operations | length'
```

### Verify Checksums

```bash
# Extract
tar -xzf artifact.bca -C ./extracted

# Verify routes.json
sha256sum extracted/routes.json
# Compare with manifest.checksums["routes.json"]
```

## Security Considerations

### Integrity

- All embedded files have SHA-256 checksums in the manifest
- The gateway can verify checksums on load (planned)

### Contents

- Source specs are embedded and served publicly via `/__barbacane/openapi`
- Do not include secrets in spec files
- Use environment variables or secret management for sensitive config

### Signing (Planned)

Future versions will support:
- GPG signatures
- Artifact signing with private keys
- Signature verification on load

## Programmatic Access

### Rust

```rust
use barbacane_compiler::{load_manifest, load_routes, load_specs, load_plugins};
use std::path::Path;

let path = Path::new("artifact.bca");

// Load manifest
let manifest = load_manifest(path)?;
println!("Routes: {}", manifest.routes_count);

// Load routes
let routes = load_routes(path)?;
for op in &routes.operations {
    println!("{} {}", op.method, op.path);
}

// Load specs
let specs = load_specs(path)?;
for (name, content) in &specs {
    println!("Spec: {} ({} bytes)", name, content.len());
}

// Load plugins
let plugins = load_plugins(path)?;
for (name, wasm_bytes) in &plugins {
    println!("Plugin: {} ({} bytes)", name, wasm_bytes.len());
}
```

## Best Practices

### Naming

Use descriptive names:
```
my-api-v2.1.0.bca
gateway-prod-2025-01-29.bca
```

### Version Control

Don't commit `.bca` files to git. Instead:
- Commit source specs
- Build artifacts in CI/CD
- Store in artifact registry

### CI/CD Pipeline

```bash
# Compile in CI
barbacane compile \
  --spec specs/*.yaml \
  --output dist/gateway-${VERSION}.bca

# Upload to registry
aws s3 cp dist/gateway-${VERSION}.bca s3://artifacts/

# Deploy
ssh prod "barbacane serve --artifact /opt/barbacane/gateway.bca"
```
