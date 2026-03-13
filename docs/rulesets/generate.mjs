#!/usr/bin/env node
// Generates vacuum validator JS files from plugins/*/config-schema.json.
// Usage: node docs/rulesets/generate.mjs

import { readFileSync, writeFileSync, readdirSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

function formatSchemas(schemas) {
  const entries = Object.entries(schemas).map(([name, s]) => {
    const props = Object.entries(s.properties)
      .map(([k, v]) => {
        const parts = [`type: "${v.type}"`];
        if (v.minimum !== undefined) parts.push(`minimum: ${v.minimum}`);
        if (v.maximum !== undefined) parts.push(`maximum: ${v.maximum}`);
        return `      ${k}: { ${parts.join(", ")} },`;
      })
      .join("\n");
    return [
      `  "${name}": {`,
      `    required: ${JSON.stringify(s.required)},`,
      `    properties: {`,
      props,
      `    },`,
      `    additionalProperties: ${s.additionalProperties},`,
      `  },`,
    ].join("\n");
  });
  return entries.join("\n\n");
}

function buildValidatorJS(kind, ruleName, description, schemas) {
  return `// AUTO-GENERATED from plugins/*/config-schema.json — do not edit by hand.
// Regenerate: node docs/rulesets/generate.mjs

const schemas = {
${formatSchemas(schemas)}
};

function getSchema() {
  return {
    name: "${ruleName}",
    description:
      "${description}",
  };
}

function runRule(input) {
  if (!input || typeof input !== "object") return [];

  const pluginName = input.name;
  const config = input.config;

  if (!pluginName) return [];

  const schema = schemas[pluginName];
  if (!schema) return []; // unknown plugin handled by enumeration rule

  if (!config && schema.required.length === 0) return [];
  if (!config && schema.required.length > 0) {
    return [{
      message: \`${kind} "\${pluginName}" requires a config object with fields: \${schema.required.join(", ")}.\`,
    }];
  }

  if (!config) return [];

  const results = [];
  for (const field of schema.required) {
    if (config[field] === undefined || config[field] === null) {
      results.push({
        message: \`${kind} "\${pluginName}" requires config field "\${field}".\`,
      });
    }
  }

  if (schema.additionalProperties === false && schema.properties) {
    const allowed = Object.keys(schema.properties);
    for (const key of Object.keys(config)) {
      if (!allowed.includes(key)) {
        results.push({
          message: \`Unknown config field "\${key}" for ${kind.toLowerCase()} "\${pluginName}". Allowed fields: \${allowed.join(", ")}.\`,
        });
      }
    }
  }

  if (schema.properties) {
    for (const [key, prop] of Object.entries(schema.properties)) {
      const value = config[key];
      if (value === undefined || value === null) continue;

      if (typeof value === "string" && (value.startsWith("env://") || value.startsWith("file://"))) {
        continue;
      }

      if (!checkType(value, prop.type)) {
        results.push({
          message: \`Config field "\${key}" for ${kind.toLowerCase()} "\${pluginName}" must be of type "\${prop.type}", got "\${typeof value}".\`,
        });
        continue;
      }

      if (prop.minimum !== undefined && typeof value === "number" && value < prop.minimum) {
        results.push({
          message: \`Config field "\${key}" for ${kind.toLowerCase()} "\${pluginName}" must be >= \${prop.minimum}.\`,
        });
      }

      if (prop.maximum !== undefined && typeof value === "number" && value > prop.maximum) {
        results.push({
          message: \`Config field "\${key}" for ${kind.toLowerCase()} "\${pluginName}" must be <= \${prop.maximum}.\`,
        });
      }
    }
  }

  return results;
}

function checkType(value, expectedType) {
  switch (expectedType) {
    case "string":
      return typeof value === "string";
    case "number":
      return typeof value === "number";
    case "integer":
      return typeof value === "number" && Number.isInteger(value);
    case "boolean":
      return typeof value === "boolean";
    case "object":
      return typeof value === "object" && !Array.isArray(value);
    case "array":
      return Array.isArray(value);
    default:
      return false;
  }
}
`;
}

function replaceEnum(yaml, rulePattern, names) {
  const enumBlock = names.map((n) => `          - ${n}`).join("\n");
  const re = new RegExp(
    `(${rulePattern}[\\s\\S]*?values:\\n)((?: {10}- .+\\n)+)`,
    "g"
  );
  return yaml.replace(re, `$1${enumBlock}\n`);
}

// ---------------------------------------------------------------------------
// Scan plugins
// ---------------------------------------------------------------------------

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "../..");
const PLUGINS = join(ROOT, "plugins");
const FUNCTIONS = join(dirname(fileURLToPath(import.meta.url)), "functions");

const dispatchers = {};
const middlewares = {};

for (const name of readdirSync(PLUGINS).sort()) {
  const dir = join(PLUGINS, name);
  let toml, schema;
  try {
    toml = readFileSync(join(dir, "plugin.toml"), "utf8");
    schema = JSON.parse(readFileSync(join(dir, "config-schema.json"), "utf8"));
  } catch { continue; }

  const type = toml.match(/^type\s*=\s*"(\w+)"/m)?.[1];
  if (!type) continue;

  const simplified = {
    required: schema.required || [],
    properties: {},
    additionalProperties: schema.additionalProperties !== false,
  };
  for (const [k, v] of Object.entries(schema.properties || {})) {
    const prop = { type: v.type };
    if (v.minimum !== undefined) prop.minimum = v.minimum;
    if (v.maximum !== undefined) prop.maximum = v.maximum;
    simplified.properties[k] = prop;
  }

  if (type === "dispatcher") dispatchers[name] = simplified;
  else if (type === "middleware") middlewares[name] = simplified;
}

// ---------------------------------------------------------------------------
// Generate validator JS files
// ---------------------------------------------------------------------------

const dispatchJS = buildValidatorJS(
  "Dispatcher",
  "barbacane-validate-dispatch-config",
  "Validates x-barbacane-dispatch.config against the dispatcher plugin schema",
  dispatchers,
);

const middlewareJS = buildValidatorJS(
  "Middleware",
  "barbacane-validate-middleware-config",
  "Validates middleware config against the plugin's JSON Schema",
  middlewares,
);

writeFileSync(join(FUNCTIONS, "barbacane-validate-dispatch-config.js"), dispatchJS);
writeFileSync(join(FUNCTIONS, "barbacane-validate-middleware-config.js"), middlewareJS);

// ---------------------------------------------------------------------------
// Update barbacane.yaml enums
// ---------------------------------------------------------------------------

let ruleset = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "barbacane.yaml"), "utf8");

const dispatchNames = Object.keys(dispatchers);
const middlewareNames = Object.keys(middlewares);

const dispatchList = dispatchNames.join(", ");
ruleset = ruleset.replace(
  /Dispatcher plugin must be one of: [^"]+/,
  `Dispatcher plugin must be one of: ${dispatchList}.`
);

ruleset = replaceEnum(ruleset, "barbacane-dispatch-known-plugin:", dispatchNames);
ruleset = replaceEnum(ruleset, "barbacane-middleware-known-plugin:", middlewareNames);
ruleset = replaceEnum(ruleset, "barbacane-op-middleware-known-plugin:", middlewareNames);

writeFileSync(join(dirname(fileURLToPath(import.meta.url)), "barbacane.yaml"), ruleset);

console.log(`Generated dispatch validator (${dispatchNames.length} dispatchers: ${dispatchList})`);
console.log(`Generated middleware validator (${middlewareNames.length} middlewares: ${middlewareNames.join(", ")})`);
console.log("Updated barbacane.yaml enums");
