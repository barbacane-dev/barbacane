use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A parsed API spec (OpenAPI or AsyncAPI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSpec {
    /// The format detected from the root field.
    pub format: SpecFormat,
    /// The spec version string (e.g. "3.1.0").
    pub version: String,
    /// The `info.title` field.
    pub title: String,
    /// Parsed path operations.
    pub operations: Vec<Operation>,
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
    /// The dispatcher configuration from `x-barbacane-dispatch`.
    pub dispatch: Option<DispatchConfig>,
    /// Operation-level `x-barbacane-*` extensions.
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Dispatcher configuration extracted from `x-barbacane-dispatch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    /// Plugin name (or name@version).
    pub name: String,
    /// Plugin-specific configuration.
    pub config: serde_json::Value,
}
