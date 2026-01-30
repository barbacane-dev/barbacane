//! Project manifest (`barbacane.yaml`) parser.
//!
//! The manifest declares which plugins are available to the gateway.
//! This follows the "explicit is better than implicit" principle from ADR-0006.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use barbacane_spec_parser::ApiSpec;
use serde::{Deserialize, Serialize};

use crate::error::CompileError;

/// A project manifest (`barbacane.yaml`).
///
/// Declares the plugins available for use in OpenAPI specs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectManifest {
    /// Plugin declarations: name -> source.
    #[serde(default)]
    pub plugins: HashMap<String, PluginSource>,
}

/// Source location for a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginSource {
    /// Local file path.
    Path(PathSource),
    /// Remote URL.
    Url(UrlSource),
}

/// Plugin sourced from a local file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathSource {
    /// Path to the .wasm file (relative to manifest or absolute).
    pub path: String,
}

/// Plugin sourced from a remote URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UrlSource {
    /// HTTPS URL to the .wasm file.
    pub url: String,
}

impl PluginSource {
    /// Get a description of the source for error messages.
    pub fn description(&self) -> String {
        match self {
            PluginSource::Path(p) => format!("path: {}", p.path),
            PluginSource::Url(u) => format!("url: {}", u.url),
        }
    }
}

/// A resolved plugin ready for bundling.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    /// Plugin name.
    pub name: String,
    /// Source description.
    pub source: String,
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
}

impl ProjectManifest {
    /// Load a manifest from a YAML file.
    pub fn load(path: &Path) -> Result<Self, CompileError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            CompileError::ManifestError(format!("failed to read {}: {}", path.display(), e))
        })?;

        Self::parse(&content, path)
    }

    /// Parse a manifest from YAML content.
    pub fn parse(content: &str, path: &Path) -> Result<Self, CompileError> {
        serde_yaml::from_str(content).map_err(|e| {
            CompileError::ManifestError(format!("failed to parse {}: {}", path.display(), e))
        })
    }

    /// Check if a plugin is declared in the manifest.
    pub fn has_plugin(&self, name: &str) -> bool {
        self.plugins.contains_key(name)
    }

    /// Get all declared plugin names.
    pub fn plugin_names(&self) -> Vec<&str> {
        self.plugins.keys().map(|s| s.as_str()).collect()
    }

    /// Resolve all plugins: load WASM bytes from their sources.
    ///
    /// The `base_path` is used to resolve relative paths in `path:` sources.
    pub fn resolve_plugins(&self, base_path: &Path) -> Result<Vec<ResolvedPlugin>, CompileError> {
        let mut resolved = Vec::new();

        for (name, source) in &self.plugins {
            let wasm_bytes = match source {
                PluginSource::Path(path_source) => {
                    let wasm_path = if Path::new(&path_source.path).is_absolute() {
                        Path::new(&path_source.path).to_path_buf()
                    } else {
                        base_path.join(&path_source.path)
                    };

                    std::fs::read(&wasm_path).map_err(|e| {
                        CompileError::PluginResolution(format!(
                            "failed to read plugin '{}' from {}: {}",
                            name,
                            wasm_path.display(),
                            e
                        ))
                    })?
                }
                PluginSource::Url(url_source) => {
                    // For now, URL resolution is not implemented
                    // This will be added when we need remote plugin fetching
                    return Err(CompileError::PluginResolution(format!(
                        "URL plugin sources not yet implemented: {} -> {}",
                        name, url_source.url
                    )));
                }
            };

            // Validate it looks like a WASM file (magic number)
            if wasm_bytes.len() < 8
                || wasm_bytes[0..4] != [0x00, 0x61, 0x73, 0x6d]
                || wasm_bytes[4..8] != [0x01, 0x00, 0x00, 0x00]
            {
                return Err(CompileError::PluginResolution(format!(
                    "plugin '{}' is not a valid WASM file (invalid magic number)",
                    name
                )));
            }

            resolved.push(ResolvedPlugin {
                name: name.clone(),
                source: source.description(),
                wasm_bytes,
            });
        }

        Ok(resolved)
    }

    /// Validate that all plugins used in specs are declared in the manifest.
    ///
    /// Returns `Ok(())` if all plugins are declared, or an error listing undeclared plugins.
    pub fn validate_specs(&self, specs: &[ApiSpec]) -> Result<(), CompileError> {
        let used = extract_plugin_names(specs);
        let undeclared: Vec<_> = used
            .iter()
            .filter(|name| !self.plugins.contains_key(*name))
            .cloned()
            .collect();

        if undeclared.is_empty() {
            Ok(())
        } else {
            // Return error for the first undeclared plugin (cleaner error messages)
            Err(CompileError::UndeclaredPlugin(undeclared[0].clone()))
        }
    }

    /// Resolve only the plugins that are actually used in the specs.
    ///
    /// This is more efficient than `resolve_plugins` when not all declared plugins are used.
    pub fn resolve_used_plugins(
        &self,
        specs: &[ApiSpec],
        base_path: &Path,
    ) -> Result<Vec<ResolvedPlugin>, CompileError> {
        // First validate all plugins are declared
        self.validate_specs(specs)?;

        let used = extract_plugin_names(specs);
        let mut resolved = Vec::new();

        for name in used {
            let source = match self.plugins.get(&name) {
                Some(s) => s,
                None => continue, // Already validated, shouldn't happen
            };

            let wasm_bytes = match source {
                PluginSource::Path(path_source) => {
                    let wasm_path = if Path::new(&path_source.path).is_absolute() {
                        Path::new(&path_source.path).to_path_buf()
                    } else {
                        base_path.join(&path_source.path)
                    };

                    std::fs::read(&wasm_path).map_err(|e| {
                        CompileError::PluginResolution(format!(
                            "failed to read plugin '{}' from {}: {}",
                            name,
                            wasm_path.display(),
                            e
                        ))
                    })?
                }
                PluginSource::Url(url_source) => {
                    return Err(CompileError::PluginResolution(format!(
                        "URL plugin sources not yet implemented: {} -> {}",
                        name, url_source.url
                    )));
                }
            };

            // Validate WASM magic number
            if wasm_bytes.len() < 8
                || wasm_bytes[0..4] != [0x00, 0x61, 0x73, 0x6d]
                || wasm_bytes[4..8] != [0x01, 0x00, 0x00, 0x00]
            {
                return Err(CompileError::PluginResolution(format!(
                    "plugin '{}' is not a valid WASM file (invalid magic number)",
                    name
                )));
            }

            resolved.push(ResolvedPlugin {
                name: name.clone(),
                source: source.description(),
                wasm_bytes,
            });
        }

        Ok(resolved)
    }
}

/// Extract all unique plugin names used in a set of specs.
///
/// Collects plugins from:
/// - `x-barbacane-dispatch.name` on each operation
/// - `x-barbacane-middlewares[].name` at both global and operation level
pub fn extract_plugin_names(specs: &[ApiSpec]) -> HashSet<String> {
    let mut plugins = HashSet::new();

    for spec in specs {
        // Global middlewares
        for mw in &spec.global_middlewares {
            plugins.insert(normalize_plugin_name(&mw.name));
        }

        // Per-operation plugins
        for op in &spec.operations {
            // Dispatcher
            if let Some(dispatch) = &op.dispatch {
                plugins.insert(normalize_plugin_name(&dispatch.name));
            }

            // Operation-level middlewares
            if let Some(middlewares) = &op.middlewares {
                for mw in middlewares {
                    plugins.insert(normalize_plugin_name(&mw.name));
                }
            }
        }
    }

    plugins
}

/// Normalize a plugin name by stripping version suffix.
///
/// Handles formats like "rate-limit@1.0.0" -> "rate-limit".
fn normalize_plugin_name(name: &str) -> String {
    match name.split_once('@') {
        Some((base, _version)) => base.to_string(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn parse_empty_manifest() {
        let content = "plugins: {}";
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        assert!(manifest.plugins.is_empty());
    }

    #[test]
    fn parse_manifest_with_path_sources() {
        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
  http-upstream:
    path: /absolute/path/to/http-upstream.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        assert_eq!(manifest.plugins.len(), 2);
        assert!(manifest.has_plugin("mock"));
        assert!(manifest.has_plugin("http-upstream"));
        assert!(!manifest.has_plugin("unknown"));
    }

    #[test]
    fn parse_manifest_with_url_sources() {
        let content = r#"
plugins:
  jwt-auth:
    url: https://plugins.barbacane.io/jwt-auth/1.0.0/jwt-auth.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        assert!(manifest.has_plugin("jwt-auth"));
        if let PluginSource::Url(u) = &manifest.plugins["jwt-auth"] {
            assert!(u.url.starts_with("https://"));
        } else {
            panic!("Expected URL source");
        }
    }

    #[test]
    fn resolve_plugins_from_path() {
        let temp = TempDir::new().unwrap();

        // Create a valid WASM file (minimal)
        let wasm_content = vec![
            0x00, 0x61, 0x73, 0x6d, // magic
            0x01, 0x00, 0x00, 0x00, // version
        ];
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let wasm_path = plugin_dir.join("mock.wasm");
        let mut file = std::fs::File::create(&wasm_path).unwrap();
        file.write_all(&wasm_content).unwrap();

        // Create manifest
        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let resolved = manifest.resolve_plugins(temp.path()).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "mock");
        assert_eq!(resolved[0].wasm_bytes, wasm_content);
    }

    #[test]
    fn resolve_plugins_invalid_wasm() {
        let temp = TempDir::new().unwrap();

        // Create an invalid WASM file
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let wasm_path = plugin_dir.join("bad.wasm");
        std::fs::write(&wasm_path, b"not a wasm file").unwrap();

        let content = r#"
plugins:
  bad:
    path: ./plugins/bad.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let result = manifest.resolve_plugins(temp.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid magic number"));
    }

    #[test]
    fn resolve_plugins_missing_file() {
        let temp = TempDir::new().unwrap();

        let content = r#"
plugins:
  missing:
    path: ./plugins/missing.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let result = manifest.resolve_plugins(temp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to read"));
    }

    #[test]
    fn extract_plugin_names_from_specs() {
        use barbacane_spec_parser::{
            ApiSpec, DispatchConfig, MiddlewareConfig, Operation, SpecFormat,
        };
        use std::collections::BTreeMap;

        let spec = ApiSpec {
            filename: Some("test.yaml".to_string()),
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "Test".to_string(),
            api_version: "1.0.0".to_string(),
            global_middlewares: vec![MiddlewareConfig {
                name: "rate-limit".to_string(),
                config: serde_json::json!({}),
            }],
            extensions: BTreeMap::new(),
            operations: vec![
                Operation {
                    path: "/health".to_string(),
                    method: "GET".to_string(),
                    operation_id: None,
                    parameters: vec![],
                    request_body: None,
                    dispatch: Some(DispatchConfig {
                        name: "mock".to_string(),
                        config: serde_json::json!({}),
                    }),
                    middlewares: None,
                    extensions: BTreeMap::new(),
                },
                Operation {
                    path: "/api".to_string(),
                    method: "GET".to_string(),
                    operation_id: None,
                    parameters: vec![],
                    request_body: None,
                    dispatch: Some(DispatchConfig {
                        name: "http-upstream".to_string(),
                        config: serde_json::json!({}),
                    }),
                    middlewares: Some(vec![MiddlewareConfig {
                        name: "jwt-auth@1.0.0".to_string(),
                        config: serde_json::json!({}),
                    }]),
                    extensions: BTreeMap::new(),
                },
            ],
        };

        let plugins = extract_plugin_names(&[spec]);

        assert!(plugins.contains("mock"));
        assert!(plugins.contains("http-upstream"));
        assert!(plugins.contains("rate-limit"));
        // Version suffix should be stripped
        assert!(plugins.contains("jwt-auth"));
        assert!(!plugins.contains("jwt-auth@1.0.0"));
        assert_eq!(plugins.len(), 4);
    }

    #[test]
    fn validate_specs_all_declared() {
        use barbacane_spec_parser::{ApiSpec, DispatchConfig, Operation, SpecFormat};
        use std::collections::BTreeMap;

        let spec = ApiSpec {
            filename: Some("test.yaml".to_string()),
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "Test".to_string(),
            api_version: "1.0.0".to_string(),
            global_middlewares: vec![],
            extensions: BTreeMap::new(),
            operations: vec![Operation {
                path: "/health".to_string(),
                method: "GET".to_string(),
                operation_id: None,
                parameters: vec![],
                request_body: None,
                dispatch: Some(DispatchConfig {
                    name: "mock".to_string(),
                    config: serde_json::json!({}),
                }),
                middlewares: None,
                extensions: BTreeMap::new(),
            }],
        };

        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let result = manifest.validate_specs(&[spec]);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_specs_undeclared_plugin() {
        use barbacane_spec_parser::{ApiSpec, DispatchConfig, Operation, SpecFormat};
        use std::collections::BTreeMap;

        let spec = ApiSpec {
            filename: Some("test.yaml".to_string()),
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "Test".to_string(),
            api_version: "1.0.0".to_string(),
            global_middlewares: vec![],
            extensions: BTreeMap::new(),
            operations: vec![Operation {
                path: "/proxy".to_string(),
                method: "GET".to_string(),
                operation_id: None,
                parameters: vec![],
                request_body: None,
                dispatch: Some(DispatchConfig {
                    name: "http-upstream".to_string(),
                    config: serde_json::json!({}),
                }),
                middlewares: None,
                extensions: BTreeMap::new(),
            }],
        };

        // Manifest declares "mock" but spec uses "http-upstream"
        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let result = manifest.validate_specs(&[spec]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("E1040"));
        assert!(err.contains("http-upstream"));
    }
}
