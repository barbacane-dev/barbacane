# Control Plane

The Barbacane Control Plane provides a REST API for managing API specifications, plugins, and compiled artifacts. It enables centralized management of your API gateway configuration with PostgreSQL-backed storage and async compilation.

## Overview

The control plane is a separate component from the data plane (gateway). While the data plane handles request routing and processing, the control plane manages:

- **Specs** - Upload, version, and manage OpenAPI/AsyncAPI specifications
- **Plugins** - Registry for WASM plugins with version management
- **Artifacts** - Compiled `.bca` files ready for deployment
- **Compilations** - Async compilation jobs with status tracking

## Quick Start

### Start the Server

```bash
# Start PostgreSQL (Docker example)
docker run -d --name barbacane-db \
  -e POSTGRES_PASSWORD=barbacane \
  -e POSTGRES_DB=barbacane \
  -p 5432:5432 \
  postgres:16

# Run the control plane
barbacane-control serve \
  --database-url postgres://postgres:barbacane@localhost/barbacane \
  --listen 127.0.0.1:9090
```

The server automatically runs database migrations on startup.

### Upload a Spec

```bash
curl -X POST http://localhost:9090/specs \
  -F "file=@api.yaml"
```

Response:
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Petstore API",
  "revision": 1,
  "sha256": "a1b2c3..."
}
```

### Start Compilation

```bash
curl -X POST http://localhost:9090/specs/550e8400-e29b-41d4-a716-446655440000/compile \
  -H "Content-Type: application/json" \
  -d '{"production": true}'
```

Response (202 Accepted):
```json
{
  "id": "660e8400-e29b-41d4-a716-446655440001",
  "spec_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending",
  "production": true,
  "started_at": "2024-01-15T10:30:00Z"
}
```

### Poll Compilation Status

```bash
curl http://localhost:9090/compilations/660e8400-e29b-41d4-a716-446655440001
```

When complete:
```json
{
  "id": "660e8400-e29b-41d4-a716-446655440001",
  "spec_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "succeeded",
  "artifact_id": "770e8400-e29b-41d4-a716-446655440002",
  "started_at": "2024-01-15T10:30:00Z",
  "completed_at": "2024-01-15T10:30:05Z"
}
```

### Download Artifact

```bash
curl -o api.bca http://localhost:9090/artifacts/770e8400-e29b-41d4-a716-446655440002/download
```

## API Reference

Full OpenAPI specification is available at [crates/barbacane-control/openapi.yaml](../../crates/barbacane-control/openapi.yaml).

### Specs

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/specs` | Upload a new spec (multipart) |
| GET | `/specs` | List all specs |
| GET | `/specs/{id}` | Get spec metadata |
| DELETE | `/specs/{id}` | Delete spec and revisions |
| GET | `/specs/{id}/history` | Get revision history |
| GET | `/specs/{id}/content` | Download spec content |

#### Query Parameters

- `type` - Filter by spec type (`openapi` or `asyncapi`)
- `name` - Filter by name (case-insensitive partial match)
- `revision` - Specific revision (for `/content` endpoint)

### Plugins

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/plugins` | Register a plugin (multipart) |
| GET | `/plugins` | List all plugins |
| GET | `/plugins/{name}` | List versions of a plugin |
| GET | `/plugins/{name}/{version}` | Get plugin metadata |
| DELETE | `/plugins/{name}/{version}` | Delete a plugin version |
| GET | `/plugins/{name}/{version}/download` | Download WASM binary |

#### Plugin Registration

```bash
curl -X POST http://localhost:9090/plugins \
  -F "name=my-middleware" \
  -F "version=1.0.0" \
  -F "type=middleware" \
  -F "description=My custom middleware" \
  -F "capabilities=[\"http\", \"log\"]" \
  -F "config_schema={\"type\": \"object\"}" \
  -F "file=@my-middleware.wasm"
```

### Artifacts

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/artifacts` | List all artifacts |
| GET | `/artifacts/{id}` | Get artifact metadata |
| DELETE | `/artifacts/{id}` | Delete an artifact |
| GET | `/artifacts/{id}/download` | Download `.bca` file |

### Compilations

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/specs/{id}/compile` | Start async compilation |
| GET | `/specs/{id}/compilations` | List compilations for a spec |
| GET | `/compilations/{id}` | Get compilation status |
| DELETE | `/compilations/{id}` | Delete compilation record |

#### Compilation Request

```json
{
  "production": true,
  "additional_specs": ["uuid-of-another-spec"]
}
```

#### Compilation Status

| Status | Description |
|--------|-------------|
| `pending` | Job queued, waiting to start |
| `compiling` | Compilation in progress |
| `succeeded` | Completed, `artifact_id` available |
| `failed` | Failed, check `errors` array |

### Health

```bash
curl http://localhost:9090/health
```

Response:
```json
{
  "status": "healthy",
  "version": "0.1.0"
}
```

## Error Handling

All errors follow RFC 9457 Problem Details format:

```json
{
  "type": "urn:barbacane:error:not-found",
  "title": "Not Found",
  "status": 404,
  "detail": "Spec 550e8400-e29b-41d4-a716-446655440000 not found"
}
```

### Error Types

| URN | Status | Description |
|-----|--------|-------------|
| `urn:barbacane:error:not-found` | 404 | Resource not found |
| `urn:barbacane:error:bad-request` | 400 | Invalid request |
| `urn:barbacane:error:conflict` | 409 | Resource already exists or is in use |
| `urn:barbacane:error:spec-invalid` | 422 | Spec validation failed |
| `urn:barbacane:error:internal-error` | 500 | Server error |

## Database Schema

The control plane uses PostgreSQL with the following tables:

- `specs` - Spec metadata (name, type, version, timestamps)
- `spec_revisions` - Version history with content (BYTEA)
- `plugins` - Plugin registry with WASM binaries
- `artifacts` - Compiled `.bca` files with manifests
- `artifact_specs` - Junction table linking artifacts to specs
- `compilations` - Async job tracking

Migrations run automatically on startup with `--migrate` (enabled by default).

## Configuration

### Environment Variables

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string |
| `RUST_LOG` | Log level (trace, debug, info, warn, error) |

### CLI Options

```bash
barbacane-control serve [OPTIONS]

Options:
  --listen <ADDR>        Listen address [default: 127.0.0.1:9090]
  --database-url <URL>   PostgreSQL URL [env: DATABASE_URL]
  --migrate              Run migrations on startup [default: true]
```

## Deployment

### Docker Compose Example

```yaml
version: '3.8'

services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: barbacane
      POSTGRES_PASSWORD: barbacane
    volumes:
      - pgdata:/var/lib/postgresql/data

  control-plane:
    image: barbacane/control:latest
    command: serve --database-url postgres://postgres:barbacane@postgres/barbacane
    ports:
      - "9090:9090"
    depends_on:
      - postgres

volumes:
  pgdata:
```

### Production Considerations

1. **Database backups** - Regular PostgreSQL backups for spec and plugin data
2. **Connection pooling** - Consider PgBouncer for high-traffic deployments
3. **Authentication** - Add a reverse proxy with authentication (not built-in)
4. **TLS** - Terminate TLS at the load balancer or reverse proxy

## What's Next?

- [CLI Reference](../reference/cli.md) - Full command-line options
- [Artifact Format](../reference/artifact.md) - Understanding `.bca` files
- [Getting Started](getting-started.md) - Basic workflow with local compilation
