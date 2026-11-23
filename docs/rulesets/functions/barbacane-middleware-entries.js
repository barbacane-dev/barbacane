// Returns a result for each way an object's x-barbacane-middlewares differs
// from what the compiler accepts (E1011): a value that is not a list, an entry
// that is not an object, an unknown key, or a field of the wrong type.
// Severity is defined in the ruleset that references this function.

const KEYS = ["name", "config", "enabled"];

function getSchema() {
  return {
    name: "barbacane-middleware-entries",
    description: "x-barbacane-middlewares must be a list of entries setting only name, config and enabled",
  };
}

function runRule(input) {
  if (!input || typeof input !== "object" || Array.isArray(input)) return [];
  if (!Object.prototype.hasOwnProperty.call(input, "x-barbacane-middlewares")) return [];

  const list = input["x-barbacane-middlewares"];
  if (!Array.isArray(list)) {
    return [{ message: "x-barbacane-middlewares must be a list (E1011)." }];
  }

  const results = [];
  list.forEach((entry, i) => {
    const at = `Entry #${i + 1} of x-barbacane-middlewares`;
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      results.push({ message: `${at} must be an object (E1011).` });
      return;
    }
    for (const key of Object.keys(entry)) {
      if (!KEYS.includes(key)) {
        results.push({
          message: `${at} sets unknown key "${key}"; expected name, config, enabled (E1011).`,
        });
      }
    }
    if (typeof entry.name !== "string") {
      results.push({ message: `${at} needs a string name (E1011).` });
    }
    if ("enabled" in entry && typeof entry.enabled !== "boolean") {
      results.push({ message: `${at} sets enabled to something other than true or false (E1011).` });
    }
  });
  return results;
}
