# CLI reference

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
| `dev` | Start a local development server with auto-reload |
| `compile` | Compile OpenAPI spec(s) into a `.bca` artifact |
| `validate` | Validate spec(s) without compiling |
| `serve` | Run the gateway server |
| `key` | Generate artifact signing keys and print their public keys |
| `control` | Manage a control plane: projects, specs, plugins, artifacts, data planes |

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
| `--fetch-plugins` | No | `false` | Download official plugins (mock, http-upstream) from the GitHub release of this version |

### Plugin Download

The `--fetch-plugins` flag downloads official Barbacane plugins from the GitHub release of the CLI's own version (`barbacane --version`), so a release candidate gets its own plugins:

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
├── barbacane.yaml    # Project manifest (plugin declarations, specs folder)
├── specs/
│   └── api.yaml      # OpenAPI 3.1 specification
├── plugins/          # Directory for WASM plugins
└── .gitignore        # Ignores *.bca, target/, plugins/*.wasm
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Directory exists and is not empty, or write error |

---

## barbacane dev

Start a local development server with automatic file watching and hot-reload. Compiles specs from `barbacane.yaml`, starts the gateway, watches for file changes, and reloads automatically on every save.

```bash
barbacane dev [OPTIONS]
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--listen` | No | `0.0.0.0:8080` | Listen address |
| `--manifest`, `-m` | No | `barbacane.yaml` | Path to `barbacane.yaml` manifest |
| `--spec`, `-s` | No | - | Override spec files (uses these instead of the manifest's `specs` folder) |
| `--log-level` | No | `info` | Log level (trace, debug, info, warn, error) |
| `--admin-bind` | No | `127.0.0.1:8081` | Admin API listen address. Set to `off` to disable |
| `--debounce-ms` | No | `300` | Debounce delay in milliseconds before recompiling after a file change |
| `--log-format` | No | `pretty` | Log format (`json` or `pretty`) |
| `--max-body-size` | No | `1048576` | Maximum request body size in bytes |
| `--max-headers` | No | `100` | Maximum number of request headers |
| `--max-header-size` | No | `8192` | Maximum size of a single header in bytes |
| `--max-uri-length` | No | `8192` | Maximum URI length in bytes |

Dev mode is always enabled (verbose errors, plaintext HTTP upstreams allowed).

### Spec Discovery

The dev server discovers spec files from the `specs` field in `barbacane.yaml`:

```yaml
specs: ./specs/
plugins:
  mock:
    path: ./plugins/mock.wasm
```

All `*.yaml`, `*.yml`, and `*.json` files in the specified folder are compiled. New files added to the folder are picked up on the next reload.

Use `--spec` to override the manifest's specs folder with explicit files.

### File Watching

The dev server watches for changes to:
- `barbacane.yaml` manifest
- The `specs` folder (recursively — new files are detected)
- All local plugin `.wasm` files referenced in the manifest

On file change, the server recompiles and hot-reloads the gateway. If compilation fails, the previous version continues serving while the error is printed to stderr.

### Examples

```bash
# Start with default settings (reads barbacane.yaml in current directory)
barbacane dev

# Custom manifest and port
barbacane dev --manifest path/to/barbacane.yaml --listen 127.0.0.1:3000

# Override spec files instead of using manifest's specs folder
barbacane dev --spec api.yaml --spec events.yaml

# Increase debounce for slow plugin builds
barbacane dev --debounce-ms 1000
```

### Example Output

```
barbacane dev: loaded barbacane.yaml (2 spec(s), 3 plugin(s))
barbacane dev: compiled 5 route(s) in 42ms
barbacane dev: listening on http://0.0.0.0:8080
barbacane dev: admin API on http://127.0.0.1:8081
barbacane dev: watching barbacane.yaml, specs/, plugins/mock.wasm

[specs/api.yaml] recompiling... reloaded 5 route(s) in 38ms

[specs/api.yaml] recompiling... compile error: E1010 — missing x-barbacane-dispatch on GET /users
barbacane dev: serving previous version
```

---

## barbacane compile

Compile one or more OpenAPI specs into a `.bca` artifact.

```bash
barbacane compile [--spec <FILES>...] --manifest <PATH> --output <PATH>
```

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--spec`, `-s` | No | - | One or more spec files (YAML or JSON). If omitted, discovers specs from the manifest's `specs` folder |
| `--output`, `-o` | Yes | - | Output artifact path |
| `--manifest`, `-m` | Yes | - | Path to `barbacane.yaml` manifest |
| `--allow-plaintext` | No | `false` | Allow `http://` upstream URLs during compilation |
| `--provenance-commit` | No | - | Git commit SHA to embed in artifact provenance metadata |
| `--provenance-source` | No | - | Build source identifier (e.g., `ci/github-actions`) to embed in artifact provenance |
| `--no-cache` | No | `false` | Bypass the plugin download cache entirely — remote plugins are re-downloaded and not cached |

### Examples

```bash
# Compile using specs folder from manifest (zero spec args)
barbacane compile -m barbacane.yaml -o api.bca

# Compile single spec with manifest
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca

# Compile multiple specs
barbacane compile -s users.yaml -s orders.yaml -m barbacane.yaml -o combined.bca

# Short form
barbacane compile -s api.yaml -m barbacane.yaml -o api.bca

# With provenance metadata (CI/CD)
barbacane compile -s api.yaml -m barbacane.yaml -o api.bca \
  --provenance-commit $(git rev-parse HEAD) \
  --provenance-source ci/github-actions
```

### Error Codes

Compilation runs every check, since it is the only command that resolves
plugins and builds an artifact.

| Code | Category | Description |
|------|----------|-------------|
| E1001 | Spec validity | Not a valid OpenAPI 3.x or AsyncAPI 3.x |
| E1002 | Spec validity | YAML/JSON parse error |
| E1003 | Spec validity | Unresolved `$ref` reference |
| E1004 | Spec validity | Schema validation error (missing info, etc.) |
| E1010 | Extension | Routing conflict (same path+method in multiple specs) |
| E1011 | Extension | Middleware entry missing `name` |
| E1012 | Extension | Middleware entry sets `enabled: false` and a `config` |
| E1013 | Extension | Operation middleware entry sets `enabled: false` but matches no global entry |
| E1014 | Extension | Global middleware entry sets `enabled: false` |
| E1015 | Extension | Unknown `x-barbacane-*` extension (warning) |
| E1016 | Plugin | A plugin imports a deprecated host function; it still loads until the release named in the [plugin guide](../contributing/plugins.md#deprecated-host-functions) (warning) |
| E1020 | Extension | Operation missing `x-barbacane-dispatch` (warning) |
| E1031 | Extension | Plaintext HTTP URL not allowed (use `--allow-plaintext` to override) |
| E1032 | Security | An operation runs an authentication middleware that reads none of the schemes its `security` requirement names |
| E1033 | Security | An operation requires a credential and runs no authentication middleware (warning) |
| E1040 | Manifest | Plugin used in spec but not declared in `barbacane.yaml` |
| E1043 | Plugin | A plugin imports a function the gateway does not provide: anything outside the `barbacane` host functions and the supported WASI functions (see the [plugin guide](../contributing/plugins.md#wasi)) |
| E1044 | Plugin | Limits for a plugin, in its `plugin.toml`, in `barbacane.yaml` or on the control plane, name an unknown key, or a value below 1 or above the gateway's ceiling (see the [plugin guide](../contributing/plugins.md#resource-limits)) |
| E1045 | Plugin | A plugin is not built for this gateway's plugin ABI: it carries no ABI version, names another one, or lacks an export the gateway calls (see the [plugin guide](../contributing/plugins.md#abi-version)); rebuild it with `barbacane-plugin-sdk` 1.x |
| E1056 | Headers | A declared header name is in the reserved `x-auth-*` namespace |
| E1057 | Security | An operation runs an authentication middleware without a `security` requirement |
| E1059 | Schema | A request schema cannot be enforced: its `$schema` names neither a JSON Schema draft nor an OpenAPI dialect, it refers to a resource outside the spec, or it does not compile (an invalid `pattern`, for one) |
| E1071 | Manifest | A plugin was bundled without its manifest or config schema, so its configured headers are not admitted (warning) |
| E1072 | Security | A security scheme and an authentication plugin's configuration name different credential headers (warning) |

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

`validate` reads the specs and nothing else: it resolves no plugins and builds
no artifact, so it reports only the codes that need neither.

| Code | Category | Description |
|------|----------|-------------|
| E1000 | I/O | The spec file could not be read |
| E1001 | Spec validity | Not a valid OpenAPI 3.x or AsyncAPI 3.x |
| E1002 | Spec validity | YAML/JSON parse error |
| E1003 | Spec validity | Unresolved `$ref` reference |
| E1004 | Spec validity | Schema validation error (missing info, etc.) |
| E1011 | Extension | Middleware entry missing `name` |
| E1012 | Extension | Middleware entry sets `enabled: false` and a `config` |
| E1013 | Extension | Operation middleware entry sets `enabled: false` but matches no global entry |
| E1014 | Extension | Global middleware entry sets `enabled: false` |
| E1020 | Extension | Operation missing `x-barbacane-dispatch` (warning) |

Every other code is reported by `barbacane compile`, whose table lists them all.

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

## barbacane key

Manage the Ed25519 keys that sign artifacts.

```bash
barbacane key generate --out <FILE>
barbacane key public --key <FILE>
```

| Command | Description |
|---------|-------------|
| `generate --out <FILE>` | Write a new PKCS#8 signing key to `<FILE>`, readable by its owner only, and print its public key. An existing file is not overwritten |
| `public --key <FILE>` | Print the public key of a signing key (PKCS#8, DER or PEM), as `BARBACANE_TRUSTED_PUBKEY` takes it |

```bash
barbacane key generate --out signing.pk8
BARBACANE_SIGNING_KEY=signing.pk8 barbacane compile -m barbacane.yaml -o api.bca
BARBACANE_TRUSTED_PUBKEY=$(barbacane key public --key signing.pk8) \
  barbacane serve --artifact api.bca
```

## barbacane control

A client for the [control plane](../guide/control-plane.md)'s REST API. Its commands are generated from the control plane's OpenAPI spec, one per operation: `barbacane control <resource> <command>`, with path and query parameters as flags.

```bash
barbacane control <resource> <command> [FLAGS]
barbacane control push --project <ID> <SPEC>...
barbacane control deploy --project <ID> [--artifact <ID>] [--wait]
```

### Connection and output

| Option | Environment | Default | Description |
|--------|-------------|---------|-------------|
| `--url` | `BARBACANE_CONTROL_URL` | `http://localhost:9090` | Base URL of the control-plane API |
| `--token` | `BARBACANE_CONTROL_ADMIN_TOKEN` | - | Admin token, sent as a bearer token |
| `--output`, `-o` | - | `table` | `table` or `json` |

A request body is JSON, read with `--file` from a file or, with `--file -`, from standard input. An upload takes one flag per form field; a file field takes a path. A download (`artifacts download`, `plugins download`, `specs download-content`) writes to standard output, or to `--out FILE`.

An error response is printed with its status and the problem's detail, as in `error: 404 Not Found: Project … not found`, and the command exits with status 1. So does a control plane that cannot be reached.

### Workflows

| Command | Description |
|---------|-------------|
| `push --project <ID> <SPEC>...` | Upload the specs to the project, compile them into one artifact (the first spec, with the others bundled), and wait for the compilation; exits 1 when it fails. `--no-compile` uploads only; `--timeout` (default 120 seconds) bounds the wait |
| `deploy --project <ID>` | Deploy an artifact (`--artifact`, or the project's latest) to the project's data planes. With `--wait`, wait until every connected data plane runs it, up to `--timeout` |

```bash
export BARBACANE_CONTROL_URL=https://control.example.com
export BARBACANE_CONTROL_ADMIN_TOKEN=...

barbacane control projects create --file - <<< '{"name": "orders"}'
barbacane control project-plugins add --project-id <ID> --file - \
  <<< '{"plugin_name": "http-upstream", "plugin_version": "0.1.0"}'
barbacane control push --project <ID> orders.yaml
barbacane control deploy --project <ID> --wait
barbacane control data-planes list --project-id <ID> -o json
```

### Commands

<!-- barbacane-control-commands:start -->

#### api-keys

API key management for data plane authentication.

| Command | Request | Flags |
|---------|---------|-------|
| `api-keys list` | `GET /projects/{projectId}/api-keys` | `--project-id` |
| `api-keys create` | `POST /projects/{projectId}/api-keys` | `--project-id`, `--file` |
| `api-keys revoke` | `DELETE /projects/{projectId}/api-keys/{keyId}` | `--project-id`, `--key-id` |

#### artifacts

Compiled artifact management.

| Command | Request | Flags |
|---------|---------|-------|
| `artifacts list` | `GET /artifacts` | - |
| `artifacts get` | `GET /artifacts/{id}` | `--id` |
| `artifacts delete` | `DELETE /artifacts/{id}` | `--id` |
| `artifacts download` | `GET /artifacts/{id}/download` | `--id`, `--out` (optional) |
| `artifacts list-project` | `GET /projects/{projectId}/artifacts` | `--project-id` |

#### compilations

Async compilation job management.

| Command | Request | Flags |
|---------|---------|-------|
| `compilations get` | `GET /compilations/{id}` | `--id` |
| `compilations delete` | `DELETE /compilations/{id}` | `--id` |
| `compilations list` | `GET /projects/{projectId}/compilations` | `--project-id` |
| `compilations list-spec` | `GET /specs/{id}/compilations` | `--id` |
| `compilations start` | `POST /specs/{id}/compile` | `--id`, `--file` (optional) |

#### data-planes

Connected data plane management.

| Command | Request | Flags |
|---------|---------|-------|
| `data-planes list` | `GET /projects/{projectId}/data-planes` | `--project-id` |
| `data-planes get` | `GET /projects/{projectId}/data-planes/{dataPlaneId}` | `--project-id`, `--data-plane-id` |
| `data-planes disconnect` | `DELETE /projects/{projectId}/data-planes/{dataPlaneId}` | `--project-id`, `--data-plane-id` |
| `data-planes deploy` | `POST /projects/{projectId}/deploy` | `--project-id`, `--file` (optional) |

#### health

Health check endpoints.

| Command | Request | Flags |
|---------|---------|-------|
| `health check` | `GET /health` | - |

#### operations

The operations of a project's specs, and their plugin bindings.

| Command | Request | Flags |
|---------|---------|-------|
| `operations list` | `GET /projects/{projectId}/operations` | `--project-id` |
| `operations patch-spec` | `PATCH /specs/{id}/operations` | `--id`, `--file` |

#### plugins

Plugin registry management.

| Command | Request | Flags |
|---------|---------|-------|
| `plugins list` | `GET /plugins` | `--type` (optional), `--name` (optional) |
| `plugins register` | `POST /plugins` | `--capabilities` (optional), `--config-schema` (optional), `--description` (optional), `--file`, `--name`, `--type`, `--version` |
| `plugins list-versions` | `GET /plugins/{name}` | `--name` |
| `plugins get` | `GET /plugins/{name}/{version}` | `--name`, `--version` |
| `plugins delete` | `DELETE /plugins/{name}/{version}` | `--name`, `--version` |
| `plugins download` | `GET /plugins/{name}/{version}/download` | `--name`, `--version`, `--out` (optional) |

#### project-plugins

The plugins a project uses, and their configuration.

| Command | Request | Flags |
|---------|---------|-------|
| `project-plugins list` | `GET /projects/{projectId}/plugins` | `--project-id` |
| `project-plugins add` | `POST /projects/{projectId}/plugins` | `--project-id`, `--file` |
| `project-plugins update` | `PUT /projects/{projectId}/plugins/{name}` | `--project-id`, `--name`, `--file` |
| `project-plugins remove` | `DELETE /projects/{projectId}/plugins/{name}` | `--project-id`, `--name` |

#### projects

Project management.

| Command | Request | Flags |
|---------|---------|-------|
| `projects init` | `POST /init` | `--file` |
| `projects list` | `GET /projects` | - |
| `projects create` | `POST /projects` | `--file` |
| `projects get` | `GET /projects/{projectId}` | `--project-id` |
| `projects update` | `PUT /projects/{projectId}` | `--project-id`, `--file` |
| `projects delete` | `DELETE /projects/{projectId}` | `--project-id` |

#### specs

API specification management.

| Command | Request | Flags |
|---------|---------|-------|
| `specs list-project` | `GET /projects/{projectId}/specs` | `--project-id` |
| `specs upload-project` | `POST /projects/{projectId}/specs` | `--project-id`, `--file` |
| `specs list` | `GET /specs` | `--type` (optional), `--name` (optional) |
| `specs upload` | `POST /specs` | `--file` |
| `specs get` | `GET /specs/{id}` | `--id` |
| `specs delete` | `DELETE /specs/{id}` | `--id` |
| `specs get-compliance` | `GET /specs/{id}/compliance` | `--id` |
| `specs download-content` | `GET /specs/{id}/content` | `--id`, `--revision` (optional), `--out` (optional) |
| `specs get-history` | `GET /specs/{id}/history` | `--id` |

<!-- barbacane-control-commands:end -->

---

## barbacane serve

Run the gateway server, loading routes from a compiled artifact.

```bash
barbacane serve --artifact <PATH> [OPTIONS]
```

The artifact must be signed by the key `BARBACANE_TRUSTED_PUBKEY` pins, and
`serve` refuses to start without one unless `--dev` or `--allow-unsigned` is
passed. See [artifact signing](configuration.md#artifact-signing-quickstart).
The examples below assume the key is set.

### Options

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--artifact` | Yes | - | Path to the `.bca` artifact file |
| `--listen` | No | `0.0.0.0:8080` | Listen address (ip:port) |
| `--dev` | No | `false` | Enable development mode |
| `--log-level` | No | `info` | Log level (trace, debug, info, warn, error) |
| `--log-format` | No | `json` | Log format (`json` or `pretty`) |
| `--otlp-endpoint` | No | - | OpenTelemetry endpoint for trace export (e.g., `http://localhost:4317`) |
| `--trace-sampling` | No | `1.0` | Trace sampling rate (0.0 to 1.0). 1.0 = 100%, 0.1 = 10%, 0.0 = disabled |
| `--max-body-size` | No | `1048576` | Maximum request body size in bytes (1MB) |
| `--max-headers` | No | `100` | Maximum number of request headers |
| `--max-header-size` | No | `8192` | Maximum size of a single header in bytes (8KB) |
| `--max-uri-length` | No | `8192` | Maximum URI length in bytes (8KB) |
| `--allow-plaintext-upstream` | No | `false` | Allow `http://` upstream URLs (dev only) |
| `--allow-unsigned` | No | `false` | Load artifacts no trusted key has signed, when `BARBACANE_TRUSTED_PUBKEY` is unset (development and testing only; every load logs a warning) |
| `--tls-cert` | No | - | Path to TLS certificate file (PEM format) |
| `--tls-key` | No | - | Path to TLS private key file (PEM format) |
| `--tls-min-version` | No | `1.2` | Minimum TLS version (`1.2` or `1.3`) |
| `--keepalive-timeout` | No | `60` | HTTP keep-alive idle timeout in seconds; also the deadline for a request's headers and for its body, answered 408 when missed |
| `--shutdown-timeout` | No | `30` | Graceful shutdown timeout in seconds |
| `--admin-bind` | No | `127.0.0.1:8081` | Admin API listen address for health, metrics, and provenance endpoints. Set to `off` to disable |
| `--no-introspection` | No | `false` | Do not serve the embedded specs at `/__barbacane/specs*`; the paths answer 404 |

**Connected mode** (optional — connect to a control plane for centralized management):

| Option | Required | Default | Description |
|--------|----------|---------|-------------|
| `--control-plane` | No | - | Control plane WebSocket URL (e.g., `wss://control:9090/ws/data-plane`). A `ws://` link to a host other than loopback is refused unless `--allow-plaintext-control-plane` is set |
| `--project-id` | No | - | Project UUID for control plane registration. Required if `--control-plane` is set |
| `--api-key` | No | - | The data plane API key the control plane issued for the project. Also read from `BARBACANE_CONTROL_API_KEY` |
| `--data-plane-name` | No | - | Name of this instance in the control plane, at most 128 characters |
| `--allow-plaintext-control-plane` | No | `false` | Allow a `ws://` control plane link to a host other than loopback; the link carries the API key |

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

# With reduced trace sampling (10%)
barbacane serve --artifact api.bca \
  --otlp-endpoint http://otel-collector:4317 \
  --trace-sampling 0.1

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
| `Server` | `barbacane`, with no version |
| `X-Request-Id` | Request ID: the incoming header when it is at most 128 bytes of visible ASCII, otherwise a generated UUID v4 |
| `X-Trace-Id` | Trace ID: the trace id of a well-formed W3C `traceparent` header, otherwise generated |
| `X-Content-Type-Options` | `nosniff` - prevents MIME sniffing attacks |
| `X-Frame-Options` | `DENY` - prevents clickjacking via iframes |

Example response headers:

```
HTTP/1.1 200 OK
Server: barbacane
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
Server: barbacane
Deprecation: true
Sunset: Sat, 31 Dec 2025 23:59:59 GMT
Content-Type: application/json
```

See [API Lifecycle](../guide/spec-configuration.md#api-lifecycle) for configuration details.

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean shutdown |
| 1 | Startup error (artifact not found, bind failed, integrity/signature/capability failure) |
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

Prometheus metrics are exposed on the admin API port (default `127.0.0.1:8081`):

```bash
curl http://localhost:8081/metrics
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
      introspection_endpoint: https://auth.example.com/introspect
      client_id: my-api-client
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
# Recommended: use the dev server (auto-compiles and hot-reloads on save)
barbacane dev

# Or manually: edit, compile, serve
barbacane validate --spec api.yaml
barbacane compile --spec api.yaml --manifest barbacane.yaml --output api.bca
barbacane serve --artifact api.bca --dev
```

### CI/CD Pipeline

```bash
#!/bin/bash
set -e

# Validate all specs
barbacane validate --spec specs/*.yaml --format json > validation.json

# Compile artifact with provenance
barbacane compile \
  --spec specs/users.yaml \
  --spec specs/orders.yaml \
  --manifest barbacane.yaml \
  --output dist/gateway.bca \
  --provenance-commit "$(git rev-parse HEAD)" \
  --provenance-source ci/github-actions

echo "Artifact built: dist/gateway.bca"
```

### Multi-Spec Gateway

```bash
# Compile multiple specs into one artifact
barbacane compile \
  --spec users-api.yaml \
  --spec orders-api.yaml \
  --spec payments-api.yaml \
  --manifest barbacane.yaml \
  --output combined.bca

# Routes from all specs are merged
# Conflicts (same path+method) cause E1010 error
```

### Testing Locally

```bash
# Start dev server (auto-compiles and reloads)
barbacane dev --listen 127.0.0.1:8080 &

# Test endpoints
curl http://localhost:8080/health
curl http://localhost:8080/__barbacane/health
curl http://localhost:8080/__barbacane/specs

# Stop gateway
kill %1
```

---

## barbacane-control

The control plane CLI for running the control plane server and managing the plugin registry.

```bash
barbacane-control <COMMAND> [OPTIONS]
```

### Commands

| Command | Description |
|---------|-------------|
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
| `--force` | No | `false` | Re-seed plugins that already exist (updates metadata and binary) |
| `--verbose` | No | `false` | Show detailed output |

The `--database-url` can also be set via the `DATABASE_URL` environment variable.

The server reads these environment variables:

| Variable | Required | Description |
|----------|----------|-------------|
| `BARBACANE_CONTROL_ADMIN_TOKEN` | Yes | Bearer token every API request carries, except `GET /health` and the data-plane WebSocket. The server refuses to start without it |
| `BARBACANE_SIGNING_KEY` | No | Path to the Ed25519 key that signs every artifact the control plane compiles (see [artifact signing](configuration.md#artifact-signing-quickstart)) |
| `BARBACANE_CONTROL_ALLOWED_ORIGINS` | No | Comma-separated allowlist of browser origins for CORS; unset means no cross-origin access |
| `RUST_LOG` | No | Log filter (`error`, `warn`, `info`, `debug`, `trace`) |

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
# Seeded 29 plugin(s) into the registry.
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
| `GET /openapi` | OpenAPI specification (YAML) |
| `GET /docs` | Interactive API documentation (Scalar) |
| `POST /init` | Initialize a new project |
| **Specs** | |
| `POST /specs` | Upload spec (multipart) |
| `GET /specs` | List specs |
| `GET /specs/{id}` | Get spec metadata |
| `DELETE /specs/{id}` | Delete spec |
| `GET /specs/{id}/history` | Revision history |
| `GET /specs/{id}/content` | Download spec content |
| `POST /specs/{id}/compile` | Start async compilation |
| `GET /specs/{id}/compilations` | List compilations for a spec |
| `PATCH /specs/{id}/operations` | Update plugin bindings for spec operations |
| **Compilations** | |
| `GET /compilations/{id}` | Poll compilation status |
| `DELETE /compilations/{id}` | Delete a compilation record |
| **Plugins** | |
| `POST /plugins` | Register plugin (multipart) |
| `GET /plugins` | List plugins |
| `GET /plugins/{name}` | List versions of a plugin |
| `GET /plugins/{name}/{version}` | Get plugin metadata |
| `DELETE /plugins/{name}/{version}` | Delete plugin |
| `GET /plugins/{name}/{version}/download` | Download WASM binary |
| **Artifacts** | |
| `GET /artifacts` | List artifacts |
| `GET /artifacts/{id}` | Get artifact metadata |
| `DELETE /artifacts/{id}` | Delete an artifact |
| `GET /artifacts/{id}/download` | Download `.bca` file |
| **Projects** | |
| `POST /projects` | Create a new project |
| `GET /projects` | List all projects |
| `GET /projects/{id}` | Get project details |
| `PUT /projects/{id}` | Update project |
| `DELETE /projects/{id}` | Delete project |
| `GET /projects/{id}/specs` | List specs in a project |
| `POST /projects/{id}/specs` | Upload spec to a project |
| `GET /projects/{id}/operations` | List all operations across project specs |
| `GET /projects/{id}/plugins` | List plugins configured for project |
| `POST /projects/{id}/plugins` | Add plugin to project |
| `PUT /projects/{id}/plugins/{name}` | Update plugin config |
| `DELETE /projects/{id}/plugins/{name}` | Remove plugin from project |
| `GET /projects/{id}/compilations` | List compilations for a project |
| `GET /projects/{id}/artifacts` | List artifacts for a project |
| `POST /projects/{id}/deploy` | Deploy artifact to connected data planes |
| **Data Planes** | |
| `GET /projects/{id}/data-planes` | List connected data planes |
| `GET /projects/{id}/data-planes/{dp_id}` | Get data plane status |
| `DELETE /projects/{id}/data-planes/{dp_id}` | Disconnect a data plane |
| **API Keys** | |
| `POST /projects/{id}/api-keys` | Create API key for data plane auth |
| `GET /projects/{id}/api-keys` | List API keys |
| `DELETE /projects/{id}/api-keys/{key_id}` | Revoke API key, ending the data plane sessions registered with it |

### Interactive API Documentation

The control plane includes interactive API documentation powered by [Scalar](https://scalar.com/). Access it at:

```
http://localhost:9090/docs
```

This provides a browsable interface for exploring and testing all API endpoints.

### Full API Specification

Full OpenAPI specification: [Control Plane OpenAPI](https://github.com/barbacane-dev/barbacane/blob/main/crates/barbacane-control/openapi.yaml), also served by a running control plane at `GET /openapi`

See the [Control Plane Guide](../guide/control-plane.md) for detailed usage examples.
