//! Configuration structures for request transformer plugin.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Header transformation configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HeaderConfig {
    /// Add or overwrite headers.
    #[serde(default)]
    pub add: BTreeMap<String, String>,

    /// Add headers only if not already present.
    #[serde(default)]
    pub set: BTreeMap<String, String>,

    /// Remove headers by name.
    #[serde(default)]
    pub remove: Vec<String>,

    /// Rename headers (old-name → new-name).
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}

/// Query string transformation configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct QueryConfig {
    /// Add or overwrite query parameters.
    #[serde(default)]
    pub add: BTreeMap<String, String>,

    /// Remove query parameters by name.
    #[serde(default)]
    pub remove: Vec<String>,

    /// Rename query parameters (old-name → new-name).
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}

/// Path rewriting configuration.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PathConfig {
    /// Remove prefix from path.
    pub strip_prefix: Option<String>,

    /// Add prefix to path.
    pub add_prefix: Option<String>,

    /// Regex replace configuration.
    pub replace: Option<PathReplaceConfig>,
}

/// Path regex replace configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct PathReplaceConfig {
    /// Regex pattern to match.
    pub pattern: String,

    /// Replacement string.
    pub replacement: String,
}

/// Body transformation configuration (JSON Pointer).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BodyConfig {
    /// Add or overwrite JSON fields using JSON Pointer paths.
    #[serde(default)]
    pub add: BTreeMap<String, String>,

    /// Remove JSON fields using JSON Pointer paths.
    #[serde(default)]
    pub remove: Vec<String>,

    /// Rename JSON fields (old-pointer → new-pointer).
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
}
