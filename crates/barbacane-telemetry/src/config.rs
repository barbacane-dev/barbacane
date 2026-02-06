//! Telemetry configuration.

/// Log output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Structured JSON output (production).
    #[default]
    Json,
    /// Human-readable pretty output (development).
    Pretty,
}

impl LogFormat {
    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "json" => Some(Self::Json),
            "pretty" => Some(Self::Pretty),
            _ => None,
        }
    }
}

/// OTLP export protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// gRPC protocol (default).
    #[default]
    Grpc,
    /// HTTP/protobuf protocol.
    Http,
}

impl OtlpProtocol {
    /// Parse from string.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "grpc" => Some(Self::Grpc),
            "http" => Some(Self::Http),
            _ => None,
        }
    }
}

/// Telemetry configuration.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Service name for telemetry (default: "barbacane").
    pub service_name: String,

    /// Log level filter (default: "info").
    pub log_level: String,

    /// Log output format.
    pub log_format: LogFormat,

    /// OTLP endpoint for trace/metric export (optional).
    /// If not set, OTLP export is disabled.
    pub otlp_endpoint: Option<String>,

    /// OTLP protocol to use.
    pub otlp_protocol: OtlpProtocol,

    /// Additional OTLP headers (e.g., for authentication).
    pub otlp_headers: Vec<(String, String)>,

    /// Global trace sampling rate (0.0 to 1.0, default: 1.0).
    pub trace_sampling: f64,

    /// Artifact hash for span attributes (set at runtime).
    pub artifact_hash: Option<String>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: "barbacane".to_string(),
            log_level: "info".to_string(),
            log_format: LogFormat::Json,
            otlp_endpoint: None,
            otlp_protocol: OtlpProtocol::Grpc,
            otlp_headers: Vec::new(),
            trace_sampling: 1.0,
            artifact_hash: None,
        }
    }
}

impl TelemetryConfig {
    /// Create a new telemetry config with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the service name.
    pub fn with_service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Set the log level.
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// Set the log format.
    pub fn with_log_format(mut self, format: LogFormat) -> Self {
        self.log_format = format;
        self
    }

    /// Set the OTLP endpoint.
    pub fn with_otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// Set the OTLP protocol.
    pub fn with_otlp_protocol(mut self, protocol: OtlpProtocol) -> Self {
        self.otlp_protocol = protocol;
        self
    }

    /// Set the trace sampling rate.
    pub fn with_trace_sampling(mut self, rate: f64) -> Self {
        self.trace_sampling = rate.clamp(0.0, 1.0);
        self
    }

    /// Set the artifact hash.
    pub fn with_artifact_hash(mut self, hash: impl Into<String>) -> Self {
        self.artifact_hash = Some(hash.into());
        self
    }
}
