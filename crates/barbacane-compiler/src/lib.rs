//! Compiles OpenAPI/AsyncAPI specs into `.bca` (Barbacane Compiled Artifact).
//!
//! Validates specs, resolves extensions, builds routing trie,
//! and produces a self-contained archive for the data plane.

pub mod artifact;
pub mod error;

pub use artifact::{
    compile, load_manifest, load_routes, load_specs, CompiledOperation, CompiledRoutes, Manifest,
    SourceSpec, ARTIFACT_VERSION, COMPILER_VERSION,
};
pub use error::CompileError;
// Re-export validation types from spec-parser
pub use barbacane_spec_parser::{ContentSchema, Parameter, RequestBody};
