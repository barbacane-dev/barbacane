// Flags x-barbacane-* keys that are not recognized by Barbacane.
// Only x-barbacane-dispatch and x-barbacane-middlewares are valid.

const KNOWN = new Set(["x-barbacane-dispatch", "x-barbacane-middlewares", "x-barbacane-mcp"]);

function getSchema() {
  return {
    name: "barbacane-no-unknown-extensions",
    description: "Detects unknown x-barbacane-* extension keys",
  };
}

function runRule(input) {
  const results = [];
  if (!input || typeof input !== "object") return results;

  collectUnknown(input, "$", results);
  return results;
}

function collectUnknown(obj, path, results) {
  if (!obj || typeof obj !== "object") return;

  for (const [key, value] of Object.entries(obj)) {
    if (key.startsWith("x-barbacane-") && !KNOWN.has(key)) {
      results.push({
        message: `Unknown Barbacane extension "${key}" at ${path}. Only x-barbacane-dispatch, x-barbacane-middlewares, and x-barbacane-mcp are recognized.`,
      });
    }

    // Recurse into objects (paths, operations, etc.)
    if (value && typeof value === "object" && !Array.isArray(value)) {
      collectUnknown(value, `${path}.${key}`, results);
    }
  }
}
