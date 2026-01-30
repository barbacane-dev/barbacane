//! HTTP client for outbound requests from WASM plugins.
//!
//! Provides connection pooling, TLS, timeouts, and circuit breaker support.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use reqwest::{Certificate, Client, Identity};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

/// TLS configuration for upstream mTLS connections.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TlsConfig {
    /// Path to PEM-encoded client certificate.
    #[serde(default)]
    pub client_cert: Option<PathBuf>,
    /// Path to PEM-encoded client private key.
    #[serde(default)]
    pub client_key: Option<PathBuf>,
    /// Path to PEM-encoded CA certificate for server verification.
    #[serde(default)]
    pub ca: Option<PathBuf>,
}

impl TlsConfig {
    /// Returns true if any TLS configuration is specified.
    pub fn is_configured(&self) -> bool {
        self.client_cert.is_some() || self.client_key.is_some() || self.ca.is_some()
    }

    /// Validate that if client_cert is set, client_key must also be set (and vice versa).
    pub fn validate(&self) -> Result<(), TlsConfigError> {
        match (&self.client_cert, &self.client_key) {
            (Some(_), None) => Err(TlsConfigError::MissingClientKey),
            (None, Some(_)) => Err(TlsConfigError::MissingClientCert),
            _ => Ok(()),
        }
    }

    /// Create a cache key for this TLS configuration.
    fn cache_key(&self) -> TlsCacheKey {
        TlsCacheKey {
            client_cert: self.client_cert.clone(),
            client_key: self.client_key.clone(),
            ca: self.ca.clone(),
        }
    }
}

/// TLS configuration errors.
#[derive(Debug, Error)]
pub enum TlsConfigError {
    #[error("client_cert specified but client_key is missing")]
    MissingClientKey,
    #[error("client_key specified but client_cert is missing")]
    MissingClientCert,
    #[error("failed to read certificate file: {0}")]
    ReadCertificate(#[source] std::io::Error),
    #[error("failed to read key file: {0}")]
    ReadKey(#[source] std::io::Error),
    #[error("failed to read CA file: {0}")]
    ReadCa(#[source] std::io::Error),
    #[error("failed to parse PEM identity: {0}")]
    ParseIdentity(#[source] reqwest::Error),
    #[error("failed to parse CA certificate: {0}")]
    ParseCaCert(#[source] reqwest::Error),
}

/// Cache key for TLS-configured clients.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TlsCacheKey {
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    ca: Option<PathBuf>,
}

impl Hash for TlsCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.client_cert.hash(state);
        self.client_key.hash(state);
        self.ca.hash(state);
    }
}

/// HTTP client with connection pooling and circuit breaker support.
#[derive(Clone)]
pub struct HttpClient {
    /// Default client (no mTLS).
    client: Client,
    /// Cached clients with specific TLS configurations.
    tls_clients: Arc<RwLock<HashMap<TlsCacheKey, Client>>>,
    /// Base config for creating new clients.
    base_config: HttpClientConfig,
    circuit_breakers: Arc<RwLock<HashMap<String, CircuitBreaker>>>,
    default_timeout: Duration,
    allow_plaintext: bool,
}

impl HttpClient {
    /// Create a new HTTP client.
    pub fn new(config: HttpClientConfig) -> Result<Self, HttpClientError> {
        let client = Client::builder()
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .connect_timeout(config.connect_timeout)
            .timeout(config.default_timeout)
            .build()
            .map_err(HttpClientError::BuildError)?;

        let default_timeout = config.default_timeout;
        let allow_plaintext = config.allow_plaintext;

        Ok(Self {
            client,
            tls_clients: Arc::new(RwLock::new(HashMap::new())),
            base_config: config,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            default_timeout,
            allow_plaintext,
        })
    }

    /// Get or create a client with the specified TLS configuration.
    fn get_or_create_tls_client(&self, tls_config: &TlsConfig) -> Result<Client, HttpClientError> {
        let cache_key = tls_config.cache_key();

        // Check if we already have a client for this config
        {
            let clients = self.tls_clients.read();
            if let Some(client) = clients.get(&cache_key) {
                return Ok(client.clone());
            }
        }

        // Create a new client with TLS config
        let client = self.build_tls_client(tls_config)?;

        // Cache it
        {
            let mut clients = self.tls_clients.write();
            clients.insert(cache_key, client.clone());
        }

        Ok(client)
    }

    /// Build a new client with the specified TLS configuration.
    fn build_tls_client(&self, tls_config: &TlsConfig) -> Result<Client, HttpClientError> {
        tls_config.validate().map_err(HttpClientError::TlsConfig)?;

        let mut builder = Client::builder()
            .pool_max_idle_per_host(self.base_config.pool_max_idle_per_host)
            .pool_idle_timeout(self.base_config.pool_idle_timeout)
            .connect_timeout(self.base_config.connect_timeout)
            .timeout(self.base_config.default_timeout);

        // Add client certificate (mTLS)
        if let (Some(cert_path), Some(key_path)) = (&tls_config.client_cert, &tls_config.client_key)
        {
            let cert_pem = std::fs::read(cert_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadCertificate(e)))?;
            let key_pem = std::fs::read(key_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadKey(e)))?;

            // Combine cert and key for Identity::from_pem
            let mut pem = cert_pem;
            pem.extend_from_slice(&key_pem);

            let identity = Identity::from_pem(&pem)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ParseIdentity(e)))?;

            builder = builder.identity(identity);
        }

        // Add custom CA certificate
        if let Some(ca_path) = &tls_config.ca {
            let ca_pem = std::fs::read(ca_path)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ReadCa(e)))?;

            let ca_cert = Certificate::from_pem(&ca_pem)
                .map_err(|e| HttpClientError::TlsConfig(TlsConfigError::ParseCaCert(e)))?;

            builder = builder.add_root_certificate(ca_cert);
        }

        builder.build().map_err(HttpClientError::BuildError)
    }

    /// Make an HTTP request.
    pub async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.call_with_tls(request, None).await
    }

    /// Make an HTTP request with optional TLS configuration for mTLS.
    pub async fn call_with_tls(
        &self,
        request: HttpRequest,
        tls_config: Option<&TlsConfig>,
    ) -> Result<HttpResponse, HttpClientError> {
        // Validate URL scheme
        let url = request
            .url
            .parse::<reqwest::Url>()
            .map_err(|e| HttpClientError::InvalidUrl(e.to_string()))?;

        if url.scheme() == "http" && !self.allow_plaintext {
            return Err(HttpClientError::PlaintextNotAllowed);
        }

        // Extract host for circuit breaker
        let host = url
            .host_str()
            .ok_or_else(|| HttpClientError::InvalidUrl("missing host".into()))?
            .to_string();

        // Check circuit breaker
        let circuit_state = self.get_circuit_state(&host);
        if circuit_state == CircuitState::Open {
            return Err(HttpClientError::CircuitOpen(host));
        }

        // Get the appropriate client (default or TLS-configured)
        let client = match tls_config {
            Some(tls) if tls.is_configured() => self.get_or_create_tls_client(tls)?,
            _ => self.client.clone(),
        };

        // Build request
        let method = request
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| HttpClientError::InvalidMethod(e.to_string()))?;

        let timeout = request.timeout.unwrap_or(self.default_timeout);

        let mut req_builder = client.request(method, url).timeout(timeout);

        // Add headers
        for (key, value) in &request.headers {
            req_builder = req_builder.header(key.as_str(), value.as_str());
        }

        // Add body
        if let Some(body) = request.body {
            req_builder = req_builder.body(body);
        }

        // Execute request
        let result = req_builder.send().await;

        match result {
            Ok(response) => {
                // Record success
                self.record_success(&host);

                let status = response.status().as_u16();
                let headers: HashMap<String, String> = response
                    .headers()
                    .iter()
                    .filter_map(|(k, v)| {
                        v.to_str()
                            .ok()
                            .map(|v| (k.as_str().to_lowercase(), v.to_string()))
                    })
                    .collect();

                let body = response
                    .bytes()
                    .await
                    .map_err(HttpClientError::ResponseReadError)?;

                Ok(HttpResponse {
                    status,
                    headers,
                    body: Some(body.to_vec()),
                })
            }
            Err(e) => {
                // Record failure
                self.record_failure(&host);

                if e.is_timeout() {
                    Err(HttpClientError::Timeout)
                } else if e.is_connect() {
                    Err(HttpClientError::ConnectionFailed(e.to_string()))
                } else {
                    Err(HttpClientError::RequestFailed(e.to_string()))
                }
            }
        }
    }

    /// Configure circuit breaker for a host.
    pub fn configure_circuit_breaker(&self, host: &str, config: CircuitBreakerConfig) {
        let mut breakers = self.circuit_breakers.write();
        breakers.insert(host.to_string(), CircuitBreaker::new(config));
    }

    fn get_circuit_state(&self, host: &str) -> CircuitState {
        let breakers = self.circuit_breakers.read();
        breakers
            .get(host)
            .map(|cb| cb.state())
            .unwrap_or(CircuitState::Closed)
    }

    fn record_success(&self, host: &str) {
        let mut breakers = self.circuit_breakers.write();
        if let Some(cb) = breakers.get_mut(host) {
            cb.record_success();
        }
    }

    fn record_failure(&self, host: &str) {
        let mut breakers = self.circuit_breakers.write();
        if let Some(cb) = breakers.get_mut(host) {
            cb.record_failure();
        }
    }
}

/// Configuration for the HTTP client.
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Maximum idle connections per host.
    pub pool_max_idle_per_host: usize,
    /// Idle connection timeout.
    pub pool_idle_timeout: Duration,
    /// Connection timeout.
    pub connect_timeout: Duration,
    /// Default request timeout.
    pub default_timeout: Duration,
    /// Allow plaintext HTTP (development only).
    pub allow_plaintext: bool,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            pool_max_idle_per_host: 10,
            pool_idle_timeout: Duration::from_secs(90),
            connect_timeout: Duration::from_secs(10),
            default_timeout: Duration::from_secs(30),
            allow_plaintext: false,
        }
    }
}

/// HTTP request from WASM plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    /// HTTP method (GET, POST, etc.)
    pub method: String,
    /// Full URL including scheme and host.
    pub url: String,
    /// Request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Request body (optional).
    #[serde(default)]
    pub body: Option<Vec<u8>>,
    /// Request timeout (optional, uses client default).
    #[serde(default, with = "option_duration_serde")]
    pub timeout: Option<Duration>,
}

/// HTTP response to WASM plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Response body (optional).
    pub body: Option<Vec<u8>>,
}

impl HttpResponse {
    /// Create an error response.
    pub fn error(status: u16, error_type: &str, title: &str, detail: &str) -> Self {
        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": detail
        });

        let mut headers = HashMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        Self {
            status,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }
}

/// HTTP client errors.
#[derive(Debug, Error)]
pub enum HttpClientError {
    #[error("failed to build HTTP client: {0}")]
    BuildError(#[source] reqwest::Error),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("invalid HTTP method: {0}")]
    InvalidMethod(String),

    #[error("plaintext HTTP not allowed")]
    PlaintextNotAllowed,

    #[error("circuit breaker open for host: {0}")]
    CircuitOpen(String),

    #[error("request timeout")]
    Timeout,

    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("failed to read response: {0}")]
    ResponseReadError(#[source] reqwest::Error),

    #[error("TLS configuration error: {0}")]
    TlsConfig(#[source] TlsConfigError),
}

/// Custom serde for Option<Duration> in seconds.
mod option_duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match duration {
            Some(d) => d.as_secs_f64().serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<f64> = Option::deserialize(deserializer)?;
        Ok(opt.map(Duration::from_secs_f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = HttpClientConfig::default();
        assert_eq!(config.pool_max_idle_per_host, 10);
        assert_eq!(config.default_timeout, Duration::from_secs(30));
        assert!(!config.allow_plaintext);
    }

    #[test]
    fn test_error_response() {
        let resp = HttpResponse::error(
            502,
            "urn:barbacane:error:upstream-unavailable",
            "Bad Gateway",
            "Failed to connect to upstream",
        );

        assert_eq!(resp.status, 502);
        assert_eq!(
            resp.headers.get("content-type"),
            Some(&"application/problem+json".to_string())
        );
    }

    #[test]
    fn test_tls_config_default() {
        let tls = TlsConfig::default();
        assert!(tls.client_cert.is_none());
        assert!(tls.client_key.is_none());
        assert!(tls.ca.is_none());
        assert!(!tls.is_configured());
    }

    #[test]
    fn test_tls_config_is_configured() {
        let mut tls = TlsConfig::default();
        assert!(!tls.is_configured());

        tls.client_cert = Some(PathBuf::from("/path/to/cert.pem"));
        assert!(tls.is_configured());

        tls.client_cert = None;
        tls.ca = Some(PathBuf::from("/path/to/ca.pem"));
        assert!(tls.is_configured());
    }

    #[test]
    fn test_tls_config_validate_success() {
        // Empty config is valid
        let tls = TlsConfig::default();
        assert!(tls.validate().is_ok());

        // CA only is valid
        let tls = TlsConfig {
            client_cert: None,
            client_key: None,
            ca: Some(PathBuf::from("/path/to/ca.pem")),
        };
        assert!(tls.validate().is_ok());

        // Both cert and key is valid
        let tls = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        assert!(tls.validate().is_ok());
    }

    #[test]
    fn test_tls_config_validate_missing_key() {
        let tls = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: None,
            ca: None,
        };
        let err = tls.validate().unwrap_err();
        assert!(matches!(err, TlsConfigError::MissingClientKey));
    }

    #[test]
    fn test_tls_config_validate_missing_cert() {
        let tls = TlsConfig {
            client_cert: None,
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let err = tls.validate().unwrap_err();
        assert!(matches!(err, TlsConfigError::MissingClientCert));
    }

    #[test]
    fn test_tls_config_serde() {
        let json = r#"{
            "client_cert": "/etc/certs/client.crt",
            "client_key": "/etc/certs/client.key",
            "ca": "/etc/certs/ca.crt"
        }"#;

        let tls: TlsConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            tls.client_cert,
            Some(PathBuf::from("/etc/certs/client.crt"))
        );
        assert_eq!(tls.client_key, Some(PathBuf::from("/etc/certs/client.key")));
        assert_eq!(tls.ca, Some(PathBuf::from("/etc/certs/ca.crt")));
    }

    #[test]
    fn test_tls_config_serde_partial() {
        let json = r#"{"ca": "/etc/certs/ca.crt"}"#;

        let tls: TlsConfig = serde_json::from_str(json).unwrap();
        assert!(tls.client_cert.is_none());
        assert!(tls.client_key.is_none());
        assert_eq!(tls.ca, Some(PathBuf::from("/etc/certs/ca.crt")));
    }

    #[test]
    fn test_tls_cache_key_equality() {
        let tls1 = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let tls2 = TlsConfig {
            client_cert: Some(PathBuf::from("/path/to/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };
        let tls3 = TlsConfig {
            client_cert: Some(PathBuf::from("/other/cert.pem")),
            client_key: Some(PathBuf::from("/path/to/key.pem")),
            ca: None,
        };

        assert_eq!(tls1.cache_key(), tls2.cache_key());
        assert_ne!(tls1.cache_key(), tls3.cache_key());
    }
}
