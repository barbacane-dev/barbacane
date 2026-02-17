//! Request transformer middleware plugin for Barbacane API gateway.
//!
//! Provides declarative request transformations for:
//! - Headers (add, set, remove, rename)
//! - Query parameters (add, remove, rename)
//! - Path rewriting (strip_prefix, add_prefix, regex replace)
//! - JSON body (add, remove, rename using JSON Pointer — RFC 6901)
//!
//! Supports variable interpolation: `$client_ip`, `$path.<name>`, `$header.<name>`,
//! `$query.<name>`, `context:<key>`

use barbacane_plugin_sdk::prelude::*;
use form_urlencoded::{parse as parse_urlencoded, Serializer};
use jsonptr::{Assign, Delete, Pointer};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Configuration structs
// ---------------------------------------------------------------------------

/// Header transformation configuration.
#[derive(Debug, Clone, Deserialize, Default)]
struct HeaderConfig {
    #[serde(default)]
    add: BTreeMap<String, String>,

    #[serde(default)]
    set: BTreeMap<String, String>,

    #[serde(default)]
    remove: Vec<String>,

    #[serde(default)]
    rename: BTreeMap<String, String>,
}

/// Query string transformation configuration.
#[derive(Debug, Clone, Deserialize, Default)]
struct QueryConfig {
    #[serde(default)]
    add: BTreeMap<String, String>,

    #[serde(default)]
    remove: Vec<String>,

    #[serde(default)]
    rename: BTreeMap<String, String>,
}

/// Path rewriting configuration.
#[derive(Debug, Clone, Deserialize, Default)]
struct PathConfig {
    strip_prefix: Option<String>,
    add_prefix: Option<String>,
    replace: Option<PathReplaceConfig>,
}

/// Path regex replace configuration.
#[derive(Debug, Clone, Deserialize)]
struct PathReplaceConfig {
    pattern: String,
    replacement: String,
}

/// Body transformation configuration (JSON Pointer).
#[derive(Debug, Clone, Deserialize, Default)]
struct BodyConfig {
    #[serde(default)]
    add: BTreeMap<String, String>,

    #[serde(default)]
    remove: Vec<String>,

    #[serde(default)]
    rename: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// Request transformer middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct RequestTransformer {
    #[serde(default)]
    headers: Option<HeaderConfig>,

    #[serde(default)]
    querystring: Option<QueryConfig>,

    #[serde(default)]
    path: Option<PathConfig>,

    #[serde(default)]
    body: Option<BodyConfig>,

    /// Compiled regex for path replacement (lazy-initialized on first request).
    #[serde(skip)]
    compiled_replace: Option<Regex>,
}

impl RequestTransformer {
    /// Handle incoming request — apply transformations.
    ///
    /// Transformations are applied in this order:
    /// 1. Path (affects routing, must be first)
    /// 2. Headers
    /// 3. Query parameters
    /// 4. Body
    ///
    /// Variable interpolation always reads from the **original** request so that
    /// earlier transforms don't affect later variable resolution.
    pub fn on_request(&mut self, mut req: Request) -> Action<Request> {
        // Snapshot the original request for interpolation — all variable
        // references ($query.*, $header.*, etc.) resolve against this snapshot
        // so that transforms don't interfere with each other.
        let original = req.clone();

        // Lazy-compile the path regex on first invocation
        if self.compiled_replace.is_none() {
            if let Some(path_config) = &self.path {
                if let Some(replace_config) = &path_config.replace {
                    match Regex::new(&replace_config.pattern) {
                        Ok(re) => self.compiled_replace = Some(re),
                        Err(e) => {
                            log_message(
                                0,
                                &format!(
                                    "Invalid regex pattern '{}': {}",
                                    replace_config.pattern, e
                                ),
                            );
                        }
                    }
                }
            }
        }

        if let Some(path_config) = &self.path {
            req.path = transform_path(&req.path, path_config, self.compiled_replace.as_ref());
        }

        if let Some(header_config) = &self.headers {
            transform_headers(&mut req.headers, header_config, &original);
        }

        if let Some(query_config) = &self.querystring {
            req.query = transform_query(&req.query, query_config, &original);
        }

        if let Some(body_config) = &self.body {
            req.body = transform_body(&req.body, body_config, &original);
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged (request-transformer only).
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }
}

// ---------------------------------------------------------------------------
// Variable interpolation
// ---------------------------------------------------------------------------

/// Interpolate a value template with request data.
///
/// Returns the resolved value, or an empty string if the variable cannot be resolved.
fn interpolate_value(template: &str, req: &Request) -> String {
    if template == "$client_ip" {
        return req.client_ip.clone();
    }

    if let Some(param_name) = template.strip_prefix("$path.") {
        return req.path_params.get(param_name).cloned().unwrap_or_default();
    }

    if let Some(header_name) = template.strip_prefix("$header.") {
        return req
            .headers
            .get(header_name)
            .or_else(|| req.headers.get(&header_name.to_lowercase()))
            .cloned()
            .unwrap_or_default();
    }

    if let Some(query_name) = template.strip_prefix("$query.") {
        return extract_query_param(&req.query, query_name);
    }

    if let Some(context_key) = template.strip_prefix("context:") {
        return context_get(context_key).unwrap_or_default();
    }

    // Literal value (no variable prefix)
    template.to_string()
}

/// Extract a single query parameter value from a query string.
fn extract_query_param(query: &Option<String>, param_name: &str) -> String {
    let query_str = match query {
        Some(q) if !q.is_empty() => q,
        _ => return String::new(),
    };

    for (key, value) in parse_urlencoded(query_str.as_bytes()) {
        if key == param_name {
            return value.into_owned();
        }
    }

    String::new()
}

// ---------------------------------------------------------------------------
// Header transformations
// ---------------------------------------------------------------------------

/// Transform request headers. Order: remove → rename → set → add.
fn transform_headers(
    headers: &mut BTreeMap<String, String>,
    config: &HeaderConfig,
    original: &Request,
) {
    for header_name in &config.remove {
        headers.remove(&header_name.to_lowercase());
    }

    for (old_name, new_name) in &config.rename {
        if let Some(value) = headers.remove(&old_name.to_lowercase()) {
            headers.insert(new_name.to_lowercase(), value);
        }
    }

    for (header_name, value_template) in &config.set {
        headers
            .entry(header_name.to_lowercase())
            .or_insert_with(|| interpolate_value(value_template, original));
    }

    for (header_name, value_template) in &config.add {
        headers.insert(
            header_name.to_lowercase(),
            interpolate_value(value_template, original),
        );
    }
}

// ---------------------------------------------------------------------------
// Query string transformations
// ---------------------------------------------------------------------------

/// Transform query string. Order: remove → rename → add.
fn transform_query(
    query: &Option<String>,
    config: &QueryConfig,
    original: &Request,
) -> Option<String> {
    let mut params = parse_query_params(query);

    for param_name in &config.remove {
        params.retain(|(k, _)| k != param_name);
    }

    for (old_name, new_name) in &config.rename {
        if let Some(pos) = params.iter().position(|(k, _)| k == old_name) {
            let value = params[pos].1.clone();
            params.remove(pos);
            params.push((new_name.clone(), value));
        }
    }

    for (param_name, value_template) in &config.add {
        let value = interpolate_value(value_template, original);
        params.retain(|(k, _)| k != param_name);
        params.push((param_name.clone(), value));
    }

    build_query_string(params)
}

fn parse_query_params(query: &Option<String>) -> Vec<(String, String)> {
    match query {
        Some(q) if !q.is_empty() => parse_urlencoded(q.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        _ => Vec::new(),
    }
}

fn build_query_string(params: Vec<(String, String)>) -> Option<String> {
    if params.is_empty() {
        return None;
    }

    let mut serializer = Serializer::new(String::new());
    for (key, value) in params {
        serializer.append_pair(&key, &value);
    }

    Some(serializer.finish())
}

// ---------------------------------------------------------------------------
// Path transformations
// ---------------------------------------------------------------------------

/// Transform path. Order: strip_prefix → add_prefix → regex replace.
fn transform_path(path: &str, config: &PathConfig, compiled_re: Option<&Regex>) -> String {
    let mut result = path.to_string();

    if let Some(prefix) = &config.strip_prefix {
        if let Some(stripped) = result.strip_prefix(prefix.as_str()) {
            result = if stripped.is_empty() || !stripped.starts_with('/') {
                format!("/{}", stripped)
            } else {
                stripped.to_string()
            };
        }
    }

    if let Some(prefix) = &config.add_prefix {
        let normalized_prefix = if prefix.starts_with('/') {
            prefix.trim_end_matches('/').to_string()
        } else {
            format!("/{}", prefix.trim_end_matches('/'))
        };

        let normalized_path = if result.starts_with('/') {
            result
        } else {
            format!("/{}", result)
        };

        result = format!("{}{}", normalized_prefix, normalized_path);
    }

    if let Some(re) = compiled_re {
        if let Some(replace_config) = &config.replace {
            result = re
                .replace_all(&result, &replace_config.replacement)
                .to_string();
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Body transformations (JSON Pointer — RFC 6901)
// ---------------------------------------------------------------------------

/// Transform JSON body. Order: remove → rename → add.
///
/// Non-JSON bodies are returned unchanged.
fn transform_body(
    body: &Option<String>,
    config: &BodyConfig,
    original: &Request,
) -> Option<String> {
    let body_str = match body {
        Some(b) if !b.is_empty() => b,
        _ => return body.clone(),
    };

    let mut json: Value = match serde_json::from_str(body_str) {
        Ok(v) => v,
        Err(_) => {
            log_message(1, "Body is not valid JSON, skipping body transformations");
            return body.clone();
        }
    };

    for pointer_str in &config.remove {
        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                json.delete(ptr);
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    for (old_pointer_str, new_pointer_str) in &config.rename {
        let (old_ptr, new_ptr) = match (
            Pointer::parse(old_pointer_str),
            Pointer::parse(new_pointer_str),
        ) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(e), _) => {
                log_message(
                    1,
                    &format!("Invalid old pointer '{}': {}", old_pointer_str, e),
                );
                continue;
            }
            (_, Err(e)) => {
                log_message(
                    1,
                    &format!("Invalid new pointer '{}': {}", new_pointer_str, e),
                );
                continue;
            }
        };

        if let Some(value) = json.pointer(old_pointer_str) {
            let value_clone = value.clone();

            if let Err(e) = json.assign(new_ptr, value_clone) {
                log_message(
                    1,
                    &format!(
                        "Failed to rename '{}' to '{}': {}",
                        old_pointer_str, new_pointer_str, e
                    ),
                );
                continue;
            }

            json.delete(old_ptr);
        }
    }

    for (pointer_str, value_template) in &config.add {
        let interpolated = interpolate_value(value_template, original);
        // When the value was interpolated from a variable ($query.page → "2"),
        // try to preserve the JSON type (number, bool). For literal config
        // values ("1.0" as a version string), keep as string.
        let json_value = if is_variable(value_template) {
            to_json_value(&interpolated)
        } else {
            Value::String(interpolated)
        };

        match Pointer::parse(pointer_str) {
            Ok(ptr) => {
                if let Err(e) = json.assign(ptr, json_value) {
                    log_message(1, &format!("Failed to add '{}': {}", pointer_str, e));
                }
            }
            Err(e) => {
                log_message(1, &format!("Invalid JSON Pointer '{}': {}", pointer_str, e));
            }
        }
    }

    match serde_json::to_string(&json) {
        Ok(s) => Some(s),
        Err(e) => {
            log_message(
                0,
                &format!("Failed to serialize JSON after transformation: {}", e),
            );
            body.clone()
        }
    }
}

/// Check if a template string contains a variable reference.
fn is_variable(template: &str) -> bool {
    template.starts_with('$') || template.starts_with("context:")
}

/// Parse a string as a JSON value, falling back to a JSON string.
///
/// This preserves numeric/boolean types when interpolating variable values
/// into JSON bodies (e.g. `$query.page` = `"2"` becomes the JSON number `2`).
fn to_json_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

// ---------------------------------------------------------------------------
// Host function bindings
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe {
        host_log(level, msg.as_ptr() as i32, msg.len() as i32);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {
    // No-op on native
}

#[cfg(target_arch = "wasm32")]
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

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::context_get(key)
}

// Native mock implementations for testing
#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|c| c.borrow().get(key).cloned())
    }

    #[cfg(test)]
    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|c| c.borrow_mut().insert(key.to_string(), value.to_string()));
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|c| c.borrow_mut().clear());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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

    // -- Interpolation tests ------------------------------------------------

    #[test]
    fn test_interpolate_client_ip() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$client_ip", &req), "192.168.1.1");
    }

    #[test]
    fn test_interpolate_path_params() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$path.id", &req), "123");
        assert_eq!(interpolate_value("$path.name", &req), "test");
        assert_eq!(interpolate_value("$path.missing", &req), "");
    }

    #[test]
    fn test_interpolate_headers() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$header.host", &req), "api.example.com");
        assert_eq!(interpolate_value("$header.Host", &req), "api.example.com");
        assert_eq!(
            interpolate_value("$header.content-type", &req),
            "application/json"
        );
        assert_eq!(interpolate_value("$header.missing", &req), "");
    }

    #[test]
    fn test_interpolate_query_params() {
        let req = create_test_request();
        assert_eq!(interpolate_value("$query.page", &req), "2");
        assert_eq!(interpolate_value("$query.limit", &req), "10");
        assert_eq!(interpolate_value("$query.missing", &req), "");
    }

    #[test]
    fn test_interpolate_query_params_empty() {
        let mut req = create_test_request();
        req.query = None;
        assert_eq!(interpolate_value("$query.page", &req), "");

        req.query = Some(String::new());
        assert_eq!(interpolate_value("$query.page", &req), "");
    }

    #[test]
    fn test_interpolate_literal_values() {
        let req = create_test_request();
        assert_eq!(interpolate_value("literal", &req), "literal");
        assert_eq!(interpolate_value("barbacane", &req), "barbacane");
        assert_eq!(interpolate_value("123", &req), "123");
    }

    #[test]
    fn test_interpolate_context() {
        mock_host::reset();
        mock_host::context_set("auth.sub", "user-42");

        let req = create_test_request();
        assert_eq!(interpolate_value("context:auth.sub", &req), "user-42");
        assert_eq!(interpolate_value("context:missing", &req), "");
    }

    // -- Header transformation tests ----------------------------------------

    #[test]
    fn test_headers_add() {
        let req = create_test_request();
        let mut headers = req.headers.clone();

        let mut config = HeaderConfig::default();
        config
            .add
            .insert("x-gateway".to_string(), "barbacane".to_string());
        config
            .add
            .insert("x-client-ip".to_string(), "$client_ip".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-gateway"), Some(&"barbacane".to_string()));
        assert_eq!(headers.get("x-client-ip"), Some(&"192.168.1.1".to_string()));
    }

    #[test]
    fn test_headers_add_overwrites_existing() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("x-test".to_string(), "old-value".to_string());

        let mut config = HeaderConfig::default();
        config
            .add
            .insert("x-test".to_string(), "new-value".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-test"), Some(&"new-value".to_string()));
    }

    #[test]
    fn test_headers_set() {
        let req = create_test_request();
        let mut headers = req.headers.clone();

        let mut config = HeaderConfig::default();
        config.set.insert("x-new".to_string(), "value".to_string());
        config
            .set
            .insert("host".to_string(), "should-not-overwrite".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-new"), Some(&"value".to_string()));
        assert_eq!(headers.get("host"), Some(&"api.example.com".to_string()));
    }

    #[test]
    fn test_headers_remove() {
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
    fn test_headers_rename() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("x-old-name".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config
            .rename
            .insert("x-old-name".to_string(), "x-new-name".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-old-name"), None);
        assert_eq!(headers.get("x-new-name"), Some(&"value".to_string()));
    }

    #[test]
    fn test_headers_variable_interpolation() {
        let req = create_test_request();
        let mut headers = BTreeMap::new();

        let mut config = HeaderConfig::default();
        config
            .add
            .insert("x-user-id".to_string(), "$path.id".to_string());
        config
            .add
            .insert("x-page".to_string(), "$query.page".to_string());
        config
            .add
            .insert("x-original-host".to_string(), "$header.host".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("x-user-id"), Some(&"123".to_string()));
        assert_eq!(headers.get("x-page"), Some(&"2".to_string()));
        assert_eq!(
            headers.get("x-original-host"),
            Some(&"api.example.com".to_string())
        );
    }

    #[test]
    fn test_headers_transformation_order() {
        let req = create_test_request();
        let mut headers = req.headers.clone();
        headers.insert("to-rename".to_string(), "value".to_string());

        let mut config = HeaderConfig::default();
        config.remove.push("host".to_string());
        config
            .rename
            .insert("to-rename".to_string(), "renamed".to_string());
        config
            .set
            .insert("x-set".to_string(), "set-value".to_string());
        config
            .add
            .insert("x-add".to_string(), "add-value".to_string());

        transform_headers(&mut headers, &config, &req);

        assert_eq!(headers.get("host"), None);
        assert_eq!(headers.get("renamed"), Some(&"value".to_string()));
        assert_eq!(headers.get("x-set"), Some(&"set-value".to_string()));
        assert_eq!(headers.get("x-add"), Some(&"add-value".to_string()));
    }

    // -- Query string transformation tests ----------------------------------

    #[test]
    fn test_query_add() {
        let req = create_test_request();
        let query = Some("existing=value".to_string());

        let mut config = QueryConfig::default();
        config
            .add
            .insert("new_param".to_string(), "new_value".to_string());
        config.add.insert("id".to_string(), "$path.id".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(result_str.contains("existing=value"));
        assert!(result_str.contains("new_param=new_value"));
        assert!(result_str.contains("id=123"));
    }

    #[test]
    fn test_query_add_overwrites_existing() {
        let req = create_test_request();
        let query = Some("page=1&limit=20".to_string());

        let mut config = QueryConfig::default();
        config.add.insert("page".to_string(), "5".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(result_str.contains("page=5"));
        assert!(result_str.contains("limit=20"));
        assert_eq!(result_str.matches("page=").count(), 1);
    }

    #[test]
    fn test_query_remove() {
        let req = create_test_request();
        let query = Some("page=2&limit=10&filter=active".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("filter".to_string());
        config.remove.push("nonexistent".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(result_str.contains("page=2"));
        assert!(result_str.contains("limit=10"));
        assert!(!result_str.contains("filter"));
    }

    #[test]
    fn test_query_remove_all() {
        let req = create_test_request();
        let query = Some("page=2&limit=10".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("page".to_string());
        config.remove.push("limit".to_string());

        let result = transform_query(&query, &config, &req);
        assert_eq!(result, None);
    }

    #[test]
    fn test_query_rename() {
        let req = create_test_request();
        let query = Some("old_name=value&other=data".to_string());

        let mut config = QueryConfig::default();
        config
            .rename
            .insert("old_name".to_string(), "new_name".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(result_str.contains("new_name=value"));
        assert!(result_str.contains("other=data"));
        assert!(!result_str.contains("old_name"));
    }

    #[test]
    fn test_query_empty() {
        let req = create_test_request();

        let mut config = QueryConfig::default();
        config
            .add
            .insert("new_param".to_string(), "value".to_string());

        let result = transform_query(&None, &config, &req);
        assert_eq!(result, Some("new_param=value".to_string()));
    }

    #[test]
    fn test_query_variable_interpolation() {
        let req = create_test_request();
        let query = Some("existing=value".to_string());

        let mut config = QueryConfig::default();
        config
            .add
            .insert("user_id".to_string(), "$path.id".to_string());
        config
            .add
            .insert("client".to_string(), "$client_ip".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(result_str.contains("user_id=123"));
        assert!(result_str.contains("client=192.168.1.1"));
    }

    #[test]
    fn test_query_transformation_order() {
        let req = create_test_request();
        let query = Some("to_remove=x&to_rename=y&to_overwrite=old".to_string());

        let mut config = QueryConfig::default();
        config.remove.push("to_remove".to_string());
        config
            .rename
            .insert("to_rename".to_string(), "renamed".to_string());
        config
            .add
            .insert("to_overwrite".to_string(), "new".to_string());
        config.add.insert("added".to_string(), "value".to_string());

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(!result_str.contains("to_remove"));
        assert!(result_str.contains("renamed=y"));
        assert!(result_str.contains("to_overwrite=new"));
        assert!(result_str.contains("added=value"));
    }

    #[test]
    fn test_query_url_encoding() {
        let req = create_test_request();
        let query = Some("name=hello%20world&special=%3D%26".to_string());

        let config = QueryConfig::default();

        let result = transform_query(&query, &config, &req);
        let result_str = result.expect("should have query string");

        assert!(
            result_str.contains("name=hello+world") || result_str.contains("name=hello%20world")
        );
        assert!(result_str.contains("special"));
    }

    // -- Path transformation tests ------------------------------------------

    #[test]
    fn test_path_strip_prefix() {
        let config = PathConfig {
            strip_prefix: Some("/api/v2".to_string()),
            add_prefix: None,
            replace: None,
        };

        assert_eq!(transform_path("/api/v2/users", &config, None), "/users");
        assert_eq!(
            transform_path("/api/v2/users/123", &config, None),
            "/users/123"
        );
        assert_eq!(transform_path("/api/v2", &config, None), "/");
        assert_eq!(transform_path("/other/path", &config, None), "/other/path");
    }

    #[test]
    fn test_path_add_prefix() {
        let config = PathConfig {
            strip_prefix: None,
            add_prefix: Some("/internal".to_string()),
            replace: None,
        };

        assert_eq!(transform_path("/users", &config, None), "/internal/users");
        assert_eq!(
            transform_path("/users/123", &config, None),
            "/internal/users/123"
        );
    }

    #[test]
    fn test_path_add_prefix_normalization() {
        let mut config = PathConfig {
            strip_prefix: None,
            add_prefix: Some("internal/".to_string()),
            replace: None,
        };

        assert_eq!(transform_path("/users", &config, None), "/internal/users");

        config.add_prefix = Some("/internal/".to_string());
        assert_eq!(transform_path("/users", &config, None), "/internal/users");
    }

    #[test]
    fn test_path_strip_and_add_prefix() {
        let config = PathConfig {
            strip_prefix: Some("/api/v2".to_string()),
            add_prefix: Some("/internal".to_string()),
            replace: None,
        };

        assert_eq!(
            transform_path("/api/v2/users", &config, None),
            "/internal/users"
        );
        assert_eq!(
            transform_path("/api/v2/users/123", &config, None),
            "/internal/users/123"
        );
    }

    #[test]
    fn test_path_regex_replace() {
        let config = PathConfig {
            strip_prefix: None,
            add_prefix: None,
            replace: Some(PathReplaceConfig {
                pattern: r"/v\d+/".to_string(),
                replacement: "/".to_string(),
            }),
        };
        let re =
            Regex::new(&config.replace.as_ref().expect("set above").pattern).expect("valid regex");

        assert_eq!(
            transform_path("/api/v1/users", &config, Some(&re)),
            "/api/users"
        );
        assert_eq!(
            transform_path("/api/v2/orders", &config, Some(&re)),
            "/api/orders"
        );
        assert_eq!(
            transform_path("/api/users", &config, Some(&re)),
            "/api/users"
        );
    }

    #[test]
    fn test_path_regex_with_capture_groups() {
        let config = PathConfig {
            strip_prefix: None,
            add_prefix: None,
            replace: Some(PathReplaceConfig {
                pattern: r"/users/(\d+)".to_string(),
                replacement: "/user/$1/profile".to_string(),
            }),
        };
        let re =
            Regex::new(&config.replace.as_ref().expect("set above").pattern).expect("valid regex");

        assert_eq!(
            transform_path("/users/123", &config, Some(&re)),
            "/user/123/profile"
        );
        assert_eq!(
            transform_path("/users/456/data", &config, Some(&re)),
            "/user/456/profile/data"
        );
    }

    #[test]
    fn test_path_all_transformations() {
        let config = PathConfig {
            strip_prefix: Some("/api".to_string()),
            add_prefix: Some("/internal".to_string()),
            replace: Some(PathReplaceConfig {
                pattern: r"/v\d+".to_string(),
                replacement: "".to_string(),
            }),
        };
        let re =
            Regex::new(&config.replace.as_ref().expect("set above").pattern).expect("valid regex");

        assert_eq!(
            transform_path("/api/v2/users", &config, Some(&re)),
            "/internal/users"
        );
    }

    #[test]
    fn test_path_empty() {
        let config = PathConfig {
            strip_prefix: None,
            add_prefix: Some("/api".to_string()),
            replace: None,
        };

        assert_eq!(transform_path("", &config, None), "/api/");
    }

    #[test]
    fn test_path_root() {
        let config = PathConfig {
            strip_prefix: None,
            add_prefix: Some("/api".to_string()),
            replace: None,
        };

        assert_eq!(transform_path("/", &config, None), "/api/");
    }

    // -- Body transformation tests ------------------------------------------

    fn create_post_request() -> Request {
        let mut path_params = BTreeMap::new();
        path_params.insert("id".to_string(), "123".to_string());

        Request {
            method: "POST".to_string(),
            path: "/users/123".to_string(),
            query: Some("page=2".to_string()),
            headers: BTreeMap::new(),
            body: Some(r#"{"user":"john","age":30}"#.to_string()),
            client_ip: "192.168.1.1".to_string(),
            path_params,
        }
    }

    #[test]
    fn test_body_add_simple_field() {
        let req = create_post_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["gateway"], "barbacane");
    }

    #[test]
    fn test_body_add_nested_field() {
        let req = create_post_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/metadata/gateway".to_string(), "barbacane".to_string());
        config
            .add
            .insert("/metadata/version".to_string(), "1.0".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["gateway"], "barbacane");
        assert_eq!(json["metadata"]["version"], "1.0");
    }

    #[test]
    fn test_body_add_with_interpolation() {
        let req = create_post_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/userId".to_string(), "$path.id".to_string());
        config
            .add
            .insert("/clientIp".to_string(), "$client_ip".to_string());
        config
            .add
            .insert("/page".to_string(), "$query.page".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        // Numeric strings from query/path should be preserved as JSON numbers
        assert_eq!(json["userId"], 123);
        assert_eq!(json["clientIp"], "192.168.1.1");
        assert_eq!(json["page"], 2);
    }

    #[test]
    fn test_body_remove_field() {
        let req = create_post_request();
        let body = Some(r#"{"user":"john","password":"secret","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/password".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("password"), None);
    }

    #[test]
    fn test_body_remove_nested_field() {
        let req = create_post_request();
        let body =
            Some(r#"{"user":"john","metadata":{"internal":true,"public":"yes"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/metadata/internal".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user"], "john");
        assert_eq!(json["metadata"]["public"], "yes");
        assert_eq!(json["metadata"].get("internal"), None);
    }

    #[test]
    fn test_body_rename_field() {
        let req = create_post_request();
        let body = Some(r#"{"userName":"john","age":30}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .rename
            .insert("/userName".to_string(), "/user_name".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["user_name"], "john");
        assert_eq!(json["age"], 30);
        assert_eq!(json.get("userName"), None);
    }

    #[test]
    fn test_body_rename_nested_field() {
        let req = create_post_request();
        let body = Some(r#"{"metadata":{"oldName":"value"}}"#.to_string());

        let mut config = BodyConfig::default();
        config.rename.insert(
            "/metadata/oldName".to_string(),
            "/metadata/newName".to_string(),
        );

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["metadata"]["newName"], "value");
        assert_eq!(json["metadata"].get("oldName"), None);
    }

    #[test]
    fn test_body_transformation_order() {
        let req = create_post_request();
        let body = Some(r#"{"toRemove":"x","toRename":"y","toOverwrite":"old"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/toRemove".to_string());
        config
            .rename
            .insert("/toRename".to_string(), "/renamed".to_string());
        config
            .add
            .insert("/toOverwrite".to_string(), "new".to_string());
        config.add.insert("/added".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json.get("toRemove"), None);
        assert_eq!(json["renamed"], "y");
        assert_eq!(json["toOverwrite"], "new");
        assert_eq!(json["added"], "value");
    }

    #[test]
    fn test_body_non_json() {
        let req = create_post_request();
        let body = Some("not json".to_string());

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);
        assert_eq!(result, Some("not json".to_string()));
    }

    #[test]
    fn test_body_empty() {
        let req = create_post_request();
        let body = None;

        let mut config = BodyConfig::default();
        config.add.insert("/field".to_string(), "value".to_string());

        let result = transform_body(&body, &config, &req);
        assert_eq!(result, None);
    }

    #[test]
    fn test_body_nonexistent_pointer() {
        let req = create_post_request();
        let body = Some(r#"{"user":"john"}"#.to_string());

        let mut config = BodyConfig::default();
        config.remove.push("/nonexistent/deeply/nested".to_string());

        let result = transform_body(&body, &config, &req);
        assert!(result.is_some());
    }

    #[test]
    fn test_body_array_pointer() {
        let req = create_post_request();
        let body = Some(r#"{"items":[{"id":1},{"id":2}]}"#.to_string());

        let mut config = BodyConfig::default();
        config
            .add
            .insert("/items/0/gateway".to_string(), "barbacane".to_string());

        let result = transform_body(&body, &config, &req);
        let json: Value =
            serde_json::from_str(&result.expect("should have body")).expect("valid json");

        assert_eq!(json["items"][0]["gateway"], "barbacane");
        assert_eq!(json["items"][1].get("gateway"), None);
    }

    // -- to_json_value tests ------------------------------------------------

    #[test]
    fn test_to_json_value_number() {
        assert_eq!(to_json_value("42"), Value::Number(42.into()));
        assert_eq!(to_json_value("2"), Value::Number(2.into()));
    }

    #[test]
    fn test_to_json_value_bool() {
        assert_eq!(to_json_value("true"), Value::Bool(true));
        assert_eq!(to_json_value("false"), Value::Bool(false));
    }

    #[test]
    fn test_to_json_value_string() {
        assert_eq!(
            to_json_value("barbacane"),
            Value::String("barbacane".to_string())
        );
        assert_eq!(
            to_json_value("192.168.1.1"),
            Value::String("192.168.1.1".to_string())
        );
    }

    // -- Integration / snapshot tests ---------------------------------------

    #[test]
    fn test_on_request_snapshot_isolation() {
        // Verify that body interpolation reads from the original request,
        // not the mutated one (query-to-body use case).
        let mut plugin = RequestTransformer {
            headers: None,
            querystring: Some(QueryConfig {
                add: BTreeMap::new(),
                remove: vec!["userId".to_string()],
                rename: BTreeMap::new(),
            }),
            path: None,
            body: Some(BodyConfig {
                add: {
                    let mut m = BTreeMap::new();
                    m.insert("/userId".to_string(), "$query.userId".to_string());
                    m
                },
                remove: vec![],
                rename: BTreeMap::new(),
            }),
            compiled_replace: None,
        };

        let req = Request {
            method: "POST".to_string(),
            path: "/query-to-body".to_string(),
            query: Some("userId=42".to_string()),
            headers: BTreeMap::new(),
            body: Some(r#"{"existing":"data"}"#.to_string()),
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };

        let result = plugin.on_request(req);
        if let Action::Continue(modified) = result {
            // userId should have been removed from query
            assert_eq!(modified.query, None);

            // userId should have been injected into body from the ORIGINAL query
            let json: Value =
                serde_json::from_str(modified.body.as_ref().expect("should have body"))
                    .expect("valid json");
            assert_eq!(json["userId"], 42);
            assert_eq!(json["existing"], "data");
        } else {
            panic!("Expected Action::Continue");
        }
    }

    #[test]
    fn test_on_request_lazy_regex_compilation() {
        let mut plugin = RequestTransformer {
            headers: None,
            querystring: None,
            path: Some(PathConfig {
                strip_prefix: None,
                add_prefix: None,
                replace: Some(PathReplaceConfig {
                    pattern: r"/v\d+".to_string(),
                    replacement: "".to_string(),
                }),
            }),
            body: None,
            compiled_replace: None,
        };

        assert!(plugin.compiled_replace.is_none());

        let req = Request {
            method: "GET".to_string(),
            path: "/api/v2/users".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };

        let result = plugin.on_request(req);
        if let Action::Continue(modified) = result {
            assert_eq!(modified.path, "/api/users");
        } else {
            panic!("Expected Action::Continue");
        }

        // Regex should now be compiled and cached
        assert!(plugin.compiled_replace.is_some());
    }

    #[test]
    fn test_config_deserialization_defaults() {
        let config: RequestTransformer = serde_json::from_str("{}").expect("valid json");
        assert!(config.headers.is_none());
        assert!(config.querystring.is_none());
        assert!(config.path.is_none());
        assert!(config.body.is_none());
    }

    #[test]
    fn test_config_deserialization_full() {
        let json = r#"{
            "headers": {
                "add": {"x-gateway": "barbacane"},
                "set": {"x-default": "value"},
                "remove": ["authorization"],
                "rename": {"x-old": "x-new"}
            },
            "querystring": {
                "add": {"version": "1.0"},
                "remove": ["internal"],
                "rename": {"old": "new"}
            },
            "path": {
                "strip_prefix": "/api/v2",
                "add_prefix": "/internal"
            },
            "body": {
                "add": {"/gateway": "barbacane"},
                "remove": ["/password"],
                "rename": {"/userName": "/user_name"}
            }
        }"#;

        let config: RequestTransformer = serde_json::from_str(json).expect("valid json");
        assert!(config.headers.is_some());
        assert!(config.querystring.is_some());
        assert!(config.path.is_some());
        assert!(config.body.is_some());

        let h = config.headers.expect("set above");
        assert_eq!(h.add.get("x-gateway"), Some(&"barbacane".to_string()));
        assert_eq!(h.set.get("x-default"), Some(&"value".to_string()));
        assert_eq!(h.remove, vec!["authorization"]);
        assert_eq!(h.rename.get("x-old"), Some(&"x-new".to_string()));
    }
}
