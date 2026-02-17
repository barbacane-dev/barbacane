//! Compiles OpenAPI/AsyncAPI specs into `.bca` (Barbacane Compiled Artifact).
//!
//! Validates specs, resolves extensions, builds routing trie,
//! and produces a self-contained archive for the data plane.

pub mod artifact;
pub mod error;
pub mod manifest;
pub mod spec_parser;

pub use artifact::{
    compile, compile_with_manifest, load_manifest, load_plugins, load_routes, load_specs,
    BundledPlugin, CompileOptions, CompileResult, CompiledOperation, CompiledRoutes, Manifest,
    PluginBundle, SourceSpec, ARTIFACT_VERSION, COMPILER_VERSION,
};
pub use error::{CompileError, CompileWarning};
pub use manifest::{
    extract_plugin_names, PathSource, PluginSource, ProjectManifest, ResolvedPlugin, UrlSource,
};
// Re-export spec-parser types for convenience
pub use spec_parser::{
    parse_spec, parse_spec_file, ApiSpec, AsyncAction, Channel, ContentSchema, DispatchConfig,
    Message, MiddlewareConfig, Operation, Parameter, ParseError, RequestBody, SpecFormat,
};
