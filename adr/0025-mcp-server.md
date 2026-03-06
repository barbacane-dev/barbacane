# ADR-0025: MCP Server Support

**Status:** Accepted
**Date:** 2026-03-03

## Context

The Model Context Protocol (MCP) is an open standard (spec version `2025-11-25`) that enables AI agents to discover and invoke external tools via a structured JSON-RPC 2.0 interface. MCP has become the universal protocol for AI agent tool integration, backed by Anthropic, OpenAI, Google, and Microsoft, with 97M+ SDK monthly downloads.

### What MCP does

MCP defines three primitives that servers expose to AI agent clients:

| Primitive | Purpose | Example |
|-----------|---------|---------|
| **Tools** | Executable functions the AI can call | `createOrder(items, address)` |
| **Resources** | Read-only data for context | API documentation, database records |
| **Prompts** | Reusable message templates | System instructions, few-shot examples |

The protocol uses JSON-RPC 2.0 over Streamable HTTP (for remote servers) or stdio (for local). A typical session:

1. Client sends `initialize` → server returns capabilities
2. Client calls `tools/list` → server returns tool declarations with JSON schemas
3. Client calls `tools/call { name, arguments }` → server executes and returns results

### Why this matters for Barbacane

API gateways are natural MCP servers. KrakenD (Enterprise v2.12), Azure API Management, and AWS API Gateway have all shipped MCP support. The value proposition: organizations have existing REST/gRPC APIs behind their gateway — MCP lets AI agents discover and use those APIs without bespoke integration.

### Barbacane's unique advantage

Barbacane already has everything needed to generate MCP tools automatically from the compiled spec:

| OpenAPI concept | MCP tool field | Mapping |
|----------------|---------------|---------|
| `operationId` | `name` | Direct — unique identifier |
| `summary` / `description` | `description` | Direct |
| `requestBody.schema` + path params + query params | `inputSchema` | Merge into single JSON Schema object |
| Response `200` schema | `outputSchema` | Direct |

This means: **define your API in OpenAPI, get an MCP server for free.** No separate MCP configuration file, no manual tool declarations. The `.bca` artifact already contains the compiled operations with their schemas. This is a differentiator no competitor currently offers — KrakenD requires separate MCP configuration; Azure API Management requires explicit export.

### Spec-driven MCP example

Given this OpenAPI spec:

```yaml
paths:
  /orders:
    post:
      operationId: createOrder
      summary: Create a new order
      requestBody:
        content:
          application/json:
            schema:
              type: object
              required: [items]
              properties:
                items:
                  type: array
                  items:
                    type: object
                    properties:
                      product_id: { type: string }
                      quantity: { type: integer }
                shipping_address:
                  type: string
      responses:
        '200':
          content:
            application/json:
              schema:
                type: object
                properties:
                  order_id: { type: string }
                  status: { type: string }
```

Barbacane would automatically expose this MCP tool:

```json
{
  "name": "createOrder",
  "description": "Create a new order",
  "inputSchema": {
    "type": "object",
    "required": ["items"],
    "properties": {
      "items": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "product_id": { "type": "string" },
            "quantity": { "type": "integer" }
          }
        }
      },
      "shipping_address": { "type": "string" }
    }
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "order_id": { "type": "string" },
      "status": { "type": "string" }
    }
  }
}
```

When an AI agent calls `tools/call { name: "createOrder", arguments: { items: [...] } }`, Barbacane translates it to `POST /orders` with the JSON body, routes it through the normal middleware pipeline (auth, rate limit, validation), dispatches to the upstream, and returns the result as an MCP tool response.

## Decision

This ADR proposes adding MCP server support to the Barbacane data plane as a **native protocol feature** (not a WASM plugin). The implementation details below are presented as options to be refined during development.

### Architecture: native data plane feature

MCP should be a built-in capability of the data plane binary, not a WASM plugin. Reasons:

- MCP needs access to the compiled artifact's route table and schemas — information that plugins don't have
- MCP is a protocol-level concern (like HTTP, WebSocket, TLS) — the data plane already handles protocol multiplexing
- Tool calls must route through the existing middleware + dispatcher pipeline — the MCP handler acts as an internal HTTP client, not an external proxy
- Stateful session management (initialization, capability negotiation) is best handled in the host runtime

### Tool call flow

```
AI Agent (MCP Client)
    │
    ▼ JSON-RPC over HTTP
┌─────────────────────────────────────────┐
│ MCP Handler                             │
│  1. Parse JSON-RPC request              │
│  2. Map tool name → operationId → route │
│  3. Construct internal HTTP request      │
│     from tool arguments                 │
│  4. Route through middleware pipeline   │
│  5. Convert HTTP response → MCP result  │
└────────────────┬────────────────────────┘
                 │ (internal)
                 ▼
┌─────────────────────────────────────────┐
│ Normal request pipeline                 │
│  [Auth] → [Rate Limit] → [Dispatcher]  │
└─────────────────────────────────────────┘
```

The middleware pipeline runs identically for MCP tool calls and regular HTTP requests. Auth, rate limiting, validation, and all other middlewares apply transparently.

### Open design choices

#### 1. Listening endpoint

| Option | Description | Trade-offs |
|--------|-------------|------------|
| **Main port, dedicated path** (e.g., `POST /mcp`) | AI agents connect to the same endpoint as regular clients | Simplest deployment. Auth middleware applies naturally. Risk: MCP endpoint accessible to anyone with network access to the main port. |
| **Separate dedicated port** (`--mcp-bind`) | Dedicated listening port specifically for MCP | Full isolation, independent TLS config possible. More operational complexity (three ports). |

The admin port (ADR-0022) is intentionally excluded — it serves internal observability endpoints (`/health`, `/metrics`) on a separate router that bypasses the spec-driven middleware pipeline. MCP tool calls need to route through the full middleware chain (auth, rate limiting, validation), which only the main router provides.

#### 2. Tool scope

Following the established `x-barbacane-middlewares` pattern (see `docs/guide/middlewares.md`), MCP uses the same global vs operation model:

**Global MCP (root level)** — applies to all operations:

```yaml
openapi: "3.1.0"
info:
  title: My API
  version: "1.0.0"

# Enable MCP for all operations
x-barbacane-mcp:
  enabled: true

paths:
  /orders:
    post:
      operationId: createOrder        # exposed as MCP tool ✓
  /orders/{id}:
    get:
      operationId: getOrder           # exposed as MCP tool ✓
```

**Per-operation override** — opt out or customize:

```yaml
paths:
  /admin/reset:
    post:
      operationId: resetDatabase
      x-barbacane-mcp:
        enabled: false                # opted out ✗
  /orders:
    post:
      operationId: createOrder
      x-barbacane-mcp:
        enabled: true
        description: "Create a new customer order with shipping address"
```

**No root declaration = MCP disabled entirely** — secure by default, no tools exposed unless explicitly enabled. This matches how an absent `x-barbacane-middlewares` at root means no global middlewares.

Open question: should the root-level config support additional server metadata?

```yaml
x-barbacane-mcp:
  enabled: true
  # MCP server metadata (returned in initialize handshake)
  server_name: "Acme Order API"
  server_version: "1.0.0"
```

#### 3. MCP authentication

| Option | Description | Trade-offs |
|--------|-------------|------------|
| **Reuse existing middleware** | MCP requests flow through the same auth middleware as HTTP requests. Agent presents Bearer token / API key in the MCP HTTP headers. | Consistent security model. Agent needs same credentials as any API client. |
| **Dedicated MCP auth** | Separate API key scope (e.g., `mcp:connect`) validated at the MCP handler level before routing to middleware. | Lets operators grant MCP-specific access. More granular control. Additional key management. |
| **No auth (network isolation)** | If MCP is on a dedicated port (`--mcp-bind`), rely on network-level isolation (e.g., bind to `127.0.0.1`, Kubernetes NetworkPolicy). | Simplest. Only viable for internal/development use. |

#### 4. Schema mapping complexity

Several edge cases need resolution:

- **Path parameters** — An operation like `GET /users/{id}` needs the path parameter merged into the tool's `inputSchema`. The tool argument `id` maps to the path segment.
- **Query parameters** — Similarly merged into `inputSchema` with metadata about where each argument goes (path vs. query vs. body).
- **Multiple content types** — If an operation accepts both `application/json` and `multipart/form-data`, which schema is used for the tool?
- **Operations without `operationId`** — These can't become tools (MCP requires a `name`). The compiler could warn about this.
- **Schema composition** — `allOf`/`oneOf`/`anyOf` in request schemas may need flattening for MCP tool schemas to be usable by AI models.

#### 5. MCP transport

The MCP spec defines two transports:

| Transport | Use case | Barbacane fit |
|-----------|----------|---------------|
| **Streamable HTTP** | Remote servers. `POST` for requests, optional SSE for streaming responses. | Natural fit — Barbacane already handles HTTP and SSE (ADR-0023). |
| **Stdio** | Local servers on same machine. | Not applicable for a gateway. |

Streamable HTTP is the only relevant transport. Tool call responses could optionally use SSE for long-running operations, reusing the streaming infrastructure from ADR-0023.

### Compile-time support

The compiler could provide MCP-specific validation:

- **Warn** when operations lack `operationId` (required for tool naming)
- **Warn** when request schemas use constructs that don't map cleanly to MCP (e.g., deeply nested `oneOf`)
- **Generate** an MCP tool manifest at compile time (embedded in `.bca`) so the data plane can serve `tools/list` without runtime schema computation
- **Validate** that `x-barbacane-mcp` annotations reference valid operations

### Relationship to AI Gateway (ADR-0024)

MCP and the AI proxy dispatcher serve **complementary but different roles**:

| | AI Proxy (ADR-0024) | MCP Server (this ADR) |
|---|---|---|
| **Direction** | Barbacane → LLM providers | AI agents → Barbacane → backends |
| **Protocol** | OpenAI-compatible HTTP | MCP (JSON-RPC 2.0) |
| **Use case** | "I want to proxy LLM calls" | "I want AI agents to call my APIs" |
| **Data flow** | Client → Barbacane → OpenAI/Anthropic | Agent → Barbacane → upstream services |

A single Barbacane instance can serve both roles simultaneously — MCP tools and `ai-proxy` routes are just different paths in the same spec with different dispatchers. An agent could discover both business APIs (`createOrder`) and LLM endpoints (`chatCompletions`) as MCP tools from one instance.

**Streaming trade-off:** When an agent calls an `ai-proxy` route via MCP, the response is a JSON-RPC result (not SSE). The full LLM response must be buffered before returning the MCP tool result, adding latency compared to direct SSE streaming. This is inherent to MCP's request/response model and acceptable for tool-use scenarios where the agent processes the complete result.

## Consequences

- **Easier:** Organizations can make their existing REST APIs AI-agent-accessible by adding annotations to their OpenAPI spec — no new services, no MCP server implementation. Spec-driven tool generation is a unique differentiator. The middleware pipeline provides auth, rate limiting, and observability for AI agent traffic with zero additional configuration.
- **Harder:** MCP is a stateful protocol (initialization handshake, capability negotiation) which adds complexity to the data plane. Schema mapping edge cases (path params, composition, multiple content types) require careful handling. The MCP spec is still evolving — the `2025-11-25` version may change.
- **Trade-offs:** Building MCP as a native feature (rather than a plugin) means it's part of the core binary, increasing the maintenance surface. However, this is justified by the deep integration with the route table and middleware pipeline that a plugin couldn't achieve.

## References

- [MCP Specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25)
- [MCP Architecture Overview](https://modelcontextprotocol.io/docs/learn/architecture)
- [KrakenD MCP Server](https://www.krakend.io/docs/enterprise/ai-gateway/mcp-server/)
- [Azure API Management MCP Export](https://learn.microsoft.com/en-us/azure/api-management/export-rest-mcp-server)
- [AWS API Gateway MCP Proxy Support](https://aws.amazon.com/about-aws/whats-new/2025/12/api-gateway-mcp-proxy-support/)

## Related ADRs

- [ADR-0004: Spec-Driven Configuration](0004-spec-driven-configuration.md)
- [ADR-0023: WASM Plugin Streaming Support](0023-wasm-plugin-streaming.md)
- [ADR-0024: AI Gateway Plugin](0024-ai-gateway-plugin.md)
