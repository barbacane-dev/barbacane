# ADR-0006: WASM Plugin Architecture

**Status:** Accepted
**Date:** 2026-01-28

## Context

Barbacane needs extensibility beyond what OpenAPI/AsyncAPI specs natively express. From prior experience, two distinct extension points exist in a gateway's request lifecycle:

1. **Middlewares** — process the incoming request before it reaches the dispatcher (auth, rate limiting, transformation, logging, etc.)
2. **Dispatchers** — deliver the request to its final destination (the dispatch interface is defined in ADR-0008)

These components must be:

- Developed independently from the gateway core
- Safe to run (no memory corruption, no access to host filesystem)
- Deployable without recompiling the gateway
- Writable in multiple languages (Rust, Go, JS/TS, etc.)

## Decision

### WASM as the Only Plugin Runtime

All plugins (middlewares and dispatchers) are compiled to **WebAssembly** and executed in a sandboxed WASM runtime (`wasmtime`).

| Concern | Approach |
|---------|----------|
| Isolation | WASM sandbox — no host access unless explicitly granted |
| Performance | Near-native with `wasmtime` AOT compilation |
| Polyglot | Authors write in Rust, Go, C, JS/TS — compiled to `.wasm` |
| Distribution | Plugins are `.wasm` artifacts, versioned and stored alongside specs |

No native/dynamic library plugins. The security and isolation guarantees of WASM outweigh the marginal performance cost.

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

### Spec Integration

Plugins are declared in OpenAPI/AsyncAPI specs via `x-barbacane-*` extensions.

#### Global middleware chain (spec root level)

```yaml
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
            quota: 1000
            window: 60
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: http://health-service:8080
```

#### Dispatch declaration

```yaml
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: http://user-service:3000
          timeout: 5s
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

1. Plugins are compiled to `.wasm` by their authors
2. Plugins are registered in the control plane (`barbacane-control`)
3. Specs reference plugins by name
4. At compilation time (CI/CD), the control plane resolves plugin references and bundles `.wasm` artifacts with the compiled spec
5. Data plane loads and AOT-compiles WASM modules at startup

## Consequences

- **Easier:** Safe extensibility, polyglot plugin development, plugins can't crash the gateway, clear separation between core and extensions
- **Harder:** WASM has limited host access (no arbitrary I/O from plugins without host functions), slight performance overhead vs native
- **Related:** Dispatch plugin interface defined in ADR-0008
