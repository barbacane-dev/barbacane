# CLI Reference

Barbacane provides a unified command-line tool with subcommands for compilation, validation, and running the gateway.

## barbacane

```bash
barbacane <COMMAND> [OPTIONS]
```

### Commands

| Command | Description |
|---------|-------------|
| `compile` | Compile OpenAPI spec(s) into a `.bca` artifact |
| `validate` | Validate spec(s) without compiling |
| `serve` | Run the gateway server |

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
| `--max-body-size` | No | `1048576` | Maximum request body size in bytes (1MB) |
| `--max-headers` | No | `100` | Maximum number of request headers |
| `--max-header-size` | No | `8192` | Maximum size of a single header in bytes (8KB) |
| `--max-uri-length` | No | `8192` | Maximum URI length in characters (8KB) |
| `--allow-plaintext-upstream` | No | `false` | Allow `http://` upstream URLs (dev only) |

### Examples

```bash
# Run with defaults
barbacane serve --artifact api.bca

# Custom port
barbacane serve --artifact api.bca --listen 127.0.0.1:3000

# Development mode (verbose errors)
barbacane serve --artifact api.bca --dev

# Production with custom limits
barbacane serve --artifact api.bca \
  --max-body-size 5242880 \
  --max-headers 50

# All options
barbacane serve --artifact api.bca \
  --listen 0.0.0.0:8080 \
  --log-level info \
  --max-body-size 1048576 \
  --max-headers 100 \
  --max-header-size 8192 \
  --max-uri-length 8192
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

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean shutdown |
| 1 | Startup error (artifact not found, bind failed) |

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Override log level (e.g., `RUST_LOG=debug`) |

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
