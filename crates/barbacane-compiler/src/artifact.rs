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
    SpecFormat,
};

use crate::error::{CompileError, CompileWarning};
use crate::manifest::ProjectManifest;

/// Current artifact format version.
pub const ARTIFACT_VERSION: u32 = 1;

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
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            allow_plaintext: false,
            max_schema_depth: 32,
            max_schema_properties: 256,
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
    #[serde(default)]
    pub plugins: Vec<BundledPlugin>,
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
    let resolved_plugins = project_manifest.resolve_used_plugins(&api_specs, manifest_base_path)?;

    // Convert to PluginBundle
    let plugin_bundles: Vec<PluginBundle> = resolved_plugins
        .into_iter()
        .map(|p| PluginBundle {
            name: p.name,
            version: p.version.unwrap_or_else(|| "0.1.0".to_string()),
            plugin_type: p.plugin_type.unwrap_or_else(|| "plugin".to_string()),
            wasm_bytes: p.wasm_bytes,
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
/// Returns a map of plugin name -> (version, WASM bytes).
pub fn load_plugins(
    artifact_path: &Path,
) -> Result<HashMap<String, (String, Vec<u8>)>, CompileError> {
    // First load manifest to get plugin metadata
    let manifest = load_manifest(artifact_path)?;

    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut plugins = HashMap::new();

    // Build a map of wasm_path -> (name, version) from manifest
    let plugin_info: HashMap<String, (String, String)> = manifest
        .plugins
        .iter()
        .map(|p| (p.wasm_path.clone(), (p.name.clone(), p.version.clone())))
        .collect();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_str = entry.path()?.to_string_lossy().into_owned();

        if let Some((name, version)) = plugin_info.get(&path_str) {
            let mut wasm_bytes = Vec::new();
            entry.read_to_end(&mut wasm_bytes)?;
            plugins.insert(name.clone(), (version.clone(), wasm_bytes));
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
}

/// Parse spec files into (ApiSpec, content, sha256) tuples.
fn parse_specs(spec_paths: &[&Path]) -> Result<Vec<(ApiSpec, String, String)>, CompileError> {
    let mut specs = Vec::new();
    for path in spec_paths {
        let content = std::fs::read_to_string(path)?;
        let sha256 = compute_sha256(&content);
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

            // Validate schema complexity and circular refs for parameters (E1051, E1052, E1053)
            for param in &op.parameters {
                if let Some(schema) = &param.schema {
                    let param_location = format!("{} parameter '{}'", location, param.name);
                    validate_schema_complexity(
                        schema,
                        options.max_schema_depth,
                        options.max_schema_properties,
                        &param_location,
                    )?;
                    let mut visited = HashSet::new();
                    detect_circular_refs(schema, schema, &mut visited, &param_location)?;
                }
            }

            // Validate request body schema complexity and circular refs (E1051, E1052, E1053)
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
                        let mut visited = HashSet::new();
                        detect_circular_refs(schema, schema, &mut visited, &body_location)?;
                    }
                }
            }

            operations.push(CompiledOperation {
                index: operations.len(),
                path: op.path.clone(),
                method: op.method.clone(),
                operation_id: op.operation_id.clone(),
                parameters: op.parameters.clone(),
                request_body: op.request_body.clone(),
                dispatch,
                middlewares,
                deprecated: op.deprecated,
                sunset: op.sunset.clone(),
                messages: op.messages.clone(),
                bindings: op.bindings.clone(),
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
    let routes_sha256 = compute_sha256(&routes_json);

    // Build plugin metadata
    let mut bundled_plugins = Vec::new();
    let mut checksums = BTreeMap::new();
    checksums.insert(
        "routes.json".to_string(),
        format!("sha256:{}", routes_sha256),
    );

    for plugin in plugins {
        let wasm_path = format!("plugins/{}.wasm", plugin.name);
        let sha256 = compute_sha256_bytes(&plugin.wasm_bytes);

        checksums.insert(wasm_path.clone(), format!("sha256:{}", sha256));

        bundled_plugins.push(BundledPlugin {
            name: plugin.name.clone(),
            version: plugin.version.clone(),
            plugin_type: plugin.plugin_type.clone(),
            wasm_path,
            sha256,
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

    let manifest = Manifest {
        barbacane_artifact_version: ARTIFACT_VERSION,
        compiled_at: chrono_lite_now(),
        compiler_version: COMPILER_VERSION.to_string(),
        source_specs,
        routes_count: routes.operations.len(),
        checksums,
        plugins: bundled_plugins,
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

/// Compute SHA-256 hash of a string.
fn compute_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

/// Compute SHA-256 hash of bytes.
fn compute_sha256_bytes(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    hex::encode(result)
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

/// Get current UTC timestamp in ISO 8601 format (without external crate).
fn chrono_lite_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();

    let secs = duration.as_secs();

    // Simple UTC timestamp calculation
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since 1970-01-01
    let mut year = 1970i32;
    let mut remaining_days = days as i32;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let days_in_months: [i32; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for days_in_month in days_in_months {
        if remaining_days < days_in_month {
            break;
        }
        remaining_days -= days_in_month;
        month += 1;
    }

    let day = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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
/// - No duplicate parameter names in the same path
fn validate_path_template(path: &str, location: &str) -> Result<(), CompileError> {
    let mut seen_params: HashSet<String> = HashSet::new();
    let mut brace_depth = 0;
    let mut current_param = String::new();
    let mut in_param = false;

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

                if current_param.is_empty() {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - empty parameter name",
                        location
                    )));
                }
                if !seen_params.insert(current_param.clone()) {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - duplicate parameter '{}'",
                        location, current_param
                    )));
                }
                current_param.clear();
            }
            _ if in_param => {
                if !ch.is_alphanumeric() && ch != '_' {
                    return Err(CompileError::InvalidPathTemplate(format!(
                        "{} - invalid character '{}' in parameter name",
                        location, ch
                    )));
                }
                current_param.push(ch);
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
/// Replaces parameter names with a placeholder: /users/{id} -> /users/{_}
fn normalize_path_template(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut in_param = false;

    for ch in path.chars() {
        match ch {
            '{' => {
                result.push('{');
                result.push('_');
                in_param = true;
            }
            '}' => {
                result.push('}');
                in_param = false;
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

/// Detect circular $ref references in a schema (E1053).
fn detect_circular_refs(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    visited: &mut HashSet<String>,
    location: &str,
) -> Result<(), CompileError> {
    if let Some(obj) = schema.as_object() {
        // Check for $ref
        if let Some(ref_val) = obj.get("$ref").and_then(|v| v.as_str()) {
            if !visited.insert(ref_val.to_string()) {
                return Err(CompileError::CircularSchemaRef(format!(
                    "{} - circular reference to '{}'",
                    location, ref_val
                )));
            }

            // Try to resolve and recurse
            if let Some(resolved) = resolve_json_ref(root, ref_val) {
                detect_circular_refs(resolved, root, visited, location)?;
            }

            visited.remove(ref_val);
        }

        // Recurse into nested schemas
        if let Some(serde_json::Value::Object(props)) = obj.get("properties") {
            for prop_value in props.values() {
                detect_circular_refs(prop_value, root, visited, location)?;
            }
        }

        if let Some(items) = obj.get("items") {
            detect_circular_refs(items, root, visited, location)?;
        }

        for key in ["allOf", "oneOf", "anyOf"] {
            if let Some(serde_json::Value::Array(schemas)) = obj.get(key) {
                for s in schemas {
                    detect_circular_refs(s, root, visited, location)?;
                }
            }
        }

        if let Some(additional) = obj.get("additionalProperties") {
            if additional.is_object() {
                detect_circular_refs(additional, root, visited, location)?;
            }
        }
    }

    Ok(())
}

/// Resolve a JSON reference (e.g., "#/components/schemas/User").
fn resolve_json_ref<'a>(
    root: &'a serde_json::Value,
    ref_path: &str,
) -> Option<&'a serde_json::Value> {
    if !ref_path.starts_with("#/") {
        return None;
    }

    let path = &ref_path[2..]; // Skip "#/"
    let mut current = root;

    for segment in path.split('/') {
        // Handle JSON Pointer escaping
        let unescaped = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&unescaped)?;
    }

    Some(current)
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
        let (version, wasm_bytes) = loaded.get("test-plugin").unwrap();
        assert_eq!(version, "1.0.0");
        assert_eq!(wasm_bytes, &fake_wasm);
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
    }

    #[test]
    fn validate_path_template_valid_cases() {
        assert!(validate_path_template("/users", "test").is_ok());
        assert!(validate_path_template("/users/{id}", "test").is_ok());
        assert!(validate_path_template("/users/{user_id}", "test").is_ok());
        assert!(validate_path_template("/users/{id}/posts/{postId}", "test").is_ok());
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
}
