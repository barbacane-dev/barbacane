use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A parsed API spec (OpenAPI or AsyncAPI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSpec {
    /// Original filename (if parsed from file).
    pub filename: Option<String>,
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
    /// Global observability config from root-level `x-barbacane-observability`.
    #[serde(default)]
    pub observability: ObservabilityConfig,
    /// Raw `x-barbacane-*` extensions at root level.
    pub extensions: BTreeMap<String, serde_json::Value>,
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
    /// Path/channel parameters defined on this operation.
    pub parameters: Vec<Parameter>,
    /// Request body definition (OpenAPI: requestBody, AsyncAPI: message payload for SEND).
    pub request_body: Option<RequestBody>,
    /// The dispatcher configuration from `x-barbacane-dispatch`.
    pub dispatch: Option<DispatchConfig>,
    /// Operation-level middlewares (replaces global chain if present).
    pub middlewares: Option<Vec<MiddlewareConfig>>,
    /// Operation-level observability config (overrides global).
    #[serde(default)]
    pub observability: Option<ObservabilityConfig>,
    /// Whether this operation is deprecated (OpenAPI `deprecated` field).
    #[serde(default)]
    pub deprecated: bool,
    /// Sunset date for deprecated operations (from `x-barbacane-sunset`).
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
}

/// A path, query, or header parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    /// Parameter name.
    pub name: String,
    /// Location: "path", "query", "header".
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

/// Observability configuration from `x-barbacane-observability`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Override trace sampling rate (0.0 to 1.0).
    #[serde(default)]
    pub trace_sampling: Option<f64>,

    /// Enable detailed validation failure logging.
    #[serde(default)]
    pub detailed_validation_logs: Option<bool>,

    /// Latency SLO threshold in milliseconds.
    /// Emit `barbacane_slo_violation_total` metric when exceeded.
    #[serde(default)]
    pub latency_slo_ms: Option<u64>,
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
