# CLI Reference

Barbacane provides two command-line tools:
- **barbacane** - Data plane (gateway) for compiling specs and serving traffic
- **barbacane-control** - Control plane for managing specs, plugins, and artifacts via REST API

## barbacane

```bash
barbacane <COMMAND> [OPTIONS]
```

### Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize a new Barbacane project |
| `compile` | Compile OpenAPI spec(s) into a `.bca` artifact |
| `validate` | Validate spec(s) without compiling |
| `serve` | Run the gateway server |

---

## barbacane init

Initialize a new Barbacane project with manifest, spec, and directory structure.

```bash
barbacane init [NAME] [OPTIONS]
```

### Arguments

| Argument | Required | Default | Description |
|----------|----------|---------|-------------|
| `NAME` | No | `.` | Project name (creates a directory with this name, or initializes in current directory if `.`) |

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--template`, `-t` | No | `basic` | Template to use: `basic` (full example) or `minimal` (bare bones) |
| `--fetch-plugins` | No | `false` | Download official plugins (mock, http-upstream) from GitHub releases |

### Plugin Download

The `--fetch-plugins` flag downloads official Barbacane plugins from GitHub releases:

- **mock** — Returns static responses (useful for testing and mocking)
- **http-upstream** — Proxies requests to HTTP/HTTPS backends

Downloaded plugins are placed in the `plugins/` directory and automatically configured in `barbacane.yaml`.

```bash
# Create project with plugins downloaded
barbacane init my-api --fetch-plugins
```

If download fails (e.g., network issues), the project is still created with an empty plugins directory.

### Templates

**basic** (default):
- Complete OpenAPI spec with `/health` and `/users` endpoints
- Example `x-barbacane-dispatch` configurations
- Ready to compile and run

**minimal**:
- Bare-bones OpenAPI spec with just the required structure
- Single `/health` endpoint placeholder
- Start from scratch

### Examples

```bash
# Create project in new directory with basic template
barbacane init my-api

# Create project with official plugins downloaded
barbacane init my-api --fetch-plugins

# Create project with minimal template
barbacane init my-api --template minimal

# Initialize in current directory
barbacane init .

# Short form
barbacane init my-api -t minimal
```

### Generated Files

```
my-api/
├── barbacane.yaml    # Project manifest (plugin declarations)
├── api.yaml          # OpenAPI 3.1 specification
├── plugins/          # Directory for WASM plugins
└── .gitignore        # Ignores *.bca, target/, plugins/*.wasm
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Directory exists and is not empty, or write error |

---

## barbacane compile

Compile one or more OpenAPI specs into a `.bca` artifact.

```bash
barbacane compile --spec <FILES>... --output <PATH>
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--spec`, `-s` | Yes | - | One or more spec files (YAML or JSON) |
| `--output`, `-o` | Yes | - | Output artifact path |
| `--manifest`, `-m` | No | - | Path to `barbacane.yaml` manifest (required for plugin bundling) |
| `--allow-plaintext` | No | `false` | Allow `http://` upstream URLs during compilation |

### Examples

```bash
# Compile single spec with manifest
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Compile multiple specs
barbacane compile -s users.yaml -s orders.yaml -m barbacane.yaml -o combined.bca

# Short form
barbacane compile -s api.yaml -m barbacane.yaml -o api.bca

# Legacy compilation without manifest (no plugins bundled)
barbacane compile -s api.yaml -o api.bca
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Compilation error (validation failed, routing conflict, undeclared plugin) |
| 2 | Manifest or plugin resolution error |

---

## barbacane validate

Validate specs without full compilation. Checks for spec validity and extension errors.

```bash
barbacane validate --spec <FILES>... [OPTIONS]
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--spec`, `-s` | Yes | - | One or more spec files to validate |
| `--format` | No | `text` | Output format: `text` or `json` |

### Error Codes

| Code | Category | Description |
|------|----------|-------------|
| E1001 | Spec validity | Not a valid OpenAPI 3.x or AsyncAPI 3.x |
| E1002 | Spec validity | YAML/JSON parse error |
| E1003 | Spec validity | Unresolved `$ref` reference |
| E1004 | Spec validity | Schema validation error (missing info, etc.) |
| E1010 | Extension | Routing conflict (same path+method in multiple specs) |
| E1011 | Extension | Middleware entry missing `name` |
| E1015 | Extension | Unknown `x-barbacane-*` extension (warning) |
| E1020 | Extension | Operation missing `x-barbacane-dispatch` (warning) |
| E1031 | Extension | Plaintext HTTP URL not allowed (use `--allow-plaintext` to override) |
| E1040 | Manifest | Plugin used in spec but not declared in `barbacane.yaml` |

### Examples

```bash
# Validate single spec
barbacane validate --spec api.yaml

# Validate multiple specs (checks for routing conflicts)
barbacane validate -s users.yaml -s orders.yaml

# JSON output (for CI/tooling)
barbacane validate --spec api.yaml --format json
```

### Output Examples

**Text format (default):**
```
✓ api.yaml is valid

validated 1 spec(s): 1 valid, 0 invalid
```

**Text format with errors:**
```
✗ api.yaml has 1 error(s)
  E1004 [api.yaml]: E1004: schema validation error: missing 'info' object

validated 1 spec(s): 0 valid, 1 invalid
```

**JSON format:**
```json
{
  "results": [
    {
      "file": "api.yaml",
      "valid": true,
      "errors": [],
      "warnings": []
    }
  ],
  "summary": {
    "total": 1,
    "valid": 1,
    "invalid": 0
  }
}
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | All specs valid |
| 1 | One or more specs have errors |

---

## barbacane serve

Run the gateway server, loading routes from a compiled artifact.

```bash
barbacane serve --artifact <PATH> [OPTIONS]
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--artifact` | Yes | - | Path to the `.bca` artifact file |
| `--listen` | No | `0.0.0.0:8080` | Listen address (ip:port) |
| `--dev` | No | `false` | Enable development mode |
| `--log-level` | No | `info` | Log level (trace, debug, info, warn, error) |
| `--log-format` | No | `json` | Log format (`json` or `pretty`) |
| `--otlp-endpoint` | No | - | OpenTelemetry endpoint for trace export (e.g., `http://localhost:4317`) |
| `--max-body-size` | No | `1048576` | Maximum request body size in bytes (1MB) |
| `--max-headers` | No | `100` | Maximum number of request headers |
| `--max-header-size` | No | `8192` | Maximum size of a single header in bytes (8KB) |
| `--max-uri-length` | No | `8192` | Maximum URI length in characters (8KB) |
| `--allow-plaintext-upstream` | No | `false` | Allow `http://` upstream URLs (dev only) |
| `--tls-cert` | No | - | Path to TLS certificate file (PEM format) |
| `--tls-key` | No | - | Path to TLS private key file (PEM format) |
| `--tls-min-version` | No | `1.2` | Minimum TLS version (`1.2` or `1.3`) |
| `--keepalive-timeout` | No | `60` | HTTP keep-alive idle timeout in seconds |
| `--shutdown-timeout` | No | `30` | Graceful shutdown timeout in seconds |

### Examples

```bash
# Run with defaults (HTTP)
barbacane serve --artifact api.bca

# Custom port
barbacane serve --artifact api.bca --listen 127.0.0.1:3000

# Development mode (verbose errors)
barbacane serve --artifact api.bca --dev

# Production with TLS (HTTPS)
barbacane serve --artifact api.bca \
  --tls-cert /etc/barbacane/certs/server.crt \
  --tls-key /etc/barbacane/certs/server.key

# Production with custom limits
barbacane serve --artifact api.bca \
  --max-body-size 5242880 \
  --max-headers 50

# With observability (OTLP export)
barbacane serve --artifact api.bca \
  --log-format json \
  --otlp-endpoint http://otel-collector:4317

# Development mode with pretty logging
barbacane serve --artifact api.bca --dev --log-format pretty

# All options
barbacane serve --artifact api.bca \
  --listen 0.0.0.0:8080 \
  --tls-cert /etc/barbacane/certs/server.crt \
  --tls-key /etc/barbacane/certs/server.key \
  --log-level info \
  --log-format json \
  --otlp-endpoint http://otel-collector:4317 \
  --max-body-size 1048576 \
  --max-headers 100 \
  --max-header-size 8192 \
  --max-uri-length 8192
```

### TLS Termination

The gateway supports HTTPS with TLS termination. To enable TLS, provide both `--tls-cert` and `--tls-key`:

```bash
barbacane serve --artifact api.bca \
  --tls-cert /path/to/server.crt \
  --tls-key /path/to/server.key
```

For maximum security with TLS 1.3 only (modern clients):

```bash
barbacane serve --artifact api.bca \
  --tls-cert /path/to/server.crt \
  --tls-key /path/to/server.key \
  --tls-min-version 1.3
```

**TLS Configuration:**
- Minimum TLS version: 1.2 (default) or 1.3 (via `--tls-min-version`)
- Modern cipher suites (via aws-lc-rs)
- ALPN support for HTTP/2 and HTTP/1.1

**Certificate Requirements:**
- Certificate and key must be in PEM format
- Certificate file can contain the full chain (cert + intermediates)
- Both `--tls-cert` and `--tls-key` must be provided together

### HTTP/2 Support

The gateway supports both HTTP/1.1 and HTTP/2 with automatic protocol detection:

- **With TLS**: HTTP/2 is negotiated via ALPN (Application-Layer Protocol Negotiation). Clients that support HTTP/2 will automatically use it when connecting over HTTPS.
- **Without TLS**: HTTP/1.1 is used by default. HTTP/2 cleartext (h2c) is also supported via protocol detection.

**HTTP/2 Features:**
- Multiplexed streams over a single connection
- Header compression (HPACK)
- Keep-alive with configurable ping intervals (20 seconds)
- Full support for all gateway features (routing, validation, middlewares)

No configuration is needed—HTTP/2 works automatically when TLS is enabled. To verify HTTP/2 is working:

```bash
# Test HTTP/2 with curl
curl -v --http2 https://localhost:8080/__barbacane/health

# Expected output shows HTTP/2:
# * Using HTTP/2
# < HTTP/2 200
```

### Development Mode

The `--dev` flag enables:
- Verbose error messages with field names, locations, and detailed reasons
- Extended RFC 9457 problem details with `errors` array
- Useful for debugging but **do not use in production** - it may expose internal information

### Request Limits

The gateway enforces request limits to protect against abuse:

| Limit | Default | Description |
|-------|---------|-------------|
| Body size | 1 MB | Requests with larger bodies are rejected with 400 |
| Header count | 100 | Requests with more headers are rejected with 400 |
| Header size | 8 KB | Individual headers larger than this are rejected |
| URI length | 8 KB | URIs longer than this are rejected with 400 |

Requests exceeding limits receive an RFC 9457 problem details response:

```json
{
  "type": "urn:barbacane:error:validation-failed",
  "title": "Request validation failed",
  "status": 400,
  "detail": "request body too large: 2000000 bytes exceeds limit of 1048576 bytes"
}
```

### Graceful Shutdown

The gateway handles shutdown signals (SIGTERM, SIGINT) gracefully:

1. **Stop accepting** new connections immediately
2. **Drain** in-flight requests for up to `--shutdown-timeout` seconds (default: 30)
3. **Force close** any remaining connections after timeout
4. **Exit** with code 0 on successful shutdown

```bash
# Send SIGTERM to gracefully shutdown
kill -TERM $(pgrep barbacane)

# Output during graceful shutdown
barbacane: received shutdown signal, draining connections...
barbacane: waiting for 3 active connection(s) to complete...
barbacane: all connections drained, shutting down
```

### Response Headers

Every response includes these standard headers:

| Header | Description |
|--------|-------------|
| `Server` | `barbacane/<version>` (e.g., `barbacane/0.1.0`) |
| `X-Request-Id` | Request ID - propagates incoming header or generates UUID v4 |
| `X-Trace-Id` | Trace ID - extracted from `traceparent` header or generated |
| `X-Content-Type-Options` | `nosniff` - prevents MIME sniffing attacks |
| `X-Frame-Options` | `DENY` - prevents clickjacking via iframes |

Example response headers:

```
HTTP/1.1 200 OK
Server: barbacane/0.1.0
X-Request-Id: 550e8400-e29b-41d4-a716-446655440000
X-Trace-Id: 4bf92f3577b34da6a3ce929d0e0e4736
X-Content-Type-Options: nosniff
X-Frame-Options: DENY
Content-Type: application/json
```

### API Lifecycle Headers

For deprecated operations, additional headers are included:

| Header | Description |
|--------|-------------|
| `Deprecation` | `true` - indicates the endpoint is deprecated (per draft-ietf-httpapi-deprecation-header) |
| `Sunset` | HTTP-date when the endpoint will be removed (per RFC 8594) |

Example for deprecated endpoint:

```
HTTP/1.1 200 OK
Server: barbacane/0.1.0
Deprecation: true
Sunset: Sat, 31 Dec 2025 23:59:59 GMT
Content-Type: application/json
```

See [API Lifecycle](#api-lifecycle) for configuration details.

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean shutdown |
| 1 | Startup error (artifact not found, bind failed) |
| 11 | Plugin hash mismatch (artifact tampering detected) |
| 13 | Secret resolution failure (missing env var or file) |

Exit code 13 occurs when a secret reference in your spec cannot be resolved:

```bash
$ export OAUTH2_SECRET=""  # unset the variable
$ unset OAUTH2_SECRET
$ barbacane serve --artifact api.bca
error: failed to resolve secrets: environment variable not found: OAUTH2_SECRET
$ echo $?
13
```

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Override log level (e.g., `RUST_LOG=debug`) |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Alternative to `--otlp-endpoint` flag |

---

## Observability

Barbacane provides built-in observability features:

### Logging

Structured logs are written to stdout in either JSON (default) or pretty format:

```bash
# JSON format (production)
barbacane serve --artifact api.bca --log-format json

# Pretty format (development)
barbacane serve --artifact api.bca --log-format pretty --log-level debug
```

### Metrics

Prometheus metrics are exposed at `/__barbacane/metrics`:

```bash
curl http://localhost:8080/__barbacane/metrics
```

Key metrics include:
- `barbacane_requests_total` - Request counter by method, path, status
- `barbacane_request_duration_seconds` - Request latency histogram
- `barbacane_active_connections` - Current connection count
- `barbacane_validation_failures_total` - Validation error counter

### Distributed Tracing

Enable OTLP export to send traces to OpenTelemetry Collector:

```bash
barbacane serve --artifact api.bca \
  --otlp-endpoint http://otel-collector:4317
```

Barbacane supports W3C Trace Context propagation (`traceparent`/`tracestate` headers) for distributed tracing across services.

---

### Secret References

Dispatcher and middleware configs can reference secrets using special URI schemes. These are resolved at startup:

| Scheme | Example | Description |
|--------|---------|-------------|
| `env://` | `env://API_KEY` | Read from environment variable |
| `file://` | `file:///etc/secrets/key` | Read from file |

Example config with secrets:
```yaml
x-barbacane-middlewares:
  - name: oauth2-auth
    config:
      client_secret: "env://OAUTH2_SECRET"
```

Run with:
```bash
export OAUTH2_SECRET="my-secret-value"
barbacane serve --artifact api.bca
```

See [Secrets Guide](../guide/secrets.md) for full documentation.

---

## Common Workflows

### Development Cycle

```bash
# Edit spec and manifest
vim api.yaml barbacane.yaml

# Validate (quick check)
barbacane validate --spec api.yaml

# Compile with manifest
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Run in dev mode
barbacane serve --artifact api.bca --dev
```

### CI/CD Pipeline

```bash
#!/bin/bash
set -e

# Validate all specs
barbacane validate --spec specs/*.yaml --format json > validation.json

# Compile artifact with manifest
barbacane compile \
  --spec specs/users.yaml \
  --spec specs/orders.yaml \
  --manifest barbacane.yaml \
  --output dist/gateway.bca

echo "Artifact built: dist/gateway.bca"
```

### Multi-Spec Gateway

```bash
# Compile multiple specs into one artifact
barbacane compile \
  --spec users-api.yaml \
  --spec orders-api.yaml \
  --spec payments-api.yaml \
  --output combined.bca

# Routes from all specs are merged
# Conflicts (same path+method) cause E1010 error
```

### Testing Locally

```bash
# Start gateway
barbacane serve --artifact api.bca --dev --listen 127.0.0.1:8080 &

# Test endpoints
curl http://localhost:8080/health
curl http://localhost:8080/__barbacane/health
curl http://localhost:8080/__barbacane/openapi

# Stop gateway
kill %1
```

---

## barbacane-control

The control plane CLI for managing specs, plugins, and artifacts via REST API.

```bash
barbacane-control <COMMAND> [OPTIONS]
```

### Commands

| Command | Description |
|---------|-------------|
| `compile` | Compile spec(s) into a `.bca` artifact (local) |
| `validate` | Validate spec(s) without compiling |
| `serve` | Start the control plane REST API server |
| `seed-plugins` | Seed the plugin registry with built-in plugins |

---

## barbacane-control seed-plugins

Seed the plugin registry with built-in plugins from the local `plugins/` directory. This command scans plugin directories, reads their manifests (`plugin.toml`), and registers them in the database.

```bash
barbacane-control seed-plugins [OPTIONS]
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--plugins-dir` | No | `plugins` | Path to the plugins directory |
| `--database-url` | Yes | - | PostgreSQL connection URL |
| `--skip-existing` | No | `true` | Skip plugins that already exist in the registry |
| `--verbose` | No | `false` | Show detailed output |

The `--database-url` can also be set via the `DATABASE_URL` environment variable.

### Plugin Directory Structure

Each plugin directory should contain:

```
plugins/
├── http-upstream/
│   ├── plugin.toml          # Plugin manifest (required)
│   ├── config-schema.json   # JSON Schema for config (optional)
│   ├── http-upstream.wasm   # Compiled WASM binary (required)
│   └── src/
│       └── lib.rs
├── rate-limit/
│   ├── plugin.toml
│   ├── config-schema.json
│   └── rate-limit.wasm
└── ...
```

### Plugin Manifest (`plugin.toml`)

```toml
[plugin]
name = "http-upstream"
version = "0.1.0"
type = "dispatcher"                    # or "middleware"
description = "HTTP upstream proxy"    # optional
wasm = "http-upstream.wasm"           # optional, defaults to {name}.wasm

[capabilities]
host_functions = ["host_http_call", "host_log"]
```

### Examples

```bash
# Build plugins and seed them into the registry
make seed-plugins

# Or manually:
cargo run -p barbacane-control -- seed-plugins \
  --plugins-dir plugins \
  --database-url postgres://localhost/barbacane \
  --verbose

# Output:
#   Registered http-upstream v0.1.0 (dispatcher)
#   Registered rate-limit v0.1.0 (middleware)
#   Registered cors v0.1.0 (middleware)
#   ...
# Seeded 9 plugin(s) into the registry.
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Error (database connection, invalid manifest, etc.) |

---

## barbacane-control serve

Start the control plane HTTP server with PostgreSQL backend.

```bash
barbacane-control serve [OPTIONS]
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--listen` | No | `127.0.0.1:9090` | Listen address (ip:port) |
| `--database-url` | Yes | - | PostgreSQL connection URL |
| `--migrate` | No | `true` | Run database migrations on startup |

The `--database-url` can also be set via the `DATABASE_URL` environment variable.

### Examples

```bash
# Start with explicit database URL
barbacane-control serve \
  --database-url postgres://postgres:password@localhost/barbacane \
  --listen 0.0.0.0:9090

# Using environment variable
export DATABASE_URL=postgres://postgres:password@localhost/barbacane
barbacane-control serve

# Skip migrations (not recommended)
barbacane-control serve \
  --database-url postgres://localhost/barbacane \
  --migrate=false
```

### Database Setup

The control plane requires PostgreSQL 14+. Tables are created automatically via migrations:

```bash
# Create database
createdb barbacane

# Start server (migrations run automatically)
barbacane-control serve --database-url postgres://localhost/barbacane
```

### API Endpoints

The server exposes a REST API for managing specs, plugins, artifacts, and projects:

| Endpoint | Description |
|----------|-------------|
| **System** | |
| `GET /health` | Health check |
| `GET /api/docs` | Interactive API documentation (Scalar) |
| **Specs** | |
| `POST /specs` | Upload spec (multipart) |
| `GET /specs` | List specs |
| `GET /specs/{id}` | Get spec metadata |
| `DELETE /specs/{id}` | Delete spec |
| `GET /specs/{id}/history` | Revision history |
| `GET /specs/{id}/content` | Download spec content |
| `POST /specs/{id}/compile` | Start async compilation |
| `GET /compilations/{id}` | Poll compilation status |
| **Plugins** | |
| `POST /plugins` | Register plugin (multipart) |
| `GET /plugins` | List plugins |
| `GET /plugins/{name}/{version}` | Get plugin metadata |
| `DELETE /plugins/{name}/{version}` | Delete plugin |
| `GET /plugins/{name}/{version}/download` | Download WASM binary |
| **Artifacts** | |
| `GET /artifacts` | List artifacts |
| `GET /artifacts/{id}` | Get artifact metadata |
| `GET /artifacts/{id}/download` | Download `.bca` file |
| **Projects** | |
| `POST /projects` | Create a new project |
| `GET /projects` | List all projects |
| `GET /projects/{id}` | Get project details |
| `PUT /projects/{id}` | Update project |
| `DELETE /projects/{id}` | Delete project |
| `GET /projects/{id}/plugins` | List plugins configured for project |
| `POST /projects/{id}/plugins` | Add plugin to project |
| `PUT /projects/{id}/plugins/{name}` | Update plugin config |
| `DELETE /projects/{id}/plugins/{name}` | Remove plugin from project |
| `POST /projects/{id}/deploy` | Deploy artifact to connected data planes |
| **Data Planes** | |
| `GET /projects/{id}/data-planes` | List connected data planes |
| `GET /data-planes/{id}` | Get data plane status |
| **API Keys** | |
| `POST /projects/{id}/api-keys` | Create API key for data plane auth |
| `GET /projects/{id}/api-keys` | List API keys |
| `DELETE /projects/{id}/api-keys/{id}` | Revoke API key |

### Interactive API Documentation

The control plane includes interactive API documentation powered by [Scalar](https://scalar.com/). Access it at:

```
http://localhost:9090/api/docs
```

This provides a browsable interface for exploring and testing all API endpoints.

### Full API Specification

Full OpenAPI specification: [Control Plane OpenAPI](../../crates/barbacane-control/openapi.yaml)

See the [Control Plane Guide](../guide/control-plane.md) for detailed usage examples.
