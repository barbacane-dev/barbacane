use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Builder;

use barbacane_spec_parser::{
    parse_spec_file, ApiSpec, DispatchConfig, MiddlewareConfig, Parameter, RequestBody, SpecFormat,
};

use crate::error::CompileError;
use crate::manifest::ProjectManifest;

/// Current artifact format version.
pub const ARTIFACT_VERSION: u32 = 1;

/// Options for compilation.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Allow plaintext HTTP upstream URLs (development only).
    /// If false, compilation fails with E1031 for http:// URLs.
    pub allow_plaintext: bool,
}

/// Compiler version (from Cargo.toml).
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The manifest.json embedded in a .bca artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub barbacane_artifact_version: u32,
    pub compiled_at: String,
    pub compiler_version: String,
    pub source_specs: Vec<SourceSpec>,
    pub routes_count: usize,
    pub checksums: HashMap<String, String>,
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
    pub path: String,
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
}

/// Compile one or more spec files into a .bca artifact.
///
/// Uses default options (plaintext http:// URLs are not allowed).
pub fn compile(spec_paths: &[&Path], output: &Path) -> Result<Manifest, CompileError> {
    compile_with_options(spec_paths, output, &CompileOptions::default())
}

/// Compile one or more spec files into a .bca artifact with options.
///
/// Note: This function does NOT validate plugins against a manifest.
/// Use `compile_with_manifest` for manifest-aware compilation.
pub fn compile_with_options(
    spec_paths: &[&Path],
    output: &Path,
    options: &CompileOptions,
) -> Result<Manifest, CompileError> {
    // Parse all specs
    let mut specs: Vec<(ApiSpec, String, String)> = Vec::new(); // (spec, content, sha256)

    for path in spec_paths {
        let content = std::fs::read_to_string(path)?;
        let sha256 = compute_sha256(&content);
        let spec = parse_spec_file(path)?;
        specs.push((spec, content, sha256));
    }

    // Validate and collect operations
    let mut operations: Vec<CompiledOperation> = Vec::new();
    let mut seen_routes: HashMap<(String, String), String> = HashMap::new(); // (path, method) -> spec file

    for (spec, _, _) in &specs {
        let spec_file = spec.filename.as_deref().unwrap_or("unknown");

        for op in &spec.operations {
            // Check for routing conflicts
            let key = (op.path.clone(), op.method.clone());
            if let Some(other_spec) = seen_routes.get(&key) {
                return Err(CompileError::RoutingConflict(format!(
                    "{} {} declared in both '{}' and '{}'",
                    op.method, op.path, other_spec, spec_file
                )));
            }
            seen_routes.insert(key, spec_file.to_string());

            // Check for missing dispatcher
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

            // Resolve middleware chain: operation-level overrides global
            let middlewares = op
                .middlewares
                .clone()
                .unwrap_or_else(|| spec.global_middlewares.clone());

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
            });
        }
    }

    // Build routes.json
    let routes = CompiledRoutes { operations };
    let routes_json = serde_json::to_string_pretty(&routes)?;
    let routes_sha256 = compute_sha256(&routes_json);

    // Build manifest
    let source_specs: Vec<SourceSpec> = specs
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

    let mut checksums = HashMap::new();
    checksums.insert(
        "routes.json".to_string(),
        format!("sha256:{}", routes_sha256),
    );

    let manifest = Manifest {
        barbacane_artifact_version: ARTIFACT_VERSION,
        compiled_at: chrono_lite_now(),
        compiler_version: COMPILER_VERSION.to_string(),
        source_specs,
        routes_count: routes.operations.len(),
        checksums,
        plugins: Vec::new(), // No plugins bundled in basic compile
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)?;

    // Create the .bca archive (tar.gz)
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest.json
    add_file_to_tar(&mut archive, "manifest.json", manifest_json.as_bytes())?;

    // Add routes.json
    add_file_to_tar(&mut archive, "routes.json", routes_json.as_bytes())?;

    // Add source specs under specs/ directory
    for (spec, content, _) in &specs {
        let filename = spec
            .filename
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("spec.yaml");
        let archive_path = format!("specs/{}", filename);
        add_file_to_tar(&mut archive, &archive_path, content.as_bytes())?;
    }

    // Finish the archive
    let encoder = archive.into_inner()?;
    encoder.finish()?;

    Ok(manifest)
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
) -> Result<Manifest, CompileError> {
    // Parse all specs
    let mut specs: Vec<(ApiSpec, String, String)> = Vec::new();

    for path in spec_paths {
        let content = std::fs::read_to_string(path)?;
        let sha256 = compute_sha256(&content);
        let spec = parse_spec_file(path)?;
        specs.push((spec, content, sha256));
    }

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

    // Validate and collect operations
    let mut operations: Vec<CompiledOperation> = Vec::new();
    let mut seen_routes: HashMap<(String, String), String> = HashMap::new();

    for (spec, _, _) in &specs {
        let spec_file = spec.filename.as_deref().unwrap_or("unknown");

        for op in &spec.operations {
            let key = (op.path.clone(), op.method.clone());
            if let Some(other_spec) = seen_routes.get(&key) {
                return Err(CompileError::RoutingConflict(format!(
                    "{} {} declared in both '{}' and '{}'",
                    op.method, op.path, other_spec, spec_file
                )));
            }
            seen_routes.insert(key, spec_file.to_string());

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

            let middlewares = op
                .middlewares
                .clone()
                .unwrap_or_else(|| spec.global_middlewares.clone());

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
            });
        }
    }

    // Build routes.json
    let routes = CompiledRoutes { operations };
    let routes_json = serde_json::to_string_pretty(&routes)?;
    let routes_sha256 = compute_sha256(&routes_json);

    // Build plugin metadata
    let mut bundled_plugins = Vec::new();
    let mut checksums = HashMap::new();
    checksums.insert(
        "routes.json".to_string(),
        format!("sha256:{}", routes_sha256),
    );

    for plugin in &plugin_bundles {
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

    // Build manifest
    let source_specs: Vec<SourceSpec> = specs
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

    // Create the .bca archive
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest.json
    add_file_to_tar(&mut archive, "manifest.json", manifest_json.as_bytes())?;

    // Add routes.json
    add_file_to_tar(&mut archive, "routes.json", routes_json.as_bytes())?;

    // Add source specs
    for (spec, content, _) in &specs {
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
    for plugin in &plugin_bundles {
        let wasm_path = format!("plugins/{}.wasm", plugin.name);
        add_file_to_tar(&mut archive, &wasm_path, &plugin.wasm_bytes)?;
    }

    // Finish the archive
    let encoder = archive.into_inner()?;
    encoder.finish()?;

    Ok(manifest)
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

/// Compile specs with bundled plugins into a .bca artifact.
pub fn compile_with_plugins(
    spec_paths: &[&Path],
    plugins: &[PluginBundle],
    output: &Path,
) -> Result<Manifest, CompileError> {
    // Parse all specs
    let mut specs: Vec<(ApiSpec, String, String)> = Vec::new();

    for path in spec_paths {
        let content = std::fs::read_to_string(path)?;
        let sha256 = compute_sha256(&content);
        let spec = parse_spec_file(path)?;
        specs.push((spec, content, sha256));
    }

    // Validate and collect operations
    let mut operations: Vec<CompiledOperation> = Vec::new();
    let mut seen_routes: HashMap<(String, String), String> = HashMap::new();

    for (spec, _, _) in &specs {
        let spec_file = spec.filename.as_deref().unwrap_or("unknown");

        for op in &spec.operations {
            let key = (op.path.clone(), op.method.clone());
            if let Some(other_spec) = seen_routes.get(&key) {
                return Err(CompileError::RoutingConflict(format!(
                    "{} {} declared in both '{}' and '{}'",
                    op.method, op.path, other_spec, spec_file
                )));
            }
            seen_routes.insert(key, spec_file.to_string());

            let dispatch = op.dispatch.clone().ok_or_else(|| {
                CompileError::MissingDispatch(format!(
                    "{} {} in '{}'",
                    op.method, op.path, spec_file
                ))
            })?;

            let middlewares = op
                .middlewares
                .clone()
                .unwrap_or_else(|| spec.global_middlewares.clone());

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
            });
        }
    }

    // Build routes.json
    let routes = CompiledRoutes { operations };
    let routes_json = serde_json::to_string_pretty(&routes)?;
    let routes_sha256 = compute_sha256(&routes_json);

    // Build plugin metadata
    let mut bundled_plugins = Vec::new();
    let mut checksums = HashMap::new();
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

    // Build manifest
    let source_specs: Vec<SourceSpec> = specs
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

    // Create the .bca archive
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest.json
    add_file_to_tar(&mut archive, "manifest.json", manifest_json.as_bytes())?;

    // Add routes.json
    add_file_to_tar(&mut archive, "routes.json", routes_json.as_bytes())?;

    // Add source specs
    for (spec, content, _) in &specs {
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

    Ok(manifest)
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

        let manifest = compile(&[spec_path.as_path()], &output_path).unwrap();

        assert_eq!(manifest.barbacane_artifact_version, ARTIFACT_VERSION);
        assert_eq!(manifest.routes_count, 1);
        assert_eq!(manifest.source_specs.len(), 1);
        assert_eq!(manifest.source_specs[0].spec_type, "openapi");

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

        let result = compile(&[spec_path.as_path()], &output_path);

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

        let result = compile(&[path1.as_path(), path2.as_path()], &output_path);

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

        compile(&[spec_path.as_path()], &output_path).unwrap();

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

        compile(&[spec_path.as_path()], &output_path).unwrap();

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
        let result = compile(&[spec_path.as_path()], &output_path);
        assert!(matches!(result, Err(CompileError::PlaintextUpstream(_))));

        // With allow_plaintext, it should succeed
        let result = compile_with_options(
            &[spec_path.as_path()],
            &output_path,
            &CompileOptions {
                allow_plaintext: true,
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
        let result = compile(&[spec_path.as_path()], &output_path);
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

        let manifest =
            compile_with_plugins(&[spec_path.as_path()], &plugins, &output_path).unwrap();

        assert_eq!(manifest.plugins.len(), 1);
        assert_eq!(manifest.plugins[0].name, "test-plugin");
        assert_eq!(manifest.plugins[0].version, "1.0.0");
        assert_eq!(manifest.plugins[0].plugin_type, "middleware");
        assert_eq!(manifest.plugins[0].wasm_path, "plugins/test-plugin.wasm");

        // Load plugins back
        let loaded = load_plugins(&output_path).unwrap();
        assert_eq!(loaded.len(), 1);
        let (version, wasm_bytes) = loaded.get("test-plugin").unwrap();
        assert_eq!(version, "1.0.0");
        assert_eq!(wasm_bytes, &fake_wasm);
    }
}
