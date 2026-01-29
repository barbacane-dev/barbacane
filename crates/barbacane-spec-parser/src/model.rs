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

/// A single API operation (path + method).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// The path template (e.g. "/users/{id}").
    pub path: String,
    /// The HTTP method (uppercase).
    pub method: String,
    /// The OpenAPI operationId, if present.
    pub operation_id: Option<String>,
    /// Path parameters defined on this operation.
    pub parameters: Vec<Parameter>,
    /// The dispatcher configuration from `x-barbacane-dispatch`.
    pub dispatch: Option<DispatchConfig>,
    /// Operation-level middlewares (replaces global chain if present).
    pub middlewares: Option<Vec<MiddlewareConfig>>,
    /// Operation-level `x-barbacane-*` extensions.
    pub extensions: BTreeMap<String, serde_json::Value>,
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
