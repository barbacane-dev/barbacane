//! HTTP client for outbound requests from WASM plugins.
//!
//! Provides connection pooling, TLS, timeouts, and circuit breaker support.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

/// HTTP client with connection pooling and circuit breaker support.
#[derive(Clone)]
pub struct HttpClient {
    client: Client,
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

        Ok(Self {
            client,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            default_timeout: config.default_timeout,
            allow_plaintext: config.allow_plaintext,
        })
    }

    /// Make an HTTP request.
    pub async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
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

        // Build request
        let method = request
            .method
            .parse::<reqwest::Method>()
            .map_err(|e| HttpClientError::InvalidMethod(e.to_string()))?;

        let timeout = request.timeout.unwrap_or(self.default_timeout);

        let mut req_builder = self.client.request(method, url).timeout(timeout);

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
}
