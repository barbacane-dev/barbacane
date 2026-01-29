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

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
