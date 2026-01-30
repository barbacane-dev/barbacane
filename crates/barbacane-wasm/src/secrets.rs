//! Secrets resolution for WASM plugins.
//!
//! Supports resolving secret references from configuration values:
//! - `env://VAR_NAME` - Environment variable
//! - `file:///path/to/secret` - File-based secret (trimmed)
//!
//! Future: `vault://`, `aws-sm://`, `k8s://` references.

use std::collections::HashMap;
use std::sync::Arc;

use thiserror::Error;

/// Errors during secret resolution.
#[derive(Debug, Error)]
pub enum SecretsError {
    #[error("environment variable not found: {0}")]
    EnvNotFound(String),

    #[error("file not found: {0}")]
    FileNotFound(String),

    #[error("failed to read file: {0}")]
    FileReadError(String),

    #[error("unsupported secret scheme: {0}")]
    UnsupportedScheme(String),

    #[error("invalid secret reference: {0}")]
    InvalidReference(String),
}

/// Resolved secrets store.
///
/// Thread-safe cache of resolved secret values, keyed by reference.
#[derive(Debug, Clone, Default)]
pub struct SecretsStore {
    secrets: Arc<HashMap<String, String>>,
}

impl SecretsStore {
    /// Create an empty secrets store.
    pub fn new() -> Self {
        Self {
            secrets: Arc::new(HashMap::new()),
        }
    }

    /// Create a secrets store from a map of resolved secrets.
    pub fn from_map(secrets: HashMap<String, String>) -> Self {
        Self {
            secrets: Arc::new(secrets),
        }
    }

    /// Get a secret by its reference.
    pub fn get(&self, reference: &str) -> Option<&String> {
        self.secrets.get(reference)
    }

    /// Check if a reference exists in the store.
    pub fn contains(&self, reference: &str) -> bool {
        self.secrets.contains_key(reference)
    }
}

/// Check if a string value is a secret reference.
pub fn is_secret_reference(value: &str) -> bool {
    value.starts_with("env://")
        || value.starts_with("file://")
        || value.starts_with("vault://")
        || value.starts_with("aws-sm://")
        || value.starts_with("k8s://")
}

/// Resolve a single secret reference.
///
/// Currently supports:
/// - `env://VAR_NAME` - Environment variable
/// - `file:///path/to/secret` - File content (trimmed)
pub fn resolve_secret(reference: &str) -> Result<String, SecretsError> {
    if let Some(var_name) = reference.strip_prefix("env://") {
        std::env::var(var_name).map_err(|_| SecretsError::EnvNotFound(var_name.to_string()))
    } else if let Some(path) = reference.strip_prefix("file://") {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    SecretsError::FileNotFound(path.to_string())
                } else {
                    SecretsError::FileReadError(format!("{}: {}", path, e))
                }
            })
    } else if reference.starts_with("vault://")
        || reference.starts_with("aws-sm://")
        || reference.starts_with("k8s://")
    {
        Err(SecretsError::UnsupportedScheme(
            reference
                .split("://")
                .next()
                .unwrap_or("unknown")
                .to_string(),
        ))
    } else {
        Err(SecretsError::InvalidReference(reference.to_string()))
    }
}

/// Scan a JSON value for secret references and collect them.
pub fn collect_secret_references(value: &serde_json::Value) -> Vec<String> {
    let mut refs = Vec::new();
    collect_refs_recursive(value, &mut refs);
    refs
}

fn collect_refs_recursive(value: &serde_json::Value, refs: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if is_secret_reference(s) {
                refs.push(s.clone());
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                collect_refs_recursive(item, refs);
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values() {
                collect_refs_recursive(v, refs);
            }
        }
        _ => {}
    }
}

/// Replace secret references in a JSON value with resolved values.
pub fn resolve_config_secrets(
    value: &serde_json::Value,
    store: &SecretsStore,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            if is_secret_reference(s) {
                if let Some(resolved) = store.get(s) {
                    serde_json::Value::String(resolved.clone())
                } else {
                    // Keep original if not resolved (shouldn't happen after validation)
                    value.clone()
                }
            } else {
                value.clone()
            }
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| resolve_config_secrets(v, store))
                .collect(),
        ),
        serde_json::Value::Object(obj) => {
            let resolved: serde_json::Map<String, serde_json::Value> = obj
                .iter()
                .map(|(k, v)| (k.clone(), resolve_config_secrets(v, store)))
                .collect();
            serde_json::Value::Object(resolved)
        }
        _ => value.clone(),
    }
}

/// Resolve all secrets from a list of configs.
///
/// Returns a SecretsStore with all resolved values, or an error if any secret
/// cannot be resolved.
pub fn resolve_all_secrets(
    configs: &[&serde_json::Value],
) -> Result<SecretsStore, Vec<SecretsError>> {
    // Collect all unique references
    let mut all_refs: Vec<String> = configs
        .iter()
        .flat_map(|c| collect_secret_references(c))
        .collect();
    all_refs.sort();
    all_refs.dedup();

    // Resolve each reference
    let mut resolved = HashMap::new();
    let mut errors = Vec::new();

    for reference in all_refs {
        match resolve_secret(&reference) {
            Ok(value) => {
                resolved.insert(reference, value);
            }
            Err(e) => {
                errors.push(e);
            }
        }
    }

    if errors.is_empty() {
        Ok(SecretsStore::from_map(resolved))
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_secret_reference() {
        assert!(is_secret_reference("env://MY_VAR"));
        assert!(is_secret_reference("file:///etc/secret"));
        assert!(is_secret_reference("vault://secrets/key"));
        assert!(is_secret_reference("aws-sm://prod/key"));
        assert!(is_secret_reference("k8s://ns/secret/key"));

        assert!(!is_secret_reference("plain-value"));
        assert!(!is_secret_reference("https://example.com"));
        assert!(!is_secret_reference(""));
    }

    #[test]
    fn test_resolve_env_secret() {
        std::env::set_var("TEST_SECRET_VAR", "secret-value");
        let result = resolve_secret("env://TEST_SECRET_VAR");
        assert_eq!(result.unwrap(), "secret-value");
        std::env::remove_var("TEST_SECRET_VAR");
    }

    #[test]
    fn test_resolve_env_not_found() {
        let result = resolve_secret("env://NONEXISTENT_VAR_12345");
        assert!(matches!(result, Err(SecretsError::EnvNotFound(_))));
    }

    #[test]
    fn test_resolve_file_secret() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "file-secret-value").unwrap();

        let result = resolve_secret(&format!("file://{}", path.display()));
        assert_eq!(result.unwrap(), "file-secret-value");
    }

    #[test]
    fn test_resolve_file_not_found() {
        let result = resolve_secret("file:///nonexistent/path/to/secret");
        assert!(matches!(result, Err(SecretsError::FileNotFound(_))));
    }

    #[test]
    fn test_unsupported_scheme() {
        let result = resolve_secret("vault://secrets/key");
        assert!(matches!(result, Err(SecretsError::UnsupportedScheme(_))));
    }

    #[test]
    fn test_collect_secret_references() {
        let config = serde_json::json!({
            "client_id": "my-client",
            "client_secret": "env://OAUTH_SECRET",
            "nested": {
                "key": "file:///etc/key"
            },
            "list": ["plain", "env://LIST_VAR"]
        });

        let refs = collect_secret_references(&config);
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&"env://OAUTH_SECRET".to_string()));
        assert!(refs.contains(&"file:///etc/key".to_string()));
        assert!(refs.contains(&"env://LIST_VAR".to_string()));
    }

    #[test]
    fn test_resolve_config_secrets() {
        let config = serde_json::json!({
            "client_id": "my-client",
            "client_secret": "env://SECRET"
        });

        let mut secrets = HashMap::new();
        secrets.insert("env://SECRET".to_string(), "resolved-secret".to_string());
        let store = SecretsStore::from_map(secrets);

        let resolved = resolve_config_secrets(&config, &store);
        assert_eq!(resolved["client_id"], "my-client");
        assert_eq!(resolved["client_secret"], "resolved-secret");
    }
}
