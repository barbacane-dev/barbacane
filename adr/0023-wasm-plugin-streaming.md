# ADR-0023: WASM Plugin Streaming Support

**Status:** Proposed
**Date:** 2026-03-03

## Context

The current dispatch contract requires plugins to return a fully buffered `Response` (status + headers + `Option<String>` body). This works for typical API proxying but breaks down for use cases that require streaming responses to clients:

- **LLM completions** — Chat APIs (OpenAI, Anthropic) stream tokens via SSE, often taking 10–60 seconds. Buffering the entire response before sending it to the client defeats the purpose of streaming and creates unacceptable latency.
- **Large file proxying** — S3 objects, report downloads, export APIs.
- **Event streams** — Server-Sent Events, webhook relays.

Barbacane's middleware pipeline (SPEC-002) currently assumes a complete response is available before the on_response chain runs:

```
Request  → [M1] → [M2] → [Dispatcher] → Response
Response ← [M2] ← [M1] ← (complete body)
```

Streaming requires the host to begin forwarding chunks to the client *before* the dispatcher returns and *before* on_response middlewares run.

## Decision

### New capability: `http_stream`

Add a new host function alongside the existing `host_http_call`:

```text
host_http_stream(req_ptr: i32, req_len: i32) -> i32
```

Behavior:

1. The plugin calls `host_http_stream` with the same `HttpRequest` JSON format used by `host_http_call`.
2. The host makes the upstream HTTP call.
3. The host immediately begins forwarding response chunks (SSE events, chunked transfer) to the client.
4. The host simultaneously buffers the complete response body internally.
5. When the upstream stream ends, `host_http_stream` returns the length of the complete buffered response (same format as `host_http_call`).
6. The plugin reads the buffered response via `host_http_read_result` for post-processing (token counting, logging, metrics).
7. The plugin returns a `Response` with a sentinel status `0`, signaling to the host that the response was already streamed. The host ignores this return value.

### Capability declaration

```toml
[capabilities]
http_stream = true
```

Plugins declaring `http_stream` also implicitly get `http_call` (same underlying HTTP client). The capability grants access to: `host_http_stream`, `host_http_call`, `host_http_read_result`.

### Middleware on_response behavior during streaming

When a dispatcher uses `host_http_stream`, the response is already sent to the client by the time the dispatch function returns. The on_response middleware chain still runs for observability (logging, metrics), but with two changes:

1. The `Response` passed to middlewares is the **buffered copy** (complete body), not the sentinel. Middlewares can inspect it for logging/metrics.
2. Header and body modifications from on_response middlewares are **silently discarded** — the response was already sent. A debug-level log warns when modifications are attempted on a streamed response.

This preserves backward compatibility: existing middlewares (http-log, observability, correlation-id) continue to receive a Response and can log/record metrics from it. They just can't modify what was already sent.

### Stream error handling

If the upstream connection fails mid-stream:

- The host closes the client connection (SSE: sends `event: error`).
- `host_http_stream` returns -1.
- The plugin can return an error Response, but since headers were already sent, the host cannot change the status code. The error is logged on the admin API.

### SDK changes

New `streamed_response` helper in the plugin SDK:

```rust
/// Marker response indicating the body was already streamed via host_http_stream.
pub fn streamed_response() -> Response {
    Response { status: 0, headers: BTreeMap::new(), body: None }
}
```

### Backward compatibility

- Existing plugins using `host_http_call` are completely unaffected.
- Existing dispatchers returning normal `Response` work identically.
- `host_http_stream` is opt-in via capability declaration.
- On_response middlewares receive a complete Response in both modes.

## Consequences

- **Easier:** Dispatcher plugins can stream SSE, chunked, and large responses to clients without buffering-induced latency. This unblocks the AI gateway plugin (ADR-0024) and future streaming use cases (file proxy, event relay).
- **Harder:** On_response middlewares cannot modify streamed responses (headers/body are already sent). Debugging mid-stream errors is harder since the status code is already committed. The host must manage concurrent streaming + buffering.
- **Trade-offs:** The "buffer everything" approach means memory usage equals response size even during streaming. This is acceptable for LLM responses (typically <100KB of text) but could be problematic for very large file streams. A future enhancement could add a "fire-and-forget" stream mode that skips buffering.

## Alternatives considered

- **Chunk-by-chunk control (`host_stream_read_chunk` / `host_stream_write_chunk`):** Gives plugins full control over each chunk (transform, filter, inject). Rejected for MVP — significantly more complex WASM ABI, harder to reason about error handling mid-stream, and the primary use case (AI proxy) doesn't need per-chunk transformation.
- **Native Rust module (bypass WASM):** Build streaming dispatchers as native Rust code instead of WASM plugins. Rejected — breaks the plugin model and the "everything is a plugin" architecture that differentiates Barbacane.
- **Non-streaming MVP:** Buffer the entire response and return it normally. Rejected — streaming is table stakes for LLM chat UX; a 30-second wait before seeing any output is unacceptable.

## Related ADRs

- [ADR-0006: WASM Plugin Architecture](0006-wasm-plugin-architecture.md)
- [ADR-0008: Dispatch Plugin Interface](0008-dispatch-plugin-interface.md)
- [ADR-0016: Plugin Development Contract](0016-plugin-development-contract.md)
- [ADR-0024: AI Gateway Plugin](0024-ai-gateway-plugin.md)
