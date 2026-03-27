//! Project manifest (`barbacane.yaml`) parser.
//!
//! The manifest declares which plugins are available to the gateway.
//! This follows the "explicit is better than implicit" principle from ADR-0006.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::cache::PluginCache;
use crate::download;
use crate::error::CompileError;
use crate::spec_parser::ApiSpec;
use serde::{Deserialize, Serialize};

/// Minimal plugin.toml structure for extracting metadata.
#[derive(Debug, Deserialize)]
struct PluginToml {
    plugin: PluginMeta,
    #[serde(default)]
    capabilities: PluginTomlCapabilities,
}

#[derive(Debug, Deserialize)]
struct PluginMeta {
    version: String,
    #[serde(rename = "type")]
    plugin_type: String,
}

#[derive(Debug, Default, Deserialize)]
struct PluginTomlCapabilities {
    #[serde(default)]
    body_access: bool,
}

/// Plugin metadata extracted from plugin.toml.
struct PluginMetadata {
    version: String,
    plugin_type: String,
    body_access: bool,
}

/// Parse plugin metadata from TOML content.
fn parse_plugin_metadata(content: &str) -> Option<PluginMetadata> {
    let parsed: PluginToml = toml::from_str(content).ok()?;
    Some(PluginMetadata {
        version: parsed.plugin.version,
        plugin_type: parsed.plugin.plugin_type,
        body_access: parsed.capabilities.body_access,
    })
}

/// Try to read plugin metadata from plugin.toml in the same directory as the WASM file.
fn read_plugin_metadata(wasm_path: &Path) -> Option<PluginMetadata> {
    let plugin_toml_path = wasm_path.parent()?.join("plugin.toml");
    let content = std::fs::read_to_string(&plugin_toml_path).ok()?;
    parse_plugin_metadata(&content)
}

/// Resolve a WASM path from a plugin source, relative to a base path.
fn resolve_wasm_path(path_source: &PathSource, base_path: &Path) -> std::path::PathBuf {
    if Path::new(&path_source.path).is_absolute() {
        Path::new(&path_source.path).to_path_buf()
    } else {
        base_path.join(&path_source.path)
    }
}

/// Resolve a single plugin: read WASM bytes, validate, and extract metadata.
fn resolve_plugin(
    name: &str,
    source: &PluginSource,
    base_path: &Path,
    no_cache: bool,
) -> Result<ResolvedPlugin, CompileError> {
    let (wasm_bytes, plugin_toml_content) = match source {
        PluginSource::Path(path_source) => {
            let wasm_path = resolve_wasm_path(path_source, base_path);
            let bytes = std::fs::read(&wasm_path).map_err(|e| {
                CompileError::PluginResolution(format!(
                    "failed to read plugin '{}' from {}: {}",
                    name,
                    wasm_path.display(),
                    e
                ))
            })?;
            (bytes, None)
        }
        PluginSource::Url(url_source) => resolve_url_plugin(name, url_source, no_cache)?,
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

    // Try to read plugin metadata from plugin.toml
    let metadata = match source {
        PluginSource::Path(path_source) => {
            let wasm_path = resolve_wasm_path(path_source, base_path);
            read_plugin_metadata(&wasm_path)
        }
        PluginSource::Url(_) => plugin_toml_content
            .as_deref()
            .and_then(parse_plugin_metadata),
    };

    Ok(ResolvedPlugin {
        name: name.to_string(),
        source: source.description(),
        wasm_bytes,
        version: metadata.as_ref().map(|m| m.version.clone()),
        plugin_type: metadata.as_ref().map(|m| m.plugin_type.clone()),
        body_access: metadata.as_ref().is_some_and(|m| m.body_access),
    })
}

/// Resolve a plugin from a remote URL, using the cache when possible.
fn resolve_url_plugin(
    name: &str,
    url_source: &UrlSource,
    no_cache: bool,
) -> Result<(Vec<u8>, Option<String>), CompileError> {
    let url = &url_source.url;

    if !url.starts_with("https://") {
        return Err(CompileError::PluginResolution(format!(
            "plugin '{name}' URL must use HTTPS: {url}"
        )));
    }

    // Check cache (unless --no-cache)
    if !no_cache {
        let cache = PluginCache::new()?;
        if let Some(cached) = cache.get(url, url_source.sha256.as_deref()) {
            tracing::info!(name, url, "using cached plugin");
            return Ok((cached.wasm_bytes, cached.plugin_toml));
        }
    }

    // Download
    let downloaded = download::download_plugin(url)?;

    // Verify checksum if provided
    if let Some(expected) = &url_source.sha256 {
        let actual = hex::encode(Sha256::digest(&downloaded.wasm_bytes));
        if actual != *expected {
            return Err(CompileError::PluginResolution(format!(
                "plugin '{name}' checksum mismatch: expected {expected}, got {actual}"
            )));
        }
    }

    // Store in cache (unless --no-cache)
    if !no_cache {
        let cache = PluginCache::new()?;
        cache.put(
            url,
            &downloaded.wasm_bytes,
            downloaded.plugin_toml.as_deref(),
        )?;
        tracing::info!(name, url, "cached remote plugin");
    }

    Ok((downloaded.wasm_bytes, downloaded.plugin_toml))
}

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
    /// Optional SHA-256 checksum for integrity verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
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
    /// Plugin version (from plugin.toml if available).
    pub version: Option<String>,
    /// Plugin type: "middleware" or "dispatcher" (from plugin.toml if available).
    pub plugin_type: Option<String>,
    /// Whether this plugin needs the request body in `on_request`.
    pub body_access: bool,
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
    /// If `no_cache` is true, remote plugins are always re-downloaded.
    pub fn resolve_plugins(
        &self,
        base_path: &Path,
        no_cache: bool,
    ) -> Result<Vec<ResolvedPlugin>, CompileError> {
        self.plugins
            .iter()
            .map(|(name, source)| resolve_plugin(name, source, base_path, no_cache))
            .collect()
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
    /// If `no_cache` is true, remote plugins are always re-downloaded.
    pub fn resolve_used_plugins(
        &self,
        specs: &[ApiSpec],
        base_path: &Path,
        no_cache: bool,
    ) -> Result<Vec<ResolvedPlugin>, CompileError> {
        self.validate_specs(specs)?;

        let used = extract_plugin_names(specs);
        let mut resolved = Vec::new();

        for name in used {
            let source = match self.plugins.get(&name) {
                Some(s) => s,
                None => continue, // Already validated, shouldn't happen
            };
            resolved.push(resolve_plugin(&name, source, base_path, no_cache)?);
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
            assert!(u.sha256.is_none());
        } else {
            panic!("Expected URL source");
        }
    }

    #[test]
    fn parse_manifest_with_url_and_sha256() {
        let content = r#"
plugins:
  jwt-auth:
    url: https://plugins.barbacane.io/jwt-auth/1.0.0/jwt-auth.wasm
    sha256: abc123def456
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        if let PluginSource::Url(u) = &manifest.plugins["jwt-auth"] {
            assert_eq!(u.sha256.as_deref(), Some("abc123def456"));
        } else {
            panic!("Expected URL source");
        }
    }

    #[test]
    fn reject_http_url_in_resolve() {
        let source = PluginSource::Url(UrlSource {
            url: "http://example.com/plugin.wasm".to_string(),
            sha256: None,
        });
        let result = resolve_plugin("test", &source, Path::new("."), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("HTTPS"));
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

        let resolved = manifest.resolve_plugins(temp.path(), false).unwrap();
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

        let result = manifest.resolve_plugins(temp.path(), false);
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

        let result = manifest.resolve_plugins(temp.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to read"));
    }

    #[test]
    fn extract_plugin_names_from_specs() {
        use crate::spec_parser::{
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
                    summary: None,
                    description: None,
                    parameters: vec![],
                    request_body: None,
                    dispatch: Some(DispatchConfig {
                        name: "mock".to_string(),
                        config: serde_json::json!({}),
                    }),
                    middlewares: None,
                    deprecated: false,
                    sunset: None,
                    extensions: BTreeMap::new(),
                    messages: vec![],
                    bindings: BTreeMap::new(),
                    responses: BTreeMap::new(),
                },
                Operation {
                    path: "/api".to_string(),
                    method: "GET".to_string(),
                    operation_id: None,
                    summary: None,
                    description: None,
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
                    deprecated: false,
                    sunset: None,
                    extensions: BTreeMap::new(),
                    messages: vec![],
                    bindings: BTreeMap::new(),
                    responses: BTreeMap::new(),
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
        use crate::spec_parser::{ApiSpec, DispatchConfig, Operation, SpecFormat};
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
                summary: None,
                description: None,
                parameters: vec![],
                request_body: None,
                dispatch: Some(DispatchConfig {
                    name: "mock".to_string(),
                    config: serde_json::json!({}),
                }),
                middlewares: None,
                deprecated: false,
                sunset: None,
                extensions: BTreeMap::new(),
                messages: vec![],
                bindings: BTreeMap::new(),
                responses: BTreeMap::new(),
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
        use crate::spec_parser::{ApiSpec, DispatchConfig, Operation, SpecFormat};
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
                summary: None,
                description: None,
                parameters: vec![],
                request_body: None,
                dispatch: Some(DispatchConfig {
                    name: "http-upstream".to_string(),
                    config: serde_json::json!({}),
                }),
                middlewares: None,
                deprecated: false,
                sunset: None,
                extensions: BTreeMap::new(),
                messages: vec![],
                bindings: BTreeMap::new(),
                responses: BTreeMap::new(),
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

    fn make_spec_using_plugin(plugin_name: &str) -> ApiSpec {
        use crate::spec_parser::{DispatchConfig, Operation, SpecFormat};
        use std::collections::BTreeMap;

        ApiSpec {
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
                summary: None,
                description: None,
                parameters: vec![],
                request_body: None,
                dispatch: Some(DispatchConfig {
                    name: plugin_name.to_string(),
                    config: serde_json::json!({}),
                }),
                middlewares: None,
                deprecated: false,
                sunset: None,
                extensions: BTreeMap::new(),
                messages: vec![],
                bindings: BTreeMap::new(),
                responses: BTreeMap::new(),
            }],
        }
    }

    fn write_valid_wasm(dir: &Path, name: &str) -> Vec<u8> {
        let wasm_content = vec![
            0x00, 0x61, 0x73, 0x6d, // magic
            0x01, 0x00, 0x00, 0x00, // version
        ];
        let path = dir.join(name);
        std::fs::write(&path, &wasm_content).unwrap();
        wasm_content
    }

    #[test]
    fn resolve_used_plugins_from_path() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let wasm_content = write_valid_wasm(&plugin_dir, "mock.wasm");

        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("mock");

        let resolved = manifest
            .resolve_used_plugins(&[spec], temp.path(), false)
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "mock");
        assert_eq!(resolved[0].wasm_bytes, wasm_content);
    }

    #[test]
    fn resolve_used_plugins_skips_unused() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_valid_wasm(&plugin_dir, "mock.wasm");
        write_valid_wasm(&plugin_dir, "unused.wasm");

        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
  unused:
    path: ./plugins/unused.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("mock");

        let resolved = manifest
            .resolve_used_plugins(&[spec], temp.path(), false)
            .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "mock");
    }

    #[test]
    fn resolve_used_plugins_invalid_wasm() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("bad.wasm"), b"not a wasm file").unwrap();

        let content = r#"
plugins:
  bad:
    path: ./plugins/bad.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("bad");

        let result = manifest.resolve_used_plugins(&[spec], temp.path(), false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid magic number"));
    }

    #[test]
    fn resolve_used_plugins_missing_file() {
        let temp = TempDir::new().unwrap();

        let content = r#"
plugins:
  missing:
    path: ./plugins/missing.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("missing");

        let result = manifest.resolve_used_plugins(&[spec], temp.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to read"));
    }

    #[test]
    fn resolve_used_plugins_rejects_undeclared() {
        let temp = TempDir::new().unwrap();

        let content = "plugins: {}";
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("unknown");

        let result = manifest.resolve_used_plugins(&[spec], temp.path(), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("E1040"));
    }

    #[test]
    fn resolve_plugins_reads_metadata() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_valid_wasm(&plugin_dir, "mock.wasm");

        // Create plugin.toml next to the wasm file
        let plugin_toml = r#"
[plugin]
name = "mock"
version = "1.2.3"
type = "dispatcher"
"#;
        std::fs::write(plugin_dir.join("plugin.toml"), plugin_toml).unwrap();

        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();

        let resolved = manifest.resolve_plugins(temp.path(), false).unwrap();
        assert_eq!(resolved[0].version, Some("1.2.3".to_string()));
        assert_eq!(resolved[0].plugin_type, Some("dispatcher".to_string()));
    }

    #[test]
    fn resolve_used_plugins_reads_metadata() {
        let temp = TempDir::new().unwrap();
        let plugin_dir = temp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_valid_wasm(&plugin_dir, "mock.wasm");

        let plugin_toml = r#"
[plugin]
name = "mock"
version = "2.0.0"
type = "middleware"
"#;
        std::fs::write(plugin_dir.join("plugin.toml"), plugin_toml).unwrap();

        let content = r#"
plugins:
  mock:
    path: ./plugins/mock.wasm
"#;
        let manifest = ProjectManifest::parse(content, Path::new("barbacane.yaml")).unwrap();
        let spec = make_spec_using_plugin("mock");

        let resolved = manifest
            .resolve_used_plugins(&[spec], temp.path(), false)
            .unwrap();
        assert_eq!(resolved[0].version, Some("2.0.0".to_string()));
        assert_eq!(resolved[0].plugin_type, Some("middleware".to_string()));
    }
}
