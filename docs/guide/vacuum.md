# Linting with Vacuum

Barbacane provides a [vacuum](https://quobix.com/vacuum/) ruleset that validates your OpenAPI specs against Barbacane-specific conventions. Catch plugin configuration errors, missing dispatch blocks, and security misconfigurations **at lint time** — before `barbacane compile` or runtime.

> **Note:** The ruleset currently supports **OpenAPI** specs only. Vacuum does not yet support AsyncAPI 3.x linting ([tracking issue](https://github.com/daveshanley/vacuum/issues/241)). AsyncAPI specs are validated at compile time by `barbacane compile`.

## Quick Start

### 1. Install vacuum

```bash
# macOS
brew install daveshanley/vacuum/vacuum

# Linux, Windows, Docker: https://quobix.com/vacuum/installing/
```

### 2. Download the Barbacane ruleset

Several rules use custom JavaScript functions (config schema validation, duplicate detection, etc.). Vacuum requires custom functions on the local filesystem, so download the ruleset and its functions:

```bash
mkdir -p .barbacane/rulesets/functions .barbacane/rulesets/schemas
curl -fsSL https://docs.barbacane.dev/rulesets/barbacane.yaml -o .barbacane/rulesets/barbacane.yaml
for f in barbacane-auth-opt-out barbacane-no-duplicate-middlewares barbacane-no-plaintext-upstream \
         barbacane-no-unknown-extensions barbacane-valid-secret-refs barbacane-validate-dispatch-config \
         barbacane-validate-middleware-config; do
  curl -fsSL "https://docs.barbacane.dev/rulesets/functions/${f}.js" -o ".barbacane/rulesets/functions/${f}.js"
done
```

This creates a `.barbacane/rulesets/` directory with the ruleset YAML and custom functions. You may want to add `.barbacane/` to your `.gitignore`.

### 3. Create a `.vacuum.yml` in your project

```yaml
extends:
  - - .barbacane/rulesets/barbacane.yaml
    - recommended
```

### 4. Lint your spec

```bash
vacuum lint -f .barbacane/rulesets/functions my-api.yaml
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
  - - .barbacane/rulesets/barbacane.yaml
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

- name: Download Barbacane ruleset
  run: |
    mkdir -p .barbacane/rulesets/functions
    curl -fsSL https://docs.barbacane.dev/rulesets/barbacane.yaml -o .barbacane/rulesets/barbacane.yaml
    for f in barbacane-auth-opt-out barbacane-no-duplicate-middlewares barbacane-no-plaintext-upstream \
             barbacane-no-unknown-extensions barbacane-valid-secret-refs barbacane-validate-dispatch-config \
             barbacane-validate-middleware-config; do
      curl -fsSL "https://docs.barbacane.dev/rulesets/functions/${f}.js" -o ".barbacane/rulesets/functions/${f}.js"
    done

- name: Lint OpenAPI spec
  run: ./vacuum lint -f .barbacane/rulesets/functions my-api.yaml
```

### Pre-commit

```bash
vacuum lint -f .barbacane/rulesets/functions my-api.yaml
```

## Custom Functions

Several rules use custom JavaScript functions for validations that go beyond what built-in vacuum functions can express (config schema validation, duplicate detection, secret reference format, etc.). Vacuum requires custom functions to be on the local filesystem — it does not fetch them from remote URLs.

The download steps above place these functions into `.barbacane/rulesets/functions/`. The `-f` flag in the `vacuum lint` command tells vacuum where to find them.

If you cloned the Barbacane repository, you can also point directly at the source:

```yaml
# .vacuum.yml
extends:
  - - path/to/Barbacane/docs/rulesets/barbacane.yaml
    - recommended
```

```bash
vacuum lint -f path/to/Barbacane/docs/rulesets/functions my-api.yaml
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
