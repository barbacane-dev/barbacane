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

    /// E1080: The WAF rule set is missing, unreadable, or cannot be enforced.
    ///
    /// Compilation refuses rather than shipping a rule set the gateway would
    /// only partly enforce: a rule that never fires is indistinguishable from
    /// a rule that found nothing.
    #[error("E1080: x-barbacane-waf: {0}")]
    WafRuleset(String),

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

    /// E1056: A declared header name is in the `x-auth-*` namespace, which
    /// belongs to the auth plugins' output. A spec able to declare one would let
    /// a client supply an identity the gateway trusts.
    #[error("E1056: reserved header name, x-auth-* belongs to auth plugins: {0}")]
    ReservedHeaderName(String),

    /// E1057: An operation runs an authentication plugin without naming the
    /// security scheme that carries the credential, so nothing says which header
    /// the caller sends it in and the operation reads as anonymous.
    #[error("E1057: authentication plugin without a security requirement: {0}")]
    MissingSecurityRequirement(String),

    /// E1023: A plugin's configuration does not satisfy the JSON Schema the
    /// plugin publishes for it. The plugin refuses such a configuration at
    /// init, which the data plane surfaces as a 500 on every request through it,
    /// so the artifact is refused instead.
    #[error("E1023: invalid config for plugin '{0}': {1}")]
    InvalidPluginConfig(String, String),

    /// E1032: An operation pairs an authentication plugin with a security
    /// requirement naming no scheme that plugin reads. Both halves are valid on
    /// their own; the pairing rejects every request, since the plugin looks for
    /// a credential the document says the client does not send.
    #[error("E1032: security scheme the authentication plugin does not implement: {0}")]
    UnimplementedSecurityScheme(String),

    /// E1051: Schema exceeds maximum nesting depth.
    #[error("E1051: schema too deep: {0}")]
    SchemaTooDeep(String),

    /// E1052: Schema exceeds maximum property count.
    #[error("E1052: schema too complex: {0}")]
    SchemaTooComplex(String),

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

    /// Artifact signing failed (bad/missing signing key).
    #[error("artifact signing error: {0}")]
    Signing(String),
}
