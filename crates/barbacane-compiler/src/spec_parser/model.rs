use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A parsed API spec (OpenAPI or AsyncAPI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSpec {
    /// Original filename (if parsed from file).
    pub filename: Option<String>,
    /// The document's `$defs` pool: every resolved definition, held once and
    /// pointed at by every schema that reaches it. `None` when no schema in the
    /// document references anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_defs: Option<serde_json::Value>,
    /// The format detected from the root field.
    pub format: SpecFormat,
    /// The spec version string (e.g. "3.1.0").
    pub version: String,
    /// The `info.title` field.
    pub title: String,
    /// The `info.version` field (API version, not spec version).
    pub api_version: String,
    /// Parsed path operations.
    pub operations: Vec<Operation>,
    /// Global middlewares from root-level `x-barbacane-middlewares`.
    pub global_middlewares: Vec<MiddlewareConfig>,
    /// Raw `x-barbacane-*` extensions at root level.
    pub extensions: BTreeMap<String, serde_json::Value>,
    /// `components.securitySchemes`, keyed by scheme name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub security_schemes: BTreeMap<String, SecurityScheme>,
    /// Root-level `security`. `None` when the key is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<SecurityRequirement>>,
}

/// One entry of a `security` list: scheme name to the scopes it requires.
///
/// An entry holding several schemes requires all of them.
pub type SecurityRequirement = BTreeMap<String, Vec<String>>;

/// A security scheme declared in `components.securitySchemes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SecurityScheme {
    /// The credential travels in a named header, query parameter or cookie.
    #[serde(rename = "apiKey")]
    ApiKey {
        /// Name of the header, query parameter or cookie carrying the key.
        name: String,
        /// The `in` value: "header", "query" or "cookie".
        location: String,
    },
    /// RFC 7235 authentication, carried in `Authorization`.
    #[serde(rename = "http")]
    Http {
        /// The `scheme` value, lowercased ("basic", "bearer", ...).
        scheme: String,
    },
    /// OAuth 2, carried in `Authorization`.
    #[serde(rename = "oauth2")]
    OAuth2,
    /// OpenID Connect Discovery, carried in `Authorization`.
    #[serde(rename = "openIdConnect")]
    OpenIdConnect,
    /// Client certificate authentication, which carries no request header.
    #[serde(rename = "mutualTLS")]
    MutualTls,
    /// A credential the transport or broker carries rather than the request.
    ///
    /// AsyncAPI defines more scheme types than OpenAPI, and most of them are
    /// of this kind: `X509` and the encryption schemes belong to the
    /// transport, `scramSha256`, `scramSha512`, `gssapi`, `plain` and
    /// `userPassword` are broker mechanisms. None puts anything in a request
    /// header, so none contributes to the allowlist. The declared type is kept
    /// for diagnostics.
    #[serde(rename = "transport")]
    Transport { kind: String },
}

impl ApiSpec {
    /// A schema with the document's definition pool attached.
    ///
    /// Definitions are held once per document and pointed at, so a schema on
    /// its own carries `#/$defs/...` pointers and nothing to resolve them. This
    /// puts the pool back, which is what the data plane does before compiling a
    /// validator, and what anything reading a schema in isolation needs.
    pub fn self_contained(&self, schema: &serde_json::Value) -> serde_json::Value {
        attach_reachable_defs(schema, self.schema_defs.as_ref())
    }
}

/// Attach the definitions a schema reaches, from a document's `$defs` pool.
///
/// Only what the schema actually reaches, not the whole pool: attaching every
/// definition to every schema would cost exactly the copy-per-schema that
/// holding them once exists to avoid.
///
/// A schema already carrying its own `$defs`, as one from an older artifact
/// does, is returned unchanged.
pub fn attach_reachable_defs(
    schema: &serde_json::Value,
    pool: Option<&serde_json::Value>,
) -> serde_json::Value {
    let (Some(serde_json::Value::Object(pool)), Some(obj)) = (pool, schema.as_object()) else {
        return schema.clone();
    };
    if obj.contains_key("$defs") {
        return schema.clone();
    }

    let mut reachable = std::collections::BTreeSet::new();
    collect_reachable_defs(schema, pool, &mut reachable);
    if reachable.is_empty() {
        return schema.clone();
    }

    let mut defs = serde_json::Map::with_capacity(reachable.len());
    for name in reachable {
        if let Some(body) = pool.get(&name) {
            defs.insert(name, body.clone());
        }
    }
    let mut merged = obj.clone();
    merged.insert("$defs".to_string(), serde_json::Value::Object(defs));
    serde_json::Value::Object(merged)
}

/// Collect the `$defs` names a value reaches, following definitions it names.
fn collect_reachable_defs(
    value: &serde_json::Value,
    pool: &serde_json::Map<String, serde_json::Value>,
    found: &mut std::collections::BTreeSet<String>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::String(pointer)) = obj.get("$ref") {
                if let Some(suffix) = pointer.strip_prefix("#/$defs/") {
                    // Pointers escape a name, the pool keys it unescaped.
                    let escaped = suffix.split('/').next().unwrap_or(suffix);
                    let name = escaped.replace("~1", "/").replace("~0", "~");
                    // A definition already seen is already followed, which is
                    // also what terminates a schema that refers to itself.
                    if found.insert(name.clone()) {
                        if let Some(body) = pool.get(&name) {
                            collect_reachable_defs(body, pool, found);
                        }
                    }
                }
            }
            for v in obj.values() {
                collect_reachable_defs(v, pool, found);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_reachable_defs(v, pool, found);
            }
        }
        _ => {}
    }
}

/// Detected spec format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecFormat {
    OpenApi,
    AsyncApi,
}

/// A single API operation (path + method for OpenAPI, channel + action for AsyncAPI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// The path template (OpenAPI: "/users/{id}", AsyncAPI: channel address).
    pub path: String,
    /// The HTTP method (OpenAPI: "GET", AsyncAPI: "SEND"/"RECEIVE").
    pub method: String,
    /// The operationId, if present.
    pub operation_id: Option<String>,
    /// The operation summary (short description for MCP tool name).
    #[serde(default)]
    pub summary: Option<String>,
    /// The operation description (detailed description for MCP tool).
    #[serde(default)]
    pub description: Option<String>,
    /// Path/channel parameters defined on this operation.
    pub parameters: Vec<Parameter>,
    /// Request body definition (OpenAPI: requestBody, AsyncAPI: message payload for SEND).
    pub request_body: Option<RequestBody>,
    /// The dispatcher configuration from `x-barbacane-dispatch`.
    pub dispatch: Option<DispatchConfig>,
    /// Operation-level middlewares (replaces global chain if present).
    pub middlewares: Option<Vec<MiddlewareConfig>>,
    /// Whether this operation is deprecated (OpenAPI `deprecated` field).
    #[serde(default)]
    pub deprecated: bool,
    /// Sunset date for deprecated operations (from `x-sunset` per RFC 8594).
    /// Format: HTTP-date per RFC 9110 (e.g., "Sat, 31 Dec 2024 23:59:59 GMT").
    pub sunset: Option<String>,
    /// Operation-level `x-barbacane-*` extensions.
    pub extensions: BTreeMap<String, serde_json::Value>,
    /// AsyncAPI messages (for async operations only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Protocol bindings (AsyncAPI: kafka, nats, mqtt, amqp, ws).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, serde_json::Value>,
    /// Response definitions keyed by status code (e.g., "200", "201").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub responses: BTreeMap<String, ResponseContent>,
    /// Operation-level `security`. `None` when the key is absent, in which case
    /// the root-level requirement applies. `Some([])` makes the operation
    /// anonymous whatever the root declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<SecurityRequirement>>,
}

/// Response content for a specific status code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseContent {
    /// Content types and their schemas (e.g., "application/json" -> schema).
    pub content: BTreeMap<String, ContentSchema>,
}

/// A path, query, or header parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    /// Parameter name.
    pub name: String,
    /// Location: the raw OpenAPI `in` value, one of "path", "query", "header",
    /// "cookie", or "querystring" (OpenAPI 3.2).
    pub location: String,
    /// Whether this parameter is required.
    pub required: bool,
    /// The parameter's schema (for validation in M2).
    pub schema: Option<serde_json::Value>,
}

/// Dispatcher configuration extracted from `x-barbacane-dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    /// Plugin name (or name@version).
    pub name: String,
    /// Plugin-specific configuration.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Middleware configuration from `x-barbacane-middlewares`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiddlewareConfig {
    /// Plugin name (or name@version).
    pub name: String,
    /// Plugin-specific configuration.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Request body definition from `requestBody`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestBody {
    /// Whether the request body is required.
    pub required: bool,
    /// Content types and their schemas (e.g., "application/json" -> schema).
    pub content: BTreeMap<String, ContentSchema>,
}

/// Content schema for a specific media type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSchema {
    /// The JSON Schema for this content type.
    pub schema: Option<serde_json::Value>,
}

/// AsyncAPI message definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Message name/ID.
    pub name: String,
    /// Message payload schema.
    pub payload: Option<serde_json::Value>,
    /// Content type (e.g., "application/json").
    pub content_type: Option<String>,
    /// Protocol-specific bindings (kafka, nats, mqtt, amqp, ws, etc.).
    #[serde(default)]
    pub bindings: BTreeMap<String, serde_json::Value>,
}

/// AsyncAPI channel definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    /// Channel address/topic (e.g., "user/signedup", "orders.{orderId}").
    pub address: String,
    /// Messages that can be sent/received on this channel.
    pub messages: Vec<Message>,
    /// Channel parameters (for templated addresses like "orders.{orderId}").
    pub parameters: Vec<Parameter>,
    /// Protocol-specific bindings.
    #[serde(default)]
    pub bindings: BTreeMap<String, serde_json::Value>,
}

/// AsyncAPI operation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AsyncAction {
    /// Gateway sends/publishes a message to the channel.
    Send,
    /// Gateway receives/subscribes to messages from the channel.
    Receive,
}

impl std::fmt::Display for AsyncAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AsyncAction::Send => write!(f, "SEND"),
            AsyncAction::Receive => write!(f, "RECEIVE"),
        }
    }
}
