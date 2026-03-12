// Returns a result when http-upstream dispatcher uses plaintext HTTP instead of HTTPS.
// Severity is defined in the ruleset that references this function.

function getSchema() {
  return {
    name: "barbacane-no-plaintext-upstream",
    description: "Warns when http-upstream uses http:// instead of https://",
  };
}

function runRule(input) {
  if (!input || typeof input !== "object") return [];
  if (input.name !== "http-upstream") return [];
  if (!input.config || !input.config.url) return [];

  const url = input.config.url;

  // Skip secret references
  if (typeof url === "string" && (url.startsWith("env://") || url.startsWith("file://"))) {
    return [];
  }

  if (typeof url === "string" && url.startsWith("http://")) {
    return [{ message: `Upstream URL "${url}" uses plaintext HTTP. Use HTTPS for production.` }];
  }

  return [];
}
