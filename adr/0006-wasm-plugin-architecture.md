# ADR-0006: WASM Plugin Architecture

**Status:** Accepted
**Date:** 2026-01-28
**Updated:** 2026-01-30

## Context

Barbacane needs extensibility beyond what OpenAPI/AsyncAPI specs natively express. From prior experience, two distinct extension points exist in a gateway's request lifecycle:

1. **Middlewares** — process the incoming request before it reaches the dispatcher (auth, rate limiting, transformation, logging, etc.)
2. **Dispatchers** — deliver the request to its final destination (the dispatch interface is defined in ADR-0008)

These components must be:

- Developed independently from the gateway core
- Safe to run (no memory corruption, no access to host filesystem)
- Deployable without recompiling the gateway
- Writable in multiple languages (Rust, Go, JS/TS, etc.)

Additionally, plugin configuration must follow the principle: **explicit is better than implicit**. Users should have full control over which plugins are available, with no "magic" built-ins that appear without declaration.

## Decision

### WASM as the Only Plugin Runtime

All plugins (middlewares and dispatchers) are compiled to **WebAssembly** and executed in a sandboxed WASM runtime (`wasmtime`).

| Concern | Approach |
|---------|----------|
| Isolation | WASM sandbox — no host access unless explicitly granted |
| Performance | Near-native with `wasmtime` AOT compilation |
| Polyglot | Authors write in Rust, Go, C, JS/TS — compiled to `.wasm` |
| Distribution | Plugins are `.wasm` artifacts, resolved at compile time |

No native/dynamic library plugins. The security and isolation guarantees of WASM outweigh the marginal performance cost.

### Bare Binary Philosophy

The `barbacane` binary contains only the core gateway runtime — **no plugins are bundled**. Every plugin, including official ones like `mock`, `http-upstream`, and `lambda`, must be explicitly declared. This ensures:

- Users know exactly what's in their gateway
- Minimal footprint when only a few plugins are needed
- No surprise behaviors from undeclared plugins
- Consistent treatment of all plugins (official and third-party)

### Plugin Types

#### Middlewares

Middlewares intercept and process requests/responses. They form an ordered chain.

```
Request → [Middleware 1] → [Middleware 2] → ... → [Dispatcher] → upstream
                                                      ↓
Response ← [Middleware 2] ← [Middleware 1] ←──────────┘
```

Each middleware can:
- Inspect/modify request headers, body, path, query
- Short-circuit the chain (e.g., return 401 without reaching backend)
- Inspect/modify the response on the way back
- Pass context to downstream middlewares (e.g., authenticated user ID)

#### Dispatchers

Dispatchers handle final request delivery to the target. The dispatch interface is defined in ADR-0008. The plugin mechanism (WASM, sandboxing, distribution) is the same as middlewares.

### Plugin Configuration

Plugin availability is configured separately from plugin usage, following the Kubernetes Gateway API pattern:

| Concern | File | Purpose |
|---------|------|---------|
| **What plugins are available** | `barbacane.yaml` | Manifest — declares plugin sources |
| **How plugins are used** | OpenAPI spec | API contract — references plugins by name |

This separation ensures the API spec remains a portable contract while deployment configuration stays in a dedicated manifest.

#### Manifest File (`barbacane.yaml`)

The manifest lives in the project root and declares all available plugins:

```yaml
# barbacane.yaml
plugins:
  # Local file
  mock:
    path: ./plugins/mock.wasm

  # Remote URL
  http-upstream:
    url: https://plugins.barbacane.io/http-upstream/0.1.0/http-upstream.wasm

  # Another local plugin
  jwt-auth:
    path: ./plugins/jwt-auth.wasm
```

#### Plugin Sources (MVP)

Initially, two sources are supported:

| Source | Syntax | Use Case |
|--------|--------|----------|
| `path` | Local filesystem path | Development, vendored plugins |
| `url` | HTTPS URL | Remote distribution |

A plugin registry may be added later when the need arises.

#### Compile-Time Resolution

Plugins are resolved at compile time and bundled into the `.bca` artifact:

```bash
barbacane compile --spec api.yaml --output api.bca
# Reads barbacane.yaml
# Resolves all plugin sources
# Bundles .wasm files into api.bca
```

The resulting artifact is **fully self-contained** — it works offline and requires no plugin resolution at serve time.

#### Validation

If the spec references a plugin not declared in the manifest, compilation fails:

```
Error E1040: Plugin 'rate-limit' used in spec but not declared in barbacane.yaml

  --> api.yaml:15:9
   |
15 |         name: rate-limit
   |         ^^^^^^^^^^^^^^^^ undeclared plugin

Help: Add 'rate-limit' to your barbacane.yaml plugins section
```

### Spec Integration

Plugins are **used** in OpenAPI/AsyncAPI specs via `x-barbacane-*` extensions. The spec references plugins by name — availability is determined by the manifest.

#### Complete Example

```yaml
# barbacane.yaml (manifest)
plugins:
  jwt-auth:
    path: ./plugins/jwt-auth.wasm
  rate-limit:
    path: ./plugins/rate-limit.wasm
  request-logger:
    path: ./plugins/request-logger.wasm
  http-upstream:
    path: ./plugins/http-upstream.wasm
```

```yaml
# api.yaml (spec)
openapi: 3.1.0
info:
  title: User API
  version: 1.0.0

x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: https://auth.example.com
      audiences: [api.example.com]
  - name: rate-limit
    config:
      quota: 100
      window: 60
      key: header:x-api-key
  - name: request-logger

paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://user-service.internal
          timeout: 5.0
```

#### Global middleware chain

Middlewares declared at spec root apply to all routes:

```yaml
x-barbacane-middlewares:
  - name: jwt-auth
    config:
      issuer: https://auth.example.com
  - name: rate-limit
    config:
      quota: 100
      window: 60
```

#### Per-route override

Routes inherit the global chain by default. They can **override** it entirely:

```yaml
paths:
  /public/health:
    get:
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 1000  # Higher limit for health checks
            window: 60
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://health-service.internal
```

#### Dispatch declaration

Every operation requires a dispatcher:

```yaml
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: https://user-service.internal
          timeout: 5.0  # seconds (numeric)
```

### Plugin Host Interface

Barbacane exposes a minimal host API to WASM plugins:

```
// Request phase
fn on_request(request: Request, config: Config) -> Action<Request>

// Response phase
fn on_response(response: Response, config: Config) -> Action<Response>
```

Where `Action` is:
- `Continue(T)` — pass to next middleware (possibly modified)
- `ShortCircuit(Response)` — stop the chain, return this response

### Plugin Lifecycle

1. Plugin authors compile their plugins to `.wasm`
2. Users declare available plugins in `barbacane.yaml` (path or URL)
3. OpenAPI specs reference plugins by name via `x-barbacane-*` extensions
4. `barbacane compile` validates that all referenced plugins are declared
5. `barbacane compile` resolves plugin sources and bundles `.wasm` into the artifact
6. `barbacane serve` loads the self-contained artifact (no external dependencies)

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│ barbacane.yaml  │     │   api.yaml      │     │    api.bca      │
│ (manifest)      │────▶│   (spec)        │────▶│   (artifact)    │
│                 │     │                 │     │                 │
│ plugins:        │     │ x-barbacane-    │     │ ├── spec        │
│   mock: ...     │     │   dispatch:     │     │ ├── mock.wasm   │
│   http: ...     │     │     name: mock  │     │ └── http.wasm   │
└─────────────────┘     └─────────────────┘     └─────────────────┘
                              compile              serve
```

### Starter Templates

To ease onboarding, Barbacane provides starter templates via `barbacane init`:

```bash
# Basic template with common plugins
barbacane init --template basic
# Creates:
#   barbacane.yaml (mock, http-upstream, lambda)
#   plugins/ (downloaded .wasm files)
#   api.yaml (example spec)

# Minimal template for advanced users
barbacane init --template minimal
# Creates:
#   barbacane.yaml (empty plugins section)
#   api.yaml (skeleton spec)
```

Templates download official plugins and set up a working project structure.

## Consequences

- **Easier:** Safe extensibility, polyglot plugin development, plugins can't crash the gateway, clear separation between core and extensions
- **Harder:** WASM has limited host access (no arbitrary I/O from plugins without host functions), slight performance overhead vs native
- **Trade-off:** Bare binary requires explicit plugin declaration even for official plugins, but ensures full transparency and minimal footprint
- **Related:** Dispatch plugin interface defined in ADR-0008
