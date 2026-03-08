// Warns when http-upstream dispatcher uses plaintext HTTP instead of HTTPS.

function getSchema() {
  return {
    name: "barbacane-no-plaintext-upstream",
    description: "Warns when http-upstream uses http:// instead of https://",
  };
}

function runRule(input) {
  const results = [];
  if (!input || typeof input !== "object") return results;

  if (input.name !== "http-upstream") return results;
  if (!input.config || !input.config.url) return results;

  const url = input.config.url;

  // Skip secret references
  if (typeof url === "string" && (url.startsWith("env://") || url.startsWith("file://"))) {
    return results;
  }

  if (typeof url === "string" && url.startsWith("http://")) {
    results.push({
      message: `Upstream URL "${url}" uses plaintext HTTP. Use HTTPS for production.`,
    });
  }

  return results;
}
