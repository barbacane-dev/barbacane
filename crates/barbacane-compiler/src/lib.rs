//! Compiles OpenAPI/AsyncAPI specs into `.bca` (Barbacane Compiled Artifact).
//!
//! Validates specs, resolves extensions, builds routing trie,
//! and produces a self-contained archive for the data plane.

pub mod artifact;
pub mod error;
pub mod manifest;

pub use artifact::{
    compile, compile_with_manifest, compile_with_options, compile_with_plugins, load_manifest,
    load_plugins, load_routes, load_specs, BundledPlugin, CompileOptions, CompiledOperation,
    CompiledRoutes, Manifest, PluginBundle, SourceSpec, ARTIFACT_VERSION, COMPILER_VERSION,
};
pub use error::CompileError;
pub use manifest::{PathSource, PluginSource, ProjectManifest, ResolvedPlugin, UrlSource};
// Re-export validation types from spec-parser
pub use barbacane_spec_parser::{ContentSchema, MiddlewareConfig, Parameter, RequestBody};
