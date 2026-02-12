//! Query string transformation functions.

use crate::config::QueryConfig;
use crate::interpolation::interpolate_value;
use barbacane_plugin_sdk::prelude::*;
use form_urlencoded::{parse, Serializer};

/// Transform query string according to configuration.
pub fn transform_query(query: &Option<String>, config: &QueryConfig, req: &Request) -> Option<String> {
    // Parse query string into parameters
    let mut params = parse_query(query);

    // Remove parameters
    for param_name in &config.remove {
        params.retain(|(k, _)| k != param_name);
    }

    // Rename parameters
    for (old_name, new_name) in &config.rename {
        if let Some(pos) = params.iter().position(|(k, _)| k == old_name) {
            let value = params[pos].1.clone();
            params.remove(pos);
            params.push((new_name.clone(), value));
        }
    }

    // Add parameters (overwrite if exists)
    for (param_name, value_template) in &config.add {
        let value = interpolate_value(value_template, req);

        // Remove existing parameter with same name
        params.retain(|(k, _)| k != param_name);

        // Add new parameter
        params.push((param_name.clone(), value));
    }

    // Build query string
    build_query(params)
}

/// Parse query string into vector of (key, value) pairs.
fn parse_query(query: &Option<String>) -> Vec<(String, String)> {
    match query {
        Some(q) if !q.is_empty() => parse(q.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Build query string from vector of (key, value) pairs.
fn build_query(params: Vec<(String, String)>) -> Option<String> {
    if params.is_empty() {
        return None;
    }

    let mut serializer = Serializer::new(String::new());
    for (key, value) in params {
        serializer.append_pair(&key, &value);
    }

    Some(serializer.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn create_test_request() -> Request {
        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "123".to_string());

        Request {
            method: "GET".to_string(),
            path: "/users/123".to_string(),
            query: Some("page=2&limit=10".to_string()),
            headers: BTreeMap::new(),
            body: None,
            client_ip: "192.168.1.1".to_string(),
            path_params,
        }
    }

    #[test]
    fn test_add_query_params() {
        let req = create_test_request();
        let query = Some("existing=value".to_string());

        let mut config = QueryConfig::default();
        config.add.insert("new_param".to_string(), "new_value".to_string());
        config.add.insert("id".to_string(), "$path.id".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        // Check that all parameters are present (order may vary)
        assert!(result_str.contains("existing=value"));
        assert!(result_str.contains("new_param=new_value"));
        assert!(result_str.contains("id=123"));
    }

    #[test]
    fn test_add_overwrites_existing() {
        let req = create_test_request();
        let query = Some("page=1&limit=20".to_string());

        let mut config = QueryConfig::default();
        config.add.insert("page".to_string(), "5".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        // Page should be 5 (overwritten), not 1
        assert!(result_str.contains("page=5"));
        assert!(result_str.contains("limit=20"));
        // Should not have duplicate page parameter
        assert_eq!(result_str.matches("page=").count(), 1);
    }

    #[test]
    fn test_remove_query_params() {
        let req = create_test_request();
        let query = Some("page=2&limit=10&filter=active".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("filter".to_string());
        config.remove.push("nonexistent".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        assert!(result_str.contains("page=2"));
        assert!(result_str.contains("limit=10"));
        assert!(!result_str.contains("filter"));
    }

    #[test]
    fn test_remove_all_params() {
        let req = create_test_request();
        let query = Some("page=2&limit=10".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("page".to_string());
        config.remove.push("limit".to_string());

        let result = transform_query(&query, &config, &req);

        // Should return None when all params removed
        assert_eq!(result, None);
    }

    #[test]
    fn test_rename_query_params() {
        let req = create_test_request();
        let query = Some("old_name=value&other=data".to_string());

        let mut config = QueryConfig::default();
        config.rename.insert("old_name".to_string(), "new_name".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        assert!(result_str.contains("new_name=value"));
        assert!(result_str.contains("other=data"));
        assert!(!result_str.contains("old_name"));
    }

    #[test]
    fn test_empty_query() {
        let req = create_test_request();

        let mut config = QueryConfig::default();
        config.add.insert("new_param".to_string(), "value".to_string());

        let result = transform_query(&None, &config, &req);
        assert_eq!(result, Some("new_param=value".to_string()));
    }

    #[test]
    fn test_variable_interpolation() {
        let req = create_test_request();
        let query = Some("existing=value".to_string());

        let mut config = QueryConfig::default();
        config.add.insert("user_id".to_string(), "$path.id".to_string());
        config.add.insert("client".to_string(), "$client_ip".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        assert!(result_str.contains("user_id=123"));
        assert!(result_str.contains("client=192.168.1.1"));
    }

    #[test]
    fn test_transformation_order() {
        let req = create_test_request();
        let query = Some("to_remove=x&to_rename=y&to_overwrite=old".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("to_remove".to_string());
        config.rename.insert("to_rename".to_string(), "renamed".to_string());
        config.add.insert("to_overwrite".to_string(), "new".to_string());
        config.add.insert("added".to_string(), "value".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        assert!(!result_str.contains("to_remove"));
        assert!(result_str.contains("renamed=y"));
        assert!(result_str.contains("to_overwrite=new"));
        assert!(result_str.contains("added=value"));
    }

    #[test]
    fn test_url_encoding() {
        let req = create_test_request();
        let query = Some("name=hello%20world&special=%3D%26".to_string());

        let config = QueryConfig::default();

        let result = transform_query(&query, &config, &req);
        let result_str = result.unwrap();

        // form_urlencoded should handle encoding/decoding
        assert!(result_str.contains("name=hello+world") || result_str.contains("name=hello%20world"));
        assert!(result_str.contains("special"));
    }
}
