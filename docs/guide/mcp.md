# MCP Server

Barbacane can act as an MCP (Model Context Protocol) server, enabling AI agents to discover and call your API operations as tools via JSON-RPC 2.0. Define your API in OpenAPI, get an MCP server for free.

## Overview

MCP is an open standard for AI agent tool integration. When enabled, Barbacane automatically generates MCP tools from your OpenAPI operations:

| OpenAPI Concept | MCP Tool Field | Mapping |
|----------------|---------------|---------|
| `operationId` | `name` | Direct |
| `summary` / `description` | `description` | First available |
| Path params + query params + request body schema | `inputSchema` | Merged into single JSON Schema |
| Response `200` schema | `outputSchema` | Direct |

Tool calls route through the full middleware pipeline (auth, rate limiting, validation) before reaching the dispatcher — identical security to regular HTTP requests.

## Enabling MCP

Add `x-barbacane-mcp` at the root level of your OpenAPI spec:

```yaml
openapi: "3.1.0"
info:
  title: Order API
  version: "1.0.0"

x-barbacane-mcp:
  enabled: true

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
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://backend.internal"
```

This exposes a `createOrder` MCP tool at `POST /__barbacane/mcp`.

## Requirements

When MCP is enabled for an operation, these fields are required:

| Field | Requirement | Used For |
|-------|-------------|----------|
| `operationId` | **Mandatory** | MCP tool name |
| `summary` or `description` | **At least one** | MCP tool description |

The compiler emits warnings (E1060, E1061) if these are missing. The vacuum ruleset enforces them as errors when linting.

## Configuration

### Root Level

```yaml
x-barbacane-mcp:
  enabled: true                   # Enable MCP for all operations
  server_name: "My API"          # Optional (defaults to info.title)
  server_version: "1.0.0"        # Optional (defaults to info.version)
```

### Per-Operation Override

Opt out specific operations:

```yaml
paths:
  /admin/reset:
    post:
      operationId: resetDatabase
      summary: Reset the database
      x-barbacane-mcp:
        enabled: false
```

Override the tool description:

```yaml
paths:
  /orders:
    post:
      operationId: createOrder
      summary: Create order
      x-barbacane-mcp:
        description: "Create a new customer order with items, quantities, and shipping address"
```

## How Tool Calls Work

When an AI agent calls `tools/call { name: "createOrder", arguments: { items: [...] } }`:

1. The MCP handler maps the tool name to the compiled operation
2. Arguments are decomposed into HTTP request components:
   - Path parameters → substituted into the URL template
   - Query parameters → appended as query string
   - Remaining arguments → JSON request body
3. Auth headers from the MCP HTTP request are forwarded
4. The request routes through the middleware + dispatcher pipeline
5. The HTTP response is wrapped as an MCP tool result

## Authentication

MCP tool calls reuse your existing auth middleware. The AI agent includes credentials in the MCP HTTP request headers:

```bash
curl -X POST http://localhost:8080/__barbacane/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <agent-token>" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"createOrder","arguments":{...}}}'
```

The `Authorization` header is forwarded to the internal dispatch, where your `jwt-auth`, `apikey-auth`, or other auth middleware validates it.

## Schema Mapping

### Input Schema

The tool's `inputSchema` is built by merging:

1. **Path parameters** — from `parameters` where `in: path`
2. **Query parameters** — from `parameters` where `in: query`
3. **Body properties** — from `requestBody.content.application/json.schema.properties`

All are merged into a single flat JSON Schema object. Required fields from each source are combined.

### Output Schema

The `outputSchema` is extracted from the `responses.200.content.application/json.schema` (or the first 2xx response). If no response schema is defined, `outputSchema` is omitted.

## Endpoint Reference

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/__barbacane/mcp` | JSON-RPC 2.0 requests |
| DELETE | `/__barbacane/mcp` | Session termination |

See [Reserved Endpoints](../reference/endpoints.md#mcp-server) for details.

## Linting

The Barbacane vacuum ruleset validates MCP configuration:

```bash
vacuum lint -r docs/rulesets/barbacane.yaml your-spec.yaml
```

The `barbacane-mcp-requires-operation-id` rule enforces that MCP-enabled operations have `operationId` and `summary`/`description`.
