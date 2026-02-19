# SPEC-006: Control Plane API

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0007

---

## 1. Overview

The control plane (`barbacane-control`) manages specs, compiles artifacts, and maintains the plugin registry. It exposes a REST API and a CLI that wraps it. This spec defines every endpoint, request/response format, and the CLI commands.

---

## 2. Architecture

```
┌──────────────────────────────────────────────┐
│              barbacane-control                │
│                                              │
│  ┌──────────┐  ┌───────────┐  ┌───────────┐ │
│  │ REST API │  │ Compiler  │  │  Plugin   │ │
│  │          │  │           │  │ Registry  │ │
│  └──────────┘  └───────────┘  └───────────┘ │
│        │              │              │       │
│  ┌─────┴──────────────┴──────────────┴─────┐ │
│  │              PostgreSQL                  │ │
│  └──────────────────────────────────────────┘ │
└──────────────────────────────────────────────┘
```

- **PostgreSQL** is the single storage backend. It stores spec metadata, spec file contents, plugin registry entries (including WASM binaries), compiled artifacts, and compilation history. No external object storage required.

---

## 3. REST API

Base path: `/`

All responses use `application/json`. Errors use RFC 9457 (`application/problem+json`).

### Versioning

The API is versioned via the `Accept` header using a vendor media type:

```
Accept: application/vnd.barbacane.v1+json
```

If no version is specified, the latest version is used. When a breaking change is introduced, a new version is added and the previous version remains supported for a deprecation period.

### 3.1 Specs

#### `POST /specs`

Upload a new spec.

**Request:**
```
Content-Type: multipart/form-data

file: <spec file>
name: <string>            (optional, defaults to file name without extension)
```

**Response (201):**
```json
{
  "id": "uuid",
  "name": "user-api",
  "type": "openapi",
  "version": "3.1.0",
  "file": "user-api.yaml",
  "sha256": "abc123...",
  "created_at": "2026-01-28T10:00:00Z",
  "validation": {
    "status": "valid",
    "errors": [],
    "warnings": []
  }
}
```

The spec is validated on upload (SPEC-001 sections 4.1 and 4.2). If validation fails, the response is `422`:

```json
{
  "type": "urn:barbacane:error:spec-invalid",
  "title": "Spec Validation Failed",
  "status": 422,
  "errors": [
    { "code": "E1004", "message": "...", "location": "user-api.yaml:12:3" }
  ]
}
```

#### `GET /specs`

List all specs.

**Response (200):**
```json
{
  "items": [
    {
      "id": "uuid",
      "name": "user-api",
      "type": "openapi",
      "version": "3.1.0",
      "sha256": "abc123...",
      "created_at": "2026-01-28T10:00:00Z",
      "latest_artifact": "uuid | null"
    }
  ]
}
```

#### `GET /specs/{id}`

Get a single spec's metadata.

**Response (200):** Same shape as a single item from the list response, plus `content` (the raw spec YAML/JSON, base64-encoded).

#### `PUT /specs/{id}`

Replace a spec (upload a new version).

Same request format as `POST`. The previous version is retained in history.

#### `DELETE /specs/{id}`

Delete a spec and all its artifacts.

**Response (204):** No body.

#### `GET /specs/{id}/history`

List all versions of a spec.

**Response (200):**
```json
{
  "items": [
    {
      "revision": 3,
      "sha256": "abc123...",
      "created_at": "2026-01-28T10:00:00Z",
      "artifact_id": "uuid | null"
    }
  ]
}
```

### 3.2 Compilation

#### `POST /specs/{id}/compile`

Compile a spec into a `.bca` artifact.

**Request (optional body):**
```json
{
  "production": true,
  "additional_specs": ["uuid", "uuid"]
}
```

- `production` (default `true`): enable production checks.
- `additional_specs`: IDs of other specs to merge into a single artifact.

**Response (202):**
```json
{
  "compilation_id": "uuid",
  "status": "pending"
}
```

Compilation is asynchronous. Poll status via `GET /compilations/{id}`.

#### `GET /compilations/{id}`

**Response (200):**
```json
{
  "id": "uuid",
  "status": "succeeded | failed | pending | compiling",
  "started_at": "2026-01-28T10:00:00Z",
  "completed_at": "2026-01-28T10:00:05Z",
  "artifact_id": "uuid | null",
  "errors": [],
  "warnings": []
}
```

### 3.3 Artifacts

#### `GET /artifacts`

List compiled artifacts.

**Response (200):**
```json
{
  "items": [
    {
      "id": "uuid",
      "compiled_at": "2026-01-28T10:00:05Z",
      "compiler_version": "0.1.0",
      "source_specs": ["user-api"],
      "routes_count": 42,
      "plugins_count": 5,
      "size_bytes": 2048000,
      "sha256": "def456..."
    }
  ]
}
```

#### `GET /artifacts/{id}`

Get artifact metadata (same shape as list item, plus full manifest).

#### `GET /artifacts/{id}/download`

Download the `.bca` file.

**Response (200):**
```
Content-Type: application/octet-stream
Content-Disposition: attachment; filename="artifact.bca"
```

#### `DELETE /artifacts/{id}`

Delete an artifact from storage.

**Response (204):** No body.

### 3.4 Plugins

#### `POST /plugins`

Register a new plugin.

**Request:**
```
Content-Type: multipart/form-data

manifest: <plugin.toml file>
wasm: <.wasm file>
schema: <config-schema.json file>
```

**Response (201):**
```json
{
  "name": "rate-limit",
  "version": "1.0.0",
  "type": "middleware",
  "description": "Token-bucket rate limiting",
  "capabilities": ["log", "context_get", "clock_now"],
  "registered_at": "2026-01-28T10:00:00Z",
  "sha256": "ghi789..."
}
```

Validation errors return `422` with details.

#### `GET /plugins`

List all registered plugins.

**Query parameters:**
- `type` (optional): filter by `middleware` or `dispatcher`
- `name` (optional): filter by name prefix

**Response (200):**
```json
{
  "items": [
    {
      "name": "rate-limit",
      "version": "1.0.0",
      "type": "middleware",
      "description": "Token-bucket rate limiting",
      "registered_at": "2026-01-28T10:00:00Z"
    }
  ]
}
```

#### `GET /plugins/{name}`

List all versions of a plugin.

**Response (200):**
```json
{
  "name": "rate-limit",
  "versions": [
    {
      "version": "1.0.0",
      "registered_at": "2026-01-28T10:00:00Z",
      "sha256": "ghi789..."
    },
    {
      "version": "1.1.0",
      "registered_at": "2026-01-29T10:00:00Z",
      "sha256": "jkl012..."
    }
  ]
}
```

#### `GET /plugins/{name}/{version}`

Get a specific plugin version's metadata, including config schema.

#### `DELETE /plugins/{name}/{version}`

Delete a plugin version. Fails with `409` if the version is referenced by any existing artifact.

### 3.5 Health

#### `GET /health`

```json
{
  "status": "healthy",
  "database": "connected"
}
```

---

## 4. CLI (`barbacane-control`)

The CLI wraps the REST API. It can also be used standalone (without a running control plane server) for local compilation.

### 4.1 Server mode

```
barbacane-control serve [OPTIONS]

OPTIONS:
  --listen <ADDR>           Listen address (default: 0.0.0.0:9090)
  --database-url <URL>      PostgreSQL connection string
  --log-level <LEVEL>       Log level (default: info)
```

### 4.2 Spec management

```
barbacane-control spec upload --file <PATH> [--name <NAME>] [--server <URL>]
barbacane-control spec list [--server <URL>]
barbacane-control spec show <ID> [--server <URL>]
barbacane-control spec delete <ID> [--server <URL>]
barbacane-control spec history <ID> [--server <URL>]
```

### 4.3 Compilation

```bash
# Local compilation (developer workflow — uses barbacane binary)
barbacane compile --spec <FILE>... --manifest barbacane.yaml --output artifact.bca

# Remote async compilation (via control plane REST API)
curl -X POST http://localhost:9090/specs/{id}/compile \
  -H "Content-Type: application/json" \
  -d '{"production": true}'
# then poll: GET /compilations/{id}
```

Local compilation resolves plugins from the `barbacane.yaml` manifest and produces a `.bca` artifact. Remote compilation is triggered via the REST API and runs asynchronously on the control plane.

### 4.4 Validation

```bash
# Quick validation — spec structure + extensions only, no plugin resolution
barbacane validate --spec <FILE>...

# JSON output for CI
barbacane validate --spec <FILE>... --format json
```

### 4.5 Plugin management

```
barbacane-control plugin register --manifest <PATH> [--wasm <PATH>] [--server <URL>]
barbacane-control plugin list [--type middleware|dispatcher] [--server <URL>]
barbacane-control plugin show <NAME> [--server <URL>]
barbacane-control plugin delete <NAME> <VERSION> [--server <URL>]
```

### 4.6 Artifact management

```
barbacane-control artifact list [--server <URL>]
barbacane-control artifact download <ID> --output <PATH> [--server <URL>]
barbacane-control artifact inspect <PATH>
```

`artifact inspect` reads a local `.bca` file and prints the manifest in human-readable format (no server needed).

---

## 5. Database Schema (PostgreSQL)

### 5.1 Tables

```
specs
  id              UUID PRIMARY KEY
  name            TEXT NOT NULL UNIQUE
  current_sha256  TEXT NOT NULL
  type            TEXT NOT NULL (openapi | asyncapi)
  spec_version    TEXT NOT NULL
  created_at      TIMESTAMPTZ NOT NULL
  updated_at      TIMESTAMPTZ NOT NULL

spec_revisions
  id              UUID PRIMARY KEY
  spec_id         UUID REFERENCES specs(id) ON DELETE CASCADE
  revision        INTEGER NOT NULL
  sha256          TEXT NOT NULL
  content         BYTEA NOT NULL
  created_at      TIMESTAMPTZ NOT NULL
  UNIQUE(spec_id, revision)

plugins
  name            TEXT NOT NULL
  version         TEXT NOT NULL
  type            TEXT NOT NULL
  description     TEXT
  capabilities    JSONB NOT NULL
  config_schema   JSONB NOT NULL
  wasm_binary     BYTEA NOT NULL
  sha256          TEXT NOT NULL
  registered_at   TIMESTAMPTZ NOT NULL
  PRIMARY KEY(name, version)

artifacts
  id              UUID PRIMARY KEY
  manifest        JSONB NOT NULL
  data            BYTEA NOT NULL
  sha256          TEXT NOT NULL
  size_bytes      BIGINT NOT NULL
  compiled_at     TIMESTAMPTZ NOT NULL

artifact_specs
  artifact_id     UUID REFERENCES artifacts(id) ON DELETE CASCADE
  spec_id         UUID REFERENCES specs(id)
  spec_revision   INTEGER NOT NULL
  PRIMARY KEY(artifact_id, spec_id)

compilations
  id              UUID PRIMARY KEY
  spec_id         UUID REFERENCES specs(id)
  status          TEXT NOT NULL (pending | compiling | succeeded | failed)
  artifact_id     UUID REFERENCES artifacts(id)
  errors          JSONB
  warnings        JSONB
  started_at      TIMESTAMPTZ NOT NULL
  completed_at    TIMESTAMPTZ
```

