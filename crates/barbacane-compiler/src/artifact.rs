use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::CompileError;

/// The manifest.json embedded in a .bca artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub barbacane_artifact_version: u32,
    pub compiled_at: String,
    pub compiler_version: String,
    pub source_specs: Vec<SourceSpec>,
    pub routes_count: usize,
    pub checksums: std::collections::HashMap<String, String>,
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

/// Compile one or more spec files into a .bca artifact.
pub fn compile(_spec_paths: &[&Path], _output: &Path) -> Result<Manifest, CompileError> {
    todo!("M1: implement compilation pipeline")
}
