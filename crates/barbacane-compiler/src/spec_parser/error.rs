use thiserror::Error;

/// Errors produced during spec parsing (E1001–E1004, E1011).
#[derive(Debug, Error)]
pub enum ParseError {
    /// E1001: File is not valid OpenAPI 3.x or AsyncAPI 3.x.
    #[error("E1001: not a valid OpenAPI 3.x or AsyncAPI 3.x spec")]
    UnknownFormat,

    /// E1002: YAML/JSON parse error.
    #[error("E1002: parse error: {0}")]
    ParseError(String),

    /// E1003: Unresolved $ref.
    #[error("E1003: unresolved $ref: {0}")]
    UnresolvedRef(String),

    /// E1004: Schema validation error.
    #[error("E1004: schema validation error: {0}")]
    SchemaError(String),

    /// E1011: An `x-barbacane-middlewares` value is not a list, or one of its
    /// entries is not an object of `name`, `config` and `enabled`.
    #[error("E1011: invalid middleware entry: {0}")]
    InvalidMiddleware(String),

    /// I/O error reading the spec file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
