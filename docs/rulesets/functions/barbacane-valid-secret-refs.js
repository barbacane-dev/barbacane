// Validates that secret references use the correct env:// or file:// format.

const SECRET_PREFIXES = ["env://", "file://"];
const ENV_REF_RE = /^env:\/\/[A-Za-z_][A-Za-z0-9_]*$/;
const FILE_REF_RE = /^file:\/\/\/.+$/;

function getSchema() {
  return {
    name: "barbacane-valid-secret-refs",
    description: "Validates env:// and file:// secret reference format",
  };
}

function runRule(input) {
  if (!input || typeof input !== "object") return [];

  const results = [];
  checkValues(input, results);
  return results;
}

function checkValues(obj, results) {
  if (!obj || typeof obj !== "object") return;

  for (const [key, value] of Object.entries(obj)) {
    if (typeof value === "string") {
      const isSecretRef = SECRET_PREFIXES.some((p) => value.startsWith(p));
      if (!isSecretRef) continue;

      if (value.startsWith("env://")) {
        if (!ENV_REF_RE.test(value)) {
          results.push({
            message: `Invalid secret reference "${value}" in field "${key}". env:// must be followed by a valid environment variable name (e.g., env://MY_SECRET).`,
          });
        }
      } else {
        // file:// — isSecretRef guarantees this branch
        if (!FILE_REF_RE.test(value)) {
          results.push({
            message: `Invalid secret reference "${value}" in field "${key}". file:// must use an absolute path (e.g., file:///run/secrets/my-secret).`,
          });
        }
      }
    } else if (typeof value === "object") {
      checkValues(value, results);
    }
  }
}
