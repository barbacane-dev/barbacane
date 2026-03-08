// Detects duplicate middleware names in a middleware chain.

function getSchema() {
  return {
    name: "barbacane-no-duplicate-middlewares",
    description: "Checks for duplicate middleware names in a chain",
  };
}

function runRule(input) {
  const results = [];
  if (!Array.isArray(input)) return results;

  const seen = new Set();
  for (const entry of input) {
    if (!entry || !entry.name) continue;
    if (seen.has(entry.name)) {
      results.push({
        message: `Duplicate middleware "${entry.name}" in chain. Each middleware should appear at most once.`,
      });
    }
    seen.add(entry.name);
  }

  return results;
}
