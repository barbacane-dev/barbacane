// Validates that path parameters are correctly defined, with support for
// Barbacane's {paramName+} wildcard catch-all syntax.
//
// Replaces Vacuum's built-in "path-params" rule which rejects the + suffix.
//
// Checks:
// 1. Every {paramName} or {paramName+} in the path has a matching parameter
//    definition (name = paramName, in = path).
// 2. Every parameter with in=path has a corresponding {paramName} or
//    {paramName+} template in the path.

function getSchema() {
  return {
    name: "barbacane-valid-path-params",
    description:
      "Validates path parameters, supporting Barbacane's {param+} wildcard syntax",
  };
}

// Given: $.paths
function runRule(input) {
  if (!input || typeof input !== "object") return [];

  const results = [];

  for (const [pathTemplate, pathItem] of Object.entries(input)) {
    if (!pathItem || typeof pathItem !== "object") continue;

    // Extract parameter names from the path template, stripping the + suffix.
    // e.g. "/storage/{bucket}/{key+}" -> ["bucket", "key"]
    const templateParams = [];
    const paramRegex = /\{([^}]+)\}/g;
    let match;
    while ((match = paramRegex.exec(pathTemplate)) !== null) {
      const raw = match[1];
      const name = raw.endsWith("+") ? raw.slice(0, -1) : raw;
      templateParams.push(name);
    }

    const methods = [
      "get",
      "put",
      "post",
      "delete",
      "options",
      "head",
      "patch",
      "trace",
    ];
    for (const method of methods) {
      const operation = pathItem[method];
      if (!operation || typeof operation !== "object") continue;

      // Collect path-level + operation-level parameters with in=path
      const pathLevelParams = Array.isArray(pathItem.parameters)
        ? pathItem.parameters
        : [];
      const opLevelParams = Array.isArray(operation.parameters)
        ? operation.parameters
        : [];

      // Operation params override path-level params by name
      const definedParams = new Map();
      for (const p of pathLevelParams) {
        if (p && p.in === "path" && p.name) {
          definedParams.set(p.name, p);
        }
      }
      for (const p of opLevelParams) {
        if (p && p.in === "path" && p.name) {
          definedParams.set(p.name, p);
        }
      }

      // Check 1: every template param has a definition
      for (const tParam of templateParams) {
        if (!definedParams.has(tParam)) {
          results.push({
            message: `${method.toUpperCase()} ${pathTemplate}: path template references "{${tParam}}" but no parameter with name "${tParam}" and in="path" is defined.`,
          });
        }
      }

      // Check 2: every defined path param appears in the template
      for (const [name] of definedParams) {
        if (!templateParams.includes(name)) {
          results.push({
            message: `${method.toUpperCase()} ${pathTemplate}: parameter "${name}" is defined with in="path" but does not appear in the path template.`,
          });
        }
      }
    }
  }

  return results;
}
