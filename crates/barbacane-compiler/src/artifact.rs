use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Builder;

use std::collections::BTreeMap;

use crate::spec_parser::{
    parse_spec_file, ApiSpec, DispatchConfig, Message, MiddlewareConfig, Parameter, RequestBody,
    ResponseContent, SpecFormat,
};

use crate::error::{CompileError, CompileWarning};
use crate::manifest::ProjectManifest;

/// Current artifact format version.
pub const ARTIFACT_VERSION: u32 = 3;

/// Options for compilation.
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Allow plaintext HTTP upstream URLs (development only).
    /// If false, compilation fails with E1031 for http:// URLs.
    pub allow_plaintext: bool,
    /// Maximum JSON Schema nesting depth (default: 32).
    pub max_schema_depth: usize,
    /// Maximum total properties in a schema (default: 256).
    pub max_schema_properties: usize,
    /// Git commit SHA for build provenance tracking.
    pub provenance_commit: Option<String>,
    /// Source identifier for build provenance (e.g., "ci/github-actions").
    pub provenance_source: Option<String>,
    /// Bypass the plugin download cache entirely (no read, no write).
    pub no_cache: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            allow_plaintext: false,
            max_schema_depth: 32,
            max_schema_properties: 256,
            provenance_commit: None,
            provenance_source: None,
            no_cache: false,
        }
    }
}

/// Compiler version (from Cargo.toml).
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Known x-barbacane-* extensions (structural spec extensions).
/// Extensions not in this list will trigger E1015 warning.
///
/// Note: Middleware functionality (rate-limit, cache, auth, etc.) is configured
/// via `x-barbacane-middlewares` with the plugin name, not as separate extensions.
/// Backend connections are configured in the `http-upstream` dispatcher config.
const KNOWN_EXTENSIONS: &[&str] = &[
    "x-barbacane-dispatch",    // Operation level - dispatcher config (required)
    "x-barbacane-middlewares", // Root or operation level - middleware chain
    "x-barbacane-mcp",         // Root or operation level - MCP server config
];

/// Result of compilation including the manifest and any warnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileResult {
    /// The compiled manifest.
    pub manifest: Manifest,
    /// Warnings produced during compilation (non-fatal issues).
    pub warnings: Vec<CompileWarning>,
}

/// The manifest.json embedded in a .bca artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub barbacane_artifact_version: u32,
    pub compiled_at: String,
    pub compiler_version: String,
    pub source_specs: Vec<SourceSpec>,
    pub routes_count: usize,
    /// Checksums use BTreeMap for deterministic JSON serialization order.
    pub checksums: BTreeMap<String, String>,
    /// Bundled plugins (empty if no plugins bundled).
    pub plugins: Vec<BundledPlugin>,
    /// Combined SHA-256 fingerprint of all artifact inputs (specs + routes + plugins).
    pub artifact_hash: String,
    /// Build provenance metadata (git commit, CI source, etc.).
    pub provenance: Provenance,
    /// MCP server configuration (from root-level x-barbacane-mcp).
    #[serde(default)]
    pub mcp: McpConfig,
}

/// MCP server configuration extracted from `x-barbacane-mcp`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpConfig {
    /// Whether MCP is enabled globally.
    pub enabled: bool,
    /// MCP server name (defaults to info.title).
    #[serde(default)]
    pub server_name: Option<String>,
    /// MCP server version (defaults to info.version).
    #[serde(default)]
    pub server_version: Option<String>,
}

/// Build provenance metadata embedded in the manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Provenance {
    /// Git commit SHA at build time.
    pub commit: Option<String>,
    /// Build source identifier (e.g., "ci/github-actions").
    pub source: Option<String>,
}

/// Metadata about a bundled plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledPlugin {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Plugin type (middleware or dispatcher).
    pub plugin_type: String,
    /// Path within the artifact (e.g., "plugins/rate-limit.wasm").
    pub wasm_path: String,
    /// SHA-256 hash of the WASM file.
    pub sha256: String,
    /// Plugin capabilities declared in plugin.toml.
    pub capabilities: PluginCapabilities,
}

/// Plugin capabilities stored in the artifact manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    /// Whether the middleware receives the request body in `on_request`.
    #[serde(default)]
    pub body_access: bool,
}

/// A plugin loaded from a .bca artifact, ready for compilation.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// Plugin version.
    pub version: String,
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
    /// Whether this plugin needs the request body.
    pub body_access: bool,
}

/// Metadata about a source spec included in the artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSpec {
    pub file: String,
    pub sha256: String,
    #[serde(rename = "type")]
    pub spec_type: String,
    pub version: String,
}

/// Compiled route data stored in routes.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledRoutes {
    pub operations: Vec<CompiledOperation>,
}

/// A compiled operation ready for the data plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledOperation {
    pub index: usize,
    /// Path template (OpenAPI: "/users/{id}", AsyncAPI: channel address).
    pub path: String,
    /// HTTP method (OpenAPI: "GET", AsyncAPI: "SEND"/"RECEIVE").
    pub method: String,
    pub operation_id: Option<String>,
    /// Operation summary (short description).
    #[serde(default)]
    pub summary: Option<String>,
    /// Operation description (detailed).
    #[serde(default)]
    pub description: Option<String>,
    /// Parameters for validation (path, query, header).
    pub parameters: Vec<Parameter>,
    /// Request body schema for validation.
    pub request_body: Option<RequestBody>,
    pub dispatch: DispatchConfig,
    /// Resolved middleware chain for this operation.
    /// If the operation has its own middlewares, uses those.
    /// Otherwise, uses the global middlewares from the spec.
    #[serde(default)]
    pub middlewares: Vec<MiddlewareConfig>,
    /// Whether this operation is deprecated.
    #[serde(default)]
    pub deprecated: bool,
    /// Sunset date for deprecated operations (HTTP-date format per RFC 9110).
    #[serde(default)]
    pub sunset: Option<String>,
    /// AsyncAPI messages (for async operations only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Protocol bindings (AsyncAPI: kafka, nats, mqtt, amqp, ws).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, serde_json::Value>,
    /// Response definitions keyed by status code.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub responses: BTreeMap<String, ResponseContent>,
    /// Whether this operation is exposed as an MCP tool.
    #[serde(default)]
    pub mcp_enabled: Option<bool>,
    /// MCP-specific tool description override.
    #[serde(default)]
    pub mcp_description: Option<String>,
}

/// Compile one or more spec files into a .bca artifact.
///
/// Bundles the provided plugins into the artifact. Pass `&[]` if the specs
/// don't reference any plugins.
///
/// This function does NOT validate that spec-referenced plugins are present
/// in `plugins` — the caller is responsible for validation (see
/// [`extract_plugin_names`] and [`ProjectManifest::validate_specs`]).
/// For manifest-based compilation with built-in validation, use
/// [`compile_with_manifest`].
pub fn compile(
    spec_paths: &[&Path],
    plugins: &[PluginBundle],
    output: &Path,
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let specs = parse_specs(spec_paths)?;
    compile_inner(&specs, plugins, output, options)
}

/// Compile specs with a project manifest into a .bca artifact.
///
/// This is the primary compilation entry point for manifest-based projects.
/// It validates that all plugins used in specs are declared in the manifest,
/// resolves them, and bundles them into the artifact.
///
/// # Arguments
/// * `spec_paths` - Paths to OpenAPI/AsyncAPI spec files
/// * `project_manifest` - The project manifest declaring available plugins
/// * `manifest_base_path` - Base path for resolving relative plugin paths
/// * `output` - Output path for the .bca artifact
/// * `options` - Compilation options
pub fn compile_with_manifest(
    spec_paths: &[&Path],
    project_manifest: &ProjectManifest,
    manifest_base_path: &Path,
    output: &Path,
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let specs = parse_specs(spec_paths)?;

    // Extract just the ApiSpec for validation
    let api_specs: Vec<ApiSpec> = specs.iter().map(|(spec, _, _)| spec.clone()).collect();

    // Validate all plugins are declared (E1040)
    project_manifest.validate_specs(&api_specs)?;

    // Resolve used plugins (loads WASM bytes)
    let resolved_plugins =
        project_manifest.resolve_used_plugins(&api_specs, manifest_base_path, options.no_cache)?;

    // Convert to PluginBundle
    let plugin_bundles: Vec<PluginBundle> = resolved_plugins
        .into_iter()
        .map(|p| PluginBundle {
            name: p.name,
            version: p.version.unwrap_or_else(|| "0.1.0".to_string()),
            plugin_type: p.plugin_type.unwrap_or_else(|| "plugin".to_string()),
            wasm_bytes: p.wasm_bytes,
            body_access: p.body_access,
        })
        .collect();

    compile_inner(&specs, &plugin_bundles, output, options)
}

/// Load a manifest from a .bca artifact.
pub fn load_manifest(artifact_path: &Path) -> Result<Manifest, CompileError> {
    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if path.to_str() == Some("manifest.json") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            let manifest: Manifest = serde_json::from_str(&content)?;
            return Ok(manifest);
        }
    }

    Err(CompileError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "manifest.json not found in artifact",
    )))
}

/// Load compiled routes from a .bca artifact.
pub fn load_routes(artifact_path: &Path) -> Result<CompiledRoutes, CompileError> {
    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        if path.to_str() == Some("routes.json") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            let routes: CompiledRoutes = serde_json::from_str(&content)?;
            return Ok(routes);
        }
    }

    Err(CompileError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "routes.json not found in artifact",
    )))
}

/// Load all source specs from a .bca artifact.
/// Returns a map of filename -> content.
pub fn load_specs(artifact_path: &Path) -> Result<HashMap<String, String>, CompileError> {
    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut specs = HashMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_str = entry.path()?.to_string_lossy().into_owned();

        if let Some(filename) = path_str.strip_prefix("specs/") {
            if !filename.is_empty() {
                let mut content = String::new();
                entry.read_to_string(&mut content)?;
                specs.insert(filename.to_string(), content);
            }
        }
    }

    Ok(specs)
}

/// Load all bundled plugins from a .bca artifact.
/// Returns a map of plugin name -> LoadedPlugin.
pub fn load_plugins(artifact_path: &Path) -> Result<HashMap<String, LoadedPlugin>, CompileError> {
    // First load manifest to get plugin metadata
    let manifest = load_manifest(artifact_path)?;

    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut plugins = HashMap::new();

    // Build a map of wasm_path -> plugin info from manifest
    let plugin_info: HashMap<String, (&BundledPlugin,)> = manifest
        .plugins
        .iter()
        .map(|p| (p.wasm_path.clone(), (p,)))
        .collect();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_str = entry.path()?.to_string_lossy().into_owned();

        if let Some((bundled,)) = plugin_info.get(&path_str) {
            let mut wasm_bytes = Vec::new();
            entry.read_to_end(&mut wasm_bytes)?;
            plugins.insert(
                bundled.name.clone(),
                LoadedPlugin {
                    version: bundled.version.clone(),
                    wasm_bytes,
                    body_access: bundled.capabilities.body_access,
                },
            );
        }
    }

    Ok(plugins)
}

/// A plugin to be bundled into an artifact.
pub struct PluginBundle {
    /// Plugin name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Plugin type ("middleware" or "dispatcher").
    pub plugin_type: String,
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
    /// Whether this plugin needs the request body.
    pub body_access: bool,
}

/// Parse spec files into (ApiSpec, content, sha256) tuples.
fn parse_specs(spec_paths: &[&Path]) -> Result<Vec<(ApiSpec, String, String)>, CompileError> {
    let mut specs = Vec::new();
    for path in spec_paths {
        let content = std::fs::read_to_string(path)?;
        let sha256 = compute_sha256(content.as_bytes());
        let spec = parse_spec_file(path)?;
        specs.push((spec, content, sha256));
    }
    Ok(specs)
}

/// Resolve middleware chain for an operation:
/// - None: use global middlewares only
/// - Some([]): explicit opt-out, no middlewares at all
/// - Some([items]): global middlewares (excluding any overridden by name) + operation-level
fn resolve_middlewares(
    global: &[MiddlewareConfig],
    operation: &Option<Vec<MiddlewareConfig>>,
) -> Vec<MiddlewareConfig> {
    match operation {
        None => global.to_vec(),
        Some(op_mw) if op_mw.is_empty() => Vec::new(),
        Some(op_mw) => {
            let op_names: HashSet<_> = op_mw.iter().map(|m| m.name.as_str()).collect();
            let mut merged: Vec<_> = global
                .iter()
                .filter(|m| !op_names.contains(m.name.as_str()))
                .cloned()
                .collect();
            merged.extend(op_mw.clone());
            merged
        }
    }
}

/// Shared compilation core: validates specs, builds operations, and writes the .bca archive.
fn compile_inner(
    specs: &[(ApiSpec, String, String)],
    plugins: &[PluginBundle],
    output: &Path,
    options: &CompileOptions,
) -> Result<CompileResult, CompileError> {
    let mut warnings: Vec<CompileWarning> = Vec::new();
    let mut operations: Vec<CompiledOperation> = Vec::new();
    let mut seen_routes: HashMap<(String, String), String> = HashMap::new();
    let mut seen_structural: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut seen_operation_ids: HashMap<String, String> = HashMap::new();

    // Extract root-level MCP config from first spec that has it
    let root_mcp_config = extract_root_mcp_config(specs);

    for (spec, _, _) in specs {
        let spec_file = spec.filename.as_deref().unwrap_or("unknown");

        // Validate global middlewares (E1011)
        for (idx, mw) in spec.global_middlewares.iter().enumerate() {
            if mw.name.is_empty() {
                return Err(CompileError::MissingMiddlewareName(format!(
                    "global middleware #{} in '{}'",
                    idx + 1,
                    spec_file
                )));
            }
        }

        // Check for unknown extensions at spec level (E1015 - warning)
        for key in spec.extensions.keys() {
            if key.starts_with("x-barbacane-") && !KNOWN_EXTENSIONS.contains(&key.as_str()) {
                warnings.push(CompileWarning {
                    code: "E1015".to_string(),
                    message: format!("unknown extension: {}", key),
                    location: Some(spec_file.to_string()),
                });
            }
        }

        for op in &spec.operations {
            let location = format!("{} {} in '{}'", op.method, op.path, spec_file);

            // Check for unknown extensions at operation level (E1015 - warning)
            for key in op.extensions.keys() {
                if key.starts_with("x-barbacane-") && !KNOWN_EXTENSIONS.contains(&key.as_str()) {
                    warnings.push(CompileWarning {
                        code: "E1015".to_string(),
                        message: format!("unknown extension: {}", key),
                        location: Some(location.clone()),
                    });
                }
            }

            // Validate path template syntax (E1054)
            validate_path_template(&op.path, &location)?;

            // Check for duplicate operationId (E1055)
            if let Some(ref op_id) = op.operation_id {
                if let Some(first_location) = seen_operation_ids.get(op_id) {
                    return Err(CompileError::DuplicateOperationId(
                        op_id.clone(),
                        format!("first at {}, duplicate at {}", first_location, location),
                    ));
                }
                seen_operation_ids.insert(op_id.clone(), location.clone());
            }

            // Check for routing conflicts (E1010)
            let key = (op.path.clone(), op.method.clone());
            if let Some(other_spec) = seen_routes.get(&key) {
                return Err(CompileError::RoutingConflict(format!(
                    "{} {} declared in both '{}' and '{}'",
                    op.method, op.path, other_spec, spec_file
                )));
            }
            seen_routes.insert(key, spec_file.to_string());

            // Check for ambiguous routes (E1050) - same structure, different param names
            let normalized = normalize_path_template(&op.path);
            let structural_key = (normalized, op.method.clone());
            if let Some((other_path, other_spec)) = seen_structural.get(&structural_key) {
                if other_path != &op.path {
                    return Err(CompileError::AmbiguousRoute(format!(
                        "'{}' and '{}' have same structure but different param names ({} in '{}' vs '{}')",
                        op.path, other_path, op.method, spec_file, other_spec
                    )));
                }
            }
            seen_structural.insert(structural_key, (op.path.clone(), spec_file.to_string()));

            // Check for missing dispatcher (E1020)
            let dispatch = op.dispatch.clone().ok_or_else(|| {
                CompileError::MissingDispatch(format!(
                    "{} {} in '{}'",
                    op.method, op.path, spec_file
                ))
            })?;

            // Check for plaintext HTTP upstream URLs (E1031)
            if !options.allow_plaintext {
                if let Some(url) = extract_upstream_url(&dispatch.config) {
                    if url.starts_with("http://") {
                        return Err(CompileError::PlaintextUpstream(format!(
                            "{} {} in '{}' - upstream URL: {}",
                            op.method, op.path, spec_file, url
                        )));
                    }
                }
            }

            let middlewares = resolve_middlewares(&spec.global_middlewares, &op.middlewares);

            // Validate middleware names (E1011)
            for (idx, mw) in middlewares.iter().enumerate() {
                if mw.name.is_empty() {
                    return Err(CompileError::MissingMiddlewareName(format!(
                        "middleware #{} in {}",
                        idx + 1,
                        location
                    )));
                }
            }

            // Validate schema complexity for parameters (E1051, E1052)
            // Note: circular $ref detection (E1053) is now performed at parse time.
            for param in &op.parameters {
                if let Some(schema) = &param.schema {
                    let param_location = format!("{} parameter '{}'", location, param.name);
                    validate_schema_complexity(
                        schema,
                        options.max_schema_depth,
                        options.max_schema_properties,
                        &param_location,
                    )?;
                }
            }

            // Validate request body schema complexity (E1051, E1052)
            if let Some(ref body) = op.request_body {
                for (content_type, content) in &body.content {
                    if let Some(schema) = &content.schema {
                        let body_location = format!("{} request body ({})", location, content_type);
                        validate_schema_complexity(
                            schema,
                            options.max_schema_depth,
                            options.max_schema_properties,
                            &body_location,
                        )?;
                    }
                }
            }

            // Resolve MCP enabled state for this operation
            let (mcp_enabled, mcp_description) =
                resolve_mcp_config(&root_mcp_config, op.extensions.get("x-barbacane-mcp"));

            // MCP warnings
            if mcp_enabled == Some(true) {
                if op.operation_id.is_none() {
                    warnings.push(CompileWarning {
                        code: "E1060".to_string(),
                        message: "operation without operationId cannot be exposed as MCP tool"
                            .to_string(),
                        location: Some(location.clone()),
                    });
                }
                if op.summary.is_none() && op.description.is_none() {
                    warnings.push(CompileWarning {
                        code: "E1061".to_string(),
                        message:
                            "MCP-enabled operation has no summary or description for tool metadata"
                                .to_string(),
                        location: Some(location.clone()),
                    });
                }
            }

            operations.push(CompiledOperation {
                index: operations.len(),
                path: op.path.clone(),
                method: op.method.clone(),
                operation_id: op.operation_id.clone(),
                summary: op.summary.clone(),
                description: op.description.clone(),
                parameters: op.parameters.clone(),
                request_body: op.request_body.clone(),
                dispatch,
                middlewares,
                deprecated: op.deprecated,
                sunset: op.sunset.clone(),
                messages: op.messages.clone(),
                bindings: op.bindings.clone(),
                responses: op.responses.clone(),
                mcp_enabled,
                mcp_description,
            });
        }
    }

    // Sort operations by (path, method) for deterministic output, then reassign indices
    operations.sort_by(|a, b| (&a.path, &a.method).cmp(&(&b.path, &b.method)));
    for (i, op) in operations.iter_mut().enumerate() {
        op.index = i;
    }

    // Build routes.json
    let routes = CompiledRoutes { operations };
    let routes_json = serde_json::to_string_pretty(&routes)?;
    let routes_sha256 = compute_sha256(routes_json.as_bytes());

    // Build plugin metadata
    let mut bundled_plugins = Vec::new();
    let mut checksums = BTreeMap::new();
    checksums.insert(
        "routes.json".to_string(),
        format!("sha256:{}", routes_sha256),
    );

    for plugin in plugins {
        let wasm_path = format!("plugins/{}.wasm", plugin.name);
        let sha256 = compute_sha256(&plugin.wasm_bytes);

        checksums.insert(wasm_path.clone(), format!("sha256:{}", sha256));

        bundled_plugins.push(BundledPlugin {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            plugin_type: plugin.plugin_type.clone(),
            wasm_path,
            sha256,
            capabilities: PluginCapabilities {
                body_access: plugin.body_access,
            },
        });
    }

    // Sort bundled_plugins by name for deterministic output
    bundled_plugins.sort_by(|a, b| a.name.cmp(&b.name));

    // Build manifest
    let mut source_specs: Vec<SourceSpec> = specs
        .iter()
        .map(|(spec, _, sha256)| SourceSpec {
            file: spec
                .filename
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            sha256: sha256.clone(),
            spec_type: match spec.format {
                SpecFormat::OpenApi => "openapi".to_string(),
                SpecFormat::AsyncApi => "asyncapi".to_string(),
            },
            version: spec.version.clone(),
        })
        .collect();

    // Sort source_specs by filename for deterministic output
    source_specs.sort_by(|a, b| a.file.cmp(&b.file));

    let artifact_hash = compute_artifact_hash(&source_specs, &checksums);

    let provenance = Provenance {
        commit: options.provenance_commit.clone(),
        source: options.provenance_source.clone(),
    };

    // Build MCP config for manifest, defaulting server_name/server_version from spec info
    let mcp = {
        let mut cfg = root_mcp_config.clone();
        if cfg.enabled {
            if cfg.server_name.is_none() {
                cfg.server_name = specs.first().map(|(s, _, _)| s.title.clone());
            }
            if cfg.server_version.is_none() {
                cfg.server_version = specs.first().map(|(s, _, _)| s.api_version.clone());
            }
        }
        cfg
    };

    let manifest = Manifest {
        barbacane_artifact_version: ARTIFACT_VERSION,
        compiled_at: now_utc_iso8601(),
        compiler_version: COMPILER_VERSION.to_string(),
        source_specs,
        routes_count: routes.operations.len(),
        checksums,
        plugins: bundled_plugins,
        artifact_hash,
        provenance,
        mcp,
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)?;

    // Create the .bca archive (tar.gz)
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest.json and routes.json
    add_file_to_tar(&mut archive, "manifest.json", manifest_json.as_bytes())?;
    add_file_to_tar(&mut archive, "routes.json", routes_json.as_bytes())?;

    // Add source specs under specs/ directory
    for (spec, content, _) in specs {
        let filename = spec
            .filename
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("spec.yaml");
        let archive_path = format!("specs/{}", filename);
        add_file_to_tar(&mut archive, &archive_path, content.as_bytes())?;
    }

    // Add plugins
    for plugin in plugins {
        let wasm_path = format!("plugins/{}.wasm", plugin.name);
        add_file_to_tar(&mut archive, &wasm_path, &plugin.wasm_bytes)?;
    }

    // Finish the archive
    let encoder = archive.into_inner()?;
    encoder.finish()?;

    // Sort warnings for deterministic output
    warnings.sort_by(|a, b| {
        (&a.location, &a.code, &a.message).cmp(&(&b.location, &b.code, &b.message))
    });

    Ok(CompileResult { manifest, warnings })
}

/// Compute SHA-256 hash of bytes.
fn compute_sha256(content: &[u8]) -> String {
    hex::encode(Sha256::new().chain_update(content).finalize())
}

/// Compute a combined artifact hash from all individual input checksums.
///
/// Produces a single SHA-256 that represents the entire artifact content by
/// hashing all source spec hashes and all checksums (routes + plugins) in
/// deterministic sorted order.
fn compute_artifact_hash(
    source_specs: &[SourceSpec],
    checksums: &BTreeMap<String, String>,
) -> String {
    let mut hasher = Sha256::new();
    // Source spec hashes (already sorted by filename before this call)
    for spec in source_specs {
        hasher.update(format!("source_spec:{}={}\n", spec.file, spec.sha256).as_bytes());
    }
    // Routes + plugin checksums (BTreeMap is sorted by key)
    for (key, value) in checksums {
        hasher.update(format!("{}={}\n", key, value).as_bytes());
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Add a file to a tar archive from bytes.
fn add_file_to_tar<W: Write>(
    archive: &mut Builder<W>,
    name: &str,
    content: &[u8],
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0); // Reproducible builds
    header.set_cksum();
    archive.append_data(&mut header, name, content)
}

/// Get current UTC timestamp in ISO 8601 format.
fn now_utc_iso8601() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Extract upstream URL from dispatch config, if present.
///
/// Looks for common URL fields in the dispatch config:
/// - `url` (e.g., for http-upstream dispatcher)
/// - `upstream` (alternative field name)
fn extract_upstream_url(config: &serde_json::Value) -> Option<String> {
    // Check for "url" field
    if let Some(url) = config.get("url").and_then(|v| v.as_str()) {
        return Some(url.to_string());
    }

    // Check for "upstream" field (which could be a URL or a name)
    if let Some(upstream) = config.get("upstream").and_then(|v| v.as_str()) {
        // Only return if it looks like a URL
        if upstream.starts_with("http://") || upstream.starts_with("https://") {
            return Some(upstream.to_string());
        }
    }

    None
}

/// Validate path template syntax (E1054).
///
/// Checks for:
/// - Balanced braces
/// - Non-empty parameter names
/// - Valid characters in parameter names (alphanumeric + underscore)
/// - Wildcard suffix `+` allowed only as the last character before `}`, and only on the final segment
/// - At most one wildcard parameter per path
/// - No duplicate parameter names in the same path
fn validate_path_template(path: &str, location: &str) -> Result<(), CompileError> {
    let mut seen_params: HashSet<String> = HashSet::new();
    let mut brace_depth = 0;
    let mut current_param = String::new();
    let mut in_param = false;
    let mut has_wildcard = false;

    // Split into segments to enforce "wildcard must be the last segment" rule.
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let last_segment = segments.last().copied().unwrap_or("");

    for ch in path.chars() {
        match ch {
            '{' => {
                if in_param {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - nested braces not allowed",
                        location
                    )));
                }
                brace_depth += 1;
                in_param = true;
            }
            '}' => {
                if !in_param {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - unmatched closing brace",
                        location
                    )));
                }
                brace_depth -= 1;
                in_param = false;

                let is_wildcard_param = current_param.ends_with('+');
                let base_name = if is_wildcard_param {
                    &current_param[..current_param.len() - 1]
                } else {
                    &current_param
                };

                if base_name.is_empty() {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - empty parameter name",
                        location
                    )));
                }

                if is_wildcard_param {
                    if has_wildcard {
                        return Err(CompileError::InvalidPathTemplate(format!(
                            "{} - at most one wildcard parameter ({{name+}}) allowed per path",
                            location
                        )));
                    }
                    // Wildcard must be the last segment
                    let param_segment = format!("{{{}}}", current_param);
                    if last_segment != param_segment {
                        return Err(CompileError::InvalidPathTemplate(format!(
                            "{} - wildcard parameter '{{{}}}' must be the last path segment",
                            location, current_param
                        )));
                    }
                    has_wildcard = true;
                }

                if !seen_params.insert(base_name.to_string()) {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - duplicate parameter '{}'",
                        location, base_name
                    )));
                }
                current_param.clear();
            }
            _ if in_param => {
                // Allow `+` only as the final character before `}` (wildcard suffix).
                // We check this lazily: accept `+` here but verify at `}` that it's last.
                if ch == '+' {
                    // Peek-ahead isn't available in a char iterator; we'll detect misplacement
                    // at `}` time by checking that `+` is the last char of current_param.
                    current_param.push(ch);
                } else if !ch.is_alphanumeric() && ch != '_' {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - invalid character '{}' in parameter name",
                        location, ch
                    )));
                } else if current_param.ends_with('+') {
                    // A non-`}` character after `+` means `+` was mid-name, not a suffix.
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - '+' is only allowed as the last character of a wildcard parameter name (e.g. {{key+}})",
                        location
                    )));
                } else {
                    current_param.push(ch);
                }
            }
            _ => {}
        }
    }

    if brace_depth != 0 {
        return Err(CompileError::InvalidPathTemplate(format!(
            "{} - unclosed brace",
            location
        )));
    }

    Ok(())
}

/// Normalize a path template for structural comparison (E1050).
///
/// Replaces parameter names with a placeholder while preserving the wildcard `+` modifier:
/// - `/users/{id}` -> `/users/{_}`
/// - `/files/{bucket}/{key+}` -> `/files/{_}/{_+}`
fn normalize_path_template(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut in_param = false;
    let mut is_wildcard = false;

    for ch in path.chars() {
        match ch {
            '{' => {
                result.push('{');
                result.push('_');
                in_param = true;
                is_wildcard = false;
            }
            '}' => {
                if is_wildcard {
                    result.push('+');
                }
                result.push('}');
                in_param = false;
                is_wildcard = false;
            }
            '+' if in_param => {
                // Mark as wildcard; the `+` is emitted at `}` time.
                is_wildcard = true;
            }
            _ if in_param => {
                // Skip parameter name characters
            }
            _ => {
                result.push(ch);
            }
        }
    }

    result
}

/// Validate schema complexity (E1051, E1052).
fn validate_schema_complexity(
    schema: &serde_json::Value,
    max_depth: usize,
    max_properties: usize,
    location: &str,
) -> Result<(), CompileError> {
    let (depth, props) = measure_schema_complexity(schema, 0);

    if depth > max_depth {
        return Err(CompileError::SchemaTooDeep(format!(
            "{} - depth {} exceeds limit {}",
            location, depth, max_depth
        )));
    }
    if props > max_properties {
        return Err(CompileError::SchemaTooComplex(format!(
            "{} - {} properties exceed limit {}",
            location, props, max_properties
        )));
    }
    Ok(())
}

/// Measure schema complexity: returns (max_depth, total_property_count).
fn measure_schema_complexity(value: &serde_json::Value, current_depth: usize) -> (usize, usize) {
    match value {
        serde_json::Value::Object(obj) => {
            let mut max_depth = current_depth;
            let mut total_props = 0;

            // Count properties in "properties" field
            if let Some(serde_json::Value::Object(props)) = obj.get("properties") {
                total_props += props.len();
                for prop_value in props.values() {
                    let (d, p) = measure_schema_complexity(prop_value, current_depth + 1);
                    max_depth = max_depth.max(d);
                    total_props += p;
                }
            }

            // Handle items (for arrays)
            if let Some(items) = obj.get("items") {
                let (d, p) = measure_schema_complexity(items, current_depth + 1);
                max_depth = max_depth.max(d);
                total_props += p;
            }

            // Handle allOf, oneOf, anyOf
            for key in ["allOf", "oneOf", "anyOf"] {
                if let Some(serde_json::Value::Array(schemas)) = obj.get(key) {
                    for schema in schemas {
                        let (d, p) = measure_schema_complexity(schema, current_depth + 1);
                        max_depth = max_depth.max(d);
                        total_props += p;
                    }
                }
            }

            // Handle additionalProperties if it's a schema
            if let Some(additional) = obj.get("additionalProperties") {
                if additional.is_object() {
                    let (d, p) = measure_schema_complexity(additional, current_depth + 1);
                    max_depth = max_depth.max(d);
                    total_props += p;
                }
            }

            (max_depth, total_props)
        }
        serde_json::Value::Array(arr) => {
            let mut max_depth = current_depth;
            let mut total_props = 0;
            for item in arr {
                let (d, p) = measure_schema_complexity(item, current_depth + 1);
                max_depth = max_depth.max(d);
                total_props += p;
            }
            (max_depth, total_props)
        }
        _ => (current_depth, 0),
    }
}

// Need to add hex encoding manually since we don't have the hex crate
mod hex {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        let bytes = bytes.as_ref();
        let mut result = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            result.push(HEX_CHARS[(byte >> 4) as usize] as char);
            result.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
        }
        result
    }
}

/// Extract root-level `x-barbacane-mcp` config from the first spec that defines it.
fn extract_root_mcp_config(specs: &[(ApiSpec, String, String)]) -> McpConfig {
    for (spec, _, _) in specs {
        if let Some(mcp_value) = spec.extensions.get("x-barbacane-mcp") {
            let enabled = mcp_value
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let server_name = mcp_value
                .get("server_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let server_version = mcp_value
                .get("server_version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return McpConfig {
                enabled,
                server_name,
                server_version,
            };
        }
    }
    McpConfig::default()
}

/// Resolve MCP enabled/description for a single operation from root + operation-level config.
fn resolve_mcp_config(
    root: &McpConfig,
    op_extension: Option<&serde_json::Value>,
) -> (Option<bool>, Option<String>) {
    if let Some(ext) = op_extension {
        let enabled = ext.get("enabled").and_then(|v| v.as_bool());
        let description = ext
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // Operation-level enabled wins; if not set, inherit from root
        let resolved_enabled = enabled.or(if root.enabled { Some(true) } else { None });
        (resolved_enabled, description)
    } else if root.enabled {
        (Some(true), None)
    } else {
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_spec(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn compile_minimal_spec() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        assert_eq!(result.manifest.barbacane_artifact_version, ARTIFACT_VERSION);
        assert_eq!(result.manifest.routes_count, 1);
        assert_eq!(result.manifest.source_specs.len(), 1);
        assert_eq!(result.manifest.source_specs[0].spec_type, "openapi");

        // Verify the artifact file was created
        assert!(output_path.exists());
    }

    #[test]
    fn compile_detects_missing_dispatch() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      operationId: getHealth
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::MissingDispatch(_))));
    }

    #[test]
    fn compile_detects_routing_conflict() {
        let temp = TempDir::new().unwrap();

        let spec1 = r#"
openapi: "3.1.0"
info:
  title: API 1
  version: "1.0.0"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec2 = r#"
openapi: "3.1.0"
info:
  title: API 2
  version: "1.0.0"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let path1 = create_test_spec(temp.path(), "api1.yaml", spec1);
        let path2 = create_test_spec(temp.path(), "api2.yaml", spec2);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[path1.as_path(), path2.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::RoutingConflict(_))));
    }

    #[test]
    fn load_artifact_manifest() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let loaded = load_manifest(&output_path).unwrap();
        assert_eq!(loaded.barbacane_artifact_version, ARTIFACT_VERSION);
        assert_eq!(loaded.routes_count, 1);
    }

    #[test]
    fn load_artifact_routes() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        assert_eq!(routes.operations.len(), 2);
    }

    #[test]
    fn compile_rejects_plaintext_http_url() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /proxy:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "http://backend.internal:8080/api"
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        // Default options reject plaintext HTTP
        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );
        assert!(matches!(result, Err(CompileError::PlaintextUpstream(_))));

        // With allow_plaintext, it should succeed
        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions {
                allow_plaintext: true,
                ..Default::default()
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn compile_allows_https_url() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /proxy:
    get:
      x-barbacane-dispatch:
        name: http-upstream
        config:
          url: "https://backend.internal:8080/api"
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        // HTTPS should be allowed by default
        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn compile_with_bundled_plugins() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        // Create a fake plugin (minimal valid WASM)
        let fake_wasm = vec![
            0x00, 0x61, 0x73, 0x6d, // magic
            0x01, 0x00, 0x00, 0x00, // version
        ];

        let plugins = vec![PluginBundle {
            name: "test-plugin".to_string(),
            version: "1.0.0".to_string(),
            plugin_type: "middleware".to_string(),
            wasm_bytes: fake_wasm.clone(),
            body_access: false,
        }];

        let result = compile(
            &[spec_path.as_path()],
            &plugins,
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        assert_eq!(result.manifest.plugins.len(), 1);
        assert_eq!(result.manifest.plugins[0].name, "test-plugin");
        assert_eq!(result.manifest.plugins[0].version, "1.0.0");
        assert_eq!(result.manifest.plugins[0].plugin_type, "middleware");
        assert_eq!(
            result.manifest.plugins[0].wasm_path,
            "plugins/test-plugin.wasm"
        );

        // Load plugins back
        let loaded = load_plugins(&output_path).unwrap();
        assert_eq!(loaded.len(), 1);
        let plugin = loaded.get("test-plugin").unwrap();
        assert_eq!(plugin.version, "1.0.0");
        assert_eq!(plugin.wasm_bytes, fake_wasm);
    }

    #[test]
    fn compile_asyncapi_spec() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
asyncapi: "3.0.0"
info:
  title: User Events API
  version: "1.0.0"
channels:
  userSignedUp:
    address: user/signedup
    messages:
      UserSignedUpMessage:
        contentType: application/json
        payload:
          type: object
          properties:
            userId:
              type: string
    bindings:
      kafka:
        topic: user-events
        partitions: 10
operations:
  processUserSignup:
    action: receive
    channel:
      $ref: '#/channels/userSignedUp'
    x-barbacane-dispatch:
      name: kafka
      config:
        topic: user-events
    bindings:
      kafka:
        groupId: user-processor
"#;
        let spec_path = create_test_spec(temp.path(), "events.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        assert_eq!(result.manifest.barbacane_artifact_version, ARTIFACT_VERSION);
        assert_eq!(result.manifest.routes_count, 1);
        assert_eq!(result.manifest.source_specs.len(), 1);
        assert_eq!(result.manifest.source_specs[0].spec_type, "asyncapi");

        // Load routes and verify AsyncAPI fields
        let routes = load_routes(&output_path).unwrap();
        assert_eq!(routes.operations.len(), 1);

        let op = &routes.operations[0];
        assert_eq!(op.path, "user/signedup");
        assert_eq!(op.method, "RECEIVE");
        assert_eq!(op.operation_id, Some("processUserSignup".to_string()));

        // Verify messages are preserved
        assert_eq!(op.messages.len(), 1);
        assert_eq!(op.messages[0].name, "UserSignedUpMessage");
        assert_eq!(
            op.messages[0].content_type,
            Some("application/json".to_string())
        );

        // Verify bindings are preserved (operation binding overrides channel)
        assert!(op.bindings.contains_key("kafka"));
        let kafka_binding = op.bindings.get("kafka").unwrap();
        assert_eq!(
            kafka_binding.get("groupId").and_then(|v| v.as_str()),
            Some("user-processor")
        );
    }

    #[test]
    fn compile_asyncapi_send_operation() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
asyncapi: "3.0.0"
info:
  title: Notification Service
  version: "1.0.0"
channels:
  notifications:
    address: notifications/{userId}
    parameters:
      userId:
        schema:
          type: string
    messages:
      NotificationMessage:
        contentType: application/json
        payload:
          type: object
          required:
            - title
          properties:
            title:
              type: string
operations:
  sendNotification:
    action: send
    channel:
      $ref: '#/channels/notifications'
    x-barbacane-dispatch:
      name: nats
      config:
        subject: notifications
"#;
        let spec_path = create_test_spec(temp.path(), "notifications.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        assert_eq!(result.manifest.routes_count, 1);

        let routes = load_routes(&output_path).unwrap();
        let op = &routes.operations[0];

        assert_eq!(op.path, "notifications/{userId}");
        assert_eq!(op.method, "SEND");

        // SEND operations should have channel parameters
        assert_eq!(op.parameters.len(), 1);
        assert_eq!(op.parameters[0].name, "userId");

        // SEND operations should have request_body from message payload
        assert!(op.request_body.is_some());
    }

    #[test]
    fn compile_detects_invalid_path_template_unclosed_brace() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{id:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::InvalidPathTemplate(_))));
    }

    #[test]
    fn compile_detects_invalid_path_template_empty_param() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::InvalidPathTemplate(_))));
    }

    #[test]
    fn compile_detects_invalid_path_template_duplicate_param() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{id}/posts/{id}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::InvalidPathTemplate(_))));
    }

    #[test]
    fn compile_detects_duplicate_operation_ids() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    get:
      operationId: getUsers
      x-barbacane-dispatch:
        name: mock
  /customers:
    get:
      operationId: getUsers
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(
            result,
            Err(CompileError::DuplicateOperationId(_, _))
        ));
    }

    #[test]
    fn compile_detects_missing_middleware_name() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-middlewares:
        - name: ""
          config: {}
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(
            result,
            Err(CompileError::MissingMiddlewareName(_))
        ));
    }

    #[test]
    fn compile_detects_missing_global_middleware_name() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: ""
    config: {}
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(
            result,
            Err(CompileError::MissingMiddlewareName(_))
        ));
    }

    #[test]
    fn compile_detects_ambiguous_routes() {
        let temp = TempDir::new().unwrap();

        let spec1 = r#"
openapi: "3.1.0"
info:
  title: API 1
  version: "1.0.0"
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec2 = r#"
openapi: "3.1.0"
info:
  title: API 2
  version: "1.0.0"
paths:
  /users/{userId}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let path1 = create_test_spec(temp.path(), "api1.yaml", spec1);
        let path2 = create_test_spec(temp.path(), "api2.yaml", spec2);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[path1.as_path(), path2.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::AmbiguousRoute(_))));
    }

    #[test]
    fn compile_allows_same_structure_same_params() {
        // Same path in different specs with same param names should work
        // (it's a routing conflict, not ambiguous)
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /users/{id}:
    get:
      x-barbacane-dispatch:
        name: mock
  /posts/{id}:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        // This should succeed - different paths, same param name is fine
        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn compile_detects_schema_too_deep() {
        let temp = TempDir::new().unwrap();

        // Create a deeply nested schema (40 levels, default limit is 32)
        let mut nested = r#"{"type": "string"}"#.to_string();
        for _ in 0..40 {
            nested = format!(
                r#"{{"type": "object", "properties": {{"nested": {}}}}}"#,
                nested
            );
        }

        let spec_content = format!(
            r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /test:
    post:
      x-barbacane-dispatch:
        name: mock
      requestBody:
        content:
          application/json:
            schema: {}
"#,
            nested
        );

        let spec_path = create_test_spec(temp.path(), "test.yaml", &spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::SchemaTooDeep(_))));
    }

    #[test]
    fn compile_detects_schema_too_complex() {
        let temp = TempDir::new().unwrap();

        // Create a schema with 300 properties (default limit is 256)
        let mut properties = String::new();
        for i in 0..300 {
            if i > 0 {
                properties.push_str(", ");
            }
            properties.push_str(&format!(r#""prop{}": {{"type": "string"}}"#, i));
        }

        let spec_content = format!(
            r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /test:
    post:
      x-barbacane-dispatch:
        name: mock
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                {{{}}}
"#,
            properties
        );

        let spec_path = create_test_spec(temp.path(), "test.yaml", &spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );

        assert!(matches!(result, Err(CompileError::SchemaTooComplex(_))));
    }

    #[test]
    fn compile_allows_schema_within_limits() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /test:
    post:
      x-barbacane-dispatch:
        name: mock
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                name:
                  type: string
                age:
                  type: integer
                address:
                  type: object
                  properties:
                    street:
                      type: string
                    city:
                      type: string
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn normalize_path_template_works() {
        assert_eq!(normalize_path_template("/users/{id}"), "/users/{_}");
        assert_eq!(normalize_path_template("/users/{userId}"), "/users/{_}");
        assert_eq!(
            normalize_path_template("/users/{id}/posts/{postId}"),
            "/users/{_}/posts/{_}"
        );
        assert_eq!(normalize_path_template("/static/path"), "/static/path");
        // Wildcard params preserve the `+` suffix in normalized form
        assert_eq!(normalize_path_template("/files/{path+}"), "/files/{_+}");
        assert_eq!(
            normalize_path_template("/files/{bucket}/{key+}"),
            "/files/{_}/{_+}"
        );
    }

    #[test]
    fn validate_path_template_valid_cases() {
        assert!(validate_path_template("/users", "test").is_ok());
        assert!(validate_path_template("/users/{id}", "test").is_ok());
        assert!(validate_path_template("/users/{user_id}", "test").is_ok());
        assert!(validate_path_template("/users/{id}/posts/{postId}", "test").is_ok());
        // Wildcard as sole param
        assert!(validate_path_template("/files/{path+}", "test").is_ok());
        // Wildcard after a regular param
        assert!(validate_path_template("/files/{bucket}/{key+}", "test").is_ok());
        // Wildcard after two regular params
        assert!(validate_path_template("/api/{version}/files/{rest+}", "test").is_ok());
    }

    #[test]
    fn validate_path_template_invalid_cases() {
        // Unclosed brace
        assert!(validate_path_template("/users/{id", "test").is_err());
        // Empty param
        assert!(validate_path_template("/users/{}", "test").is_err());
        // Duplicate param
        assert!(validate_path_template("/users/{id}/posts/{id}", "test").is_err());
        // Nested braces
        assert!(validate_path_template("/users/{{id}}", "test").is_err());
        // Invalid character in param name
        assert!(validate_path_template("/users/{id-name}", "test").is_err());
        // Wildcard not at end
        assert!(validate_path_template("/users/{id+}/orders", "test").is_err());
        // Multiple wildcards
        assert!(validate_path_template("/a/{x+}/{y+}", "test").is_err());
        // `+` in the middle of the name (not a suffix)
        assert!(validate_path_template("/users/{na+me}", "test").is_err());
        // Empty base name with wildcard only
        assert!(validate_path_template("/users/{+}", "test").is_err());
    }

    #[test]
    fn compile_inherits_global_middlewares_when_none() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 60
  - name: cors
    config:
      allow_origin: "*"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        assert_eq!(routes.operations.len(), 1);
        let op = &routes.operations[0];
        assert_eq!(op.middlewares.len(), 2);
        assert_eq!(op.middlewares[0].name, "rate-limit");
        assert_eq!(op.middlewares[1].name, "cors");
    }

    #[test]
    fn compile_empty_middlewares_opts_out_of_globals() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 60
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-middlewares: []
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        let op = &routes.operations[0];
        assert_eq!(op.middlewares.len(), 0);
    }

    #[test]
    fn compile_merges_global_and_operation_middlewares() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 60
  - name: cors
    config:
      allow_origin: "*"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-middlewares:
        - name: auth
          config:
            type: bearer
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        let op = &routes.operations[0];
        // Global middlewares first, then operation-level
        assert_eq!(op.middlewares.len(), 3);
        assert_eq!(op.middlewares[0].name, "rate-limit");
        assert_eq!(op.middlewares[1].name, "cors");
        assert_eq!(op.middlewares[2].name, "auth");
    }

    #[test]
    fn compile_operation_middleware_overrides_global_by_name() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 60
  - name: cors
    config:
      allow_origin: "*"
paths:
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
      x-barbacane-middlewares:
        - name: rate-limit
          config:
            quota: 1000
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        let op = &routes.operations[0];
        // cors from global + rate-limit from operation (overrides global rate-limit)
        assert_eq!(op.middlewares.len(), 2);
        assert_eq!(op.middlewares[0].name, "cors");
        assert_eq!(op.middlewares[1].name, "rate-limit");
        // The operation-level config should be used
        assert_eq!(op.middlewares[1].config.get("quota").unwrap(), 1000);
    }

    #[test]
    fn compile_inherits_global_middlewares() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: rate-limit
    config:
      quota: 60
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let plugins = vec![];
        let result = compile(
            &[spec_path.as_path()],
            &plugins,
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        let routes = load_routes(&output_path).unwrap();
        let op = &routes.operations[0];
        assert_eq!(op.middlewares.len(), 1);
        assert_eq!(op.middlewares[0].name, "rate-limit");
        assert_eq!(result.manifest.plugins.len(), 0);
    }

    #[test]
    fn artifact_hash_is_deterministic() {
        let source_specs = vec![
            SourceSpec {
                file: "api.yaml".to_string(),
                sha256: "aaa".to_string(),
                spec_type: "openapi".to_string(),
                version: "3.1.0".to_string(),
            },
            SourceSpec {
                file: "events.yaml".to_string(),
                sha256: "bbb".to_string(),
                spec_type: "asyncapi".to_string(),
                version: "3.0.0".to_string(),
            },
        ];
        let mut checksums = BTreeMap::new();
        checksums.insert("routes.json".to_string(), "ccc".to_string());
        checksums.insert("plugins/mock.wasm".to_string(), "ddd".to_string());

        let hash1 = compute_artifact_hash(&source_specs, &checksums);
        let hash2 = compute_artifact_hash(&source_specs, &checksums);

        assert_eq!(hash1, hash2, "Same inputs must produce same hash");
        assert!(
            hash1.starts_with("sha256:"),
            "Hash must have sha256: prefix"
        );
    }

    #[test]
    fn artifact_hash_differs_with_different_specs() {
        let specs_a = vec![SourceSpec {
            file: "api.yaml".to_string(),
            sha256: "aaa".to_string(),
            spec_type: "openapi".to_string(),
            version: "3.1.0".to_string(),
        }];
        let specs_b = vec![SourceSpec {
            file: "api.yaml".to_string(),
            sha256: "bbb".to_string(),
            spec_type: "openapi".to_string(),
            version: "3.1.0".to_string(),
        }];
        let checksums = BTreeMap::new();

        let hash_a = compute_artifact_hash(&specs_a, &checksums);
        let hash_b = compute_artifact_hash(&specs_b, &checksums);

        assert_ne!(
            hash_a, hash_b,
            "Different spec hashes must produce different artifact hashes"
        );
    }

    #[test]
    fn artifact_hash_differs_with_different_checksums() {
        let specs = vec![SourceSpec {
            file: "api.yaml".to_string(),
            sha256: "aaa".to_string(),
            spec_type: "openapi".to_string(),
            version: "3.1.0".to_string(),
        }];
        let mut checksums_a = BTreeMap::new();
        checksums_a.insert("routes.json".to_string(), "v1".to_string());
        let mut checksums_b = BTreeMap::new();
        checksums_b.insert("routes.json".to_string(), "v2".to_string());

        let hash_a = compute_artifact_hash(&specs, &checksums_a);
        let hash_b = compute_artifact_hash(&specs, &checksums_b);

        assert_ne!(
            hash_a, hash_b,
            "Different route checksums must produce different artifact hashes"
        );
    }

    #[test]
    fn provenance_serialization_round_trip() {
        let prov = Provenance {
            commit: Some("abc123".to_string()),
            source: Some("ci/github-actions".to_string()),
        };

        let json = serde_json::to_string(&prov).unwrap();
        let deserialized: Provenance = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.commit, Some("abc123".to_string()));
        assert_eq!(deserialized.source, Some("ci/github-actions".to_string()));
    }

    #[test]
    fn provenance_defaults_to_none() {
        let prov = Provenance::default();

        assert!(prov.commit.is_none());
        assert!(prov.source.is_none());

        // Round-trip with nulls
        let json = serde_json::to_string(&prov).unwrap();
        let deserialized: Provenance = serde_json::from_str(&json).unwrap();
        assert!(deserialized.commit.is_none());
        assert!(deserialized.source.is_none());
    }

    #[test]
    fn compile_produces_artifact_hash_and_provenance() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions {
                provenance_commit: Some("deadbeef".to_string()),
                provenance_source: Some("test".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        // Artifact hash is present and well-formed
        assert!(result.manifest.artifact_hash.starts_with("sha256:"));
        assert!(result.manifest.artifact_hash.len() > 10);

        // Provenance is present
        assert_eq!(
            result.manifest.provenance.commit,
            Some("deadbeef".to_string())
        );
        assert_eq!(result.manifest.provenance.source, Some("test".to_string()));

        // Round-trip: load manifest and verify
        let loaded = load_manifest(&output_path).unwrap();
        assert_eq!(loaded.artifact_hash, result.manifest.artifact_hash);
        assert_eq!(loaded.provenance.commit, Some("deadbeef".to_string()));
        assert_eq!(loaded.provenance.source, Some("test".to_string()));
    }

    #[test]
    fn compile_without_provenance_has_none_fields() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let output_path = temp.path().join("artifact.bca");

        let result = compile(
            &[spec_path.as_path()],
            &[],
            &output_path,
            &CompileOptions::default(),
        )
        .unwrap();

        // Hash is always present
        assert!(result.manifest.artifact_hash.starts_with("sha256:"));

        // Provenance fields are None when not provided
        assert!(result.manifest.provenance.commit.is_none());
        assert!(result.manifest.provenance.source.is_none());
    }

    #[test]
    fn same_spec_produces_same_artifact_hash() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);

        let out1 = temp.path().join("artifact1.bca");
        let out2 = temp.path().join("artifact2.bca");

        let r1 = compile(
            &[spec_path.as_path()],
            &[],
            &out1,
            &CompileOptions::default(),
        )
        .unwrap();
        let r2 = compile(
            &[spec_path.as_path()],
            &[],
            &out2,
            &CompileOptions::default(),
        )
        .unwrap();

        assert_eq!(
            r1.manifest.artifact_hash, r2.manifest.artifact_hash,
            "Compiling the same spec twice must produce the same artifact hash"
        );
    }

    #[test]
    fn different_specs_produce_different_artifact_hashes() {
        let temp = TempDir::new().unwrap();

        let spec_a = r#"
openapi: "3.1.0"
info:
  title: API A
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let spec_b = r#"
openapi: "3.1.0"
info:
  title: API B
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
  /users:
    get:
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#;
        let path_a = create_test_spec(temp.path(), "a.yaml", spec_a);
        let path_b = create_test_spec(temp.path(), "b.yaml", spec_b);
        let out_a = temp.path().join("a.bca");
        let out_b = temp.path().join("b.bca");

        let ra = compile(&[path_a.as_path()], &[], &out_a, &CompileOptions::default()).unwrap();
        let rb = compile(&[path_b.as_path()], &[], &out_b, &CompileOptions::default()).unwrap();

        assert_ne!(
            ra.manifest.artifact_hash, rb.manifest.artifact_hash,
            "Different specs must produce different artifact hashes"
        );
    }

    #[test]
    fn provenance_does_not_affect_artifact_hash() {
        let temp = TempDir::new().unwrap();

        let spec_content = r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
paths:
  /health:
    get:
      x-barbacane-dispatch:
        name: mock
"#;
        let spec_path = create_test_spec(temp.path(), "test.yaml", spec_content);
        let out1 = temp.path().join("a.bca");
        let out2 = temp.path().join("b.bca");

        let r1 = compile(
            &[spec_path.as_path()],
            &[],
            &out1,
            &CompileOptions {
                provenance_commit: Some("commit-a".to_string()),
                provenance_source: Some("source-a".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let r2 = compile(
            &[spec_path.as_path()],
            &[],
            &out2,
            &CompileOptions {
                provenance_commit: Some("commit-b".to_string()),
                provenance_source: Some("source-b".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            r1.manifest.artifact_hash, r2.manifest.artifact_hash,
            "Provenance metadata must not affect artifact hash"
        );
    }

    // --- MCP config tests ---

    #[test]
    fn extract_root_mcp_config_enabled() {
        let spec = ApiSpec {
            filename: None,
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "My API".to_string(),
            api_version: "2.0.0".to_string(),
            operations: vec![],
            global_middlewares: vec![],
            extensions: BTreeMap::from([(
                "x-barbacane-mcp".to_string(),
                serde_json::json!({
                    "enabled": true,
                    "server_name": "Custom Name"
                }),
            )]),
        };
        let specs = vec![(spec, String::new(), String::new())];
        let cfg = extract_root_mcp_config(&specs);
        assert!(cfg.enabled);
        assert_eq!(cfg.server_name.as_deref(), Some("Custom Name"));
        assert!(cfg.server_version.is_none());
    }

    #[test]
    fn extract_root_mcp_config_disabled_by_default() {
        let spec = ApiSpec {
            filename: None,
            format: SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "Test".to_string(),
            api_version: "1.0.0".to_string(),
            operations: vec![],
            global_middlewares: vec![],
            extensions: BTreeMap::new(),
        };
        let specs = vec![(spec, String::new(), String::new())];
        let cfg = extract_root_mcp_config(&specs);
        assert!(!cfg.enabled);
    }

    #[test]
    fn resolve_mcp_config_inherits_from_root() {
        let root = McpConfig {
            enabled: true,
            server_name: None,
            server_version: None,
        };
        // No operation-level extension → inherits root
        let (enabled, desc) = resolve_mcp_config(&root, None);
        assert_eq!(enabled, Some(true));
        assert!(desc.is_none());
    }

    #[test]
    fn resolve_mcp_config_operation_overrides_root() {
        let root = McpConfig {
            enabled: true,
            server_name: None,
            server_version: None,
        };
        // Operation opts out
        let ext = serde_json::json!({"enabled": false});
        let (enabled, _) = resolve_mcp_config(&root, Some(&ext));
        assert_eq!(enabled, Some(false));
    }

    #[test]
    fn resolve_mcp_config_operation_description_override() {
        let root = McpConfig {
            enabled: true,
            server_name: None,
            server_version: None,
        };
        let ext = serde_json::json!({"description": "Custom tool description"});
        let (enabled, desc) = resolve_mcp_config(&root, Some(&ext));
        // enabled not set at operation level → inherits root true
        assert_eq!(enabled, Some(true));
        assert_eq!(desc.as_deref(), Some("Custom tool description"));
    }

    #[test]
    fn resolve_mcp_config_root_disabled_no_inheritance() {
        let root = McpConfig {
            enabled: false,
            server_name: None,
            server_version: None,
        };
        let (enabled, _) = resolve_mcp_config(&root, None);
        assert!(enabled.is_none());
    }
}
