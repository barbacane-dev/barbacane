//! Plugin manifest (plugin.toml) parsing and validation.
//!
//! Per SPEC-003 section 2.1, the plugin manifest defines:
//! - Plugin metadata (name, version, type, description)
//! - WASM binary path
//! - Required capabilities (host functions)

use serde::Deserialize;

use crate::error::WasmError;

/// A parsed and validated plugin manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    /// Plugin metadata.
    pub plugin: PluginMeta,

    /// Plugin capabilities.
    pub capabilities: Capabilities,
}

/// Plugin metadata from the [plugin] section.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    /// Unique identifier, lowercase, kebab-case.
    pub name: String,

    /// Semantic version string.
    pub version: String,

    /// Plugin type: "middleware" or "dispatcher".
    #[serde(rename = "type")]
    pub plugin_type: PluginType,

    /// Optional description for registry display.
    pub description: Option<String>,

    /// Path to WASM binary, relative to plugin.toml.
    pub wasm: String,
}

/// Plugin type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginType {
    /// Middleware plugin (on_request, on_response).
    Middleware,

    /// Dispatcher plugin (dispatch).
    Dispatcher,
}

impl PluginType {
    /// Get the required WASM exports for this plugin type.
    pub fn required_exports(&self) -> &'static [&'static str] {
        match self {
            PluginType::Middleware => &["init", "on_request", "on_response"],
            PluginType::Dispatcher => &["init", "dispatch"],
        }
    }
}

/// Plugin capabilities from the [capabilities] section.
#[derive(Debug, Clone, Deserialize)]
pub struct Capabilities {
    /// List of host functions this plugin requires.
    #[serde(default)]
    pub host_functions: Vec<String>,
}

impl PluginManifest {
    /// Parse a plugin manifest from TOML content.
    pub fn from_toml(content: &str) -> Result<Self, WasmError> {
        let manifest: Self = toml::from_str(content)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest fields.
    fn validate(&self) -> Result<(), WasmError> {
        // Validate name: ^[a-z][a-z0-9-]*$, max 64 chars
        if self.plugin.name.is_empty() || self.plugin.name.len() > 64 {
            return Err(WasmError::ManifestValidation(
                "plugin name must be 1-64 characters".into(),
            ));
        }

        let name_regex = regex_lite::Regex::new(r"^[a-z][a-z0-9-]*$").unwrap();
        if !name_regex.is_match(&self.plugin.name) {
            return Err(WasmError::ManifestValidation(
                "plugin name must be lowercase, kebab-case (^[a-z][a-z0-9-]*$)".into(),
            ));
        }

        // Validate version: valid semver
        if semver::Version::parse(&self.plugin.version).is_err() {
            return Err(WasmError::ManifestValidation(format!(
                "invalid semver version: {}",
                self.plugin.version
            )));
        }

        // Validate description length
        if let Some(desc) = &self.plugin.description {
            if desc.len() > 256 {
                return Err(WasmError::ManifestValidation(
                    "description must be at most 256 characters".into(),
                ));
            }
        }

        // Validate wasm path is not empty
        if self.plugin.wasm.is_empty() {
            return Err(WasmError::ManifestValidation(
                "wasm path cannot be empty".into(),
            ));
        }

        // Validate host functions are known
        for func in &self.capabilities.host_functions {
            if !is_known_capability(func) {
                return Err(WasmError::UnknownCapability(func.clone()));
            }
        }

        Ok(())
    }

    /// Check if this plugin declares a specific capability.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.capabilities.host_functions.iter().any(|c| c == capability)
    }
}

/// Known host function capability names.
const KNOWN_CAPABILITIES: &[&str] = &[
    "log",
    "context_get",
    "context_set",
    "clock_now",
    "get_secret",
    "http_call",
    "kafka_publish",
    "nats_publish",
    "telemetry",
];

/// Check if a capability name is known.
fn is_known_capability(name: &str) -> bool {
    KNOWN_CAPABILITIES.contains(&name)
}

/// Get the host function names for a capability.
pub fn capability_to_imports(capability: &str) -> &'static [&'static str] {
    match capability {
        "log" => &["host_log"],
        "context_get" => &["host_context_get", "host_context_read_result"],
        "context_set" => &["host_context_set"],
        "clock_now" => &["host_clock_now"],
        "get_secret" => &["host_get_secret", "host_secret_read_result"],
        "http_call" => &["host_http_call", "host_http_read_result"],
        "kafka_publish" => &["host_kafka_publish"],
        "nats_publish" => &["host_nats_publish"],
        "telemetry" => &[
            "host_metric_counter_inc",
            "host_metric_histogram_observe",
            "host_span_start",
            "host_span_end",
            "host_span_set_attribute",
        ],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
[plugin]
name = "my-plugin"
version = "1.0.0"
type = "middleware"
description = "A test plugin"
wasm = "my_plugin.wasm"

[capabilities]
host_functions = ["log", "context_get"]
"#;

    #[test]
    fn parse_valid_manifest() {
        let manifest = PluginManifest::from_toml(VALID_MANIFEST).unwrap();
        assert_eq!(manifest.plugin.name, "my-plugin");
        assert_eq!(manifest.plugin.version, "1.0.0");
        assert_eq!(manifest.plugin.plugin_type, PluginType::Middleware);
        assert_eq!(manifest.plugin.description, Some("A test plugin".into()));
        assert_eq!(manifest.plugin.wasm, "my_plugin.wasm");
        assert_eq!(manifest.capabilities.host_functions.len(), 2);
    }

    #[test]
    fn parse_dispatcher_manifest() {
        let manifest_str = r#"
[plugin]
name = "http-upstream"
version = "2.0.0"
type = "dispatcher"
wasm = "http_upstream.wasm"

[capabilities]
host_functions = ["http_call", "log"]
"#;
        let manifest = PluginManifest::from_toml(manifest_str).unwrap();
        assert_eq!(manifest.plugin.plugin_type, PluginType::Dispatcher);
    }

    #[test]
    fn reject_invalid_name() {
        let manifest_str = r#"
[plugin]
name = "MyPlugin"
version = "1.0.0"
type = "middleware"
wasm = "my_plugin.wasm"

[capabilities]
host_functions = []
"#;
        let result = PluginManifest::from_toml(manifest_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("kebab-case"));
    }

    #[test]
    fn reject_invalid_version() {
        let manifest_str = r#"
[plugin]
name = "my-plugin"
version = "not-semver"
type = "middleware"
wasm = "my_plugin.wasm"

[capabilities]
host_functions = []
"#;
        let result = PluginManifest::from_toml(manifest_str);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("semver"));
    }

    #[test]
    fn reject_unknown_capability() {
        let manifest_str = r#"
[plugin]
name = "my-plugin"
version = "1.0.0"
type = "middleware"
wasm = "my_plugin.wasm"

[capabilities]
host_functions = ["unknown_function"]
"#;
        let result = PluginManifest::from_toml(manifest_str);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WasmError::UnknownCapability(_)));
    }

    #[test]
    fn has_capability() {
        let manifest = PluginManifest::from_toml(VALID_MANIFEST).unwrap();
        assert!(manifest.has_capability("log"));
        assert!(manifest.has_capability("context_get"));
        assert!(!manifest.has_capability("http_call"));
    }

    #[test]
    fn required_exports_middleware() {
        let exports = PluginType::Middleware.required_exports();
        assert!(exports.contains(&"init"));
        assert!(exports.contains(&"on_request"));
        assert!(exports.contains(&"on_response"));
    }

    #[test]
    fn required_exports_dispatcher() {
        let exports = PluginType::Dispatcher.required_exports();
        assert!(exports.contains(&"init"));
        assert!(exports.contains(&"dispatch"));
    }
}
