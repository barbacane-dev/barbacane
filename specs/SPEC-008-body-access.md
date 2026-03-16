# SPEC-008: Middleware Body Access Control

**Status:** Draft
**Date:** 2026-03-15
**Derived from:** SPEC-003 (Plugin System)

---

## 1. Problem Statement

Barbacane currently serializes the full request body (base64-encoded) into every middleware's `Request` JSON. A 2MB file upload becomes ~2.7MB of base64, copied into every WASM instance in the chain — even for auth plugins that only inspect headers.

This is the opposite of what every major gateway does:

| Gateway | Body default |
|---------|-------------|
| Kong (Lua) | Not read. Plugin must call `kong.request.get_raw_body()` explicitly |
| Kong (proxy-wasm) | Streaming callbacks with explicit buffer/continue semantics |
| Tyk | Not passed to plugins by default. Explicit opt-in required |
| KrakenD | Not forwarded by default. Requires `body_forwarding: true` per endpoint |

For a route with 4 middlewares and a 2MB upload, the current design wastes ~11MB of base64 copies + WASM memory before the body reaches the dispatcher.

---

## 2. Design

### 2.1 New capability: `body_access`

A new capability in `plugin.toml` controls whether a middleware receives the request body:

```toml
[capabilities]
host_functions = ["log", "context_get"]
body_access = true    # default: false
```

**Rules:**
- `body_access` defaults to `false` for middleware plugins.
- `body_access` is always implicitly `true` for dispatcher plugins (they need the body to proxy it upstream). It is ignored if set explicitly on a dispatcher.
- `body_access` has no effect on `on_response` — response bodies are always passed to middleware. The response body is produced by the dispatcher (typically small — error messages, API responses) and cannot be stripped without breaking response-transformer plugins.

### 2.2 Middleware behavior

| `body_access` | `on_request` receives | `on_response` receives |
|---------------|----------------------|----------------------|
| `false` (default) | Request with `body: null` | Full response (unchanged) |
| `true` | Full request with body | Full response (unchanged) |

When `body_access` is `false`, the host sets `body` to `null` in the JSON before passing it to the middleware. The original body is held aside by the host and re-attached after the middleware chain completes, before dispatching.

### 2.3 Body preservation across the chain

The chain execution loop is unchanged — middlewares still run in order, each receiving the previous middleware's output as its input. The only difference is whether `body` is present or `null` when a specific middleware sees the request.

The host holds the body aside in a separate variable and manages it around each middleware call:

```
current_request = full request (with body)
held_body = current_request.body

for each middleware in chain order:
    if middleware.body_access:
        # Pass full request (body included) to WASM
        current_request = middleware.on_request(current_request)
        held_body = current_request.body       # middleware may have modified body
    else:
        # Strip body before WASM, re-attach after
        current_request.body = null
        current_request = middleware.on_request(current_request)
        current_request.body = held_body       # restore original/last-modified body

# Dispatcher always gets the final body
dispatcher.dispatch(current_request)
```

**Concrete example** — chain: `[apikey-auth, rate-limit, request-transformer, cors]`
where only `request-transformer` has `body_access = true`:

```
Step 0: current_request has 2MB body, held_body = 2MB body

Step 1: apikey-auth (body_access = false)
    → strip body → pass headers-only request to WASM → get output
    → current_request = output (may have added X-Consumer-Id header)
    → current_request.body = held_body (re-attach 2MB body)

Step 2: rate-limit (body_access = false)
    → strip body → pass headers-only request → get output
    → current_request = output
    → current_request.body = held_body

Step 3: request-transformer (body_access = true)
    → pass full request WITH 2MB body to WASM → get output
    → current_request = output (body may now be different — e.g. added a JSON field)
    → held_body = current_request.body (update held body with transformer's changes)

Step 4: cors (body_access = false)
    → strip body → pass headers-only request → get output
    → current_request = output
    → current_request.body = held_body (transformer's modified body)

Step 5: dispatcher receives current_request with final body
```

Key behaviors:
- **Chain order is unchanged.** Each middleware receives the previous middleware's output (headers, path, query modifications are always preserved).
- If a `body_access = true` middleware modifies the body, the modification flows to all subsequent middlewares and the dispatcher.
- If a `body_access = false` middleware returns a body (it shouldn't, since it received `null`), the host ignores it and uses the held-aside body.
- The dispatcher always receives the final body.

### 2.4 Plugin classification

**Middleware that needs `body_access = true`:**

| Plugin | Reason |
|--------|--------|
| `request-transformer` | Modifies the request body |
| `response-transformer` | Modifies the response body (but only `on_response`, not `on_request`) |
| `request-size-limit` | Checks `body.len()` — can be refactored to use `content-length` header instead |
| `cel` | CEL expressions may reference `request.body` |

**Middleware that does NOT need body access (default `body_access = false`):**

| Plugin | Inspects |
|--------|----------|
| `apikey-auth` | Headers only |
| `basic-auth` | `Authorization` header |
| `jwt-auth` | `Authorization` header |
| `oauth2-auth` | `Authorization` header |
| `oidc-auth` | `Authorization` header |
| `cors` | `Origin` + method headers |
| `rate-limit` | Client IP / headers |
| `ip-restriction` | Client IP |
| `acl` | Context / headers |
| `bot-detection` | `User-Agent` header |
| `observability` | Headers, method, path |
| `opa-authz` | Headers, path, method |
| `http-log` | Headers, path, status (body size from `content-length`) |

### 2.5 Opt-in body access for `request-size-limit`

`request-size-limit` currently reads `req.body` to check its length. Post-SPEC-008, it should be refactored to read the `content-length` header instead, allowing it to run with `body_access = false`. This avoids buffering the entire body just to check its size.

For chunked transfers without `content-length`, the data plane's existing `--max-body-size` flag handles enforcement before the body reaches plugins.

---

## 3. Artifact Format Changes

### 3.1 Problem

Plugin capabilities declared in `plugin.toml` are currently validated at compile time but **not stored in the `.bca` artifact**. The `BundledPlugin` struct only carries:

```rust
pub struct BundledPlugin {
    pub name: String,
    pub version: String,
    pub plugin_type: String,
    pub wasm_path: String,
    pub sha256: String,
}
```

The data plane has no way to know which capabilities a plugin declared.

### 3.2 Solution: Add capabilities to `BundledPlugin`

Extend the manifest's `BundledPlugin` to include capability metadata:

```rust
pub struct BundledPlugin {
    pub name: String,
    pub version: String,
    pub plugin_type: String,
    pub wasm_path: String,
    pub sha256: String,
    /// Capabilities declared in plugin.toml.
    #[serde(default)]
    pub capabilities: PluginCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Host functions this plugin imports.
    #[serde(default)]
    pub host_functions: Vec<String>,
    /// Whether the middleware receives the request body.
    /// Always true for dispatchers. Defaults to false for middleware.
    #[serde(default)]
    pub body_access: bool,
}
```

### 3.3 Artifact version

No version bump needed. Barbacane is pre-1.0 — existing artifacts must be recompiled after this change. Old artifacts without the `capabilities` field are not supported.

---

## 4. Runtime Changes

### 4.1 Compiler

When building the artifact, the compiler reads `body_access` from `plugin.toml` and writes it into `BundledPlugin.capabilities` in `manifest.json`.

### 4.2 Data plane plugin loading

`load_plugins` currently returns `HashMap<String, (String, Vec<u8>)>` (name → version, wasm_bytes). Extend to include capabilities:

```rust
pub struct LoadedPlugin {
    pub version: String,
    pub wasm_bytes: Vec<u8>,
    pub capabilities: PluginCapabilities,
}
```

### 4.3 Middleware chain execution

In `execute_middleware_on_request` (or the chain execution layer), before calling `instance.on_request(data)`:

1. Check if this middleware has `body_access`.
2. If not, strip the `body` field from the request JSON before passing it to the WASM instance.
3. After the middleware returns, re-attach the held-aside body to the output request.

The body stripping and re-attachment happen at the JSON level (`serde_json::Value` manipulation), which is already the pattern used in the test suite (see `gateway_flow_body_stripped_then_reattached` in `types.rs`).

### 4.4 Where body access is checked

Two options for where the `body_access` flag is available at request time:

**Option A: On `PluginInstance`** — The pool stores `body_access` per compiled module. When `get_instance` returns, the flag is available on the instance. The chain executor reads it per-middleware.

**Option B: On `MiddlewareConfig` in the compiled routes** — The compiler writes `body_access` into each `MiddlewareConfig` in `routes.json`. The data plane reads it without needing to look up plugin metadata.

**Recommendation: Option A.** `body_access` is a property of the plugin, not the route. Storing it per-instance avoids duplicating the flag across every route that uses the plugin.

---

## 5. Rollout

1. Add `body_access` to `Capabilities` struct, `plugin.toml` schema.
2. Add `capabilities` to `BundledPlugin` and `PluginBundle`. Propagate through compiler and data plane.
3. Add `body_access = true` to `request-transformer`, `cel` `plugin.toml` files.
4. Implement body stripping in the chain executor.
5. Refactor `request-size-limit` to use `content-length` header.

Existing artifacts must be recompiled after this change (pre-1.0, no backward compatibility).

---

## 6. Performance Impact

For a route with 4 auth middlewares and a 2MB body:

| Metric | Before (current) | After (body_access) |
|--------|------------------|---------------------|
| Base64 copies into WASM | 4 x 2.7MB = 10.8MB | 0 (auth plugins get null) |
| WASM memory per auth middleware | ~8MB peak | ~1MB peak |
| JSON parse time per middleware | ~5ms (with body) | ~0.2ms (headers only) |
| Total chain overhead (4 MW) | ~20ms + 32MB memory | ~0.8ms + 4MB memory |

The dispatcher still receives the full body (one copy), which is unavoidable.
