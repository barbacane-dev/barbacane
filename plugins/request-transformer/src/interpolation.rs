//! Variable interpolation engine for request transformations.
//!
//! Supports the following variable types:
//! - `$client_ip` - Client IP address
//! - `$path.<name>` - Route path parameters
//! - `$header.<name>` - Request headers (case-insensitive)
//! - `$query.<name>` - Query parameters
//! - `context:<key>` - Middleware context values
//! - Literal strings (no prefix)

use barbacane_plugin_sdk::prelude::*;
use form_urlencoded::parse;

/// Interpolate a value template with request data.
///
/// Returns the resolved value or an empty string if the variable cannot be resolved.
pub fn interpolate_value(template: &str, req: &Request) -> String {
    // $client_ip - Client IP address
    if template == "$client_ip" {
        return req.client_ip.clone();
    }

    // $path.<name> - Route path parameters
    if let Some(param_name) = template.strip_prefix("$path.") {
        return req
            .path_params
            .get(param_name)
            .cloned()
            .unwrap_or_default();
    }

    // $header.<name> - Request headers (case-insensitive lookup)
    if let Some(header_name) = template.strip_prefix("$header.") {
        return req
            .headers
            .get(header_name)
            .or_else(|| req.headers.get(&header_name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
    }

    // $query.<name> - Query parameters
    if let Some(query_name) = template.strip_prefix("$query.") {
        return extract_query_param(&req.query, query_name);
    }

    // context:<key> - Middleware context values
    if let Some(context_key) = template.strip_prefix("context:") {
        return context_get(context_key).unwrap_or_default();
    }

    // Literal value (no variable prefix)
    template.to_string()
}

/// Extract a query parameter value from the query string.
fn extract_query_param(query: &Option<String>, param_name: &str) -> String {
    let query_str = match query {
        Some(q) if !q.is_empty() => q,
        _ => return String::new(),
    };

    // Parse query string and find the parameter
    for (key, value) in parse(query_str.as_bytes()) {
        if key == param_name {
            return value.into_owned();
        }
    }

    String::new()
}

/// Get a value from the request context using host function.
fn context_get(key: &str) -> Option<String> {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_get(key_ptr: i32, key_len: i32) -> i32;
        fn host_context_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }

    unsafe {
        let len = host_context_get(key.as_ptr() as i32, key.len() as i32);
        if len <= 0 {
            return None;
        }

        let mut buf = vec![0u8; len as usize];
        let read_len = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read_len != len {
            return None;
        }

        String::from_utf8(buf).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn create_test_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "api.example.com".to_string());
        headers.insert("content-type".to_string(), "application/json".to_string());

        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "123".to_string());
        path_params.insert("name".to_string(), "test".to_string());

        Request {
            method: "GET".to_string(),
            path: "/users/123".to_string(),
            query: Some("page=2&limit=10".to_string()),
            headers,
            body: None,
            client_ip: "192.168.1.1".to_string(),
            path_params,
        }
    }

    #[test]
    fn test_client_ip() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$client_ip", &req), "192.168.1.1");
    }

    #[test]
    fn test_path_params() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$path.id", &req), "123");
        assert_eq!(interpolate_value("$path.name", &req), "test");
        assert_eq!(interpolate_value("$path.missing", &req), "");
    }

    #[test]
    fn test_headers() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$header.host", &req), "api.example.com");
        assert_eq!(interpolate_value("$header.Host", &req), "api.example.com"); // case-insensitive
        assert_eq!(interpolate_value("$header.content-type", &req), "application/json");
        assert_eq!(interpolate_value("$header.missing", &req), "");
    }

    #[test]
    fn test_query_params() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$query.page", &req), "2");
        assert_eq!(interpolate_value("$query.limit", &req), "10");
        assert_eq!(interpolate_value("$query.missing", &req), "");
    }

    #[test]
    fn test_query_params_empty() {
        let mut req = create_test_request();
        req.query = None;
        assert_eq!(interpolate_value("$query.page", &req), "");

        req.query = Some(String::new());
        assert_eq!(interpolate_value("$query.page", &req), "");
    }

    #[test]
    fn test_literal_values() {
        let req = create_test_request();
        assert_eq!(interpolate_value("literal", &req), "literal");
        assert_eq!(interpolate_value("barbacane", &req), "barbacane");
        assert_eq!(interpolate_value("123", &req), "123");
    }
}
