//! Compiles OpenAPI/AsyncAPI specs into `.bca` (Barbacane Compiled Artifact).
//!
//! Validates specs, resolves extensions, builds routing trie,
//! and produces a self-contained archive for the data plane.

pub mod artifact;
pub(crate) mod cache;
pub(crate) mod download;
pub mod error;
pub mod limits;
pub mod manifest;
pub mod request_schema;
pub mod signing;
pub mod spec_parser;
pub mod wasi;
pub mod yaml;

pub use artifact::compile_with_capabilities;
pub use artifact::{
    check_disabled_middlewares, compile, compile_with_manifest, load_manifest, load_routes,
    recompute_artifact_hash, verify_artifact_hash, verify_artifact_signature, ArtifactArchive,
    BundledPlugin, CompileOptions, CompileResult, CompiledOperation, CompiledRoutes,
    IntegrityError, LoadedPlugin, Manifest, PluginBundle, PluginCapabilities, Provenance,
    SourceSpec, VerifiedArtifact, ARTIFACT_VERSION, COMPILER_VERSION, MAX_ARTIFACT_BYTES,
};
pub use error::{CompileError, CompileWarning};
pub use limits::DeclaredLimits;
pub use manifest::{
    embedded_capabilities, embedded_category, embedded_config_schema, embedded_implements,
    extract_plugin_names, PathSource, PluginSource, ProjectManifest, ResolvedPlugin, UrlSource,
};
pub use request_schema::request_validator;
// Re-export spec-parser types for convenience
pub use spec_parser::{
    parse_spec, parse_spec_file, ApiSpec, AsyncAction, Channel, ContentSchema, DispatchConfig,
    Message, MiddlewareConfig, Operation, Parameter, ParseError, RequestBody, ResponseContent,
    SpecFormat,
};
