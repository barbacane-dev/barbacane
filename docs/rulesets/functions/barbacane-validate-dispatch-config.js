// Validates x-barbacane-dispatch.config against the plugin's JSON Schema.
//
// Each dispatcher plugin ships a config-schema.json that defines required
// fields, types, and constraints.  This function loads the matching schema
// by plugin name and checks the config object against it.

const schemas = {
  mock: {
    type: "object",
    required: [],
    properties: {
      status: { type: "integer", minimum: 100, maximum: 599 },
      body: { type: "string" },
      headers: { type: "object" },
      content_type: { type: "string" },
    },
    additionalProperties: false,
  },
  "http-upstream": {
    type: "object",
    required: ["url"],
    properties: {
      url: { type: "string" },
      path: { type: "string" },
      timeout: { type: "number", minimum: 0 },
    },
    additionalProperties: false,
  },
  kafka: {
    type: "object",
    required: ["brokers", "topic"],
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
  nats: {
    type: "object",
    required: ["url", "subject"],
    properties: {
      url: { type: "string" },
      subject: { type: "string" },
      ack_response: { type: "object" },
      headers_from_request: { type: "array" },
    },
    additionalProperties: false,
  },
  s3: {
    type: "object",
    required: ["access_key_id", "secret_access_key", "region"],
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
      timeout: { type: "number", minimum: 0 },
    },
    additionalProperties: false,
  },
  lambda: {
    type: "object",
    required: ["url"],
    properties: {
      url: { type: "string" },
      timeout: { type: "number", minimum: 1, maximum: 900 },
      pass_through_headers: { type: "boolean" },
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
  const results = [];
  if (!input || typeof input !== "object") return results;

  const pluginName = input.name;
  const config = input.config;

  if (!pluginName || !config) return results;

  const schema = schemas[pluginName];
  if (!schema) return results; // unknown plugin handled by enumeration rule

  // Check required fields
  if (schema.required) {
    for (const field of schema.required) {
      if (config[field] === undefined || config[field] === null) {
        results.push({
          message: `Dispatcher "${pluginName}" requires config field "${field}".`,
        });
      }
    }
  }

  // Check for unknown fields
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

  // Check field types
  if (schema.properties) {
    for (const [key, prop] of Object.entries(schema.properties)) {
      const value = config[key];
      if (value === undefined || value === null) continue;

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
      return true;
  }
}
