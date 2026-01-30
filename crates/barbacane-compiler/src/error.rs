use thiserror::Error;

/// Errors produced during compilation.
#[derive(Debug, Error)]
pub enum CompileError {
    /// Spec parsing failed.
    #[error(transparent)]
    Parse(#[from] barbacane_spec_parser::ParseError),

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
