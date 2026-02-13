use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A warning produced during compilation (non-blocking).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileWarning {
    /// Warning code (e.g., "E1015").
    pub code: String,
    /// Warning message.
    pub message: String,
    /// Location in the spec (e.g., "GET /users in 'api.yaml'").
    pub location: Option<String>,
}

/// Errors produced during compilation.
#[derive(Debug, Error)]
pub enum CompileError {
    /// Spec parsing failed.
    #[error(transparent)]
    Parse(#[from] crate::spec_parser::ParseError),

    /// E1010: Routing conflict.
    #[error("E1010: routing conflict: {0}")]
    RoutingConflict(String),

    /// E1020: Operation has no dispatcher.
    #[error("E1020: operation has no x-barbacane-dispatch: {0}")]
    MissingDispatch(String),

    /// E1031: Plaintext HTTP upstream URL in production mode.
    #[error("E1031: plaintext HTTP upstream URL not allowed in production: {0}")]
    PlaintextUpstream(String),

    /// E1040: Plugin used in spec but not declared in manifest.
    #[error("E1040: plugin '{0}' used in spec but not declared in barbacane.yaml")]
    UndeclaredPlugin(String),

    /// E1011: Middleware entry missing required 'name' field.
    #[error("E1011: middleware missing 'name': {0}")]
    MissingMiddlewareName(String),

    /// E1050: Ambiguous route - paths are structurally equivalent but differ in parameter names.
    #[error("E1050: ambiguous route: {0}")]
    AmbiguousRoute(String),

    /// E1051: Schema exceeds maximum nesting depth.
    #[error("E1051: schema too deep: {0}")]
    SchemaTooDeep(String),

    /// E1052: Schema exceeds maximum property count.
    #[error("E1052: schema too complex: {0}")]
    SchemaTooComplex(String),

    /// E1053: Circular $ref detected in schema.
    #[error("E1053: circular schema reference: {0}")]
    CircularSchemaRef(String),

    /// E1054: Invalid path template syntax.
    #[error("E1054: invalid path template: {0}")]
    InvalidPathTemplate(String),

    /// E1055: Duplicate operationId across specs.
    #[error("E1055: duplicate operationId '{0}': {1}")]
    DuplicateOperationId(String, String),

    /// Manifest parsing or loading error.
    #[error("manifest error: {0}")]
    ManifestError(String),

    /// Plugin resolution error (loading WASM bytes).
    #[error("plugin resolution error: {0}")]
    PluginResolution(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
