# Linting with Vacuum

Barbacane provides a [vacuum](https://quobix.com/vacuum/) ruleset that validates your OpenAPI specs against Barbacane-specific conventions. Catch plugin configuration errors, missing dispatch blocks, and security misconfigurations **at lint time** — before `barbacane compile` or runtime.

## Quick Start

### 1. Install vacuum

```bash
# macOS
brew install daveshanley/vacuum/vacuum

# or download from https://github.com/daveshanley/vacuum/releases
```

### 2. Create a `.vacuum.yml` in your project

```yaml
extends:
  - - https://docs.barbacane.dev/rulesets/barbacane.yaml
    - recommended
```

### 3. Lint your spec

```bash
vacuum lint my-api.yaml
```

## Rules

The Barbacane ruleset includes the following rules, grouped by category.

### Dispatch Validation

| Rule | Severity | Description |
|------|----------|-------------|
| `barbacane-dispatch-required` | error | Every operation must declare `x-barbacane-dispatch` |
| `barbacane-dispatch-has-name` | error | Dispatch block must include a `name` field |
| `barbacane-dispatch-known-plugin` | error | Plugin name must be a known dispatcher (`mock`, `http-upstream`, `kafka`, `nats`, `s3`, `lambda`) |
| `barbacane-dispatch-has-config` | error | Dispatch block must include a `config` object |
| `barbacane-dispatch-config-valid` | error | Config must validate against the plugin's JSON Schema (required fields, types, no unknown fields) |

### Middleware Validation

| Rule | Severity | Description |
|------|----------|-------------|
| `barbacane-middleware-has-name` | error | Each middleware entry must include a `name` field |
| `barbacane-middleware-known-plugin` | warn | Name must be a known middleware plugin |
| `barbacane-middleware-config-valid` | error | Config must validate against the plugin's JSON Schema |
| `barbacane-middleware-no-duplicate` | warn | No duplicate middleware names in a chain |

The same rules apply to operation-level middlewares (`barbacane-op-middleware-*`).

### Extension Hygiene

| Rule | Severity | Description |
|------|----------|-------------|
| `barbacane-no-unknown-extension` | warn | Only `x-barbacane-dispatch` and `x-barbacane-middlewares` are recognized |

### Upstream & Secrets

| Rule | Severity | Description |
|------|----------|-------------|
| `barbacane-no-plaintext-upstream` | warn | `http-upstream` URLs should use HTTPS |
| `barbacane-secret-ref-format` | error | Secret references must match `env://VAR_NAME` or `file:///path` |

### Auth Safety

| Rule | Severity | Description |
|------|----------|-------------|
| `barbacane-auth-opt-out-explicit` | info | When global auth is set, operations that override middlewares without auth should use `x-barbacane-middlewares: []` to explicitly opt out |

## Extending the Ruleset

You can override individual rules in your `.vacuum.yml`:

```yaml
extends:
  - - https://docs.barbacane.dev/rulesets/barbacane.yaml
    - recommended

rules:
  # Downgrade to warning instead of error
  barbacane-dispatch-required: warn

  # Disable a rule entirely
  barbacane-no-plaintext-upstream: off
```

## CI Integration

### GitHub Actions

```yaml
- name: Install vacuum
  run: |
    curl -fsSL https://github.com/daveshanley/vacuum/releases/latest/download/vacuum_linux_amd64 -o vacuum
    chmod +x vacuum

- name: Lint OpenAPI spec
  run: ./vacuum lint -r .vacuum.yml my-api.yaml
```

### Pre-commit

```bash
vacuum lint -r .vacuum.yml my-api.yaml
```

## Plugin Config Schemas

The ruleset validates plugin configurations against their JSON Schemas. These schemas are published alongside the ruleset at:

```
https://docs.barbacane.dev/rulesets/schemas/<plugin-name>.json
```

For example:
- `https://docs.barbacane.dev/rulesets/schemas/http-upstream.json`
- `https://docs.barbacane.dev/rulesets/schemas/jwt-auth.json`
- `https://docs.barbacane.dev/rulesets/schemas/rate-limit.json`
