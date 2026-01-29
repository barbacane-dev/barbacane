# CLI Reference

Barbacane provides two command-line tools:

- **`barbacane-control`** - Compile and validate specs (control plane)
- **`barbacane`** - Run the gateway (data plane)

## barbacane-control

The control plane CLI for spec compilation and validation.

### compile

Compile one or more OpenAPI specs into a `.bca` artifact.

```bash
barbacane-control compile --specs <FILES>... [OPTIONS]
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `--specs` | Yes | One or more spec files (YAML or JSON) |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--output` | `artifact.bca` | Output artifact path |
| `--production` | `true` | Enable production checks |
| `--development` | `false` | Disable production checks |
| `--verbose` | `false` | Show detailed output |

#### Examples

```bash
# Compile single spec
barbacane-control compile --specs api.yaml

# Compile multiple specs
barbacane-control compile --specs users.yaml orders.yaml payments.yaml

# Custom output path
barbacane-control compile --specs api.yaml --output my-gateway.bca

# Verbose output
barbacane-control compile --specs api.yaml --verbose
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Validation error (missing dispatch, routing conflict) |
| 2 | Plugin resolution error |
| 3 | I/O error (file not found, write failed) |

### validate

Validate specs without full compilation.

```bash
barbacane-control validate --specs <FILES>... [OPTIONS]
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `--specs` | Yes | One or more spec files to validate |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--verbose` | `false` | Show detailed output per spec |

#### Examples

```bash
# Validate single spec
barbacane-control validate --specs api.yaml

# Validate multiple specs
barbacane-control validate --specs users.yaml orders.yaml

# Verbose output
barbacane-control validate --specs api.yaml --verbose
```

#### Output

```bash
# Success (non-verbose)
All specs valid.

# Success (verbose)
Validating 2 spec(s)...
  users.yaml - OK (openapi 3.1.0, 5 operations)
  orders.yaml - OK (openapi 3.1.0, 8 operations)

# Error
error[E1020]: operation has no x-barbacane-dispatch: GET /users in 'users.yaml'
```

---

## barbacane

The data plane - runs the gateway and processes HTTP requests.

### Usage

```bash
barbacane --artifact <PATH> [OPTIONS]
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `--artifact` | Yes | Path to the `.bca` artifact file |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--listen` | `0.0.0.0:8080` | Listen address (ip:port) |
| `--dev` | `false` | Enable development mode |
| `--log-level` | `info` | Log level (trace, debug, info, warn, error) |

#### Examples

```bash
# Run with defaults
barbacane --artifact api.bca

# Custom port
barbacane --artifact api.bca --listen 127.0.0.1:3000

# Development mode
barbacane --artifact api.bca --dev

# Production with custom address
barbacane --artifact api.bca --listen 0.0.0.0:80
```

#### Development Mode

The `--dev` flag enables:
- Verbose error messages (includes dispatcher name, internal details)
- Detailed request logging
- Relaxed production checks

**Do not use in production** - it may expose internal information.

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Clean shutdown |
| 1 | Startup error (artifact not found, invalid config) |

---

## Environment Variables

| Variable | Used By | Description |
|----------|---------|-------------|
| `RUST_LOG` | Both | Override log level (e.g., `RUST_LOG=debug`) |
| `BARBACANE_DEV` | barbacane | Enable dev mode (alternative to `--dev`) |

---

## Common Workflows

### Development Cycle

```bash
# Edit spec
vim api.yaml

# Validate
barbacane-control validate --specs api.yaml --verbose

# Compile
barbacane-control compile --specs api.yaml --output api.bca

# Run
barbacane --artifact api.bca --dev
```

### CI/CD Pipeline

```bash
# Validate in CI
barbacane-control validate --specs specs/*.yaml
if [ $? -ne 0 ]; then
  echo "Spec validation failed"
  exit 1
fi

# Compile artifact
barbacane-control compile \
  --specs specs/*.yaml \
  --output dist/gateway.bca \
  --production

# Deploy artifact to production server
scp dist/gateway.bca server:/opt/barbacane/
ssh server "systemctl restart barbacane"
```

### Multi-Spec Gateway

```bash
# Compile multiple specs into one artifact
barbacane-control compile \
  --specs users-api.yaml \
  --specs orders-api.yaml \
  --specs payments-api.yaml \
  --output combined.bca \
  --verbose

# Routes from all specs are merged
# Conflicts (same path+method) cause compilation error
```
