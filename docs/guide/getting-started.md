# Getting Started

This guide walks you through creating your first Barbacane-powered API gateway.

## Prerequisites

- An OpenAPI 3.x specification
- One of the installation methods below

## Installation

### Pre-built Binaries (Recommended)

Download the latest release for your platform from [GitHub Releases](https://github.com/barbacane-dev/barbacane/releases):

```bash
# Linux (x86_64)
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-x86_64-unknown-linux-gnu
chmod +x barbacane-x86_64-unknown-linux-gnu
sudo mv barbacane-x86_64-unknown-linux-gnu /usr/local/bin/barbacane

# Linux (ARM64)
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-aarch64-unknown-linux-gnu
chmod +x barbacane-aarch64-unknown-linux-gnu
sudo mv barbacane-aarch64-unknown-linux-gnu /usr/local/bin/barbacane

# macOS (Intel)
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-x86_64-apple-darwin
chmod +x barbacane-x86_64-apple-darwin
sudo mv barbacane-x86_64-apple-darwin /usr/local/bin/barbacane

# macOS (Apple Silicon)
curl -LO https://github.com/barbacane-dev/barbacane/releases/latest/download/barbacane-aarch64-apple-darwin
chmod +x barbacane-aarch64-apple-darwin
sudo mv barbacane-aarch64-apple-darwin /usr/local/bin/barbacane
```

Verify installation:
```bash
barbacane --version
```

### Container Images

For Docker or Kubernetes deployments:

```bash
# Data plane (from Docker Hub)
docker pull barbacane/barbacane:latest

# Control plane (from Docker Hub)
docker pull barbacane/barbacane-control:latest
```

Also available from GitHub Container Registry:
```bash
docker pull ghcr.io/barbacane-dev/barbacane:latest
docker pull ghcr.io/barbacane-dev/barbacane-control:latest
```

Quick start with Docker:
```bash
docker run -v ./api.bca:/config/api.bca -p 8080:8080 \
  ghcr.io/barbacane-dev/barbacane serve --artifact /config/api.bca
```

### Using Cargo

If you have Rust installed:

```bash
cargo install barbacane
cargo install barbacane-control  # Optional: control plane CLI
```

### From Source

For development or custom builds:

```bash
git clone https://github.com/barbacane-dev/barbacane.git
cd barbacane
cargo build --release

# Binaries are in target/release/
```

## Your First Gateway

### Quick Start with `barbacane init`

The fastest way to start a new project:

```bash
# Create a new project with example spec and official plugins
barbacane init my-api --fetch-plugins

cd my-api
```

This creates:
- `barbacane.yaml` — project manifest with plugins configured
- `api.yaml` — OpenAPI spec with example endpoints
- `plugins/mock.wasm` — mock dispatcher plugin
- `plugins/http-upstream.wasm` — HTTP proxy plugin
- `.gitignore` — ignores build artifacts

For a minimal skeleton without example endpoints:

```bash
barbacane init my-api --template minimal --fetch-plugins
```

Skip to [Step 3: Validate the Spec](#3-validate-the-spec) if using `barbacane init`.

### 1. Create an OpenAPI Spec

Create a file called `api.yaml`:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

paths:
  /health:
    get:
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{"status":"ok"}'
      responses:
        "200":
          description: Health check response

  /users:
    get:
      operationId: listUsers
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users
      responses:
        "200":
          description: List of users

  /users/{id}:
    get:
      operationId: getUser
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://api.example.com"
          path: /api/users/{id}
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
            format: uuid
      responses:
        "200":
          description: User details
```

The key additions are:
- `x-barbacane-dispatch` on each operation: tells Barbacane how to handle the request

### 2. Create a Manifest

Create a `barbacane.yaml` manifest to declare which plugins to use:

```yaml
plugins:
  mock:
    path: ./plugins/mock.wasm
  http-upstream:
    path: ./plugins/http-upstream.wasm
```

The manifest declares all WASM plugins used by your spec. Plugins can be sourced from:
- **Local path**: `path: ./plugins/name.wasm`
- **URL** (planned): `url: https://plugins.example.com/name.wasm`

### 3. Validate the Spec

```bash
barbacane validate --spec api.yaml
```

Output:
```
✓ api.yaml is valid

validated 1 spec(s): 1 valid, 0 invalid
```

### 4. Compile to Artifact

```bash
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca
```

Output:
```
compiled 1 spec(s) to api.bca (3 routes, 2 plugin(s) bundled)
```

The `.bca` (Barbacane Compiled Artifact) file contains:
- Compiled routing table
- Embedded source specs (for `/__barbacane/specs`)
- Bundled WASM plugins
- Manifest with checksums

### 5. Run the Gateway

```bash
barbacane serve --artifact api.bca --listen 127.0.0.1:8080 --dev
```

Output:
```
barbacane: loaded 3 route(s) from artifact
barbacane: listening on 127.0.0.1:8080
```

### 6. Test It

```bash
# Health check (mock dispatcher)
curl http://127.0.0.1:8080/health
# {"status":"ok"}

# Gateway health
curl http://127.0.0.1:8080/__barbacane/health
# {"status":"healthy","artifact_version":1,"compiler_version":"0.1.0","routes_count":3}

# View the API specs
curl http://127.0.0.1:8080/__barbacane/specs
# Returns index of specs with links to merged OpenAPI/AsyncAPI

# Try a non-existent route
curl http://127.0.0.1:8080/nonexistent
# {"error":"not found"}

# Try wrong method
curl -X POST http://127.0.0.1:8080/health
# {"error":"method not allowed"}
```

## What's Next?

- [Spec Configuration](spec-configuration.md) - Learn about all `x-barbacane-*` extensions
- [Dispatchers](dispatchers.md) - Route to HTTP backends, mock responses, and more
- [Middlewares](middlewares.md) - Add authentication, rate limiting, CORS
- [Secrets](secrets.md) - Manage API keys, tokens, and passwords securely
- [Observability](observability.md) - Metrics, logging, and distributed tracing
- [Control Plane](control-plane.md) - Manage specs and artifacts via REST API
- [Web UI](web-ui.md) - Visual interface for managing your gateway

## Development Mode

The `--dev` flag enables:
- Verbose error messages with dispatcher details
- Detailed logging
- No production-only restrictions

For production, omit the flag:
```bash
barbacane serve --artifact api.bca --listen 0.0.0.0:8080
```

## Observability

Barbacane includes built-in observability features:

```bash
# Pretty logs for development
barbacane serve --artifact api.bca --log-format pretty --log-level debug

# JSON logs with OTLP tracing for production
barbacane serve --artifact api.bca \
  --log-format json \
  --otlp-endpoint http://otel-collector:4317
```

Prometheus metrics are available at `/__barbacane/metrics`:

```bash
curl http://127.0.0.1:8080/__barbacane/metrics
```

See the [Observability Guide](observability.md) for full details.
