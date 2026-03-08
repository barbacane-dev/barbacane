// Checks that when global auth middleware is configured, operations that don't
// use it explicitly opt out with x-barbacane-middlewares: [].
//
// This prevents accidentally leaving endpoints unprotected when global auth
// is intended.

const AUTH_PLUGINS = new Set([
  "jwt-auth",
  "apikey-auth",
  "basic-auth",
  "oauth2-auth",
  "oidc-auth",
]);

const HTTP_METHODS = new Set([
  "get", "put", "post", "delete", "patch", "options", "head", "trace",
]);

function getSchema() {
  return {
    name: "barbacane-auth-opt-out",
    description:
      "Checks that operations explicitly opt out of global auth middleware",
  };
}

function runRule(input) {
  const results = [];
  if (!input || typeof input !== "object") return results;

  // Check if global middlewares include an auth plugin
  const globalMiddlewares = input["x-barbacane-middlewares"];
  if (!Array.isArray(globalMiddlewares)) return results;

  const hasGlobalAuth = globalMiddlewares.some(
    (m) => m && AUTH_PLUGINS.has(m.name)
  );
  if (!hasGlobalAuth) return results;

  // Scan all operations
  const paths = input.paths;
  if (!paths || typeof paths !== "object") return results;

  for (const [path, pathItem] of Object.entries(paths)) {
    if (!pathItem || typeof pathItem !== "object") continue;

    for (const [method, operation] of Object.entries(pathItem)) {
      if (!HTTP_METHODS.has(method)) continue;
      if (!operation || typeof operation !== "object") continue;

      // If the operation has no x-barbacane-middlewares, it inherits global
      // (which includes auth) — that's fine.
      if (!operation.hasOwnProperty("x-barbacane-middlewares")) continue;

      const opMiddlewares = operation["x-barbacane-middlewares"];

      // Empty array is an explicit opt-out — that's fine.
      if (Array.isArray(opMiddlewares) && opMiddlewares.length === 0) continue;

      // If the operation overrides middlewares, check if auth is still present.
      if (Array.isArray(opMiddlewares)) {
        const hasOpAuth = opMiddlewares.some(
          (m) => m && AUTH_PLUGINS.has(m.name)
        );
        if (!hasOpAuth) {
          results.push({
            message: `Operation ${method.toUpperCase()} ${path} overrides global middlewares but does not include auth. If intentional, use x-barbacane-middlewares: [] to explicitly opt out.`,
          });
        }
      }
    }
  }

  return results;
}
