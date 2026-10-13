// Not referenced by barbacane.yaml: it reports nothing.
//
// Operation middlewares are merged with the global ones, so an operation keeps
// the global auth unless it sets `x-barbacane-middlewares: []`. This file stays
// published because download scripts that list it fetch it with `curl -f`.

function getSchema() {
  return {
    name: "barbacane-auth-opt-out",
    description: "Reports nothing; kept for download scripts that fetch it",
  };
}

function runRule() {
  return [];
}
