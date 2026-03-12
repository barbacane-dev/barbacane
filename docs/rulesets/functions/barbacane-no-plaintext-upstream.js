// Returns a result when upstream dispatchers use plaintext protocols (HTTP, WS)
// instead of their secure counterparts (HTTPS, WSS).
// Severity is defined in the ruleset that references this function.

function getSchema() {
  return {
    name: "barbacane-no-plaintext-upstream",
    description: "Warns when upstream dispatchers use plaintext protocols instead of HTTPS/WSS",
  };
}

function runRule(input) {
  if (!input || typeof input !== "object") return [];
  if (!input.config || !input.config.url) return [];

  const url = input.config.url;

  // Skip secret references
  if (typeof url === "string" && (url.startsWith("env://") || url.startsWith("file://"))) {
    return [];
  }

  if (input.name === "http-upstream" && typeof url === "string" && url.startsWith("http://")) {
    return [{ message: `Upstream URL "${url}" uses plaintext HTTP. Use HTTPS for production.` }];
  }

  if (input.name === "ws-upstream" && typeof url === "string" && url.startsWith("ws://")) {
    return [{ message: `Upstream URL "${url}" uses plaintext WebSocket. Use WSS for production.` }];
  }

  return [];
}
