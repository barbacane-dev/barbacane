# SPEC-001: Spec Compilation & Artifact Format

**Status:** Draft
**Date:** 2026-01-28
**Derived from:** ADR-0004, ADR-0011, ADR-0015

---

## 1. Overview

The compiler transforms OpenAPI 3.x and AsyncAPI 3.x specifications into a self-contained binary artifact (`.bca`) that the data plane loads at startup. This spec defines the compiler's inputs, validation rules, output format, and CLI interface.

---

## 2. Input

### 2.1 Accepted spec formats

| Format | Versions | File extensions |
|--------|----------|-----------------|
| OpenAPI | 3.0.x, 3.1.x | `.yaml`, `.yml`, `.json` |
| AsyncAPI | 3.0.x | `.yaml`, `.yml`, `.json` |

The compiler auto-detects the format from the root `openapi` or `asyncapi` field. If neither is present, compilation fails with `E1001`.

### 2.2 Multiple specs

The compiler accepts one or more spec files in a single invocation:

```bash
barbacane-control compile --specs user-api.yaml billing-api.yaml
```

Multiple specs are merged into a single artifact. Routing conflicts (same path + method across specs) fail compilation with `E1010`.

### 2.3 `x-barbacane-*` extensions

All gateway-specific configuration lives under `x-barbacane-*` vendor extensions. These are the only non-standard fields the compiler processes. Unknown `x-barbacane-*` keys produce a warning; unknown `x-other-*` keys are silently ignored.

---

## 3. Extension Schema Reference

### 3.1 `x-barbacane-middlewares`

**Placement:** spec root (global) or operation level (per-route override).

```yaml
x-barbacane-middlewares:
  - name: <string>              # required — plugin name (or name@version)
    config: <object>            # optional — plugin-specific config, validated against plugin schema
```

**Behavior:**
- Global middlewares apply to all operations in order.
- Per-operation `x-barbacane-middlewares` **replaces** the global chain entirely for that operation. There is no merge. If an operation declares its own chain, only that chain runs.
- An empty array (`x-barbacane-middlewares: []`) explicitly disables all middlewares for that operation.

### 3.2 `x-barbacane-dispatch`

**Placement:** operation level. Required on every operation.

```yaml
x-barbacane-dispatch:
  name: <string>                # required — dispatcher plugin name (or name@version)
  config: <object>              # optional — dispatcher-specific config
```

An operation without `x-barbacane-dispatch` fails compilation with `E1020`.

### 3.3 `x-barbacane-ratelimit`

**Placement:** spec root (global) or operation level (override).

Aligned with [draft-ietf-httpapi-ratelimit-headers](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/) for vocabulary and response header behavior.

```yaml
x-barbacane-ratelimit:
  policy_name: <string>         # optional — policy identifier, appears in RateLimit-Policy header (default: "default")
  quota: <integer>              # required — max quota units allowed in the window
  window: <integer>             # required — time window in seconds
  quota_unit: <string>          # optional — "requests" | "content-bytes" | "concurrent-requests" (default: "requests")
  key: <string>                 # optional — partition key, default "client_ip"
                                #   format: "client_ip" | "header:<name>" | "context:<key>"
```

Rate limiting is implemented as a built-in middleware. This extension is syntactic sugar — the compiler transforms it into a middleware entry in the chain.

The rate-limit middleware emits `RateLimit-Policy` and `RateLimit` response headers on every response (not just 429s), following the IETF draft format:

```
RateLimit-Policy: "default";q=100;w=60
RateLimit: "default";r=73;t=45
```

### 3.4 `x-barbacane-cache`

**Placement:** operation level only.

```yaml
x-barbacane-cache:
  ttl: <duration>               # required — cache duration (e.g. "60s", "5m")
  vary: [<string>]              # optional — headers that vary the cache key
```

### 3.5 `x-barbacane-sunset`

**Placement:** operation level, alongside `deprecated: true`.

```yaml
x-barbacane-sunset: "<ISO-8601 date>"   # e.g. "2026-06-01"
```

If `x-barbacane-sunset` is present but `deprecated` is not `true`, compilation fails with `E1030`.

### 3.6 `x-barbacane-observability`

**Placement:** spec root (global) or operation level (override).

```yaml
x-barbacane-observability:
  trace_sampling: <float>       # optional — 0.0 to 1.0, default 1.0
  detailed_validation_logs: <boolean>  # optional — default false
  latency_slo: <duration>       # optional — emit alert metric when exceeded
```

---

## 4. Validation Rules

The compiler performs validation in order. Compilation stops at the first category of failures (all errors within a category are reported, then compilation aborts).

### 4.1 Spec validity

| Code | Condition |
|------|-----------|
| `E1001` | File is not valid OpenAPI 3.x or AsyncAPI 3.x |
| `E1002` | YAML/JSON parse error |
| `E1003` | `$ref` reference cannot be resolved |
| `E1004` | OpenAPI/AsyncAPI schema validation error (e.g. missing `info`, invalid `paths` structure) |

### 4.2 Extension validity

| Code | Condition |
|------|-----------|
| `E1010` | Routing conflict: same path + method declared in multiple specs |
| `E1011` | `x-barbacane-middlewares` entry missing `name` |
| `E1012` | `x-barbacane-ratelimit` missing required field (`quota` or `window`) |
| `E1013` | `x-barbacane-ratelimit.quota_unit` is not one of `requests`, `content-bytes`, `concurrent-requests` |
| `E1014` | `x-barbacane-cache.ttl` is not a valid duration |
| `E1015` | Unknown `x-barbacane-*` extension key (warning, not error) |

### 4.3 Plugin resolution

| Code | Condition |
|------|-----------|
| `E1020` | Operation has no `x-barbacane-dispatch` |
| `E1021` | Referenced plugin name not found in the registry |
| `E1022` | Referenced plugin version not found |
| `E1023` | Plugin config does not match the plugin's declared JSON Schema |
| `E1024` | Plugin type mismatch (e.g. a dispatcher referenced as middleware) |

### 4.4 Security checks

| Code | Condition |
|------|-----------|
| `E1030` | `x-barbacane-sunset` present but `deprecated` is not `true` |
| `E1031` | `http://` upstream URL in production mode |
| `E1032` | Operation declares `security` but no matching auth middleware in the chain |

### 4.5 Completeness checks

| Code | Condition |
|------|-----------|
| `E1040` | `securitySchemes` defined but never referenced by any operation |
| `E1041` | Middleware in chain references a `context:*` key that no prior middleware in the chain produces (warning) |

---

## 5. Compilation Output

### 5.1 Artifact format (`.bca`)

The compiled artifact is a tar archive (gzip-compressed) with the extension `.bca` (Barbacane Compiled Artifact).

```
artifact.bca
├── manifest.json
├── routes.fb
├── schemas.fb
├── middleware-chains.fb
├── plugins/
│   ├── <plugin-name>.wasm
│   └── ...
└── policies/
    └── <policy-name>.wasm
```

### 5.2 `manifest.json`

```json
{
  "barbacane_artifact_version": 1,
  "compiled_at": "<ISO-8601 UTC>",
  "compiler_version": "<semver>",
  "source_specs": [
    {
      "file": "<filename>",
      "sha256": "<hex>",
      "type": "openapi | asyncapi",
      "version": "<spec version string>"
    }
  ],
  "plugins": [
    {
      "name": "<string>",
      "version": "<semver>",
      "sha256": "<hex>",
      "type": "middleware | dispatcher"
    }
  ],
  "routes_count": "<integer>",
  "checksums": {
    "<filename>": "sha256:<hex>"
  }
}
```

`barbacane_artifact_version` is `1` for this spec. The data plane refuses to load an artifact with an unsupported version.

### 5.3 FlatBuffers files

| File | Contents |
|------|----------|
| `routes.fb` | Prefix-trie routing table. Each node contains: path segment (static or parameter slot with type), HTTP method set, pointer to middleware chain, pointer to dispatcher config. |
| `schemas.fb` | Precompiled JSON Schema validators for request parameters, headers, and bodies. Indexed by operation ID. |
| `middleware-chains.fb` | Resolved middleware chain per operation. Global chain merged with per-operation overrides. Each entry: plugin name, config blob (JSON bytes), execution order. |

The data plane memory-maps these files at startup. No deserialization — zero-copy reads.

### 5.4 Plugin binaries

All referenced `.wasm` binaries are copied into `plugins/`. OPA policies compiled to WASM go into `policies/`. File names match the plugin/policy name. Each file's SHA-256 must match `manifest.json`.

---

## 6. CLI Interface

### 6.1 `barbacane-control compile`

```
barbacane-control compile [OPTIONS] --specs <FILE>...

OPTIONS:
  --specs <FILE>...          One or more OpenAPI/AsyncAPI spec files (required)
  --output <PATH>            Output artifact path (default: ./artifact.bca)
  --registry <URL>           Plugin registry URL (default: from config)
  --production               Enable production checks (reject http:// upstreams) [default]
  --development              Disable production-only checks
  --verbose                  Show detailed compilation output
```

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | Compilation succeeded |
| `1` | Validation error (see stderr for error codes) |
| `2` | Plugin resolution error |
| `3` | I/O error (file not found, registry unreachable) |

### 6.2 `barbacane-control validate`

Quick validation without full compilation (no plugin resolution, no artifact output):

```
barbacane-control validate [OPTIONS] --specs <FILE>...

OPTIONS:
  --specs <FILE>...          One or more spec files (required)
  --verbose                  Show detailed output
```

Runs checks from sections 4.1 and 4.2 only. Does not contact the plugin registry.

### 6.3 Error output format

Errors are printed to stderr in a compiler-style format:

```
error[E1023]: invalid config for plugin "rate-limit" at path /users GET
  --> user-api.yaml:42:9
   |
42 |       quota: "fast"
   |               ^^^^^ expected integer, got string
   |
   = schema: rate-limit/config-schema.json#/properties/quota

error[E1020]: operation has no dispatcher
  --> user-api.yaml:58:5
   |
58 |     delete:
   |     ^^^^^^^ missing x-barbacane-dispatch
```

Warnings use the same format with `warning` instead of `error`. Warnings do not affect the exit code.

---

## 7. Routing Table Construction

### 7.1 Prefix trie

OpenAPI `paths` are compiled into a prefix trie. Each path segment is a trie node:

- **Static segment:** `/users` — exact match
- **Parameter segment:** `/{id}` — captures a value, constrained by the parameter's `schema`
- **Method leaf:** each terminal node contains a set of allowed HTTP methods, each pointing to its operation config

Example: `/users/{id}/orders/{orderId}`

```
root
└── "users"
    └── {id: integer}
        └── "orders"
            └── {orderId: string(uuid)}
                ├── GET  → operation config
                └── POST → operation config
```

### 7.2 Routing precedence

When ambiguity exists (e.g. `/users/me` vs `/users/{id}`), static segments take precedence over parameter segments. This matches OpenAPI's intent: a literal path is more specific than a parameterized one.

### 7.3 Path normalization

Before trie lookup:
- Trailing slashes are stripped (`/users/` → `/users`)
- Double slashes are collapsed (`/users//123` → `/users/123`)
- Percent-encoded characters are decoded for matching, but the original value is preserved for upstream forwarding

---

## 8. Schema Compilation

### 8.1 What is compiled

Every JSON Schema referenced in the spec (request bodies, parameters, headers) is precompiled into a validator stored in `schemas.fb`.

### 8.2 Supported JSON Schema features

The compiler supports JSON Schema Draft 2020-12 (aligned with OpenAPI 3.1) with the following features:

- All type keywords (`type`, `enum`, `const`)
- Numeric constraints (`minimum`, `maximum`, `multipleOf`, `exclusiveMinimum`, `exclusiveMaximum`)
- String constraints (`minLength`, `maxLength`, `pattern`, `format`)
- Array constraints (`minItems`, `maxItems`, `uniqueItems`, `items`, `prefixItems`)
- Object constraints (`required`, `properties`, `additionalProperties`, `minProperties`, `maxProperties`, `patternProperties`)
- Composition (`allOf`, `anyOf`, `oneOf`, `not`)
- Conditional (`if`, `then`, `else`)
- References (`$ref`, `$defs`)

### 8.3 `format` validation

The following `format` values are validated at runtime (not just documentation):

| Format | Validation |
|--------|-----------|
| `date-time` | RFC 3339 |
| `date` | RFC 3339 full-date |
| `time` | RFC 3339 full-time |
| `email` | RFC 5321 |
| `uri` | RFC 3986 |
| `uuid` | RFC 4122 |
| `ipv4` | RFC 2673 |
| `ipv6` | RFC 4291 |

Other format values are ignored (treated as documentation only).
