# Control plane

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

### Signing artifacts

Data planes serve only artifacts signed by the key they pin. Generate a key with `barbacane key generate --out signing.pk8` and set `BARBACANE_SIGNING_KEY` on the control plane to its path: every artifact the control plane compiles is then signed. The control plane reads the key when it starts, and refuses to start when it cannot. `key generate` writes the file readable by its owner only; the control plane's image runs as uid 65532, so give that user the file (`sudo chown 65532 signing.pk8`) before mounting it. Each data plane pins the matching public key (`barbacane key public --key signing.pk8`) with `BARBACANE_TRUSTED_PUBKEY`. The control plane does not hand the key out, so the channel that delivers artifacts cannot also choose the key that vouches for them.

### Point the CLI at it

The `barbacane` binary carries a client for this API, [`barbacane control`](../reference/cli.md#barbacane-control), whose commands are generated from the API's OpenAPI spec:

```bash
export BARBACANE_CONTROL_URL=http://localhost:9090
export BARBACANE_CONTROL_ADMIN_TOKEN=<the token the server runs with>
```

### Upload, compile and download

```bash
barbacane control specs upload --file api.yaml
barbacane control compilations start --id <SPEC_ID>
barbacane control compilations get --id <COMPILATION_ID>
barbacane control artifacts download --id <ARTIFACT_ID> --out api.bca
```

`compilations start` takes its options as JSON from `--file`, such as `{"production": false}` to allow plaintext upstreams; without `--file`, it compiles with the defaults. A compilation runs in the background: `compilations get` shows `pending` or `compiling` until it is `succeeded`, with an `artifact_id`, or `failed`, with its `errors`. For a project, `push` does the upload, the compilation and the wait in one command:

```bash
barbacane control push --project <PROJECT_ID> api.yaml
```

## API Reference

Full OpenAPI specification is available at [crates/barbacane-control/openapi.yaml](https://github.com/barbacane-dev/barbacane/blob/main/crates/barbacane-control/openapi.yaml), and a running control plane serves it at `GET /openapi` (browsable at `/docs`).

### Lists

A list returns at most 100 items unless the request asks for others with `limit`, from 1 to 1000, and skips the first `offset` items. Read a longer list page by page: `?limit=1000&offset=0`, then `offset=1000`, until a page comes back shorter than `limit`. A `limit` or `offset` out of range is answered `400`. The CLI takes them as `--limit` and `--offset`. A project's plugin configurations and the versions of one plugin are returned whole.

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
barbacane control plugins register --name my-middleware --version 1.0.0 \
  --type middleware --description "My custom middleware" --file my-middleware.wasm
```

A compilation reads each plugin's capabilities, category and config schema from the `plugin.toml` and `config-schema.json` embedded in its WASM by the `barbacane-plugin-sdk` macros, and the artifact enforces those capabilities on load. A plugin without an embedded manifest cannot be compiled by the control plane (E1041): rebuild it with the current SDK.

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

Compilations run one at a time, in the order they were requested. A compilation the control plane was running when it stopped goes back to `pending` at the next start, and every `pending` compilation is queued again then: none is lost to a restart.

### Health

```bash
barbacane control health check
```

`/health` needs no token, so a probe can call it directly. It says the process is up and makes no database query: use it as a liveness probe. `/ready`, which needs no token either, answers `200` when the database answers and `503` otherwise, so it serves as a readiness probe; its answer is cached for one second.

The `barbacane-control` image has no shell. Its `HEALTHCHECK` runs [`barbacane-control health`](../reference/cli.md#barbacane-control-health), which calls `/health`; a Compose health check uses the same command, `["CMD", "/usr/local/bin/barbacane-control", "health"]`.

Response:
```json
{
  "status": "healthy",
  "version": "1.0.0"
}
```

### Metrics

`GET /metrics` serves Prometheus metrics, behind the admin token like the rest of the API (in a Prometheus scrape configuration, `authorization: { credentials: <token> }`):

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `barbacane_control_requests_total` | counter | `method`, `route`, `status` | API requests; `route` is the route template (`/projects/{id}`), not the path |
| `barbacane_control_compilations_total` | counter | `outcome` (`succeeded`, `failed`) | Finished compilations |
| `barbacane_control_data_planes_connected` | gauge | - | Data planes with an open session |

## Projects

Projects organize your APIs and configure which plugins to use. Each project can have its own set of specs, plugin configurations, and connected data planes.

Deleting a project deletes everything in it: specs, compilations, artifacts, plugin configurations, API keys and data plane records. The default project, `00000000-0000-0000-0000-000000000001`, receives the specs uploaded with `POST /specs` and cannot be deleted (`409`).

### Create a Project

```bash
barbacane control projects create --file - \
  <<< '{"name": "My API Gateway", "description": "Production gateway"}'
```

### Configure Plugins for a Project

A project's specs may use the plugins added to it, at the registry version each names:

```bash
barbacane control project-plugins add --project-id <PROJECT_ID> --file - \
  <<< '{"plugin_name": "rate-limit", "plugin_version": "0.1.0"}'
```

A compilation refuses a spec that uses a plugin the project does not have, or has with `"enabled": false` (`E1040`). The specs configure each plugin where they use it (`x-barbacane-middlewares`, `x-barbacane-dispatch`), and the compilation validates that configuration against the plugin's JSON Schema.

`limits` sets the plugin's resource limits for the project, replacing its own key by key, as `limits` does in `barbacane.yaml`; a value above the gateway's ceiling is refused with `E1044`. Updating with `"limits": {}` removes them:

```bash
barbacane control project-plugins update --project-id <PROJECT_ID> --name ip-restriction --file - \
  <<< '{"limits": {"memory_mib": 32}}'
```

## Data Planes

Data planes are gateway instances that connect to the control plane to receive configuration updates.

### Data Plane Connection

Data planes connect via WebSocket to receive artifacts and configuration:

```bash
# Start a data plane connected to the control plane
BARBACANE_TRUSTED_PUBKEY=<public key of the control plane's signing key> \
barbacane serve \
  --control-plane wss://control.example.com/ws/data-plane \
  --project-id 880e8400-e29b-41d4-a716-446655440003 \
  --api-key dp_key_abc123
```

Started without `--artifact`, the data plane fetches the project's deployment (`GET /projects/{id}/deployment` with its key), downloads it and serves it; it refuses to start when the project has none yet. Started with `--artifact`, it serves that file until the control plane sends the deployment, which it does when the data plane registers without it. Deployed artifacts are kept in the artifact directory, `--artifact-dir` (`BARBACANE_ARTIFACT_DIR`, `/var/lib/barbacane` in the image), else the directory of `--artifact`: the artifact as `current.bca` and its id as `current.id`. A data plane started again loads `current.bca` and registers as running it. The directory must be writable by the user the data plane runs as, uid 65532 in the image: a named volume mounted at `/var/lib/barbacane` is; a host directory bind-mounted there needs `chown 65532`.

The link carries the data plane's API key, so it uses `wss://`: the control plane serves plain HTTP, and TLS is terminated in front of it by a load balancer or reverse proxy. A `ws://` link is accepted to a loopback address only (`ws://localhost:9090/ws/data-plane` on a development machine), or with `--allow-plaintext-control-plane` on a network you trust.

### Create API Key for Data Plane

```bash
barbacane control api-keys create --project-id <PROJECT_ID> --file - \
  <<< '{"name": "Production Data Plane"}'
```

The response carries the key, prefixed `bbk_`, under `key`.

**Note:** The API key is only shown once at creation time. Store it securely.

The data plane presents its key as `Authorization: Bearer <key>` on the WebSocket upgrade, and the control plane checks it before upgrading: an unknown or expired key, or one without the `data-plane:connect` scope, is answered `401`, and the connection holds no session. The data plane then has 10 seconds to send its registration, which names the same key. A data plane that sends nothing, not even its heartbeat, for three heartbeat intervals (90 seconds) is taken as gone and its session ended.

Revoking a key (`DELETE /projects/{id}/api-keys/{key_id}`) ends the data plane sessions registered with it: each receives a `disconnect` message and the socket closes. Every session also rechecks its key at the heartbeat interval (30 seconds), so an expired key, or one revoked through another control-plane instance, ends the session too.

### List Connected Data Planes

```bash
barbacane control data-planes list --project-id <PROJECT_ID>
```

Each data plane reports its `status` (`online`, `deploying` or `offline`) and the `artifact_id` it runs. It is `deploying` from the moment it is sent the project's deployment until it reports loading it, or failing to: `deploy_error` then says why, and is cleared once it loads an artifact. `drift_detected` is set when the artifact it reports is not the project's deployment.

## Deploy

Deploy compiled artifacts to connected data planes for zero-downtime updates.

### Trigger Deployment

```bash
barbacane control deploy --project <PROJECT_ID> --artifact <ARTIFACT_ID> --wait
```

Without `--artifact`, the project's latest artifact is deployed. `--wait` returns once every connected data plane runs it, or fails after `--timeout` seconds (120 by default).

A deploy makes the artifact the project's deployment (`GET /projects/{id}/deployment` returns it, to the admin token or a data-plane key of the project). The control plane sends it to the connected data planes, which download it with their API key (`GET /artifacts/{id}/download` takes the admin token or the data-plane key of the artifact's project), verify its checksum, and perform a hot-reload. A data plane that registers later without it, a new replica or one that was offline, is sent it on registration. A deployment that arrives while a reload runs waits for it.

## Interactive API Documentation

The control plane includes interactive API documentation powered by Scalar:

```
http://localhost:9090/docs
```

This provides a browsable interface for exploring and testing all API endpoints directly from your browser. `/docs` and the description it loads from `/openapi` need no token; the calls made from the page do.

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
| `urn:barbacane:error:bad-request` | 400 | Invalid request: a parameter, a spec, or a JSON body that does not parse or does not fit the operation |
| `urn:barbacane:error:unauthorized` | 401 | No token, or one the operation does not accept |
| `urn:barbacane:error:not-found` | 404 | Resource or path not found |
| `urn:barbacane:error:method-not-allowed` | 405 | The path has no such method; `Allow` lists those it has |
| `urn:barbacane:error:conflict` | 409 | Resource already exists or is in use |
| `urn:barbacane:error:content-too-large` | 413 | The request body is over the limit |
| `urn:barbacane:error:unsupported-media-type` | 415 | The body is not `application/json` where JSON is expected |
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

Migrations run on startup; `--migrate=false` skips them.

## Configuration

### Environment Variables

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | PostgreSQL connection string, as `--database-url` takes it |
| `BARBACANE_CONTROL_ADMIN_TOKEN` | **Required**, at least 32 bytes. Bearer token every API request carries, except `GET /health`, `GET /ready`, `GET /openapi`, `GET /docs` and the data-plane WebSocket, which take none; `GET /artifacts/{id}/download` and `GET /projects/{id}/deployment` take it or a data-plane API key of the project. The server refuses to start without it |
| `BARBACANE_CONTROL_ADMIN_TOKEN_PREVIOUS` | The token being replaced, accepted during a rotation |
| `BARBACANE_SIGNING_KEY` | Path to the Ed25519 key that signs every artifact the control plane compiles; unset, artifacts are built unsigned |
| `BARBACANE_CONTROL_ALLOWED_ORIGINS` | Comma-separated allowlist of browser origins for CORS; unset means no cross-origin access |
| `RUST_LOG` | Log level (trace, debug, info, warn, error); every request that changes something is logged at `info` with its method, path and status |

To rotate the admin token: start the control plane with the new token in `BARBACANE_CONTROL_ADMIN_TOKEN` and the old one in `BARBACANE_CONTROL_ADMIN_TOKEN_PREVIOUS`, move every client to the new token, then restart without the previous one.

### PostgreSQL over TLS

`DATABASE_URL` takes libpq's TLS parameters:

| Parameter | Effect |
|-----------|--------|
| `sslmode=disable` | Plaintext |
| `sslmode=prefer` (the default) | TLS when the server offers it, without checking its certificate; plaintext otherwise |
| `sslmode=require` | TLS, without checking the server's certificate |
| `sslmode=verify-ca` | TLS; the server's certificate must come from a trusted CA |
| `sslmode=verify-full` | As `verify-ca`, and the certificate must name the host in the URL |
| `sslrootcert=<path>` | A PEM file of CAs to trust, besides the public CAs the control plane ships with (Mozilla's list) |
| `sslcert=<path>`, `sslkey=<path>` | A client certificate and its key, for a server that asks for one |

In production, use `verify-full`, with `sslrootcert` when the database's certificate comes from the provider's own CA:

```text
postgres://barbacane:secret@db.example.com:5432/barbacane?sslmode=verify-full&sslrootcert=/etc/barbacane/db-ca.pem
```

The [configuration reference](../reference/configuration.md) describes each variable.

### CLI Options

```bash
barbacane-control serve [OPTIONS]

Options:
  --listen <ADDR>        Listen address [default: 127.0.0.1:9090]
  --database-url <URL>   PostgreSQL URL [env: DATABASE_URL]
  --migrate[=<BOOL>]     Run migrations on startup [default: true]
```

## Deployment

### Container Images

Official container images are available from Docker Hub:

```bash
# Control plane
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
- `x.y.z` - Specific version (e.g., `1.0.0`)
- `x.y` - Latest patch for minor version (e.g., `1.0`)
- `x` - Latest minor for major version (e.g., `1`)
- `x.y.z-rc.n` - A pre-release (e.g., `1.0.0-rc.1`), published under its own tag only: `latest`, `x.y` and `x` stay on the last stable release

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
      BARBACANE_CONTROL_ADMIN_TOKEN: ${BARBACANE_CONTROL_ADMIN_TOKEN}
      BARBACANE_SIGNING_KEY: /run/secrets/signing.pk8
    volumes:
      - ./signing.pk8:/run/secrets/signing.pk8:ro
    ports:
      - "9090:9090"  # API
    depends_on:
      - postgres

  data-plane:
    image: ghcr.io/barbacane-dev/barbacane:latest
    # Plaintext over compose's private network only; use wss:// elsewhere.
    # Without --artifact, it starts from the project's deployment.
    command: >
      serve
      --control-plane ws://control-plane:9090/ws/data-plane
      --allow-plaintext-control-plane
      --project-id ${PROJECT_ID}
      --api-key ${DATA_PLANE_API_KEY}
    environment:
      BARBACANE_TRUSTED_PUBKEY: ${BARBACANE_TRUSTED_PUBKEY}
    volumes:
      - artifacts:/var/lib/barbacane
    ports:
      - "8080:8080"
    depends_on:
      - control-plane

volumes:
  pgdata:
  artifacts:
```

### Production Considerations

1. **Database backups** - Regular PostgreSQL backups for spec and plugin data
2. **Connection pooling** - Consider PgBouncer for high-traffic deployments
3. **Authentication** - Every API request carries the admin token (`BARBACANE_CONTROL_ADMIN_TOKEN`), and data planes present a project API key; keep the token in a secret store and rotate the keys from `api-keys`
4. **TLS** - Terminate TLS at the load balancer or reverse proxy
5. **One instance** - Run a single control plane per database. Deployment notifications reach the data planes connected to the instance that handles the deploy request, and compilations are queued in that instance; a second instance on the same database does not see either.
6. **Shutdown** - On `SIGTERM` or `SIGINT` the control plane stops accepting connections, closes the data planes' sessions (they reconnect, to the next instance once it is up), lets the compilation in progress finish, and exits 0. Requests in flight and that compilation get 30 seconds; a compilation still running then is compiled again at the next start.

## What's Next?

- [CLI Reference](../reference/cli.md) - Full command-line options
- [Artifact Format](../reference/artifact.md) - Understanding `.bca` files
- [Getting Started](getting-started.md) - Basic workflow with local compilation
