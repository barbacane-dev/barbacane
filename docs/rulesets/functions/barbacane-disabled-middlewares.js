// Checks `enabled: false` on x-barbacane-middlewares entries (ADR-0034, SPEC-001 §3.1).
//
// Each rule selects one check through `functionOptions.check`:
//   global-disabled       a global entry sets enabled: false (E1014); input: a global entry
//   disabled-with-config  an entry sets enabled: false and a config (E1012); input: any entry
//   disabled-unmatched    an operation entry disables a plugin no global entry has (E1013)
//   global-auth-disabled  an operation disables a global authentication plugin
//   auth-plugins-stacked  an operation adds an authentication plugin and keeps a
//                         different global one, so a request needs both credentials
// The last three take an operation and read the global chain from the document root.
// vacuum calls runRule with the input only; the options and the document come from
// its global `context` (`context.options`, `context.specInfo.getSpecJSON()`).
// Entries match by plugin name without the `@version` suffix, as the compiler does.

// BEGIN GENERATED AUTH PLUGINS
const AUTH_PLUGINS = ["apikey-auth","basic-auth","jwt-auth","ldap-auth","oauth2-auth","oidc-auth"];
// END GENERATED AUTH PLUGINS

function getSchema() {
  return {
    name: "barbacane-disabled-middlewares",
    description: "Checks enabled: false on x-barbacane-middlewares entries",
  };
}

function pluginKey(name) {
  return String(name).split("@")[0];
}

function entries(list) {
  return Array.isArray(list) ? list.filter((m) => m && typeof m === "object" && m.name) : [];
}

function globalEntries() {
  const root = context.specInfo.getSpecJSON();
  return entries(root && root["x-barbacane-middlewares"]);
}

function runRule(input) {
  if (!input || typeof input !== "object") return [];
  const check = context.options && context.options.check;

  if (check === "global-disabled") {
    return input.enabled === false
      ? [{ message: `Global middleware "${input.name}" sets enabled: false; only an operation entry can disable a global middleware (E1014).` }]
      : [];
  }

  if (check === "disabled-with-config") {
    return input.enabled === false && input.config !== undefined && input.config !== null
      ? [{ message: `Middleware "${input.name}" sets enabled: false and a config that would never be used (E1012).` }]
      : [];
  }

  const operationEntries = entries(input["x-barbacane-middlewares"]);
  if (operationEntries.length === 0) return [];
  const globals = globalEntries();
  const globalKeys = new Set(globals.map((m) => pluginKey(m.name)));
  const disabled = operationEntries.filter((m) => m.enabled === false);

  if (check === "disabled-unmatched") {
    return disabled
      .filter((m) => !globalKeys.has(pluginKey(m.name)))
      .map((m) => ({
        message: `Middleware "${m.name}" sets enabled: false but no global middleware entry matches it, so it removes nothing (E1013).`,
      }));
  }

  if (check === "global-auth-disabled") {
    return disabled
      .filter((m) => AUTH_PLUGINS.includes(pluginKey(m.name)) && globalKeys.has(pluginKey(m.name)))
      .map((m) => ({
        message: `This operation disables the global authentication plugin "${pluginKey(m.name)}".`,
      }));
  }

  if (check === "auth-plugins-stacked") {
    const operationKeys = new Set(operationEntries.map((m) => pluginKey(m.name)));
    const keptGlobalAuth = [...globalKeys].filter(
      (k) => AUTH_PLUGINS.includes(k) && !operationKeys.has(k),
    );
    const addedAuth = [
      ...new Set(
        operationEntries
          .filter((m) => m.enabled !== false)
          .map((m) => pluginKey(m.name))
          .filter((k) => AUTH_PLUGINS.includes(k) && !globalKeys.has(k)),
      ),
    ];
    if (keptGlobalAuth.length === 0 || addedAuth.length === 0) return [];
    return [
      {
        message: `This operation runs "${addedAuth.join('", "')}" and the global "${keptGlobalAuth.join('", "')}", so a request needs both credentials. Add an entry with enabled: false for the global plugin if the operation's replaces it.`,
      },
    ];
  }

  return [];
}
