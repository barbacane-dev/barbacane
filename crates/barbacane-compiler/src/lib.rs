//! Compiles OpenAPI/AsyncAPI specs into `.bca` (Barbacane Compiled Artifact).
//!
//! Validates specs, resolves extensions, builds routing trie,
//! and produces a self-contained archive for the data plane.

pub mod artifact;
pub mod error;

pub use error::CompileError;
