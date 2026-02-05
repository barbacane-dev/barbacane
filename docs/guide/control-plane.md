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

## API Versioning

All JSON responses from the control plane include a versioned content type:

```
Content-Type: application/vnd.barbacane.v1+json
```

This allows clients to detect the API version and handle future breaking changes gracefully.

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

## Projects

Projects organize your APIs and configure which plugins to use. Each project can have its own set of specs, plugin configurations, and connected data planes.

### Create a Project

```bash
curl -X POST http://localhost:9090/projects \
  -H "Content-Type: application/json" \
  -d '{"name": "My API Gateway", "description": "Production gateway"}'
```

Response:
```json
{
  "id": "880e8400-e29b-41d4-a716-446655440003",
  "name": "My API Gateway",
  "description": "Production gateway",
  "created_at": "2024-01-15T10:00:00Z"
}
```

### Configure Plugins for a Project

Add plugins from the registry to your project with custom configuration:

```bash
curl -X POST http://localhost:9090/projects/880e8400.../plugins \
  -H "Content-Type: application/json" \
  -d '{
    "plugin_name": "rate-limit",
    "plugin_version": "0.1.0",
    "enabled": true,
    "config": {
      "quota": 1000,
      "window": 60
    }
  }'
```

Each plugin's configuration is validated against its JSON Schema (if one is defined).

## Data Planes

Data planes are gateway instances that connect to the control plane to receive configuration updates.

### Data Plane Connection

Data planes connect via WebSocket to receive artifacts and configuration:

```bash
# Start a data plane connected to the control plane
barbacane serve \
  --control-plane ws://localhost:9090/ws/data-plane \
  --project-id 880e8400-e29b-41d4-a716-446655440003 \
  --api-key dp_key_abc123
```

### Create API Key for Data Plane

```bash
curl -X POST http://localhost:9090/projects/880e8400.../api-keys \
  -H "Content-Type: application/json" \
  -d '{"name": "Production Data Plane"}'
```

Response:
```json
{
  "id": "990e8400-e29b-41d4-a716-446655440004",
  "name": "Production Data Plane",
  "key": "dp_key_abc123...",
  "created_at": "2024-01-15T10:30:00Z"
}
```

**Note:** The API key is only shown once at creation time. Store it securely.

### List Connected Data Planes

```bash
curl http://localhost:9090/projects/880e8400.../data-planes
```

Response:
```json
[
  {
    "id": "aa0e8400-e29b-41d4-a716-446655440005",
    "name": "production-1",
    "status": "connected",
    "current_artifact_id": "770e8400...",
    "connected_at": "2024-01-15T10:35:00Z"
  }
]
```

## Deploy

Deploy compiled artifacts to connected data planes for zero-downtime updates.

### Trigger Deployment

```bash
curl -X POST http://localhost:9090/projects/880e8400.../deploy \
  -H "Content-Type: application/json" \
  -d '{"artifact_id": "770e8400-e29b-41d4-a716-446655440002"}'
```

Response:
```json
{
  "deployment_id": "bb0e8400-e29b-41d4-a716-446655440006",
  "artifact_id": "770e8400...",
  "target_data_planes": 3,
  "status": "in_progress"
}
```

The control plane notifies all connected data planes, which download the new artifact, verify its checksum, and perform a hot-reload.

## Web UI

The control plane includes a web-based management interface at `http://localhost:5173` (when running the UI development server).

### Running the UI

```bash
# Using Makefile
make ui

# Or manually
cd ui && npm run dev
```

The UI provides:

- **Dashboard** - Overview of specs, artifacts, and data planes
- **Specs Management** - Upload, view, and delete API specifications
- **Plugin Registry** - Browse registered plugins with their schemas
- **Projects** - Create projects and configure plugins
- **Artifacts** - View compiled artifacts and download them

### Plugin Configuration

When adding plugins to a project, the UI:
- Shows the plugin's JSON Schema (if available)
- Pre-fills a skeleton configuration based on required fields
- Validates configuration in real-time before saving

## Interactive API Documentation

The control plane includes interactive API documentation powered by Scalar:

```
http://localhost:9090/api/docs
```

This provides a browsable interface for exploring and testing all API endpoints directly from your browser.

## Seeding the Plugin Registry

Use the `seed-plugins` command to populate the plugin registry with built-in plugins:

```bash
# Using Makefile (builds plugins first)
make seed-plugins

# Or manually
barbacane-control seed-plugins \
  --plugins-dir plugins \
  --database-url postgres://localhost/barbacane \
  --verbose
```

This scans the `plugins/` directory for plugin manifests (`plugin.toml`) and registers them in the database along with their WASM binaries and JSON Schemas.

See [CLI Reference](../reference/cli.md#barbacane-control-seed-plugins) for full options.

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
- `plugins` - Plugin registry with WASM binaries and JSON Schemas
- `artifacts` - Compiled `.bca` files with manifests
- `artifact_specs` - Junction table linking artifacts to specs
- `compilations` - Async job tracking
- `projects` - Project definitions
- `project_plugin_configs` - Plugin configurations per project
- `data_planes` - Connected gateway instances
- `api_keys` - Authentication keys for data planes

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

### Container Images

Official container images are available from Docker Hub:

```bash
# Control plane (includes web UI)
docker pull barbacane/barbacane-control:latest

# Data plane
docker pull barbacane/barbacane:latest
```

Also available from GitHub Container Registry:
```bash
docker pull ghcr.io/barbacane-dev/barbacane-control:latest
docker pull ghcr.io/barbacane-dev/barbacane:latest
```

Images are available for:
- `linux/amd64` (x86_64)
- `linux/arm64` (ARM64/Graviton)

Tags:
- `latest` - Latest stable release
- `x.y.z` - Specific version (e.g., `0.2.0`)
- `x.y` - Latest patch for minor version (e.g., `0.2`)
- `x` - Latest minor for major version (e.g., `0`)

### Docker Compose Example

```yaml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: barbacane
      POSTGRES_PASSWORD: barbacane
    volumes:
      - pgdata:/var/lib/postgresql/data

  control-plane:
    image: ghcr.io/barbacane-dev/barbacane-control:latest
    environment:
      DATABASE_URL: postgres://postgres:barbacane@postgres/barbacane
    ports:
      - "80:80"      # Web UI
      - "9090:9090"  # API
    depends_on:
      - postgres

  data-plane:
    image: ghcr.io/barbacane-dev/barbacane:latest
    command: >
      serve
      --control-plane ws://control-plane:9090/ws/data-plane
      --project-id ${PROJECT_ID}
      --api-key ${DATA_PLANE_API_KEY}
    ports:
      - "8080:8080"
    depends_on:
      - control-plane

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
