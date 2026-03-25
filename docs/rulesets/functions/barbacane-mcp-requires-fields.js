// When MCP is enabled (globally or per-operation), validates that required
// fields are present: operationId (mandatory) and summary or description
// (at least one required).
//
// This function is called with each operation object as input.
// It checks the root-level x-barbacane-mcp via context and the
// operation-level x-barbacane-mcp override.

function getSchema() {
  return {
    name: "barbacane-mcp-requires-fields",
    description:
      "Validates that MCP-enabled operations have operationId and summary/description",
  };
}

function runRule(input, opts, context) {
  const results = [];
  if (!input || typeof input !== "object") return results;

  // Determine if MCP is enabled for this operation
  const opMcp = input["x-barbacane-mcp"];
  const opEnabled = opMcp && typeof opMcp === "object" ? opMcp.enabled : undefined;

  // Check root-level MCP config via the full document context
  let rootEnabled = false;
  if (context && context.document && context.document.data) {
    const rootMcp = context.document.data["x-barbacane-mcp"];
    if (rootMcp && typeof rootMcp === "object" && rootMcp.enabled === true) {
      rootEnabled = true;
    }
  }

  // Resolve: operation-level overrides root-level
  let mcpEnabled;
  if (opEnabled !== undefined) {
    mcpEnabled = opEnabled;
  } else {
    mcpEnabled = rootEnabled;
  }

  if (!mcpEnabled) return results;

  // MCP is enabled — check required fields
  if (!input.operationId) {
    results.push({
      message:
        "MCP-enabled operation must have an operationId (used as the MCP tool name).",
    });
  }

  if (!input.summary && !input.description) {
    results.push({
      message:
        "MCP-enabled operation must have a summary or description (used as the MCP tool description).",
    });
  }

  return results;
}
