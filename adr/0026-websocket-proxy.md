# ADR-0026: WebSocket Transparent Proxy

**Status:** Proposed
**Date:** 2026-03-08

## Context

Barbacane supports HTTP reverse proxying (ADR-0008) and sync-to-async event bridges for Kafka/NATS via AsyncAPI. A recurring need is **WebSocket proxying** — allowing clients to establish persistent bidirectional connections to upstream services through the gateway.

Use cases include:
- Real-time dashboards and notifications
- Chat and collaboration features
- Live data feeds (trading, IoT telemetry)
- Multiplayer gaming backends

WebSocket fundamentally differs from Barbacane's request/response model. A WebSocket connection starts as an HTTP Upgrade request, then transitions to a persistent, bidirectional, frame-based protocol that lives outside the normal middleware pipeline.

### The tension

Barbacane's pipeline assumes a single request produces a single response:

```
Request → [Middleware 1] → [Middleware 2] → [Dispatcher] → Response
```

WebSocket requires the gateway to maintain a long-lived, stateful connection after the initial handshake — with an unbounded number of frames flowing in both directions. Adding per-frame middleware processing would fundamentally change the gateway's architecture, increase memory footprint, and complicate the WASM plugin model (bidirectional streaming ABI, backpressure signaling, frame buffering).

### Design principle

The gateway should **not become a WebSocket application server**. Its role is to authenticate and authorize the upgrade request, then get out of the way.

## Decision

### WebSocket as a transparent proxy dispatcher

WebSocket support is implemented as a new dispatcher plugin (`ws-upstream`) that follows the existing dispatch model (ADR-0008) for the handshake phase, then delegates to the **host runtime** for frame relay.

### Two-phase architecture

#### Phase 1: Handshake (spec-driven, middleware-enabled)

The WebSocket upgrade request (`Connection: Upgrade`, `Upgrade: websocket`) is routed and processed like any HTTP request:

1. The data plane router matches the path and method (always GET for WebSocket).
2. The full middleware chain runs on the upgrade request — auth (JWT, API key, OAuth), rate limiting, request validation, logging. All existing middleware plugins work unchanged.
3. The `ws-upstream` dispatcher plugin receives the processed request and returns an `UpgradeResponse` — a 101 Switching Protocols response with the upstream WebSocket URL.

```yaml
paths:
  /ws/notifications:
    get:
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: https://auth.example.com
      x-barbacane-dispatch:
        name: ws-upstream
        config:
          url: ws://notification-service:8080/ws
          connect_timeout: 5s
```

If any middleware short-circuits (returns 401, 429, etc.), the upgrade is rejected — the client receives a normal HTTP error response, and no WebSocket connection is established.

#### Phase 2: Frame relay (host-managed, no WASM involvement)

Once the dispatcher signals a successful upgrade:

1. The host runtime completes the WebSocket handshake with the client.
2. The host establishes a WebSocket connection to the upstream service.
3. The host spawns two relay tasks (tokio): client→upstream and upstream→client.
4. Frames are forwarded as-is — no inspection, no transformation, no WASM calls.
5. When either side closes (or the connection errors), the host closes the other side and cleans up.

```
Client ←──WebSocket──→ [Barbacane Host] ←──WebSocket──→ Upstream
           (frames relayed transparently)
```

### New host function: `host_ws_upgrade`

```text
host_ws_upgrade(req_ptr: i32, req_len: i32) -> i32
```

The plugin calls `host_ws_upgrade` with a JSON payload:

```json
{
  "url": "ws://notification-service:8080/ws",
  "connect_timeout_ms": 5000,
  "headers": { "X-Forwarded-For": "1.2.3.4" }
}
```

Behavior:

1. The host connects to the upstream WebSocket endpoint.
2. If the upstream connection succeeds, the function returns `0` (success). The host takes ownership of the bidirectional relay.
3. If the upstream connection fails (timeout, refused, TLS error), the function returns `-1`. The plugin reads the error via `host_http_read_result` and returns an appropriate error response (502 Bad Gateway).
4. After a successful `host_ws_upgrade`, the plugin returns a sentinel `Response { status: 101, headers: {}, body: None }`. The host ignores this return value (similar to `host_http_stream` in ADR-0023).

### Capability declaration

```toml
[plugin]
name = "ws-upstream"
version = "0.1.0"
type = "dispatcher"
description = "Transparent WebSocket proxy"
wasm = "ws-upstream.wasm"

[capabilities]
ws_upgrade = true
log = true
```

The `ws_upgrade` capability grants access to `host_ws_upgrade` and `host_http_read_result`.

### Middleware behavior during WebSocket connections

| Phase | Middleware involvement |
|-------|----------------------|
| Upgrade request | Full middleware chain runs normally (auth, rate-limit, logging, etc.) |
| on_response | Receives the 101 Switching Protocols response for observability (logging, metrics). Modifications are silently discarded — the upgrade was already committed. Same behavior as streamed responses (ADR-0023). |
| Frame relay | No middleware involvement. Frames are opaque to the gateway. |
| Connection close | No middleware callback. Close is logged at debug level by the host. |

### Connection lifecycle management

| Concern | Behavior |
|---------|----------|
| **Idle timeout** | Configurable per-route. Default: no timeout (rely on WebSocket ping/pong). |
| **Max connections** | Enforced by the host via a semaphore. Upgrade requests beyond the limit receive 503 Service Unavailable before reaching the dispatcher. |
| **Ping/pong** | The host relays ping/pong frames transparently. It does not inject its own. |
| **Backpressure** | Standard TCP backpressure. If the client stops reading, the upstream→client relay pauses (tokio AsyncRead naturally blocks). |
| **TLS** | Upstream `wss://` supported via the existing rustls stack. |

### Spec declaration

WebSocket routes are declared in OpenAPI specs like any other operation. The `ws-upstream` dispatcher signals the gateway to expect an upgrade:

```yaml
paths:
  /ws/chat:
    get:
      summary: Chat WebSocket endpoint
      x-barbacane-middlewares:
        - name: jwt-auth
          config:
            issuer: https://auth.example.com
        - name: rate-limit
          config:
            quota: 10
            window: 60
            key: claim:sub
      x-barbacane-dispatch:
        name: ws-upstream
        config:
          url: ws://chat-service:8080/ws
          connect_timeout: 5s
          idle_timeout: 300s
          max_frame_size: 64KB
```

No AsyncAPI needed — WebSocket proxy is a transport concern, not an event schema concern. If users want message schema validation, that belongs in the upstream service, not the gateway.

### What this is NOT

- **Not a WebSocket application server** — no message routing, pub/sub, room management, or presence tracking.
- **Not per-message middleware** — no frame inspection, transformation, or filtering at the gateway level.
- **Not a protocol translator** — no HTTP-to-WebSocket or WebSocket-to-Kafka bridging. The sync-to-async bridge (Kafka/NATS dispatchers) handles that pattern.
- **Not a WebSocket load balancer** — no sticky sessions or connection draining. The upstream service handles horizontal scaling.

## Consequences

- **Easier:** WebSocket endpoints get the same auth/rate-limit/observability as HTTP endpoints with zero middleware changes. The `ws-upstream` dispatcher is a simple WASM plugin (~100 lines). The heavy lifting (frame relay) is in the host runtime, which is well-suited for it (tokio, native async I/O).
- **Harder:** The data plane now holds long-lived connections, increasing memory footprint proportional to concurrent WebSocket sessions. Graceful shutdown must drain active WebSocket connections (configurable drain timeout). Monitoring must track WebSocket-specific metrics (active connections, frame throughput, upgrade success/failure rates).
- **Trade-off:** No per-message middleware means the gateway cannot inspect or transform WebSocket traffic. This is intentional — keeping the gateway lean and predictable. Use cases that need message-level processing (schema validation, message routing, pub/sub fan-out) should use the sync-to-async bridge pattern (HTTP → Kafka/NATS) or handle it in the upstream service.

## Alternatives considered

- **Per-message WASM middleware:** Extend the middleware model with `on_ws_message(Frame) -> Action<Frame>` callbacks. Rejected — fundamentally changes the gateway's memory model (frame buffering), complicates the WASM ABI (streaming frames in/out of sandbox), and the primary use case (transparent proxy) doesn't need it. Can be revisited if demand materializes.
- **AsyncAPI `ws` channel binding:** Describe WebSocket endpoints in AsyncAPI with message schemas. Rejected for v1 — adds spec complexity for a feature that doesn't benefit from schema validation at the gateway level. The gateway's job is auth and proxying, not message validation.
- **Native Rust dispatcher (no WASM):** Build the WebSocket dispatcher directly in the host. Rejected — breaks the "everything is a plugin" model (ADR-0006). The WASM plugin is thin (just calls `host_ws_upgrade`), and the host function does the real work.

## Related ADRs

- [ADR-0005: Proxy Framework](0005-proxy-framework.md) — hyper with `serve_connection_with_upgrades` enables HTTP Upgrade
- [ADR-0006: WASM Plugin Architecture](0006-wasm-plugin-architecture.md) — plugin model and bare binary philosophy
- [ADR-0008: Dispatch Plugin Interface](0008-dispatch-plugin-interface.md) — one route, one dispatcher
- [ADR-0016: Plugin Development Contract](0016-plugin-development-contract.md) — plugin capabilities and manifest
- [ADR-0023: WASM Plugin Streaming](0023-wasm-plugin-streaming.md) — sentinel response pattern reused here
