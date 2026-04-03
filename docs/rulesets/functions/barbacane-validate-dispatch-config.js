// AUTO-GENERATED from plugins/*/config-schema.json — do not edit by hand.
// Regenerate: node docs/rulesets/generate.mjs

const schemas = {
  "ai-proxy": {
    required: [],
    properties: {
      provider: { type: "string" },
      model: { type: "string" },
      api_key: { type: "string" },
      base_url: { type: "string" },
      timeout: { type: "integer", minimum: 1 },
      max_tokens: { type: "integer", minimum: 1 },
      fallback: { type: "array" },
      targets: { type: "object" },
      default_target: { type: "string" },
    },
    additionalProperties: false,
  },

  "fire-and-forget": {
    required: ["url"],
    properties: {
      url: { type: "string" },
      timeout_ms: { type: "integer", minimum: 1 },
      response: { type: "object" },
    },
    additionalProperties: false,
  },

  "http-upstream": {
    required: ["url"],
    properties: {
      url: { type: "string" },
      path: { type: "string" },
      timeout: { type: "number", minimum: 0 },
    },
    additionalProperties: false,
  },

  "kafka": {
    required: ["brokers","topic"],
    properties: {
      brokers: { type: "string" },
      topic: { type: "string" },
      key: { type: "string" },
      ack_response: { type: "object" },
      include_metadata: { type: "boolean" },
      headers_from_request: { type: "array" },
    },
    additionalProperties: false,
  },

  "lambda": {
    required: ["url"],
    properties: {
      url: { type: "string" },
      timeout: { type: "number", minimum: 1, maximum: 900 },
      pass_through_headers: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "mock": {
    required: [],
    properties: {
      status: { type: "integer", minimum: 100, maximum: 599 },
      body: { type: "string" },
      headers: { type: "object" },
      content_type: { type: "string" },
    },
    additionalProperties: false,
  },

  "nats": {
    required: ["url","subject"],
    properties: {
      url: { type: "string" },
      subject: { type: "string" },
      ack_response: { type: "object" },
      headers_from_request: { type: "array" },
    },
    additionalProperties: false,
  },

  "s3": {
    required: ["access_key_id","secret_access_key","region"],
    properties: {
      access_key_id: { type: "string" },
      secret_access_key: { type: "string" },
      session_token: { type: "string" },
      region: { type: "string" },
      endpoint: { type: "string" },
      force_path_style: { type: "boolean" },
      bucket: { type: "string" },
      bucket_param: { type: "string" },
      key_param: { type: "string" },
      fallback_key: { type: "string" },
      timeout: { type: "number", minimum: 0 },
    },
    additionalProperties: false,
  },

  "ws-upstream": {
    required: ["url"],
    properties: {
      url: { type: "string" },
      connect_timeout: { type: "number", minimum: 0.1, maximum: 300 },
      path: { type: "string" },
    },
    additionalProperties: false,
  },
};

function getSchema() {
  return {
    name: "barbacane-validate-dispatch-config",
    description:
      "Validates x-barbacane-dispatch.config against the dispatcher plugin schema",
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
      message: `Dispatcher "${pluginName}" requires a config object with fields: ${schema.required.join(", ")}.`,
    }];
  }

  if (!config) return [];

  const results = [];
  for (const field of schema.required) {
    if (config[field] === undefined || config[field] === null) {
      results.push({
        message: `Dispatcher "${pluginName}" requires config field "${field}".`,
      });
    }
  }

  if (schema.additionalProperties === false && schema.properties) {
    const allowed = Object.keys(schema.properties);
    for (const key of Object.keys(config)) {
      if (!allowed.includes(key)) {
        results.push({
          message: `Unknown config field "${key}" for dispatcher "${pluginName}". Allowed fields: ${allowed.join(", ")}.`,
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
          message: `Config field "${key}" for dispatcher "${pluginName}" must be of type "${prop.type}", got "${typeof value}".`,
        });
        continue;
      }

      if (prop.minimum !== undefined && typeof value === "number" && value < prop.minimum) {
        results.push({
          message: `Config field "${key}" for dispatcher "${pluginName}" must be >= ${prop.minimum}.`,
        });
      }

      if (prop.maximum !== undefined && typeof value === "number" && value > prop.maximum) {
        results.push({
          message: `Config field "${key}" for dispatcher "${pluginName}" must be <= ${prop.maximum}.`,
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
