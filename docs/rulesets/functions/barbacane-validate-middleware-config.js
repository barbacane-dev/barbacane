// AUTO-GENERATED from plugins/*/config-schema.json — do not edit by hand.
// Regenerate: node docs/rulesets/generate.mjs

const schemas = {
  "acl": {
    required: [],
    properties: {
      allow: { type: "array" },
      deny: { type: "array" },
      allow_consumers: { type: "array" },
      deny_consumers: { type: "array" },
      consumer_groups: { type: "object" },
      message: { type: "string" },
      hide_consumer_in_errors: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "ai-cost-tracker": {
    required: ["prices"],
    properties: {
      prices: { type: "object" },
      warn_unknown_model: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "ai-prompt-guard": {
    required: ["default_profile","profiles"],
    properties: {
      context_key: { type: "string" },
      default_profile: { type: "string" },
      profiles: { type: "object" },
    },
    additionalProperties: false,
  },

  "ai-response-guard": {
    required: ["default_profile","profiles"],
    properties: {
      context_key: { type: "string" },
      default_profile: { type: "string" },
      profiles: { type: "object" },
    },
    additionalProperties: false,
  },

  "ai-token-limit": {
    required: ["default_profile","profiles"],
    properties: {
      context_key: { type: "string" },
      default_profile: { type: "string" },
      profiles: { type: "object" },
      policy_name: { type: "string" },
      partition_key: { type: "string" },
      count: { type: "string" },
    },
    additionalProperties: false,
  },

  "apikey-auth": {
    required: [],
    properties: {
      key_location: { type: "string" },
      header_name: { type: "string" },
      query_param: { type: "string" },
      keys: { type: "array" },
    },
    additionalProperties: false,
  },

  "basic-auth": {
    required: [],
    properties: {
      realm: { type: "string" },
      strip_credentials: { type: "boolean" },
      credentials: { type: "array" },
    },
    additionalProperties: false,
  },

  "bot-detection": {
    required: [],
    properties: {
      deny: { type: "array" },
      allow: { type: "array" },
      block_empty_ua: { type: "boolean" },
      message: { type: "string" },
      status: { type: "integer", minimum: 100, maximum: 599 },
    },
    additionalProperties: false,
  },

  "cache": {
    required: [],
    properties: {
      ttl: { type: "integer", minimum: 1, maximum: 86400 },
      vary: { type: "array" },
      methods: { type: "array" },
      cacheable_status: { type: "array" },
    },
    additionalProperties: false,
  },

  "cel": {
    required: ["expression"],
    properties: {
      expression: { type: "string" },
      deny_message: { type: "string" },
      on_match: { type: "object" },
    },
    additionalProperties: false,
  },

  "correlation-id": {
    required: [],
    properties: {
      header_name: { type: "string" },
      generate_if_missing: { type: "boolean" },
      trust_incoming: { type: "boolean" },
      include_in_response: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "cors": {
    required: ["allowed_origins"],
    properties: {
      allowed_origins: { type: "array" },
      allowed_methods: { type: "array" },
      allowed_headers: { type: "array" },
      expose_headers: { type: "array" },
      max_age: { type: "integer", minimum: 0 },
      allow_credentials: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "http-log": {
    required: ["endpoint"],
    properties: {
      endpoint: { type: "string" },
      method: { type: "string" },
      timeout_ms: { type: "integer", minimum: 100, maximum: 10000 },
      content_type: { type: "string" },
      include_headers: { type: "boolean" },
      include_body: { type: "boolean" },
      custom_fields: { type: "object" },
    },
    additionalProperties: false,
  },

  "ip-restriction": {
    required: [],
    properties: {
      allow: { type: "array" },
      deny: { type: "array" },
      message: { type: "string" },
      status: { type: "integer", minimum: 100, maximum: 599 },
    },
    additionalProperties: false,
  },

  "jwt-auth": {
    required: [],
    properties: {
      issuer: { type: "string" },
      audience: { type: "string" },
      clock_skew_seconds: { type: "integer", minimum: 0 },
      skip_signature_validation: { type: "boolean" },
      jwks_url: { type: "string" },
      groups_claim: { type: "string" },
      public_key_pem: { type: "string" },
    },
    additionalProperties: false,
  },

  "oauth2-auth": {
    required: ["introspection_endpoint","client_id","client_secret"],
    properties: {
      introspection_endpoint: { type: "string" },
      client_id: { type: "string" },
      client_secret: { type: "string" },
      required_scopes: { type: "string" },
      timeout: { type: "number", minimum: 0 },
    },
    additionalProperties: false,
  },

  "observability": {
    required: [],
    properties: {
      latency_slo_ms: { type: "integer", minimum: 1 },
      detailed_request_logs: { type: "boolean" },
      detailed_response_logs: { type: "boolean" },
      emit_latency_histogram: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "oidc-auth": {
    required: ["issuer_url"],
    properties: {
      issuer_url: { type: "string" },
      audience: { type: "string" },
      required_scopes: { type: "string" },
      issuer_override: { type: "string" },
      clock_skew_seconds: { type: "integer", minimum: 0 },
      jwks_refresh_seconds: { type: "integer", minimum: 10 },
      timeout: { type: "number", minimum: 0 },
      allow_query_token: { type: "boolean" },
      groups_claim: { type: "string" },
      groups_claim_separator: { type: "string" },
    },
    additionalProperties: false,
  },

  "opa-authz": {
    required: ["opa_url"],
    properties: {
      opa_url: { type: "string" },
      timeout: { type: "number", minimum: 0 },
      include_body: { type: "boolean" },
      include_claims: { type: "boolean" },
      deny_message: { type: "string" },
    },
    additionalProperties: false,
  },

  "rate-limit": {
    required: ["quota","window"],
    properties: {
      quota: { type: "integer", minimum: 1 },
      window: { type: "integer", minimum: 1 },
      policy_name: { type: "string" },
      partition_key: { type: "string" },
    },
    additionalProperties: false,
  },

  "redirect": {
    required: ["rules"],
    properties: {
      status_code: { type: "integer" },
      preserve_query: { type: "boolean" },
      rules: { type: "array" },
    },
    additionalProperties: false,
  },

  "request-size-limit": {
    required: [],
    properties: {
      max_bytes: { type: "integer", minimum: 0 },
      check_content_length: { type: "boolean" },
    },
    additionalProperties: false,
  },

  "request-transformer": {
    required: [],
    properties: {
      headers: { type: "object" },
      querystring: { type: "object" },
      path: { type: "object" },
      body: { type: "object" },
    },
    additionalProperties: false,
  },

  "response-transformer": {
    required: [],
    properties: {
      status: { type: "object" },
      headers: { type: "object" },
      body: { type: "object" },
    },
    additionalProperties: false,
  },
};

function getSchema() {
  return {
    name: "barbacane-validate-middleware-config",
    description:
      "Validates middleware config against the plugin's JSON Schema",
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
      message: `Middleware "${pluginName}" requires a config object with fields: ${schema.required.join(", ")}.`,
    }];
  }

  if (!config) return [];

  const results = [];
  for (const field of schema.required) {
    if (config[field] === undefined || config[field] === null) {
      results.push({
        message: `Middleware "${pluginName}" requires config field "${field}".`,
      });
    }
  }

  if (schema.additionalProperties === false && schema.properties) {
    const allowed = Object.keys(schema.properties);
    for (const key of Object.keys(config)) {
      if (!allowed.includes(key)) {
        results.push({
          message: `Unknown config field "${key}" for middleware "${pluginName}". Allowed fields: ${allowed.join(", ")}.`,
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
          message: `Config field "${key}" for middleware "${pluginName}" must be of type "${prop.type}", got "${typeof value}".`,
        });
        continue;
      }

      if (prop.minimum !== undefined && typeof value === "number" && value < prop.minimum) {
        results.push({
          message: `Config field "${key}" for middleware "${pluginName}" must be >= ${prop.minimum}.`,
        });
      }

      if (prop.maximum !== undefined && typeof value === "number" && value > prop.maximum) {
        results.push({
          message: `Config field "${key}" for middleware "${pluginName}" must be <= ${prop.maximum}.`,
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
