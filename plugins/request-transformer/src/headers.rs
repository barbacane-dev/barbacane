//! Header transformation functions.

use crate::config::HeaderConfig;
use crate::interpolation::interpolate_value;
use barbacane_plugin_sdk::prelude::*;
use std::collections::BTreeMap;

/// Transform request headers according to configuration.
pub fn transform_headers(headers: &mut BTreeMap<String, String>, config: &HeaderConfig, req: &Request) {
    // Remove headers first
    for header_name in &config.remove {
        let header_lower = header_name.to_lowercase();
        headers.remove(&header_lower);
    }

    // Rename headers (move value from old to new, delete old)
    for (old_name, new_name) in &config.rename {
        let old_lower = old_name.to_lowercase();
        let new_lower = new_name.to_lowercase();

        if let Some(value) = headers.remove(&old_lower) {
            headers.insert(new_lower, value);
        }
    }

    // Set headers (add only if not present)
    for (header_name, value_template) in &config.set {
        let header_lower = header_name.to_lowercase();

        if !headers.contains_key(&header_lower) {
            let value = interpolate_value(value_template, req);
            headers.insert(header_lower, value);
        }
    }

    // Add headers (insert or overwrite)
    for (header_name, value_template) in &config.add {
        let header_lower = header_name.to_lowercase();
        let value = interpolate_value(value_template, req);
        headers.insert(header_lower, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "api.example.com".to_string());

        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "123".to_string());

        Request {
            method: "GET".to_string(),
            path: "/users/123".to_string(),
            query: Some("page=2".to_string()),
            headers,
            body: None,
            client_ip: "192.168.1.1".to_string(),
            path_params,
        }
    }

    #[test]
    fn test_add_headers() {
        let req = create_test_request();
        let mut headers = req.headers.clone();

        let mut config = HeaderConfig::default();
        config.add.insert("x-gateway".to_string(), "barbacane".to_string());
        config.add.insert("x-client-ip".to_string(), "$client_ip".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-gateway"), Some(&"barbacane".to_string()));
        assert_eq!(headers.get("x-client-ip"), Some(&"192.168.1.1".to_string()));
    }

    #[test]
    fn test_add_overwrites_existing() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("x-test".to_string(), "old-value".to_string());

        let mut config = HeaderConfig::default();
        config.add.insert("x-test".to_string(), "new-value".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-test"), Some(&"new-value".to_string()));
    }

    #[test]
    fn test_set_headers() {
        let req = create_test_request();
        let mut headers = req.headers.clone();

        let mut config = HeaderConfig::default();
        config.set.insert("x-new".to_string(), "value".to_string());
        config.set.insert("host".to_string(), "should-not-overwrite".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-new"), Some(&"value".to_string()));
        assert_eq!(headers.get("host"), Some(&"api.example.com".to_string())); // Not overwritten
    }

    #[test]
    fn test_remove_headers() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("x-to-remove".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("x-to-remove".to_string());
        config.remove.push("host".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-to-remove"), None);
        assert_eq!(headers.get("host"), None);
    }

    #[test]
    fn test_rename_headers() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("x-old-name".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config.rename.insert("x-old-name".to_string(), "x-new-name".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-old-name"), None);
        assert_eq!(headers.get("x-new-name"), Some(&"value".to_string()));
    }

    #[test]
    fn test_variable_interpolation() {
        let req = create_test_request();
        let mut headers = BTreeMap::new();

        let mut config = HeaderConfig::default();
        config.add.insert("x-user-id".to_string(), "$path.id".to_string());
        config.add.insert("x-page".to_string(), "$query.page".to_string());
        config.add.insert("x-original-host".to_string(), "$header.host".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-user-id"), Some(&"123".to_string()));
        assert_eq!(headers.get("x-page"), Some(&"2".to_string()));
        assert_eq!(headers.get("x-original-host"), Some(&"api.example.com".to_string()));
    }

    #[test]
    fn test_transformation_order() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("to-rename".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("host".to_string());
        config.rename.insert("to-rename".to_string(), "renamed".to_string());
        config.set.insert("x-set".to_string(), "set-value".to_string());
        config.add.insert("x-add".to_string(), "add-value".to_string());

        transform_headers(&mut headers, &config, &req);

        // Verify order: remove, rename, set, add
        assert_eq!(headers.get("host"), None);
        assert_eq!(headers.get("renamed"), Some(&"value".to_string()));
        assert_eq!(headers.get("x-set"), Some(&"set-value".to_string()));
        assert_eq!(headers.get("x-add"), Some(&"add-value".to_string()));
    }
}
