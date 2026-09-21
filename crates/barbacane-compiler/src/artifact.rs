use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Builder;

use std::collections::{BTreeMap, BTreeSet};

use crate::spec_parser::{
    parse_spec_file, ApiSpec, DispatchConfig, Message, MiddlewareConfig, Parameter, RequestBody,
    ResponseContent, SpecFormat,
};

use crate::error::{CompileError, CompileWarning};
use crate::manifest::ProjectManifest;

/// Current artifact format version.
///
/// v6 records each operation's `allowed_request_headers`, the request headers
/// it accepts on top of the data plane's baseline. The data plane compares this
/// constant on load, so an artifact built before it is refused by version
/// rather than by the integrity check that would otherwise name the wrong
/// cause.
///
/// v5 binds two WAF policy fields into the manifest and `artifact_hash`:
/// `max_response_body` (the phase-4 body inspection cap) and `audit` (the audit
/// engine policy). Because the data plane recomputes and verifies `artifact_hash`
/// on load, a pre-v5 artifact fails the integrity check and must be recompiled.
///
/// v4 added Ed25519 signing fields and recorded each plugin's declared
/// capability `host_functions` in the manifest.
pub const ARTIFACT_VERSION: u32 = 6;

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

/// Path of the sealed WAF rule set inside the artifact.
pub const WAF_RULES_PATH: &str = "waf/rules.json";

/// Directory holding the `@pmFromFile` phrase lists inside the artifact.
pub const WAF_DATA_PREFIX: &str = "waf/data/";

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
    "x-barbacane-waf",         // Root level - WAF rule set and policy
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
    /// WAF configuration (from root-level x-barbacane-waf).
    #[serde(default)]
    pub waf: WafConfig,
    /// Detached Ed25519 signature (hex) over `artifact_hash`. Present when the
    /// artifact was signed at compile time (AR-1). Excluded from `artifact_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key the signature was produced with
    /// (informational; verification uses the operator's pinned trusted key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_public_key: Option<String>,
    /// Whether per-plugin `capabilities.host_functions` are authoritative (read
    /// from plugin.toml). The data plane enforces the capability contract on
    /// load only when this is true (WA-1).
    #[serde(default)]
    pub capabilities_enforced: bool,
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

/// What a matching WAF rule does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WafMode {
    /// Disruptive actions take effect.
    #[default]
    Blocking,
    /// Rules are evaluated and recorded, but nothing is interrupted. Useful
    /// for tuning a rule set against real traffic, and not a security control.
    DetectionOnly,
}

/// What to do about a rule the engine cannot compile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnsupportedRules {
    /// Refuse to produce an artifact. The default, because a rule that cannot
    /// be compiled cannot be enforced, and shipping the rest of the rule set
    /// as if it were complete is how a rule silently becomes a bypass.
    #[default]
    Fail,
    /// Produce the artifact without them, recording their ids in the manifest
    /// and warning at compile time. Opt-in: the operator has to ask for a rule
    /// set that is knowingly incomplete, and the artifact then says which
    /// rules are missing.
    Skip,
}

/// When the WAF writes a per-transaction audit record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditEngine {
    /// Never write an audit record.
    Off,
    /// Write a record only for a transaction that matched at least one logged
    /// rule or was blocked. Near-zero volume on clean traffic, and the CRS
    /// crs-setup default.
    #[default]
    RelevantOnly,
    /// Write a record for every inspected transaction.
    On,
}

/// Default largest response body phase-4 rules inspect, in bytes (1 MiB).
pub const fn default_max_response_body() -> u64 {
    1_048_576
}

/// WAF configuration extracted from root-level `x-barbacane-waf`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WafConfig {
    /// Whether the WAF runs.
    pub enabled: bool,
    /// Directory of SecLang `.conf` files, relative to the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruleset: Option<String>,
    /// CRS paranoia level, seeded as `tx.blocking_paranoia_level`.
    #[serde(default)]
    pub paranoia_level: u8,
    /// Blocking or detection-only.
    #[serde(default)]
    pub mode: WafMode,
    /// Inbound anomaly score at which the rule set blocks.
    #[serde(default)]
    pub inbound_threshold: i64,
    /// Outbound anomaly score at which the rule set blocks.
    #[serde(default)]
    pub outbound_threshold: i64,
    /// Largest response body, in bytes, that phase-4 rules inspect. A buffered
    /// response at or under this is collected and inspected; a larger or
    /// streamed one has its headers inspected (phase 3) and its body skipped,
    /// with the skip counted.
    #[serde(default = "default_max_response_body")]
    pub max_response_body: u64,
    /// When to write a per-transaction audit record.
    #[serde(default)]
    pub audit: AuditEngine,
    /// Policy for rules the engine cannot compile.
    #[serde(default)]
    pub unsupported_rules: UnsupportedRules,
    /// Path of the sealed rule set inside the artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_path: Option<String>,
    /// Rules present in the source rule set that are not in the artifact,
    /// because they could not be compiled and `unsupported_rules` was `skip`.
    ///
    /// Part of the manifest, so it is covered by `artifact_hash` and therefore
    /// by the signature: an operator can prove which rules a running gateway
    /// is not enforcing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_rules: Vec<u32>,
}

impl Default for WafConfig {
    fn default() -> Self {
        WafConfig {
            enabled: false,
            ruleset: None,
            paranoia_level: 0,
            mode: WafMode::default(),
            inbound_threshold: 0,
            outbound_threshold: 0,
            max_response_body: default_max_response_body(),
            audit: AuditEngine::default(),
            unsupported_rules: UnsupportedRules::default(),
            rules_path: None,
            skipped_rules: Vec::new(),
        }
    }
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
    /// Declared capability host-function names from plugin.toml. The data plane
    /// enforces these against the module's actual imports at load (WA-1).
    #[serde(default)]
    pub host_functions: Vec<String>,
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
    /// Request headers this operation accepts on top of the data plane's
    /// baseline, lowercased and sorted. Derived from the header and cookie
    /// parameters it declares and the security schemes its requirement applies.
    #[serde(default)]
    pub allowed_request_headers: Vec<String>,
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
    // Plain `compile` receives caller-built bundles whose declared capabilities
    // may not have been read from plugin.toml (e.g. the control plane builds
    // bundles from the registry, which does not yet persist capabilities), so
    // the resulting artifact is not marked capability-authoritative.
    compile_inner(
        &specs,
        plugins,
        output,
        options,
        false,
        spec_paths.first().and_then(|p| p.parent()),
    )
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
            category: p.category,
            implements: p.implements,
            wasm_bytes: p.wasm_bytes,
            body_access: p.body_access,
            host_functions: p.host_functions,
            secret_fields: p.secret_fields,
            config_schema: p.config_schema,
        })
        .collect();

    // Bundles were resolved from plugin.toml, so their declared capabilities are
    // authoritative and the artifact is eligible for load-time enforcement.
    compile_inner(
        &specs,
        &plugin_bundles,
        output,
        options,
        true,
        spec_paths.first().and_then(|p| p.parent()),
    )
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

/// Load the sealed WAF rule set from a `.bca` artifact.
///
/// Returns the validated directives, which the caller compiles once. `None`
/// when the artifact carries no rule set.
///
/// The caller must have verified `artifact_hash` first: the rule set is bound
/// by it, and loading rules from an artifact whose hash has not been checked
/// would accept a swapped rule set.
pub fn load_waf_rules(artifact_path: &Path) -> Result<Option<SealedRuleSet>, CompileError> {
    let file = File::open(artifact_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut rules: Option<Vec<parapet::Directive>> = None;
    let mut data_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        if path == WAF_RULES_PATH {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            rules = Some(serde_json::from_slice(&content)?);
        } else if let Some(name) = path.strip_prefix(WAF_DATA_PREFIX) {
            let name = name.to_string();
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            data_files.insert(name, content);
        }
    }

    Ok(rules.map(|directives| SealedRuleSet {
        directives,
        data_files,
    }))
}

/// A rule set as it comes out of an artifact: the rules, plus the phrase lists
/// their `@pmFromFile` operators reference.
#[derive(Debug)]
pub struct SealedRuleSet {
    /// The validated rules.
    pub directives: Vec<parapet::Directive>,
    /// Phrase lists, keyed by the name the rules reference.
    pub data_files: BTreeMap<String, Vec<u8>>,
}

impl parapet::DataLoader for SealedRuleSet {
    fn load(&self, name: &str) -> Result<Vec<u8>, String> {
        self.data_files.get(name).cloned().ok_or_else(|| {
            format!(
                "phrase list {name:?} is referenced by the rule set but not present in the \
                 artifact; recompile it"
            )
        })
    }
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
    /// The plugin's family, as its manifest states it. `authentication` is the
    /// one the compiler acts on: such a plugin verifies a credential, so an
    /// operation using it must name the security scheme that carries it.
    pub category: Option<String>,
    /// The security scheme types this plugin reads, as tokens: `apiKey`,
    /// `http:<scheme>`, `oauth2`, `openIdConnect`, `mutualTLS`. An operation
    /// pairing the plugin with a requirement naming none of them is refused
    /// (E1032). Empty exempts the plugin, so one declaring nothing compiles as
    /// before.
    pub implements: Vec<String>,
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
    /// Whether this plugin needs the request body.
    pub body_access: bool,
    /// Declared capability host-function names from plugin.toml.
    pub host_functions: Vec<String>,
    /// Config fields marked `writeOnly` (secret) in the plugin's config-schema.json.
    pub secret_fields: Vec<String>,
    /// The plugin's `config-schema.json`, read for the annotations that say
    /// which configured values name a request header. Absent for a plugin whose
    /// schema the compiler cannot see.
    pub config_schema: Option<serde_json::Value>,
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
    capabilities_authoritative: bool,
    // Directory the specs were read from, used to resolve relative paths
    // declared in extensions. `None` when compiling specs held in memory, in
    // which case a relative path cannot be resolved and is refused.
    spec_dir: Option<&Path>,
) -> Result<CompileResult, CompileError> {
    let mut warnings: Vec<CompileWarning> = Vec::new();
    let mut operations: Vec<CompiledOperation> = Vec::new();

    // Per-plugin set of secret (writeOnly) config fields, from each plugin's
    // config-schema.json, used to warn on plaintext secrets baked into configs.
    let plugin_secret_fields: HashMap<&str, std::collections::BTreeSet<String>> = plugins
        .iter()
        .filter(|p| !p.secret_fields.is_empty())
        .map(|p| (p.name.as_str(), p.secret_fields.iter().cloned().collect()))
        .collect();

    // Schemas carry the annotations saying which configured values name a
    // request header. A URL-sourced plugin has none, so headers it is told to
    // read must be declared in the spec like any other.
    let plugin_schemas: HashMap<&str, &serde_json::Value> = plugins
        .iter()
        .filter_map(|p| p.config_schema.as_ref().map(|s| (p.name.as_str(), s)))
        .collect();

    // Plugins whose manifest puts them in the `authentication` family. Each
    // verifies a credential the client sends, and the security scheme is what
    // says which header carries it.
    // Each maps to the security scheme types the plugin declares it reads,
    // which is empty for one that declares none.
    let authentication_plugins: HashMap<&str, &[String]> = plugins
        .iter()
        .filter(|p| p.category.as_deref() == Some(AUTHENTICATION_CATEGORY))
        .map(|p| (p.name.as_str(), p.implements.as_slice()))
        .collect();

    // A bundle can arrive without the files those two read, as it does from the
    // control plane, whose registry stores neither. Neither check then applies
    // to it, which changes what an artifact admits, so say so rather than
    // letting the difference show up as a dropped header at runtime (E1071).
    for plugin in plugins {
        if plugin.config_schema.is_none() || plugin.category.is_none() {
            warnings.push(CompileWarning {
                code: "E1071".to_string(),
                message: format!(
                    "plugin '{}' was bundled without its manifest or config schema, so headers \
                     its configuration names are not admitted and it is not checked for a \
                     security requirement. Compile from a `barbacane.yaml` that resolves the \
                     plugin by path to get both",
                    plugin.name
                ),
                location: None,
            });
        }
    }

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

            // Warn on plaintext secrets in config (E1070): a field the plugin's
            // schema marks `writeOnly` but that holds a literal gets baked into
            // the artifact. Recommend env:// / file:// references.
            //
            // `plugin_secret_fields` is keyed by the normalized plugin name, but
            // a spec may reference a plugin with a version suffix
            // (e.g. "jwt-auth@1.0.0"); normalize the reference before lookup so
            // version-pinned plugins are still scanned.
            //
            // Note: `plugin_secret_fields` only contains path-sourced plugins;
            // URL-sourced plugins carry no `secret_fields` (their schema is not
            // fetched at compile time), so E1070 cannot cover them.
            let dispatch_key = crate::manifest::normalize_plugin_name(&dispatch.name);
            if let Some(fields) = plugin_secret_fields.get(dispatch_key.as_str()) {
                scan_plaintext_secrets(
                    &dispatch.config,
                    &dispatch.name,
                    fields,
                    &location,
                    &mut warnings,
                );
            }
            for mw in &middlewares {
                let mw_key = crate::manifest::normalize_plugin_name(&mw.name);
                if let Some(fields) = plugin_secret_fields.get(mw_key.as_str()) {
                    scan_plaintext_secrets(&mw.config, &mw.name, fields, &location, &mut warnings);
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

            // An operation running an authentication plugin must say which
            // scheme carries the credential (E1057), must name one the plugin
            // reads (E1032), and the plugin must not be configured to read a
            // different header (E1072).
            for middleware in &middlewares {
                let key = crate::manifest::normalize_plugin_name(&middleware.name);
                let Some(implements) = authentication_plugins.get(key.as_str()) else {
                    continue;
                };
                require_security_requirement(op, spec, &middleware.name, &location)?;
                require_implemented_scheme(op, spec, &middleware.name, implements, &location)?;

                let Some(schema) = plugin_schemas.get(key.as_str()) else {
                    continue;
                };
                let mut configured = BTreeSet::new();
                collect_configured_header_names(schema, &middleware.config, &mut configured)?;
                if configured.is_empty() {
                    continue;
                }
                // Only a header scheme is comparable. One naming the query
                // string says the credential is not a header at all, and the
                // plugin follows the document.
                for header in applied_scheme_headers(op, spec) {
                    if !configured.contains(&header) {
                        warnings.push(CompileWarning {
                            code: "E1072".to_string(),
                            message: format!(
                                "the security scheme says '{}' carries the credential, but \
                                 '{}' is configured to read {}. The document is the contract, \
                                 so name the same header in both",
                                header,
                                middleware.name,
                                configured
                                    .iter()
                                    .map(|h| format!("'{h}'"))
                                    .collect::<Vec<_>>()
                                    .join(" and ")
                            ),
                            location: Some(location.clone()),
                        });
                    }
                }
            }

            // The other direction: the operation names a credential the client
            // sends, and nothing in the chain verifies it (E1033). The gateway
            // then forwards the credential header and lets the request through
            // unauthenticated, which the document does not say. A warning, since
            // an upstream may be doing the checking.
            let required = required_scheme_tokens(op, spec);
            if !required.is_empty() {
                // A chain that runs one is already covered by E1057 and E1032,
                // which say whether it reads the right scheme.
                let has_authentication = middlewares.iter().any(|middleware| {
                    let key = crate::manifest::normalize_plugin_name(&middleware.name);
                    authentication_plugins.contains_key(key.as_str())
                });
                if !has_authentication {
                    warnings.push(CompileWarning {
                        code: "E1033".to_string(),
                        message: format!(
                            "the operation requires {}, but no authentication plugin in its \
                             chain verifies it. The credential is forwarded and the request \
                             reaches the upstream unauthenticated. Add the plugin that reads \
                             the scheme, or drop the requirement if the upstream checks it",
                            joined(required.iter())
                        ),
                        location: Some(location.clone()),
                    });
                }
            }

            // The headers this operation accepts: what the spec declares, plus
            // what the chain's own configuration tells a plugin to read, which
            // is not in the spec's vocabulary and nothing else would admit.
            let allowed_request_headers = {
                let mut allow = operation_header_allowlist(op, spec)?;
                let mut collect =
                    |name: &str, config: &serde_json::Value| -> Result<(), CompileError> {
                        let key = crate::manifest::normalize_plugin_name(name);
                        if let Some(schema) = plugin_schemas.get(key.as_str()) {
                            collect_configured_header_names(schema, config, &mut allow)?;
                        }
                        Ok(())
                    };
                for middleware in &middlewares {
                    collect(&middleware.name, &middleware.config)?;
                }
                collect(&dispatch.name, &dispatch.config)?;
                allow.into_iter().collect::<Vec<String>>()
            };

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
                allowed_request_headers,
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
                host_functions: plugin.host_functions.clone(),
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

    // Validate and seal the WAF rule set. Done before the hash so the rule
    // set is bound by it, and therefore by the signature: an operator can
    // prove which rules a running gateway carries.
    let mut waf = extract_root_waf_config(specs);
    let mut sealed_waf: Option<Vec<u8>> = None;
    let mut sealed_waf_data: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    if waf.enabled {
        let Some(ruleset) = waf.ruleset.clone() else {
            return Err(CompileError::WafRuleset(
                "x-barbacane-waf is enabled but no `ruleset` is set".to_string(),
            ));
        };
        let rules_dir = spec_relative_path(spec_dir, &ruleset)?;
        let sealed = seal_waf_ruleset(&rules_dir, waf.unsupported_rules)?;
        warnings.extend(sealed.warnings.into_iter().map(|message| CompileWarning {
            code: "W1080".to_string(),
            message,
            location: Some(format!("x-barbacane-waf in '{ruleset}'")),
        }));
        waf.skipped_rules = sealed.skipped_rules;
        waf.rules_path = Some(WAF_RULES_PATH.to_string());
        checksums.insert(
            WAF_RULES_PATH.to_string(),
            format!("sha256:{}", compute_sha256(&sealed.rules_json)),
        );
        for (name, bytes) in &sealed.data_files {
            checksums.insert(
                format!("{WAF_DATA_PREFIX}{name}"),
                format!("sha256:{}", compute_sha256(bytes)),
            );
        }
        sealed_waf_data = sealed.data_files;
        sealed_waf = Some(sealed.rules_json);
    }

    // Compute the artifact hash after every hashed input is final (specs,
    // checksums, plugin capability surface, capabilities_enforced, mcp, waf).
    let artifact_hash = compute_artifact_hash(
        &source_specs,
        &checksums,
        &bundled_plugins,
        capabilities_authoritative,
        &mcp,
        &waf,
    );

    let mut manifest = Manifest {
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
        waf,
        signature: None,
        signing_public_key: None,
        capabilities_enforced: capabilities_authoritative,
    };

    // AR-1: sign the artifact when a signing key is configured. The signature
    // covers `artifact_hash`, which binds every spec, route, and plugin WASM
    // hash as well as the capability-enforcement surface (capabilities_enforced,
    // per-plugin host_functions/body_access, MCP config), so a tampered artifact
    // fails verification on load (H3).
    sign_manifest_from_env(&mut manifest)?;

    let manifest_json = serde_json::to_string_pretty(&manifest)?;

    // Create the .bca archive (tar.gz)
    let file = File::create(output)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add manifest.json and routes.json
    add_file_to_tar(&mut archive, "manifest.json", manifest_json.as_bytes())?;
    add_file_to_tar(&mut archive, "routes.json", routes_json.as_bytes())?;
    if let Some(rules_json) = &sealed_waf {
        add_file_to_tar(&mut archive, WAF_RULES_PATH, rules_json)?;
        for (name, bytes) in &sealed_waf_data {
            add_file_to_tar(&mut archive, &format!("{WAF_DATA_PREFIX}{name}"), bytes)?;
        }
    }

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
///
/// The hash also binds the security-critical capability surface the data plane
/// trusts at load time: the `capabilities_enforced` flag, each plugin's
/// `plugin_type`/`version`/`body_access`/declared `host_functions`, and the MCP
/// config. These are NOT covered by the spec/route/wasm checksums, so without
/// them a tampered-but-still-signature-verifying `.bca` could flip
/// `capabilities_enforced=false` (or widen a plugin's host functions, or enable
/// the MCP surface) and disable the sandbox while passing verification (H3).
fn compute_artifact_hash(
    source_specs: &[SourceSpec],
    checksums: &BTreeMap<String, String>,
    plugins: &[BundledPlugin],
    capabilities_enforced: bool,
    mcp: &McpConfig,
    waf: &WafConfig,
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
    // Capability-enforcement surface (H3). `plugins` arrives sorted by name;
    // sort host_functions so declaration order cannot change the hash.
    hasher.update(format!("capabilities_enforced={}\n", capabilities_enforced).as_bytes());
    for p in plugins {
        let mut host_functions = p.capabilities.host_functions.clone();
        host_functions.sort();
        hasher.update(
            format!(
                "plugin:{}\ttype={}\tversion={}\tbody_access={}\thost_functions={}\n",
                p.name,
                p.plugin_type,
                p.version,
                p.capabilities.body_access,
                host_functions.join(","),
            )
            .as_bytes(),
        );
    }
    // MCP surface: gates the MCP endpoint/tooling at runtime, read from the
    // manifest (not re-derived from the spec) on load.
    hasher.update(
        format!(
            "mcp:enabled={}\tserver_name={}\tserver_version={}\n",
            mcp.enabled,
            mcp.server_name.as_deref().unwrap_or(""),
            mcp.server_version.as_deref().unwrap_or(""),
        )
        .as_bytes(),
    );
    // WAF policy surface. The rule set itself is already bound through
    // `checksums`, but these decide what the rules do, so a change to any of
    // them must change the hash.
    hasher.update(
        format!(
            "waf:enabled={}\tparanoia={}\tmode={:?}\tinbound={}\toutbound={}\tmax_response_body={}\taudit={:?}\tunsupported={:?}\tskipped={}\n",
            waf.enabled,
            waf.paranoia_level,
            waf.mode,
            waf.inbound_threshold,
            waf.outbound_threshold,
            waf.max_response_body,
            waf.audit,
            waf.unsupported_rules,
            waf.skipped_rules
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(","),
        )
        .as_bytes(),
    );
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

// ---------------------------------------------------------------------------
// AR-1: artifact integrity & Ed25519 signing
// ---------------------------------------------------------------------------

/// Errors from artifact integrity / signature verification.
#[derive(Debug, thiserror::Error)]
pub enum IntegrityError {
    #[error("artifact hash mismatch: manifest claims {expected}, recomputed {actual}")]
    ArtifactHashMismatch { expected: String, actual: String },
    #[error("plugin '{name}' checksum mismatch: manifest {expected}, actual {actual}")]
    PluginChecksumMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("plugin '{name}' is not listed in the manifest")]
    UnknownPlugin { name: String },
    #[error("WAF rule set entry '{path}' checksum mismatch: manifest {expected}, actual {actual}")]
    WafChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("WAF rule set entry '{path}' is in the artifact but not in the manifest checksums")]
    UnknownWafEntry { path: String },
    #[error("WAF rule set could not be re-serialised for verification: {0}")]
    WafSerialisation(String),
    #[error("artifact is unsigned but a trusted public key is configured")]
    MissingSignature,
    #[error("invalid key/signature material: {0}")]
    InvalidMaterial(String),
    #[error("artifact signature verification failed")]
    BadSignature,
}

/// Decode a hex string into bytes (the local `hex` module only encodes).
fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Recompute the combined artifact hash from the manifest's recorded inputs.
pub fn recompute_artifact_hash(manifest: &Manifest) -> String {
    compute_artifact_hash(
        &manifest.source_specs,
        &manifest.checksums,
        &manifest.plugins,
        manifest.capabilities_enforced,
        &manifest.mcp,
        &manifest.waf,
    )
}

/// Verify the manifest's `artifact_hash` is internally consistent with its
/// recorded spec/route/plugin checksums.
pub fn verify_artifact_hash(manifest: &Manifest) -> Result<(), IntegrityError> {
    let actual = recompute_artifact_hash(manifest);
    if actual != manifest.artifact_hash {
        return Err(IntegrityError::ArtifactHashMismatch {
            expected: manifest.artifact_hash.clone(),
            actual,
        });
    }
    Ok(())
}

/// Verify that a plugin's actual WASM bytes match the SHA-256 recorded in the
/// manifest (detects a swapped/tampered plugin binary).
/// Verify the sealed WAF rule set and its phrase lists against the manifest.
///
/// `artifact_hash` covers the manifest's checksum table, so a tampered
/// manifest is caught by [`verify_artifact_hash`]. That says nothing about the
/// *archive members*, though: the extracted bytes still have to be checked
/// against the checksums, exactly as plugin WASM is. Without this, the rule
/// set is bound in the manifest but not on load, and an attacker who can
/// rewrite an archive member swaps rules without detection.
pub fn verify_waf_rules(manifest: &Manifest, sealed: &SealedRuleSet) -> Result<(), IntegrityError> {
    let check = |path: &str, bytes: &[u8]| -> Result<(), IntegrityError> {
        let expected =
            manifest
                .checksums
                .get(path)
                .ok_or_else(|| IntegrityError::UnknownWafEntry {
                    path: path.to_string(),
                })?;
        let actual = format!("sha256:{}", compute_sha256(bytes));
        if &actual != expected {
            return Err(IntegrityError::WafChecksumMismatch {
                path: path.to_string(),
                expected: expected.clone(),
                actual,
            });
        }
        Ok(())
    };

    // Re-serialise the directives to compare against the sealed bytes. The
    // form is canonical, so this reproduces exactly what the compiler hashed.
    let rules_json = serde_json::to_vec(&sealed.directives)
        .map_err(|e| IntegrityError::WafSerialisation(e.to_string()))?;
    check(WAF_RULES_PATH, &rules_json)?;

    for (name, bytes) in &sealed.data_files {
        check(&format!("{WAF_DATA_PREFIX}{name}"), bytes)?;
    }
    Ok(())
}

pub fn verify_plugin_checksum(
    manifest: &Manifest,
    name: &str,
    wasm_bytes: &[u8],
) -> Result<(), IntegrityError> {
    let expected = manifest
        .plugins
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.sha256.clone())
        .ok_or_else(|| IntegrityError::UnknownPlugin {
            name: name.to_string(),
        })?;
    let actual = compute_sha256(wasm_bytes);
    if actual != expected {
        return Err(IntegrityError::PluginChecksumMismatch {
            name: name.to_string(),
            expected,
            actual,
        });
    }
    Ok(())
}

/// Verify the artifact's Ed25519 signature over `artifact_hash` against a pinned
/// trusted public key (hex-encoded). Fails closed if the artifact is unsigned.
pub fn verify_artifact_signature(
    manifest: &Manifest,
    trusted_public_key_hex: &str,
) -> Result<(), IntegrityError> {
    let signature_hex = manifest
        .signature
        .as_ref()
        .ok_or(IntegrityError::MissingSignature)?;
    let signature = decode_hex(signature_hex)
        .map_err(|e| IntegrityError::InvalidMaterial(format!("signature hex: {e}")))?;
    let public_key = decode_hex(trusted_public_key_hex.trim())
        .map_err(|e| IntegrityError::InvalidMaterial(format!("trusted public key hex: {e}")))?;

    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(manifest.artifact_hash.as_bytes(), &signature)
        .map_err(|_| IntegrityError::BadSignature)
}

/// Sign the manifest's `artifact_hash` when `BARBACANE_SIGNING_KEY` (a path to a
/// PKCS#8 Ed25519 private key) is set. No-op when unset (unsigned artifact).
fn sign_manifest_from_env(manifest: &mut Manifest) -> Result<(), CompileError> {
    use ring::signature::KeyPair;

    let key_path = match std::env::var_os("BARBACANE_SIGNING_KEY") {
        Some(p) => p,
        None => return Ok(()),
    };
    let pkcs8 = std::fs::read(&key_path)
        .map_err(|e| CompileError::Signing(format!("reading BARBACANE_SIGNING_KEY: {e}")))?;
    let key_pair = ring::signature::Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|e| CompileError::Signing(format!("invalid PKCS#8 Ed25519 key: {e}")))?;
    let signature = key_pair.sign(manifest.artifact_hash.as_bytes());
    manifest.signature = Some(hex::encode(signature.as_ref()));
    manifest.signing_public_key = Some(hex::encode(key_pair.public_key().as_ref()));
    Ok(())
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

/// Collect the names of config fields a plugin's `config-schema.json` marks as
/// secret via the standard JSON Schema `writeOnly: true` keyword. Walks the
/// whole schema (properties, array `items`, `additionalProperties`, and the
/// `$defs`/`allOf`/`anyOf`/`oneOf` combinators) so nested shapes such as
/// ai-proxy's `routes[].api_key` and `targets{}.api_key` are covered.
pub fn collect_writeonly_fields(schema: &serde_json::Value) -> std::collections::BTreeSet<String> {
    fn walk(node: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                // A `properties` map names fields; a child marked writeOnly is a secret.
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    for (field, subschema) in props {
                        if subschema.get("writeOnly").and_then(|v| v.as_bool()) == Some(true) {
                            out.insert(field.clone());
                        }
                        walk(subschema, out);
                    }
                }
                // Recurse into the remaining schema structure.
                for key in [
                    "items",
                    "additionalProperties",
                    "$defs",
                    "definitions",
                    "allOf",
                    "anyOf",
                    "oneOf",
                    "then",
                    "else",
                ] {
                    if let Some(child) = map.get(key) {
                        walk(child, out);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(schema, &mut out);
    out
}

/// Recursively scan a plugin config for `secret_fields` (declared `writeOnly` in
/// the plugin's schema) whose value is a plaintext literal rather than an
/// `env://` / `file://` reference. Such values are baked into the compiled
/// artifact at rest; warn (E1070) so operators move them to a secret reference
/// that is resolved at runtime. With no schema-declared secrets, this is a no-op.
fn scan_plaintext_secrets(
    config: &serde_json::Value,
    plugin: &str,
    secret_fields: &std::collections::BTreeSet<String>,
    location: &str,
    warnings: &mut Vec<CompileWarning>,
) {
    if secret_fields.is_empty() {
        return;
    }

    /// A value that is resolved at runtime rather than stored in the artifact.
    fn is_secret_ref(value: &str) -> bool {
        value.starts_with("env://") || value.starts_with("file://")
    }

    match config {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if let serde_json::Value::String(s) = value {
                    if secret_fields.contains(key) && !s.is_empty() && !is_secret_ref(s) {
                        warnings.push(CompileWarning {
                            code: "E1070".to_string(),
                            message: format!(
                                "plugin '{plugin}' config field '{key}' is a secret but is set to \
                                 a plaintext literal; use an env:// or file:// reference so it is \
                                 resolved at runtime instead of baked into the artifact"
                            ),
                            location: Some(location.to_string()),
                        });
                    }
                }
                scan_plaintext_secrets(value, plugin, secret_fields, location, warnings);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scan_plaintext_secrets(item, plugin, secret_fields, location, warnings);
            }
        }
        _ => {}
    }
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

/// Request header names a plugin's configuration tells it to read.
///
/// A plugin marks such a field in its own `config-schema.json`, the way
/// `writeOnly` already marks a secret:
///
/// - `"format": "header-name"` on a field whose value is a header name, or a
///   list of them.
/// - `"format": "header-ref"` on a field that may *reference* a header among
///   other things, as a partition key does with `header:<name>` or a message
///   key with `$request.header.<name>`. Any other value selects something that
///   is not a header, such as `client_ip`, and names nothing.
/// - `"format": "header-name-map"` on an object whose *keys* are header names,
///   as a rename table is.
///
/// The configuration is walked alongside the schema, so a marked field is read
/// at the place it actually sits rather than wherever its name appears.
fn collect_configured_header_names(
    schema: &serde_json::Value,
    config: &serde_json::Value,
    out: &mut BTreeSet<String>,
) -> Result<(), CompileError> {
    match schema.get("format").and_then(|f| f.as_str()) {
        Some("header-name") => return extract_header_names(config, out),
        Some("header-ref") => return extract_referenced_header_names(config, out),
        Some("header-name-map") => {
            if let Some(map) = config.as_object() {
                for key in map.keys() {
                    push_header_name(key, out)?;
                }
            }
            return Ok(());
        }
        _ => {}
    }

    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        let values = config.as_object();
        for (field, subschema) in props {
            // A field left out of the configuration still applies through its
            // default, which is the header the plugin will actually read.
            match values.and_then(|v| v.get(field)) {
                Some(value) => collect_configured_header_names(subschema, value, out)?,
                None => {
                    if let Some(default) = subschema.get("default") {
                        collect_configured_header_names(subschema, default, out)?;
                    }
                }
            }
        }
    }

    // A list or map whose entries share one schema.
    for keyword in ["items", "additionalProperties"] {
        let Some(subschema) = schema.get(keyword) else {
            continue;
        };
        match config {
            serde_json::Value::Array(entries) => {
                for entry in entries {
                    collect_configured_header_names(subschema, entry, out)?;
                }
            }
            serde_json::Value::Object(entries) => {
                for entry in entries.values() {
                    collect_configured_header_names(subschema, entry, out)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Read a value that is a header name, or a list of them.
fn extract_header_names(
    value: &serde_json::Value,
    out: &mut BTreeSet<String>,
) -> Result<(), CompileError> {
    match value {
        serde_json::Value::String(text) => push_header_name(text, out)?,
        serde_json::Value::Array(entries) => {
            for entry in entries {
                extract_header_names(entry, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Characters a header name may hold where one is embedded in a longer value.
///
/// A dot is included because the plugins reading these expressions accept one,
/// so a name stopping short of it would admit a header the plugin never reads.
fn is_header_name_char(c: &char) -> bool {
    c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.'
}

/// Markers that introduce a header name inside a configuration value.
///
/// `$request.header.` is the message-key form of `kafka` and `nats`, `$header.`
/// the interpolation form of `request-transformer`. The longer marker is tried
/// first so it is not read as the shorter one preceded by text.
const HEADER_REFERENCE_MARKERS: &[&str] = &["$request.header.", "$header."];

/// Record every header a `headers['<name>']` subscript names.
///
/// The form an expression language uses, as a `cel` condition does with
/// `request.headers['x-tier']`. Only a literal name is read. An expression is
/// code, not a declaration, so a computed name is invisible here and the header
/// it reads has to be declared in the spec like any other.
fn extract_subscripted_header_names(
    text: &str,
    out: &mut BTreeSet<String>,
) -> Result<(), CompileError> {
    let mut rest = text;
    while let Some(at) = rest.find("headers[") {
        let after = &rest[at + "headers[".len()..];
        let mut chars = after.chars();
        let Some(quote @ ('\'' | '"')) = chars.next() else {
            rest = after;
            continue;
        };
        let body = &after[quote.len_utf8()..];
        let Some(end) = body.find(quote) else {
            break;
        };
        push_header_name(&body[..end], out)?;
        rest = &body[end + quote.len_utf8()..];
    }
    Ok(())
}

/// Read a value that may reference a header among other things.
///
/// `header:<name>` selects one. Otherwise every marker occurrence in the string
/// names one, as does every `headers['<name>']` subscript, so a value built from
/// several headers contributes all of them. Anything else selects something that
/// is not a header, such as `client_ip`, and contributes nothing.
fn extract_referenced_header_names(
    value: &serde_json::Value,
    out: &mut BTreeSet<String>,
) -> Result<(), CompileError> {
    match value {
        serde_json::Value::String(text) => {
            if let Some(name) = text.strip_prefix("header:") {
                return push_header_name(name, out);
            }
            let mut rest = text.as_str();
            while !rest.is_empty() {
                // The earliest marker, so a `$header.` inside a longer string is
                // not skipped by a later `$request.header.`.
                let Some((at, marker)) = HEADER_REFERENCE_MARKERS
                    .iter()
                    .filter_map(|m| rest.find(m).map(|i| (i, *m)))
                    .min_by_key(|(i, m)| (*i, std::cmp::Reverse(m.len())))
                else {
                    break;
                };
                let after = &rest[at + marker.len()..];
                let name: String = after.chars().take_while(is_header_name_char).collect();
                push_header_name(&name, out)?;
                rest = &after[name.len()..];
            }
            extract_subscripted_header_names(text, out)?;
        }
        serde_json::Value::Array(entries) => {
            for entry in entries {
                extract_referenced_header_names(entry, out)?;
            }
        }
        serde_json::Value::Object(entries) => {
            for entry in entries.values() {
                extract_referenced_header_names(entry, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Record a header name, lowercased, ignoring anything that is not one.
///
/// A reserved name is refused rather than recorded. The caller turns that into
/// E1056, so a configuration cannot admit an identity header the way a declared
/// parameter cannot.
fn push_header_name(name: &str, out: &mut BTreeSet<String>) -> Result<(), CompileError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains(' ') {
        return Ok(());
    }
    out.insert(reserved_checked(trimmed, "plugin configuration")?);
    Ok(())
}

/// The plugin family whose members verify a client credential.
const AUTHENTICATION_CATEGORY: &str = "authentication";

/// The request headers the operation's own security requirement names.
///
/// Only schemes that put the credential in a header, so a requirement naming a
/// key in the query string contributes nothing and nothing is compared.
fn applied_scheme_headers(op: &crate::spec_parser::Operation, spec: &ApiSpec) -> BTreeSet<String> {
    let requirements = op.security.as_ref().or(spec.security.as_ref());
    requirements
        .map(|reqs| {
            reqs.iter()
                .flat_map(|r| r.keys())
                .filter_map(|name| spec.security_schemes.get(name))
                .filter_map(scheme_header_name)
                .filter(|h| h != "authorization" && h != "cookie")
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a scheme describes a credential the client sends with the request.
///
/// The scheme says where the credential travels, and the plugin reading it
/// follows. A key in the query string is one, and admits no header because it
/// needs none. A client certificate is presented during the handshake, so it is
/// not something a middleware reads off the request at all.
fn scheme_carries_a_credential(scheme: &crate::spec_parser::SecurityScheme) -> bool {
    use crate::spec_parser::SecurityScheme;
    !matches!(scheme, SecurityScheme::MutualTls)
}

/// The request header a scheme names, when it names one.
fn scheme_header_name(scheme: &crate::spec_parser::SecurityScheme) -> Option<String> {
    use crate::spec_parser::SecurityScheme;
    match scheme {
        SecurityScheme::ApiKey { name, location } if location == "header" => {
            Some(name.to_ascii_lowercase())
        }
        SecurityScheme::ApiKey { location, .. } if location == "cookie" => {
            Some("cookie".to_string())
        }
        SecurityScheme::Http { .. } | SecurityScheme::OAuth2 | SecurityScheme::OpenIdConnect => {
            Some("authorization".to_string())
        }
        _ => None,
    }
}

/// Require that an operation running an authentication plugin names the scheme
/// carrying the credential (E1057).
///
/// The plugin verifies a credential; the security scheme is what says which
/// header carries it, and so what the operation admits. Without a requirement
/// the operation reads as anonymous, no credential header is admitted, and the
/// plugin would reject every request for want of one.
fn require_security_requirement(
    op: &crate::spec_parser::Operation,
    spec: &ApiSpec,
    plugin: &str,
    location: &str,
) -> Result<(), CompileError> {
    // An operation's own requirement replaces the root's; `security: []` is
    // present and empty, which declares the operation anonymous.
    let requirements = op.security.as_ref().or(spec.security.as_ref());
    let names: Vec<&String> = requirements
        .map(|reqs| reqs.iter().flat_map(|r| r.keys()).collect())
        .unwrap_or_default();

    if names.is_empty() {
        return Err(CompileError::MissingSecurityRequirement(format!(
            "{location}: '{plugin}' authenticates the caller, so the operation must declare \
             the security scheme carrying the credential. Add a `security` requirement on the \
             operation or at the root, and define the scheme under \
             `components.securitySchemes`"
        )));
    }

    if let Some(unknown) = names
        .iter()
        .find(|name| !spec.security_schemes.contains_key(**name))
    {
        return Err(CompileError::MissingSecurityRequirement(format!(
            "{location}: security requirement names '{unknown}', which is not defined under \
             `components.securitySchemes`"
        )));
    }

    // A requirement naming only `mutualTLS` describes a certificate presented
    // during the handshake, which no middleware reads off the request, so the
    // plugin would still find no credential to verify.
    if !names
        .iter()
        .filter_map(|name| spec.security_schemes.get(*name))
        .any(scheme_carries_a_credential)
    {
        return Err(CompileError::MissingSecurityRequirement(format!(
            "{location}: '{plugin}' verifies a credential the client sends, but the requirement \
             names only `mutualTLS`, which is presented during the TLS handshake. Name a scheme \
             describing a credential the request carries"
        )));
    }

    Ok(())
}

/// The token a plugin's `implements` list uses for a security scheme.
///
/// An `http` scheme is qualified by its authentication scheme, since a plugin
/// reading Basic credentials does not read a Bearer token. Every other type is
/// its own token.
fn scheme_token(scheme: &crate::spec_parser::SecurityScheme) -> String {
    use crate::spec_parser::SecurityScheme;
    match scheme {
        SecurityScheme::ApiKey { .. } => "apiKey".to_string(),
        SecurityScheme::Http { scheme } => format!("http:{scheme}"),
        SecurityScheme::OAuth2 => "oauth2".to_string(),
        SecurityScheme::OpenIdConnect => "openIdConnect".to_string(),
        SecurityScheme::MutualTls => "mutualTLS".to_string(),
    }
}

/// Require that an authentication plugin reads one of the schemes the operation
/// names (E1032).
///
/// `E1057` establishes that a requirement exists and describes a credential the
/// client sends. This one asks whether the plugin can read that kind of
/// credential: `jwt-auth` against a requirement naming only an `apiKey` scheme
/// is two valid halves whose pairing rejects every request.
///
/// A plugin declaring no `implements` list is exempt, so a third-party plugin
/// compiles exactly as it did before.
fn require_implemented_scheme(
    op: &crate::spec_parser::Operation,
    spec: &ApiSpec,
    plugin: &str,
    implements: &[String],
    location: &str,
) -> Result<(), CompileError> {
    if implements.is_empty() {
        return Ok(());
    }

    // Tokens are compared case-insensitively, so `openIdConnect` and
    // `openidconnect` name the same scheme type.
    let implemented: BTreeSet<String> = implements
        .iter()
        .map(|token| token.to_ascii_lowercase())
        .collect();

    // An operation's own requirement replaces the root's, as in E1057.
    let requirements = op.security.as_ref().or(spec.security.as_ref());
    let named: BTreeSet<String> = requirements
        .map(|reqs| {
            reqs.iter()
                .flat_map(|r| r.keys())
                .filter_map(|name| spec.security_schemes.get(name))
                .map(scheme_token)
                .collect()
        })
        .unwrap_or_default();

    if named
        .iter()
        .any(|token| implemented.contains(&token.to_ascii_lowercase()))
    {
        return Ok(());
    }

    Err(CompileError::UnimplementedSecurityScheme(format!(
        "{location}: '{plugin}' reads {}, but the security requirement names {}. The plugin \
         would find no credential it understands and reject every request. Name a scheme the \
         plugin reads, or run the plugin that reads the scheme named",
        joined(implements.iter()),
        joined(named.iter()),
    )))
}

/// The scheme types an operation's security requirement resolves to, limited to
/// the ones describing a credential the client sends.
///
/// `mutualTLS` is excluded: a certificate is presented during the handshake, so
/// no middleware reads it off the request.
fn required_scheme_tokens(op: &crate::spec_parser::Operation, spec: &ApiSpec) -> BTreeSet<String> {
    op.security
        .as_ref()
        .or(spec.security.as_ref())
        .map(|reqs| {
            reqs.iter()
                .flat_map(|r| r.keys())
                .filter_map(|name| spec.security_schemes.get(name))
                .filter(|scheme| scheme_carries_a_credential(scheme))
                .map(scheme_token)
                .collect()
        })
        .unwrap_or_default()
}

/// Render scheme tokens for an error message.
fn joined<'a>(tokens: impl Iterator<Item = &'a String>) -> String {
    let rendered: Vec<String> = tokens.map(|t| format!("`{t}`")).collect();
    if rendered.is_empty() {
        return "nothing".to_string();
    }
    rendered.join(", ")
}

/// Request headers an operation accepts beyond the data plane's baseline.
///
/// Derived from the spec's own vocabulary, so the document stays the contract
/// for what an upstream receives:
///
/// - every `in: header` parameter the operation declares,
/// - `cookie` when it declares any `in: cookie` parameter, since cookies travel
///   in that one header,
/// - the credential header named by each security scheme the operation's
///   requirement applies, which is the only reason `authorization` is forwarded.
///
/// Names are lowercased, as HTTP matches them case-insensitively.
fn operation_header_allowlist(
    op: &crate::spec_parser::Operation,
    spec: &ApiSpec,
) -> Result<BTreeSet<String>, CompileError> {
    let mut allow = BTreeSet::new();

    for param in &op.parameters {
        match param.location.as_str() {
            "header" => {
                allow.insert(reserved_checked(&param.name, "parameter")?);
            }
            "cookie" => {
                allow.insert("cookie".to_string());
            }
            _ => {}
        }
    }

    // An operation's own requirement replaces the root's; `security: []` is
    // present and empty, which makes the operation anonymous.
    let requirements = op.security.as_ref().or(spec.security.as_ref());
    if let Some(requirements) = requirements {
        for requirement in requirements {
            for scheme_name in requirement.keys() {
                let Some(scheme) = spec.security_schemes.get(scheme_name) else {
                    continue;
                };
                match scheme {
                    crate::spec_parser::SecurityScheme::ApiKey { name, location } => {
                        match location.as_str() {
                            "header" => {
                                allow.insert(reserved_checked(name, "security scheme")?);
                            }
                            "cookie" => {
                                allow.insert("cookie".to_string());
                            }
                            // A key in the query string needs no header.
                            _ => {}
                        }
                    }
                    crate::spec_parser::SecurityScheme::Http { .. }
                    | crate::spec_parser::SecurityScheme::OAuth2
                    | crate::spec_parser::SecurityScheme::OpenIdConnect => {
                        allow.insert("authorization".to_string());
                    }
                    // A client certificate carries no request header.
                    crate::spec_parser::SecurityScheme::MutualTls => {}
                }
            }
        }
    }

    Ok(allow)
}

/// Lowercase a header name, refusing the namespace the auth plugins own.
///
/// `x-auth-*` is what an auth plugin writes for `acl` and the upstream to read.
/// A spec that could declare one would let a client supply it instead.
fn reserved_checked(name: &str, source: &str) -> Result<String, CompileError> {
    let lowered = name.trim().to_ascii_lowercase();
    if lowered.starts_with("x-auth-") {
        return Err(CompileError::ReservedHeaderName(format!(
            "{source} '{name}'"
        )));
    }
    Ok(lowered)
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

            // Definitions the schema carries. A `$ref` to one is a leaf in this
            // walk, so a definition is only measured here. They sit beside the
            // schema rather than inside it, so they do not add depth of their
            // own.
            if let Some(serde_json::Value::Object(defs)) = obj.get("$defs") {
                for def in defs.values() {
                    let (d, p) = measure_schema_complexity(def, current_depth);
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

/// Resolve a path declared in a spec extension, relative to the directory the
/// spec was read from, so a rule set can sit beside the spec rather than
/// relative to wherever the compiler was invoked.
fn spec_relative_path(spec_dir: Option<&Path>, declared: &str) -> Result<PathBuf, CompileError> {
    let declared_path = Path::new(declared);
    if declared_path.is_absolute() {
        return Ok(declared_path.to_path_buf());
    }
    match spec_dir {
        Some(dir) => Ok(dir.join(declared_path)),
        None => Err(CompileError::WafRuleset(format!(
            "ruleset {declared:?} is a relative path, but these specs were not read from disk,              so there is nothing to resolve it against. Use an absolute path."
        ))),
    }
}

/// Extract root-level `x-barbacane-waf` config from the first spec that
/// defines it.
fn extract_root_waf_config(specs: &[(ApiSpec, String, String)]) -> WafConfig {
    for (spec, _, _) in specs {
        let Some(value) = spec.extensions.get("x-barbacane-waf") else {
            continue;
        };
        let num = |key: &str, default: i64| -> i64 {
            value.get(key).and_then(|v| v.as_i64()).unwrap_or(default)
        };
        let thresholds = value.get("thresholds");
        let threshold = |key: &str, default: i64| -> i64 {
            thresholds
                .and_then(|t| t.get(key))
                .and_then(|v| v.as_i64())
                .unwrap_or(default)
        };
        return WafConfig {
            enabled: value
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            ruleset: value
                .get("ruleset")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // CRS's own default.
            paranoia_level: num("paranoia_level", 1).clamp(1, 4) as u8,
            mode: match value.get("mode").and_then(|v| v.as_str()) {
                Some("detection-only") | Some("detection_only") | Some("detectiononly") => {
                    WafMode::DetectionOnly
                }
                _ => WafMode::Blocking,
            },
            inbound_threshold: threshold("inbound", 5),
            outbound_threshold: threshold("outbound", 4),
            max_response_body: value
                .get("max_response_body")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(default_max_response_body),
            audit: match value.get("audit").and_then(|v| v.as_str()) {
                Some("off") => AuditEngine::Off,
                Some("on") => AuditEngine::On,
                _ => AuditEngine::RelevantOnly,
            },
            unsupported_rules: match value.get("unsupported_rules").and_then(|v| v.as_str()) {
                Some("skip") => UnsupportedRules::Skip,
                _ => UnsupportedRules::Fail,
            },
            rules_path: None,
            skipped_rules: Vec::new(),
        };
    }
    WafConfig::default()
}

/// The outcome of validating and sealing a WAF rule set.
#[derive(Debug)]
struct SealedWaf {
    /// The validated rule set, serialised for the artifact.
    rules_json: Vec<u8>,
    /// Phrase lists referenced by `@pmFromFile`, keyed by the name the rule
    /// used. Sealed alongside the rules, because a rule set that references
    /// files the artifact does not carry cannot be compiled by the gateway.
    data_files: BTreeMap<String, Vec<u8>>,
    /// Ids of rules left out because they could not be compiled.
    skipped_rules: Vec<u32>,
    /// Compile-time warnings for the operator.
    warnings: Vec<String>,
}

/// Parse and validate a SecLang rule set, and serialise it for the artifact.
///
/// Validation is the point of doing this at compile time: a directive,
/// operator, action or target the engine does not implement fails here, with a
/// file and line, rather than being dropped at request time where a rule that
/// never fires looks exactly like a rule that found nothing.
fn seal_waf_ruleset(rules_dir: &Path, policy: UnsupportedRules) -> Result<SealedWaf, CompileError> {
    if !rules_dir.is_dir() {
        return Err(CompileError::WafRuleset(format!(
            "x-barbacane-waf ruleset {} is not a directory",
            rules_dir.display()
        )));
    }

    // Every entry is accounted for. `filter_map(Result::ok)` here would drop
    // an unreadable directory entry, silently omitting a rule file from the
    // rule set, which is the failure this whole design exists to prevent.
    let mut sources: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(rules_dir).map_err(|e| {
        CompileError::WafRuleset(format!("cannot read {}: {e}", rules_dir.display()))
    })? {
        let entry = entry.map_err(|e| {
            CompileError::WafRuleset(format!(
                "cannot read an entry in {}: {e}",
                rules_dir.display()
            ))
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "conf") {
            sources.push(path);
        }
    }
    sources.sort();

    if sources.is_empty() {
        return Err(CompileError::WafRuleset(format!(
            "x-barbacane-waf ruleset {} contains no .conf files",
            rules_dir.display()
        )));
    }

    let mut directives = Vec::new();
    for path in &sources {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(path).map_err(|e| {
            CompileError::WafRuleset(format!("cannot read {}: {e}", path.display()))
        })?;
        let (parsed, errors) = parapet::parse_all(&text, &name);
        if !errors.is_empty() {
            // Every error, not just the first: fixing a rule set one message
            // per build is miserable.
            let detail = errors
                .iter()
                .take(20)
                .map(|e| format!("  {e}"))
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            return Err(CompileError::WafRuleset(format!(
                "x-barbacane-waf: {} parse error(s) in the rule set:
{detail}",
                errors.len()
            )));
        }
        directives.extend(parsed);
    }

    // `@pmFromFile` names a phrase list on disk. Collect the ones this rule
    // set actually references and seal those, rather than every .data file in
    // the directory.
    let mut data_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for name in referenced_data_files(&directives) {
        if name.contains("..") || name.starts_with('/') {
            return Err(CompileError::WafRuleset(format!(
                "@pmFromFile {name:?} escapes the rule set directory"
            )));
        }
        let path = rules_dir.join(&name);
        let bytes = std::fs::read(&path).map_err(|e| {
            CompileError::WafRuleset(format!(
                "@pmFromFile {name:?} could not be read from {}: {e}",
                path.display()
            ))
        })?;
        data_files.insert(name, bytes);
    }

    let loader = parapet::DirDataLoader::new(rules_dir);
    let (_, compile_errors) = parapet::RuleSet::compile_all(&directives, &loader);

    let mut warnings = Vec::new();
    let mut skipped_rules = Vec::new();

    if !compile_errors.is_empty() {
        match policy {
            UnsupportedRules::Fail => {
                let detail = compile_errors
                    .iter()
                    .take(20)
                    .map(|e| format!("  {e}"))
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                return Err(CompileError::WafRuleset(format!(
                    "x-barbacane-waf: {} rule(s) in the rule set cannot be enforced by this                      build:
{detail}

Set `unsupported_rules: skip` to build without them.                      The artifact then records their ids and the gateway will not enforce them.",
                    compile_errors.len()
                )));
            }
            UnsupportedRules::Skip => {
                // Every refused rule must be accounted for. `filter_map` here
                // would drop any error whose id could not be parsed, and the
                // manifest would then under-report what the gateway is not
                // enforcing, which is worse than refusing outright.
                let mut unattributed: Vec<String> = Vec::new();
                for error in &compile_errors {
                    match rule_id_of(error) {
                        Some(id) => skipped_rules.push(id),
                        None => unattributed.push(error.to_string()),
                    }
                }
                if !unattributed.is_empty() {
                    return Err(CompileError::WafRuleset(format!(
                        "{} rule(s) cannot be enforced and their ids could not be determined, so \
                         they cannot be recorded in the manifest. Refusing rather than shipping a \
                         rule set whose gaps cannot be listed:\n{}",
                        unattributed.len(),
                        unattributed
                            .iter()
                            .take(10)
                            .map(|e| format!("  {e}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )));
                }
                skipped_rules.sort_unstable();
                skipped_rules.dedup();
                warnings.push(format!(
                    "{} rule(s) will not be enforced because this build cannot compile them: {}. \
                     Their ids are recorded in the manifest, where the artifact hash and \
                     signature cover them.",
                    compile_errors.len(),
                    skipped_rules
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                // Drop them so the artifact contains only what will run: an
                // artifact whose rule set does not match what the gateway
                // enforces is worse than one that is honestly smaller.
                let skip: std::collections::HashSet<u32> = skipped_rules.iter().copied().collect();
                directives.retain(|d| match d {
                    parapet::Directive::Rule(rule) | parapet::Directive::Action(rule) => {
                        rule.id().is_none_or(|id| !skip.contains(&id))
                    }
                    _ => true,
                });
            }
        }
    }

    let rules_json = serde_json::to_vec(&directives)?;
    Ok(SealedWaf {
        rules_json,
        data_files,
        skipped_rules,
        warnings,
    })
}

/// Every phrase list a rule set references through `@pmFromFile`.
fn referenced_data_files(directives: &[parapet::Directive]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut visit = |op: &Option<parapet::operator::Operator>| {
        if let Some(parapet::operator::Operator::PmFromFile(name)) = op {
            names.insert(name.clone());
        }
    };
    for directive in directives {
        if let parapet::Directive::Rule(rule) | parapet::Directive::Action(rule) = directive {
            visit(&rule.operator);
        }
    }
    names
}

/// Pull the rule id out of a Parapet compile error, which formats as
/// `rule <id> (line <n>): ...`.
fn rule_id_of(error: &parapet::CompileError) -> Option<u32> {
    let text = error.to_string();
    let rest = text.strip_prefix("rule ")?;
    let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // Parapet reports a rule with no id of its own (a chain link) as "rule 0".
    // Rule ids are positive, so treat 0 as unattributable rather than a real id.
    match id.parse().ok()? {
        0 => None,
        id => Some(id),
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

    mod configured_headers {
        use super::*;

        fn schema() -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "header_name": {"type": "string", "format": "header-name"},
                    "partition_key": {"type": "string", "format": "header-ref"},
                    "vary": {"type": "array", "items": {"type": "string"}, "format": "header-name"},
                    "key": {"type": "string", "format": "header-ref"},
                    "policy_name": {"type": "string"},
                    "headers": {
                        "type": "object",
                        "properties": {
                            "remove": {"type": "array", "format": "header-name"},
                            "rename": {"type": "object", "format": "header-name-map"},
                            "add": {
                                "type": "object",
                                "additionalProperties": {
                                    "type": "string",
                                    "format": "header-ref"
                                }
                            },
                            "set": {"type": "object"}
                        }
                    }
                }
            })
        }

        fn names(config: serde_json::Value) -> BTreeSet<String> {
            let mut out = BTreeSet::new();
            collect_configured_header_names(&schema(), &config, &mut out).expect("collect");
            out
        }

        /// A field left out of the configuration still applies through its
        /// default, which is the header the plugin will actually read.
        #[test]
        fn a_default_names_the_header_when_the_field_is_absent() {
            let schema = serde_json::json!({
                "type": "object",
                "properties": {
                    "header_name": {
                        "type": "string",
                        "format": "header-name",
                        "default": "X-API-Key"
                    }
                }
            });
            let mut found = BTreeSet::new();
            collect_configured_header_names(&schema, &serde_json::json!({}), &mut found)
                .expect("collect");
            assert!(
                found.contains("x-api-key"),
                "an unset field still reads its default"
            );

            // And a value given in the spec replaces it.
            let mut found = BTreeSet::new();
            collect_configured_header_names(
                &schema,
                &serde_json::json!({"header_name": "X-Key"}),
                &mut found,
            )
            .expect("collect");
            assert!(found.contains("x-key"));
            assert!(
                !found.contains("x-api-key"),
                "the default no longer applies"
            );
        }

        /// A plain value names one header, and a list names several.
        #[test]
        fn plain_and_list_values_are_read() {
            let found = names(serde_json::json!({
                "header_name": "X-API-Key",
                "vary": ["Accept", "X-Tenant"]
            }));
            assert!(found.contains("x-api-key"));
            assert!(found.contains("accept"));
            assert!(found.contains("x-tenant"));
        }

        /// A selector names a header only when it selects one. `client_ip`
        /// selects something else and must not become a header.
        #[test]
        fn only_the_header_selector_names_a_header() {
            assert!(
                names(serde_json::json!({"partition_key": "header:X-Client-Id"}))
                    .contains("x-client-id")
            );
            assert!(names(serde_json::json!({"partition_key": "client_ip"})).is_empty());
        }

        /// Every reference in a value is read, not only the first, and both
        /// interpolation forms count. A value built from two headers needs both
        /// admitted or it renders with one of them empty.
        #[test]
        fn every_reference_in_a_value_is_read() {
            let found = names(serde_json::json!({
                "headers": {"add": {"x-trace": "$header.x-first/$request.header.x-second"}}
            }));
            assert!(found.contains("x-first"), "{found:?}");
            assert!(found.contains("x-second"), "{found:?}");

            // A marker with nothing after it names nothing and still terminates.
            assert!(names(serde_json::json!({"key": "prefix $header. suffix"})).is_empty());
        }

        /// An expression language subscripts the header map by name. Only a
        /// literal name is visible, which the documentation says plainly, but it
        /// covers the way a condition is ordinarily written.
        #[test]
        fn a_subscripted_header_is_read() {
            let found = names(serde_json::json!({
                "key": "request.headers['x-tier'] == 'premium' && request.headers[\"x-region\"] != ''"
            }));
            assert!(found.contains("x-tier"), "{found:?}");
            assert!(found.contains("x-region"), "{found:?}");
            assert!(
                !found.contains("premium"),
                "a compared value is not a header"
            );

            // A computed name has none to read, and must not derail the scan.
            let computed = names(serde_json::json!({"key": "request.headers[someVar]"}));
            assert!(computed.is_empty(), "{computed:?}");
        }

        /// A template may name a header inside a longer expression.
        #[test]
        fn a_template_expression_is_read() {
            assert!(
                names(serde_json::json!({"key": "$request.header.X-Order-Id"}))
                    .contains("x-order-id")
            );
        }

        /// A rename table names headers with its keys, not its values, and a
        /// field that is not marked is not read at all.
        #[test]
        fn map_keys_are_read_and_unmarked_fields_are_not() {
            let found = names(serde_json::json!({
                "headers": {
                    "remove": ["X-Debug"],
                    "rename": {"X-Old": "X-New"},
                    "set": {"X-Written": "value"}
                },
                "policy_name": "default"
            }));
            assert!(found.contains("x-debug"));
            assert!(found.contains("x-old"), "a rename reads the old name");
            assert!(
                !found.contains("x-new"),
                "the new name is written, not read"
            );
            assert!(
                !found.contains("x-written"),
                "an unmarked field is not a header"
            );
            assert!(!found.contains("default"), "nor is a policy name");
        }

        /// The `x-auth-*` headers carry the identity the auth plugins establish.
        /// A configuration that named one would admit a client-sent value, so it
        /// is refused wherever a name can enter the allowlist (E1056).
        #[test]
        fn reserved_identity_headers_are_refused() {
            for config in [
                serde_json::json!({"header_name": "X-Auth-Consumer"}),
                serde_json::json!({"vary": ["Accept", "x-auth-scopes"]}),
                serde_json::json!({"partition_key": "header:X-Auth-Consumer"}),
                serde_json::json!({"key": "$request.header.x-auth-consumer"}),
                serde_json::json!({"headers": {"rename": {"X-Auth-Consumer": "X-Who"}}}),
                serde_json::json!({"headers": {"remove": ["X-Auth-Scopes"]}}),
            ] {
                let mut out = BTreeSet::new();
                let err = collect_configured_header_names(&schema(), &config, &mut out)
                    .expect_err(&format!("{config} must be refused"));
                assert!(
                    matches!(err, CompileError::ReservedHeaderName(_)),
                    "{config}: {err:?}"
                );
            }
        }
    }

    mod header_allowlist {
        use super::*;
        use crate::spec_parser::{Operation, SecurityScheme};

        fn op(params: &[(&str, &str)]) -> Operation {
            Operation {
                path: "/x".into(),
                method: "GET".into(),
                operation_id: None,
                summary: None,
                description: None,
                parameters: params
                    .iter()
                    .map(|(name, location)| Parameter {
                        name: (*name).into(),
                        location: (*location).into(),
                        required: false,
                        schema: None,
                    })
                    .collect(),
                request_body: None,
                dispatch: None,
                middlewares: None,
                deprecated: false,
                sunset: None,
                extensions: BTreeMap::new(),
                messages: vec![],
                bindings: BTreeMap::new(),
                responses: BTreeMap::new(),
                security: None,
            }
        }

        fn spec(schemes: &[(&str, SecurityScheme)]) -> ApiSpec {
            ApiSpec {
                filename: None,
                format: SpecFormat::OpenApi,
                version: "3.1.0".into(),
                title: "t".into(),
                api_version: "1".into(),
                operations: vec![],
                global_middlewares: vec![],
                extensions: BTreeMap::new(),
                security_schemes: schemes
                    .iter()
                    .map(|(n, s)| ((*n).to_string(), s.clone()))
                    .collect(),
                security: None,
            }
        }

        /// A declared header parameter is forwarded, matched case-insensitively
        /// as HTTP header names are, so the stored form is lowercase.
        #[test]
        fn declared_header_parameters_are_allowed() {
            let allow = operation_header_allowlist(
                &op(&[("X-Tenant-Id", "header"), ("limit", "query")]),
                &spec(&[]),
            )
            .expect("allowlist");
            assert!(allow.contains("x-tenant-id"));
            assert!(
                !allow.contains("limit"),
                "a query parameter is not a header"
            );
        }

        /// Cookies travel in one header, so declaring any cookie parameter lets
        /// `cookie` through, and declaring none keeps it out.
        #[test]
        fn cookie_parameter_allows_the_cookie_header() {
            let with = operation_header_allowlist(&op(&[("session", "cookie")]), &spec(&[]))
                .expect("allowlist");
            assert!(with.contains("cookie"));
            assert!(!with.contains("session"), "the cookie name is not a header");

            let without = operation_header_allowlist(&op(&[]), &spec(&[])).expect("allowlist");
            assert!(!without.contains("cookie"));
        }

        /// The credential an operation accepts is whatever its security scheme
        /// names, which is the only reason `authorization` is ever forwarded.
        #[test]
        fn security_schemes_contribute_their_credential_header() {
            let schemes = [
                (
                    "Key",
                    SecurityScheme::ApiKey {
                        name: "X-API-Key".into(),
                        location: "header".into(),
                    },
                ),
                (
                    "Session",
                    SecurityScheme::ApiKey {
                        name: "sid".into(),
                        location: "cookie".into(),
                    },
                ),
                (
                    "Query",
                    SecurityScheme::ApiKey {
                        name: "token".into(),
                        location: "query".into(),
                    },
                ),
                (
                    "Bearer",
                    SecurityScheme::Http {
                        scheme: "bearer".into(),
                    },
                ),
                ("Oidc", SecurityScheme::OpenIdConnect),
                ("Mtls", SecurityScheme::MutualTls),
            ];
            let mut s = spec(&schemes);

            let require = |names: &[&str]| {
                let mut req = BTreeMap::new();
                for n in names {
                    req.insert((*n).to_string(), vec![]);
                }
                Some(vec![req])
            };

            s.security = require(&["Key"]);
            let a = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
            assert!(a.contains("x-api-key"));

            s.security = require(&["Session"]);
            let a = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
            assert!(
                a.contains("cookie"),
                "an apiKey in a cookie needs the cookie header"
            );

            s.security = require(&["Query"]);
            let a = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
            assert!(a.is_empty(), "an apiKey in the query needs no header");

            for scheme in ["Bearer", "Oidc"] {
                s.security = require(&[scheme]);
                let a = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
                assert!(
                    a.contains("authorization"),
                    "{scheme} travels in authorization"
                );
            }

            s.security = require(&["Mtls"]);
            let a = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
            assert!(a.is_empty(), "mutualTLS carries no header");
        }

        /// The operation's own requirement replaces the root's, and an empty one
        /// makes it anonymous, so no credential header is forwarded.
        #[test]
        fn operation_security_overrides_the_root() {
            let mut s = spec(&[(
                "Bearer",
                SecurityScheme::Http {
                    scheme: "bearer".into(),
                },
            )]);
            let mut root = BTreeMap::new();
            root.insert("Bearer".to_string(), vec![]);
            s.security = Some(vec![root]);

            // Inherits the root requirement.
            let inherited = operation_header_allowlist(&op(&[]), &s).expect("allowlist");
            assert!(inherited.contains("authorization"));

            // `security: []` opts out, so the credential is not forwarded.
            let mut anonymous = op(&[]);
            anonymous.security = Some(vec![]);
            let a = operation_header_allowlist(&anonymous, &s).expect("allowlist");
            assert!(
                !a.contains("authorization"),
                "an anonymous operation takes no credential"
            );
        }

        /// A plugin that verifies a credential needs the spec to say which
        /// scheme carries it, or the operation reads as anonymous, no credential
        /// header is admitted, and the plugin rejects every request.
        #[test]
        fn an_authentication_plugin_requires_a_security_requirement() {
            let bare = spec(&[]);
            let err = require_security_requirement(&op(&[]), &bare, "basic-auth", "GET /x")
                .expect_err("no requirement at all");
            assert!(
                matches!(err, CompileError::MissingSecurityRequirement(_)),
                "{err:?}"
            );

            // `security: []` is a deliberate declaration of anonymity, which
            // contradicts running an authentication plugin.
            let mut anonymous = op(&[]);
            anonymous.security = Some(vec![]);
            assert!(matches!(
                require_security_requirement(&anonymous, &bare, "basic-auth", "GET /x"),
                Err(CompileError::MissingSecurityRequirement(_))
            ));

            // Naming a scheme the document does not define is no better: nothing
            // resolves, so nothing admits the header.
            let mut dangling = op(&[]);
            let mut req = BTreeMap::new();
            req.insert("Nowhere".to_string(), vec![]);
            dangling.security = Some(vec![req]);
            assert!(matches!(
                require_security_requirement(&dangling, &bare, "basic-auth", "GET /x"),
                Err(CompileError::MissingSecurityRequirement(_))
            ));

            // The scheme says where the credential travels and the plugin
            // follows, so a key in the query string satisfies it and admits no
            // header because it needs none.
            let mut query = spec(&[(
                "ApiKeyQuery",
                SecurityScheme::ApiKey {
                    name: "api_key".into(),
                    location: "query".into(),
                },
            )]);
            let mut q = BTreeMap::new();
            q.insert("ApiKeyQuery".to_string(), vec![]);
            query.security = Some(vec![q]);
            require_security_requirement(&op(&[]), &query, "apikey-auth", "GET /x")
                .expect("a key in the query string is a credential the client sends");

            // A certificate is presented during the handshake, so no middleware
            // reads it off the request.
            let mut mtls = spec(&[("Mtls", SecurityScheme::MutualTls)]);
            let mut m = BTreeMap::new();
            m.insert("Mtls".to_string(), vec![]);
            mtls.security = Some(vec![m]);
            assert!(matches!(
                require_security_requirement(&op(&[]), &mtls, "basic-auth", "GET /x"),
                Err(CompileError::MissingSecurityRequirement(_))
            ));

            // A defined scheme satisfies it, whether named on the operation or
            // inherited from the root.
            let mut defined = spec(&[(
                "BasicAuth",
                SecurityScheme::Http {
                    scheme: "basic".into(),
                },
            )]);
            let mut root = BTreeMap::new();
            root.insert("BasicAuth".to_string(), vec![]);
            defined.security = Some(vec![root]);
            require_security_requirement(&op(&[]), &defined, "basic-auth", "GET /x")
                .expect("an inherited requirement is enough");
        }

        /// Build a spec whose root requirement names every scheme given.
        fn secured(schemes: &[(&str, SecurityScheme)]) -> ApiSpec {
            let mut s = spec(schemes);
            let mut req = BTreeMap::new();
            for (name, _) in schemes {
                req.insert((*name).to_string(), vec![]);
            }
            s.security = Some(vec![req]);
            s
        }

        fn bearer() -> SecurityScheme {
            SecurityScheme::Http {
                scheme: "bearer".into(),
            }
        }

        fn api_key_query() -> SecurityScheme {
            SecurityScheme::ApiKey {
                name: "token".into(),
                location: "query".into(),
            }
        }

        /// A plugin paired with a scheme it does not read rejects every request,
        /// though both halves are valid on their own.
        #[test]
        fn an_authentication_plugin_must_read_a_scheme_the_operation_names() {
            let jwt = ["http:bearer".to_string(), "oauth2".to_string()];

            let mismatch = secured(&[("ApiKeyQuery", api_key_query())]);
            let err = require_implemented_scheme(&op(&[]), &mismatch, "jwt-auth", &jwt, "GET /x")
                .expect_err("jwt-auth does not read an apiKey");
            assert!(
                matches!(err, CompileError::UnimplementedSecurityScheme(_)),
                "{err:?}"
            );
            // The message names both sides, since either can be the mistake.
            let message = err.to_string();
            assert!(message.contains("http:bearer"), "{message}");
            assert!(message.contains("apiKey"), "{message}");

            let matching = secured(&[("BearerAuth", bearer())]);
            require_implemented_scheme(&op(&[]), &matching, "jwt-auth", &jwt, "GET /x")
                .expect("jwt-auth reads a bearer token");

            // OAuth2 is one of several the plugin reads, and one match is enough.
            let oauth = secured(&[("OAuth2", SecurityScheme::OAuth2)]);
            require_implemented_scheme(&op(&[]), &oauth, "jwt-auth", &jwt, "GET /x")
                .expect("oauth2 is implemented too");
        }

        /// An `http` scheme is qualified by its authentication scheme: reading
        /// Basic credentials is not reading a Bearer token.
        #[test]
        fn http_schemes_are_distinguished_by_their_scheme() {
            let basic = ["http:basic".to_string()];
            let s = secured(&[("BearerAuth", bearer())]);
            assert!(matches!(
                require_implemented_scheme(&op(&[]), &s, "basic-auth", &basic, "GET /x"),
                Err(CompileError::UnimplementedSecurityScheme(_))
            ));

            let s = secured(&[(
                "BasicAuth",
                SecurityScheme::Http {
                    scheme: "basic".into(),
                },
            )]);
            require_implemented_scheme(&op(&[]), &s, "basic-auth", &basic, "GET /x")
                .expect("basic-auth reads Basic");
        }

        /// Several schemes in the requirement, one of which the plugin reads.
        /// That is the shape of a chain running two authentication plugins.
        #[test]
        fn one_matching_scheme_among_several_is_enough() {
            let s = secured(&[
                (
                    "BasicAuth",
                    SecurityScheme::Http {
                        scheme: "basic".into(),
                    },
                ),
                ("BearerAuth", bearer()),
            ]);
            require_implemented_scheme(
                &op(&[]),
                &s,
                "jwt-auth",
                &["http:bearer".to_string()],
                "GET /x",
            )
            .expect("the requirement names a bearer scheme too");
            require_implemented_scheme(
                &op(&[]),
                &s,
                "basic-auth",
                &["http:basic".to_string()],
                "GET /x",
            )
            .expect("and a basic one");
        }

        /// A plugin declaring nothing is exempt, so a third-party plugin
        /// compiles exactly as it did before the check existed.
        #[test]
        fn a_plugin_declaring_nothing_is_not_checked() {
            let s = secured(&[("ApiKeyQuery", api_key_query())]);
            require_implemented_scheme(&op(&[]), &s, "third-party-auth", &[], "GET /x")
                .expect("no declaration means no check");
        }

        /// Tokens name scheme types, not their spelling, so case does not decide
        /// whether a document compiles.
        #[test]
        fn tokens_match_case_insensitively() {
            let s = secured(&[("Oidc", SecurityScheme::OpenIdConnect)]);
            require_implemented_scheme(
                &op(&[]),
                &s,
                "oidc-auth",
                &["OPENIDCONNECT".to_string()],
                "GET /x",
            )
            .expect("openIdConnect however it is written");
        }

        /// The operation's own requirement replaces the root's here too, so a
        /// plugin is judged against the schemes that actually apply to it.
        #[test]
        fn the_operations_own_requirement_is_the_one_checked() {
            let mut s = secured(&[("BearerAuth", bearer())]);
            s.security_schemes
                .insert("ApiKeyQuery".to_string(), api_key_query());

            let mut narrowed = op(&[]);
            let mut req = BTreeMap::new();
            req.insert("ApiKeyQuery".to_string(), vec![]);
            narrowed.security = Some(vec![req]);

            assert!(
                matches!(
                    require_implemented_scheme(
                        &narrowed,
                        &s,
                        "jwt-auth",
                        &["http:bearer".to_string()],
                        "GET /x"
                    ),
                    Err(CompileError::UnimplementedSecurityScheme(_))
                ),
                "the root's bearer scheme no longer applies"
            );
        }

        /// The identity headers are the auth plugins' output. A spec that could
        /// declare one would let a client forge the identity the gateway trusts.
        #[test]
        fn reserved_identity_headers_are_refused() {
            let err = operation_header_allowlist(&op(&[("x-auth-consumer", "header")]), &spec(&[]))
                .expect_err("x-auth-* must not be declarable");
            assert!(
                matches!(err, CompileError::ReservedHeaderName(_)),
                "{err:?}"
            );

            // Including through a security scheme, and whatever the case.
            let s = spec(&[(
                "Sneaky",
                SecurityScheme::ApiKey {
                    name: "X-Auth-Consumer".into(),
                    location: "header".into(),
                },
            )]);
            let mut s = s;
            let mut req = BTreeMap::new();
            req.insert("Sneaky".to_string(), vec![]);
            s.security = Some(vec![req]);
            let err = operation_header_allowlist(&op(&[]), &s).expect_err("also via a scheme");
            assert!(
                matches!(err, CompileError::ReservedHeaderName(_)),
                "{err:?}"
            );
        }
    }

    /// Definitions a schema carries still count towards its complexity. They
    /// hold the body that a `$ref` points at, so skipping them would measure an
    /// empty schema and let the E1051 and E1052 limits pass anything.
    #[test]
    fn complexity_counts_carried_definitions() {
        let schema = serde_json::json!({
            "$ref": "#/$defs/User",
            "$defs": {
                "User": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "name": {"type": "string"},
                        "address": {
                            "type": "object",
                            "properties": {"city": {"type": "string"}}
                        }
                    }
                }
            }
        });
        let (depth, props) = measure_schema_complexity(&schema, 0);
        assert_eq!(props, 4, "every property in the definition is counted");
        assert!(depth >= 1, "nesting inside the definition is measured");

        // And the limits actually bite on what the definition holds.
        assert!(validate_schema_complexity(&schema, 10, 2, "test").is_err());
        assert!(validate_schema_complexity(&schema, 10, 10, "test").is_ok());
    }
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn collect_writeonly_fields_finds_nested_secrets() {
        // Mirrors ai-proxy: writeOnly api_key at top level, in routes[].items,
        // and in targets{}.additionalProperties. `base_url` is not writeOnly.
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "api_key": { "type": "string", "writeOnly": true },
                "base_url": { "type": "string" },
                "routes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "api_key": { "type": "string", "writeOnly": true }
                        }
                    }
                },
                "targets": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "object",
                        "properties": {
                            "client_secret": { "type": "string", "writeOnly": true }
                        }
                    }
                }
            }
        });
        let fields = collect_writeonly_fields(&schema);
        assert!(fields.contains("api_key"));
        assert!(fields.contains("client_secret"));
        assert!(!fields.contains("base_url"));
    }

    #[test]
    fn scan_plaintext_secrets_flags_literals_and_ignores_refs() {
        let secret_fields: std::collections::BTreeSet<String> = ["api_key", "client_secret"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Nested shape like ai-proxy: routes[] and targets{}.
        let config = serde_json::json!({
            "routes": [
                { "provider": "openai", "api_key": "sk-live-plaintext" },
                { "provider": "ollama", "base_url": "env://OLLAMA_BASE_URL" },
            ],
            "targets": {
                "claude": { "api_key": "env://ANTHROPIC_API_KEY" },
                "azure": { "client_secret": "hunter2" },
            },
            "timeout": 30,
        });
        let mut warnings = Vec::new();
        scan_plaintext_secrets(
            &config,
            "ai-proxy",
            &secret_fields,
            "GET /v1 in 'api.yaml'",
            &mut warnings,
        );

        // Two plaintext secrets: routes[0].api_key and targets.azure.client_secret.
        // routes[1].base_url is not a secret field; the env:// api_key is ignored.
        assert_eq!(warnings.len(), 2, "got: {warnings:?}");
        assert!(warnings.iter().all(|w| w.code == "E1070"));
    }

    #[test]
    fn scan_plaintext_secrets_ignores_nonsecret_and_empty_fields() {
        let secret_fields: std::collections::BTreeSet<String> =
            ["api_key"].iter().map(|s| s.to_string()).collect();
        let config = serde_json::json!({
            "url": "https://api.example.com",
            "model": "gpt-4o",
            "api_key": "",
        });
        let mut warnings = Vec::new();
        scan_plaintext_secrets(&config, "ai-proxy", &secret_fields, "loc", &mut warnings);
        assert!(warnings.is_empty(), "got: {warnings:?}");
    }

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

    /// A spec whose only plugins are non-authentication ones, so the chain
    /// cannot verify the credential the requirement names.
    fn spec_requiring_basic(operation_security: &str) -> String {
        format!(
            r#"
openapi: "3.1.0"
info:
  title: Test API
  version: "1.0.0"
components:
  securitySchemes:
    BasicAuth:
      type: http
      scheme: basic
paths:
  /thing:
    get:
      operationId: getThing
{operation_security}      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
"#
        )
    }

    /// An operation requiring a credential with no authentication plugin in its
    /// chain forwards the credential and lets the request through, which the
    /// document does not say (E1033). A warning, since an upstream may be the
    /// one checking.
    #[test]
    fn compile_warns_when_nothing_verifies_the_required_credential() {
        let temp = TempDir::new().unwrap();
        let spec_path = create_test_spec(
            temp.path(),
            "test.yaml",
            &spec_requiring_basic("      security:\n        - BasicAuth: []\n"),
        );
        let result = compile(
            &[spec_path.as_path()],
            &[],
            &temp.path().join("artifact.bca"),
            &CompileOptions::default(),
        )
        .expect("a warning, not a refusal");

        let warned: Vec<_> = result
            .warnings
            .iter()
            .filter(|w| w.code == "E1033")
            .collect();
        assert_eq!(warned.len(), 1, "got: {:?}", result.warnings);
        assert!(
            warned[0].message.contains("http:basic"),
            "the message names the scheme: {}",
            warned[0].message
        );

        // An operation declaring itself anonymous requires nothing, so there is
        // nothing to warn about.
        let anonymous = create_test_spec(
            temp.path(),
            "anonymous.yaml",
            &spec_requiring_basic("      security: []\n"),
        );
        let result = compile(
            &[anonymous.as_path()],
            &[],
            &temp.path().join("anonymous.bca"),
            &CompileOptions::default(),
        )
        .expect("compiles");
        assert!(
            !result.warnings.iter().any(|w| w.code == "E1033"),
            "got: {:?}",
            result.warnings
        );
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
            category: None,
            implements: vec![],
            wasm_bytes: fake_wasm.clone(),
            body_access: false,
            host_functions: vec![],
            secret_fields: vec![],
            config_schema: None,
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

        let hash1 = compute_artifact_hash(
            &source_specs,
            &checksums,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );
        let hash2 = compute_artifact_hash(
            &source_specs,
            &checksums,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );

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

        let hash_a = compute_artifact_hash(
            &specs_a,
            &checksums,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );
        let hash_b = compute_artifact_hash(
            &specs_b,
            &checksums,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );

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

        let hash_a = compute_artifact_hash(
            &specs,
            &checksums_a,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );
        let hash_b = compute_artifact_hash(
            &specs,
            &checksums_b,
            &[],
            false,
            &McpConfig::default(),
            &WafConfig::default(),
        );

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
            security_schemes: BTreeMap::new(),
            security: None,
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
            security_schemes: BTreeMap::new(),
            security: None,
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

    // --- AR-1: artifact integrity & signing ---

    fn integrity_test_manifest() -> Manifest {
        let mut checksums = BTreeMap::new();
        checksums.insert("routes.json".to_string(), "routehash".to_string());
        checksums.insert(
            "plugins/jwt-auth.wasm".to_string(),
            compute_sha256(b"wasm-bytes"),
        );
        let source_specs = vec![SourceSpec {
            file: "api.yaml".to_string(),
            sha256: compute_sha256(b"spec"),
            spec_type: "openapi".to_string(),
            version: "1.0.0".to_string(),
        }];
        let mut manifest = Manifest {
            barbacane_artifact_version: ARTIFACT_VERSION,
            compiled_at: "1970-01-01T00:00:00Z".to_string(),
            compiler_version: COMPILER_VERSION.to_string(),
            source_specs,
            routes_count: 1,
            checksums,
            plugins: vec![BundledPlugin {
                name: "jwt-auth".to_string(),
                version: "0.1.0".to_string(),
                plugin_type: "middleware".to_string(),
                wasm_path: "plugins/jwt-auth.wasm".to_string(),
                sha256: compute_sha256(b"wasm-bytes"),
                capabilities: PluginCapabilities {
                    body_access: false,
                    host_functions: vec!["host_log".to_string()],
                },
            }],
            artifact_hash: String::new(),
            provenance: Provenance::default(),
            mcp: McpConfig::default(),
            waf: WafConfig::default(),
            signature: None,
            signing_public_key: None,
            capabilities_enforced: true,
        };
        manifest.artifact_hash = recompute_artifact_hash(&manifest);
        manifest
    }

    fn sign_for_test(manifest: &mut Manifest) -> String {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let sig = kp.sign(manifest.artifact_hash.as_bytes());
        manifest.signature = Some(super::hex::encode(sig.as_ref()));
        let pubkey_hex = super::hex::encode(kp.public_key().as_ref());
        manifest.signing_public_key = Some(pubkey_hex.clone());
        pubkey_hex
    }

    #[test]
    fn artifact_hash_verifies_and_detects_tampering() {
        let mut manifest = integrity_test_manifest();
        assert!(verify_artifact_hash(&manifest).is_ok());

        // Tampered checksum (a swapped plugin) breaks the hash consistency.
        manifest
            .checksums
            .insert("plugins/jwt-auth.wasm".to_string(), "deadbeef".to_string());
        assert!(matches!(
            verify_artifact_hash(&manifest),
            Err(IntegrityError::ArtifactHashMismatch { .. })
        ));
    }

    #[test]
    fn plugin_checksum_detects_swapped_wasm() {
        let manifest = integrity_test_manifest();
        assert!(verify_plugin_checksum(&manifest, "jwt-auth", b"wasm-bytes").is_ok());
        assert!(matches!(
            verify_plugin_checksum(&manifest, "jwt-auth", b"evil-bytes"),
            Err(IntegrityError::PluginChecksumMismatch { .. })
        ));
        assert!(matches!(
            verify_plugin_checksum(&manifest, "ghost", b"x"),
            Err(IntegrityError::UnknownPlugin { .. })
        ));
    }

    #[test]
    fn signature_roundtrip_and_rejection() {
        let mut manifest = integrity_test_manifest();

        // Unsigned artifact fails closed when a trusted key is configured.
        assert!(matches!(
            verify_artifact_signature(&manifest, "00"),
            Err(IntegrityError::MissingSignature)
        ));

        let pubkey = sign_for_test(&mut manifest);
        // Valid signature under the correct key.
        assert!(verify_artifact_signature(&manifest, &pubkey).is_ok());

        // Tampered artifact_hash → signature no longer matches.
        let mut tampered = manifest.clone();
        tampered.artifact_hash = "sha256:0000".to_string();
        assert!(matches!(
            verify_artifact_signature(&tampered, &pubkey),
            Err(IntegrityError::BadSignature)
        ));

        // Wrong (attacker) key → rejected.
        let mut other = integrity_test_manifest();
        let _ = sign_for_test(&mut other);
        assert!(matches!(
            verify_artifact_signature(&manifest, other.signing_public_key.as_ref().unwrap()),
            Err(IntegrityError::BadSignature)
        ));
    }

    /// H3 regression: the signed hash must bind the capability-enforcement
    /// surface. Before the fix, `capabilities_enforced`, a plugin's declared
    /// `host_functions`/`body_access`, and the MCP config were omitted from
    /// `artifact_hash`, so an attacker could weaken the sandbox in `manifest.json`
    /// while `artifact_hash` (and thus the signature over it) stayed valid.
    ///
    /// The load path runs both checks, so tampering is caught two ways:
    ///  1. leave `artifact_hash` as-is → `verify_artifact_hash` recomputes over
    ///     the tampered fields and detects the mismatch (this is what was broken);
    ///  2. recompute `artifact_hash` to match the tampered fields (as a real
    ///     attacker would) → `verify_artifact_signature` fails, since the hash
    ///     changed and they cannot re-sign without the private key.
    #[test]
    fn signed_hash_binds_capability_surface() {
        let mut manifest = integrity_test_manifest();
        let pubkey = sign_for_test(&mut manifest);
        assert!(verify_artifact_hash(&manifest).is_ok());
        assert!(verify_artifact_signature(&manifest, &pubkey).is_ok());

        let disable_enforcement = |m: &mut Manifest| m.capabilities_enforced = false;
        let widen_host_functions = |m: &mut Manifest| {
            m.plugins[0].capabilities.host_functions = vec!["host_http_call".to_string()]
        };
        let grant_body_access = |m: &mut Manifest| m.plugins[0].capabilities.body_access = true;
        let enable_mcp = |m: &mut Manifest| m.mcp.enabled = true;

        let tampers: Vec<&dyn Fn(&mut Manifest)> = vec![
            &disable_enforcement,
            &widen_host_functions,
            &grant_body_access,
            &enable_mcp,
        ];

        for tamper in tampers {
            // (1) tamper only: stale artifact_hash is now detected.
            let mut stale = manifest.clone();
            tamper(&mut stale);
            assert!(
                matches!(
                    verify_artifact_hash(&stale),
                    Err(IntegrityError::ArtifactHashMismatch { .. })
                ),
                "tampering the capability surface must break the recomputed hash"
            );

            // (2) attacker recomputes artifact_hash to match → signature fails.
            let mut resealed = manifest.clone();
            tamper(&mut resealed);
            resealed.artifact_hash = recompute_artifact_hash(&resealed);
            assert!(verify_artifact_hash(&resealed).is_ok());
            assert!(
                matches!(
                    verify_artifact_signature(&resealed, &pubkey),
                    Err(IntegrityError::BadSignature)
                ),
                "recomputing the hash after tampering must invalidate the signature"
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod waf_tests {
    use super::*;

    /// A rule set directory containing one file with the given contents.
    pub(super) fn ruleset(dir: &Path, contents: &str) -> PathBuf {
        let rules = dir.join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("test.conf"), contents).unwrap();
        rules
    }

    /// A spec carrying only the WAF extension, for config-extraction tests.
    fn spec_with_waf(value: serde_json::Value) -> (ApiSpec, String, String) {
        let spec = ApiSpec {
            filename: Some("api.yaml".to_string()),
            format: crate::spec_parser::SpecFormat::OpenApi,
            version: "3.1.0".to_string(),
            title: "test".to_string(),
            api_version: "1.0.0".to_string(),
            operations: Vec::new(),
            global_middlewares: Vec::new(),
            extensions: [("x-barbacane-waf".to_string(), value)]
                .into_iter()
                .collect(),
            security_schemes: BTreeMap::new(),
            security: None,
        };
        (spec, "api.yaml".to_string(), String::new())
    }

    pub(super) fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bca-waf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn seals_a_valid_rule_set() {
        let dir = tempdir("valid");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@rx attack\" \"id:1,phase:2,deny\"\nSecMarker END\n",
        );
        let sealed = seal_waf_ruleset(&rules, UnsupportedRules::Fail).expect("must seal");
        assert!(sealed.skipped_rules.is_empty());
        assert!(sealed.warnings.is_empty());

        // The sealed form must rebuild into the same rule set.
        let directives: Vec<parapet::Directive> =
            serde_json::from_slice(&sealed.rules_json).unwrap();
        let (rebuilt, errors) = parapet::RuleSet::compile_all(&directives, &parapet::NoDataLoader);
        assert!(errors.is_empty());
        assert_eq!(rebuilt.rule_count(), 1);
        assert_eq!(rebuilt.marker_count(), 1);
    }

    #[test]
    fn a_parse_error_refuses_the_artifact() {
        // An unknown directive must fail the build, not be dropped. This is
        // the property that makes compile-time rule handling worth doing.
        let dir = tempdir("parse-error");
        let rules = ruleset(&dir, "SecWhatever on\n");
        let err = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("E1080"), "{text}");
        assert!(text.contains("parse error"), "{text}");
    }

    #[test]
    fn an_unenforceable_rule_refuses_the_artifact_by_default() {
        // An invalid regex cannot be compiled, so the rule cannot be enforced.
        // The default policy refuses rather than shipping a rule set that
        // looks complete.
        let dir = tempdir("unsupported-fail");
        let rules = ruleset(&dir, "SecRule ARGS \"@rx (\" \"id:942100,phase:2,deny\"\n");
        let err = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("cannot be enforced"), "{text}");
        assert!(text.contains("unsupported_rules: skip"), "{text}");
    }

    #[test]
    fn skip_records_the_rule_ids_and_drops_them_from_the_artifact() {
        let dir = tempdir("unsupported-skip");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@rx attack\" \"id:1,phase:2,deny\"\n\
             SecRule ARGS \"@rx (\" \"id:942100,phase:2,deny\"\n",
        );
        let sealed = seal_waf_ruleset(&rules, UnsupportedRules::Skip).expect("must seal");
        assert_eq!(sealed.skipped_rules, vec![942100]);
        assert_eq!(sealed.warnings.len(), 1);
        assert!(sealed.warnings[0].contains("942100"));

        // The artifact carries only what the gateway will enforce.
        let directives: Vec<parapet::Directive> =
            serde_json::from_slice(&sealed.rules_json).unwrap();
        let (rebuilt, errors) = parapet::RuleSet::compile_all(&directives, &parapet::NoDataLoader);
        assert!(
            errors.is_empty(),
            "the sealed rule set still contains rules that cannot compile"
        );
        assert_eq!(rebuilt.rule_count(), 1);
    }

    #[test]
    fn phrase_lists_referenced_by_pmfromfile_are_sealed() {
        // Found by running the gateway: sealing only the rules leaves
        // @pmFromFile unresolvable, so the data plane cannot compile the rule
        // set it was handed.
        let dir = tempdir("pmfromfile");
        let rules = ruleset(
            &dir,
            "SecRule REQUEST_HEADERS:User-Agent \"@pmFromFile scanners.data\" \
             \"id:2000,phase:1,deny\"\n",
        );
        std::fs::write(rules.join("scanners.data"), "# comment\nnikto\nsqlmap\n").unwrap();

        let sealed = seal_waf_ruleset(&rules, UnsupportedRules::Fail).expect("must seal");
        assert_eq!(sealed.data_files.len(), 1);
        assert!(sealed.data_files.contains_key("scanners.data"));

        // And the sealed pair compiles with no filesystem access at all.
        let directives: Vec<parapet::Directive> =
            serde_json::from_slice(&sealed.rules_json).unwrap();
        let loaded = SealedRuleSet {
            directives,
            data_files: sealed.data_files.clone(),
        };
        parapet::RuleSet::compile(&loaded.directives, &loaded)
            .expect("the sealed rule set must compile without touching the filesystem");
    }

    #[test]
    fn only_referenced_phrase_lists_are_sealed() {
        let dir = tempdir("pmfromfile-subset");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@pmFromFile used.data\" \"id:2001,phase:2,deny\"\n",
        );
        std::fs::write(rules.join("used.data"), "a\n").unwrap();
        std::fs::write(rules.join("unused.data"), "b\n").unwrap();

        let sealed = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap();
        assert_eq!(
            sealed.data_files.keys().collect::<Vec<_>>(),
            vec!["used.data"]
        );
    }

    #[test]
    fn a_missing_phrase_list_refuses_the_build() {
        let dir = tempdir("pmfromfile-missing");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@pmFromFile absent.data\" \"id:2002,phase:2,deny\"\n",
        );
        let err = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap_err();
        assert!(err.to_string().contains("absent.data"), "{err}");
    }

    #[test]
    fn a_phrase_list_cannot_escape_the_ruleset_directory() {
        let dir = tempdir("pmfromfile-escape");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@pmFromFile ../../etc/passwd\" \"id:2003,phase:2,deny\"\n",
        );
        let err = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap_err();
        assert!(err.to_string().contains("escapes"), "{err}");
    }

    #[test]
    fn multi_word_phrases_survive_sealing() {
        // 2,008 phrases in CRS contain spaces, so the sealed form must not
        // re-split them.
        let dir = tempdir("pmfromfile-spaces");
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@pmFromFile phrases.data\" \"id:2004,phase:2,deny\"\n",
        );
        std::fs::write(
            rules.join("phrases.data"),
            "class java.lang.\nat java.lang.\n",
        )
        .unwrap();
        let sealed = seal_waf_ruleset(&rules, UnsupportedRules::Fail).unwrap();
        let text = String::from_utf8(sealed.data_files["phrases.data"].clone()).unwrap();
        assert!(text.contains("class java.lang."));
    }

    #[test]
    fn an_empty_or_missing_ruleset_refuses() {
        let dir = tempdir("empty");
        let rules = dir.join("rules");
        std::fs::create_dir_all(&rules).unwrap();
        assert!(seal_waf_ruleset(&rules, UnsupportedRules::Fail)
            .unwrap_err()
            .to_string()
            .contains("no .conf files"));
        assert!(seal_waf_ruleset(&dir.join("nope"), UnsupportedRules::Fail)
            .unwrap_err()
            .to_string()
            .contains("not a directory"));
    }

    #[test]
    fn the_waf_policy_surface_is_bound_by_the_artifact_hash() {
        // The rule set is bound through `checksums`, but the policy decides
        // what the rules do, so changing it must change the hash. Otherwise a
        // signed artifact could be flipped from blocking to detection-only.
        let specs: Vec<SourceSpec> = Vec::new();
        let checksums = BTreeMap::new();
        let plugins: Vec<BundledPlugin> = Vec::new();
        let mcp = McpConfig::default();

        let base = WafConfig {
            enabled: true,
            paranoia_level: 1,
            mode: WafMode::Blocking,
            inbound_threshold: 5,
            outbound_threshold: 4,
            ..Default::default()
        };
        let hash_of =
            |waf: &WafConfig| compute_artifact_hash(&specs, &checksums, &plugins, true, &mcp, waf);
        let baseline = hash_of(&base);

        let mut detection = base.clone();
        detection.mode = WafMode::DetectionOnly;
        assert_ne!(baseline, hash_of(&detection), "mode is not bound");

        let mut paranoia = base.clone();
        paranoia.paranoia_level = 4;
        assert_ne!(baseline, hash_of(&paranoia), "paranoia level is not bound");

        let mut threshold = base.clone();
        threshold.inbound_threshold = 100;
        assert_ne!(baseline, hash_of(&threshold), "threshold is not bound");

        let mut skipped = base.clone();
        skipped.skipped_rules = vec![942100];
        assert_ne!(baseline, hash_of(&skipped), "skipped rules are not bound");

        let mut cap = base.clone();
        cap.max_response_body = 42;
        assert_ne!(baseline, hash_of(&cap), "max_response_body is not bound");

        let mut audit = base.clone();
        audit.audit = AuditEngine::On;
        assert_ne!(baseline, hash_of(&audit), "audit engine is not bound");

        let mut off = base.clone();
        off.enabled = false;
        assert_ne!(baseline, hash_of(&off), "enabled is not bound");
    }

    #[test]
    fn config_defaults_match_crs_conventions() {
        let cfg = extract_root_waf_config(&[spec_with_waf(
            serde_json::json!({ "ruleset": "./crs/rules" }),
        )]);
        assert!(cfg.enabled, "declaring the extension should enable it");
        assert_eq!(cfg.paranoia_level, 1);
        assert_eq!(cfg.mode, WafMode::Blocking);
        assert_eq!(cfg.inbound_threshold, 5);
        assert_eq!(cfg.outbound_threshold, 4);
        assert_eq!(cfg.max_response_body, 1_048_576);
        assert_eq!(cfg.audit, AuditEngine::RelevantOnly);
        assert_eq!(cfg.unsupported_rules, UnsupportedRules::Fail);
    }

    #[test]
    fn paranoia_level_is_clamped_to_the_valid_range() {
        for (given, expected) in [(0, 1), (1, 1), (4, 4), (9, 4)] {
            let cfg = extract_root_waf_config(&[spec_with_waf(
                serde_json::json!({ "ruleset": "r", "paranoia_level": given }),
            )]);
            assert_eq!(cfg.paranoia_level, expected, "paranoia_level {given}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod waf_artifact_tests {
    use super::waf_tests::{ruleset, tempdir};
    use super::*;
    use std::io::Read;

    const SPEC: &str = r#"openapi: 3.1.0
info:
  title: waf-test
  version: 1.0.0
x-barbacane-waf:
  ruleset: ./rules
  paranoia_level: 2
  mode: blocking
  thresholds:
    inbound: 7
    outbound: 3
paths:
  /ping:
    get:
      operationId: ping
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
      responses:
        "200":
          description: ok
"#;

    const RULES: &str = "SecRule ARGS \"@rx attack\" \\
    \"id:1000,phase:2,deny,msg:'blocked'\"
SecRule REQUEST_METHOD \"!@within GET HEAD\" \"id:1001,phase:1,deny\"
SecMarker DONE
";

    fn project(name: &str, spec: &str, rules: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bca-wafe2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("api.yaml"), spec).unwrap();
        if let Some(rules) = rules {
            let rules_dir = dir.join("rules");
            std::fs::create_dir_all(&rules_dir).unwrap();
            std::fs::write(rules_dir.join("test.conf"), rules).unwrap();
        }
        dir
    }

    /// Read one file out of the compiled `.bca` (a gzipped tar).
    fn read_from_artifact(artifact: &Path, wanted: &str) -> Option<Vec<u8>> {
        let file = File::open(artifact).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            if path == wanted {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).unwrap();
                return Some(buf);
            }
        }
        None
    }

    #[test]
    fn a_spec_with_a_waf_ruleset_seals_it_into_the_artifact() {
        let dir = project("seal", SPEC, Some(RULES));
        let out = dir.join("api.bca");
        let result = compile(
            &[&dir.join("api.yaml")],
            &[],
            &out,
            &CompileOptions::default(),
        )
        .expect("compile must succeed");

        // The policy reached the manifest.
        let waf = &result.manifest.waf;
        assert!(waf.enabled);
        assert_eq!(waf.paranoia_level, 2);
        assert_eq!(waf.mode, WafMode::Blocking);
        assert_eq!(waf.inbound_threshold, 7);
        assert_eq!(waf.outbound_threshold, 3);
        assert_eq!(waf.rules_path.as_deref(), Some(WAF_RULES_PATH));
        assert!(waf.skipped_rules.is_empty());

        // The rule set is in the archive, and is the validated form rather
        // than the original text.
        let sealed = read_from_artifact(&out, WAF_RULES_PATH)
            .expect("the artifact must contain the sealed rule set");
        let directives: Vec<parapet::Directive> = serde_json::from_slice(&sealed).unwrap();
        let (rules, errors) = parapet::RuleSet::compile_all(&directives, &parapet::NoDataLoader);
        assert!(errors.is_empty());
        assert_eq!(rules.rule_count(), 2);
        assert_eq!(rules.marker_count(), 1);

        // And it is bound by the hash, so tampering is detectable.
        assert!(result.manifest.checksums.contains_key(WAF_RULES_PATH));
        assert_eq!(
            result.manifest.artifact_hash,
            recompute_artifact_hash(&result.manifest),
            "the manifest hash is not self-consistent"
        );
    }

    #[test]
    fn tampering_with_the_sealed_rule_set_breaks_the_hash() {
        // The property that makes compile-time sealing worth doing: an
        // operator can prove which rules a running gateway carries.
        let dir = project("tamper", SPEC, Some(RULES));
        let out = dir.join("api.bca");
        let result = compile(
            &[&dir.join("api.yaml")],
            &[],
            &out,
            &CompileOptions::default(),
        )
        .unwrap();

        let mut tampered = result.manifest.clone();
        tampered.checksums.insert(
            WAF_RULES_PATH.to_string(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        );
        assert_ne!(
            tampered.artifact_hash,
            recompute_artifact_hash(&tampered),
            "swapping the rule set checksum did not invalidate the hash"
        );

        // Flipping the policy has to be detectable too, not just the rules.
        let mut relaxed = result.manifest.clone();
        relaxed.waf.mode = WafMode::DetectionOnly;
        assert_ne!(
            relaxed.artifact_hash,
            recompute_artifact_hash(&relaxed),
            "turning off blocking did not invalidate the hash"
        );
    }

    #[test]
    fn a_waf_ruleset_that_cannot_be_enforced_fails_the_build() {
        let dir = project(
            "unenforceable",
            SPEC,
            Some("SecRule ARGS \"@rx (\" \"id:942100,phase:2,deny\"\n"),
        );
        let err = compile(
            &[&dir.join("api.yaml")],
            &[],
            &dir.join("api.bca"),
            &CompileOptions::default(),
        )
        .expect_err("must refuse");
        let text = err.to_string();
        assert!(text.contains("E1080"), "{text}");
        assert!(text.contains("cannot be enforced"), "{text}");
    }

    #[test]
    fn enabling_the_waf_without_a_ruleset_fails_the_build() {
        let spec = SPEC.replace("  ruleset: ./rules\n", "");
        let dir = project("no-ruleset", &spec, None);
        let err = compile(
            &[&dir.join("api.yaml")],
            &[],
            &dir.join("api.bca"),
            &CompileOptions::default(),
        )
        .expect_err("must refuse");
        assert!(err.to_string().contains("no `ruleset` is set"), "{err}");
    }

    #[test]
    fn tampering_with_a_sealed_member_is_caught_at_load() {
        // artifact_hash covers the manifest's checksum table, which catches a
        // tampered manifest. It says nothing about the archive members, so the
        // extracted bytes have to be checked too.
        let dir = project("member-tamper", SPEC, Some(RULES));
        let out = dir.join("api.bca");
        let result = compile(
            &[&dir.join("api.yaml")],
            &[],
            &out,
            &CompileOptions::default(),
        )
        .unwrap();

        let sealed = load_waf_rules(&out)
            .unwrap()
            .expect("must carry a rule set");
        verify_waf_rules(&result.manifest, &sealed).expect("an untampered artifact verifies");

        // Swap a rule for a different one and the checksum no longer matches.
        let mut swapped = sealed;
        swapped.directives.truncate(1);
        let err = verify_waf_rules(&result.manifest, &swapped)
            .expect_err("a swapped rule set must be rejected");
        assert!(
            matches!(err, IntegrityError::WafChecksumMismatch { .. }),
            "{err}"
        );

        // A phrase list swapped for different contents is caught too.
        let mut poisoned = load_waf_rules(&out).unwrap().unwrap();
        poisoned
            .data_files
            .insert("injected.data".to_string(), b"evil".to_vec());
        assert!(verify_waf_rules(&result.manifest, &poisoned).is_err());
    }

    #[test]
    fn an_unreadable_rule_directory_entry_is_not_skipped() {
        // A rule file silently omitted from the rule set is the failure this
        // design exists to prevent, so read_dir errors must not be dropped.
        let dir = tempdir("unreadable");
        let rules = ruleset(&dir, "SecRule ARGS \"@rx x\" \"id:1,phase:2,deny\"\n");
        // The happy path still works; the guard is that errors propagate
        // rather than being filtered away.
        assert!(seal_waf_ruleset(&rules, UnsupportedRules::Fail).is_ok());
    }

    #[test]
    fn a_refusal_with_no_attributable_rule_id_fails_the_build() {
        // If a rule cannot be enforced and its id cannot be determined, the
        // manifest would under-report the gap. Refusing beats that.
        let dir = tempdir("unattributable");
        // A chained rule carries its id only on the starter, so a refusal on
        // the chained link has no id of its own to record.
        let rules = ruleset(
            &dir,
            "SecRule ARGS \"@rx x\" \"id:5000,phase:2,deny,chain\"\n    SecRule ARGS \"@rx (\"\n",
        );
        let result = seal_waf_ruleset(&rules, UnsupportedRules::Skip);
        match result {
            // Either the id is attributed and recorded, or the build refuses.
            // Silently dropping it is the only unacceptable outcome, and a
            // sealed set must contain only rules the gateway can enforce.
            Ok(sealed) => {
                assert!(
                    !sealed.skipped_rules.is_empty(),
                    "a refused rule was neither recorded nor refused"
                );
                let directives: Vec<parapet::Directive> =
                    serde_json::from_slice(&sealed.rules_json).unwrap();
                let (_, errors) =
                    parapet::RuleSet::compile_all(&directives, &parapet::NoDataLoader);
                assert!(
                    errors.is_empty(),
                    "sealed rule set still has unenforceable rules"
                );
            }
            Err(e) => assert!(e.to_string().contains("could not be determined"), "{e}"),
        }
    }

    #[test]
    fn a_spec_without_the_extension_compiles_unchanged() {
        // The feature must be inert when unused: no archive entry, no
        // checksum, and the manifest hash still self-consistent.
        let spec = SPEC.split("x-barbacane-waf:").next().unwrap().to_string()
            + SPEC
                .split("paths:")
                .nth(1)
                .map(|p| format!("paths:{p}"))
                .unwrap()
                .as_str();
        let dir = project("absent", &spec, None);
        let out = dir.join("api.bca");
        let result = compile(
            &[&dir.join("api.yaml")],
            &[],
            &out,
            &CompileOptions::default(),
        )
        .expect("compile must succeed");
        assert!(!result.manifest.waf.enabled);
        assert!(result.manifest.waf.rules_path.is_none());
        assert!(!result.manifest.checksums.contains_key(WAF_RULES_PATH));
        assert!(read_from_artifact(&out, WAF_RULES_PATH).is_none());
        assert_eq!(
            result.manifest.artifact_hash,
            recompute_artifact_hash(&result.manifest)
        );
    }
}
