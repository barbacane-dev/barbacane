//! Path rewriting functions.

use crate::config::PathConfig;
use regex::Regex;

/// Transform path according to configuration.
///
/// Returns the transformed path, or the original path if transformation fails.
pub fn transform_path(path: &str, config: &PathConfig) -> String {
    let mut result = path.to_string();

    // Strip prefix
    if let Some(prefix) = &config.strip_prefix {
        if let Some(stripped) = result.strip_prefix(prefix) {
            // Ensure path starts with /
            result = if stripped.is_empty() || !stripped.starts_with('/') {
                format!("/{}", stripped)
            } else {
                stripped.to_string()
            };
        }
    }

    // Add prefix
    if let Some(prefix) = &config.add_prefix {
        // Normalize prefix (ensure it starts with / and doesn't end with /)
        let normalized_prefix = if prefix.starts_with('/') {
            prefix.trim_end_matches('/').to_string()
        } else {
            format!("/{}", prefix.trim_end_matches('/'))
        };

        // Ensure result starts with /
        let normalized_path = if result.starts_with('/') {
            result
        } else {
            format!("/{}", result)
        };

        result = format!("{}{}", normalized_prefix, normalized_path);
    }

    // Regex replace
    if let Some(replace_config) = &config.replace {
        match Regex::new(&replace_config.pattern) {
            Ok(re) => {
                result = re.replace_all(&result, &replace_config.replacement).to_string();
            }
            Err(e) => {
                // Log error and skip replacement
                log_message(1, &format!("Invalid regex pattern '{}': {}", replace_config.pattern, e));
            }
        }
    }

    result
}

/// Log a message via host_log.
#[cfg(not(test))]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

/// Mock log_message for tests.
#[cfg(test)]
fn log_message(_level: i32, _msg: &str) {
    // No-op in tests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PathReplaceConfig;

    #[test]
    fn test_strip_prefix() {
        let mut config = PathConfig::default();
        config.strip_prefix = Some("/api/v2".to_string());

        assert_eq!(transform_path("/api/v2/users", &config), "/users");
        assert_eq!(transform_path("/api/v2/users/123", &config), "/users/123");
        assert_eq!(transform_path("/api/v2", &config), "/");
        assert_eq!(transform_path("/other/path", &config), "/other/path"); // No match
    }

    #[test]
    fn test_add_prefix() {
        let mut config = PathConfig::default();
        config.add_prefix = Some("/internal".to_string());

        assert_eq!(transform_path("/users", &config), "/internal/users");
        assert_eq!(transform_path("/users/123", &config), "/internal/users/123");
    }

    #[test]
    fn test_add_prefix_normalization() {
        let mut config = PathConfig::default();
        config.add_prefix = Some("internal/".to_string()); // No leading /, trailing /

        assert_eq!(transform_path("/users", &config), "/internal/users");

        config.add_prefix = Some("/internal/".to_string()); // Trailing /
        assert_eq!(transform_path("/users", &config), "/internal/users");
    }

    #[test]
    fn test_strip_and_add_prefix() {
        let mut config = PathConfig::default();
        config.strip_prefix = Some("/api/v2".to_string());
        config.add_prefix = Some("/internal".to_string());

        assert_eq!(transform_path("/api/v2/users", &config), "/internal/users");
        assert_eq!(transform_path("/api/v2/users/123", &config), "/internal/users/123");
    }

    #[test]
    fn test_regex_replace() {
        let mut config = PathConfig::default();
        config.replace = Some(PathReplaceConfig {
            pattern: r"/v\d+/".to_string(),
            replacement: "/".to_string(),
        });

        assert_eq!(transform_path("/api/v1/users", &config), "/api/users");
        assert_eq!(transform_path("/api/v2/orders", &config), "/api/orders");
        assert_eq!(transform_path("/api/users", &config), "/api/users"); // No match
    }

    #[test]
    fn test_regex_with_capture_groups() {
        let mut config = PathConfig::default();
        config.replace = Some(PathReplaceConfig {
            pattern: r"/users/(\d+)".to_string(),
            replacement: "/user/$1/profile".to_string(),
        });

        assert_eq!(transform_path("/users/123", &config), "/user/123/profile");
        assert_eq!(transform_path("/users/456/data", &config), "/user/456/profile/data");
    }

    #[test]
    fn test_all_transformations() {
        let mut config = PathConfig::default();
        config.strip_prefix = Some("/api".to_string());
        config.add_prefix = Some("/internal".to_string());
        config.replace = Some(PathReplaceConfig {
            pattern: r"/v\d+".to_string(),
            replacement: "".to_string(),
        });

        // /api/v2/users → /v2/users (strip) → /internal/v2/users (add) → /internal/users (replace)
        assert_eq!(transform_path("/api/v2/users", &config), "/internal/users");
    }

    #[test]
    fn test_invalid_regex() {
        let mut config = PathConfig::default();
        config.replace = Some(PathReplaceConfig {
            pattern: "[invalid(".to_string(), // Invalid regex
            replacement: "replacement".to_string(),
        });

        // Should return original path and log error (but not panic)
        assert_eq!(transform_path("/users/123", &config), "/users/123");
    }

    #[test]
    fn test_empty_path() {
        let mut config = PathConfig::default();
        config.add_prefix = Some("/api".to_string());

        assert_eq!(transform_path("", &config), "/api/");
    }

    #[test]
    fn test_root_path() {
        let mut config = PathConfig::default();
        config.add_prefix = Some("/api".to_string());

        assert_eq!(transform_path("/", &config), "/api/");
    }
}
