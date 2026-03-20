//! HTTP redirect middleware plugin for Barbacane API gateway.
//!
//! Redirects requests based on configurable path rules.
//! Supports exact path matching, prefix matching with path rewriting,
//! and query string preservation.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// HTTP redirect middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct Redirect {
    /// Default HTTP status code for redirects.
    #[serde(default = "default_status_code")]
    status_code: u16,

    /// Whether to preserve the original query string in redirects.
    #[serde(default = "default_preserve_query")]
    preserve_query: bool,

    /// Redirect rules evaluated in order. First match wins.
    rules: Vec<Rule>,
}

#[derive(Deserialize, Clone)]
struct Rule {
    /// Exact path to match. If omitted (and no prefix), matches all requests.
    path: Option<String>,

    /// Path prefix to match. The matched prefix is stripped
    /// and the remainder is appended to target.
    prefix: Option<String>,

    /// Redirect target URL or path.
    target: String,

    /// Override the top-level status_code for this rule.
    status_code: Option<u16>,
}

fn default_status_code() -> u16 {
    302
}

fn default_preserve_query() -> bool {
    true
}

impl Redirect {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        for rule in &self.rules {
            if let Some(location) = self.match_rule(rule, &req) {
                let status = rule.status_code.unwrap_or(self.status_code);
                return Action::ShortCircuit(redirect_response(status, &location));
            }
        }
        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    fn match_rule(&self, rule: &Rule, req: &Request) -> Option<String> {
        if let Some(exact) = &rule.path {
            if req.path != *exact {
                return None;
            }
            return Some(self.build_location(&rule.target, "", &req.query));
        }

        if let Some(prefix) = &rule.prefix {
            if !req.path.starts_with(prefix.as_str()) {
                return None;
            }
            let remainder = &req.path[prefix.len()..];
            return Some(self.build_location(&rule.target, remainder, &req.query));
        }

        // No path or prefix — matches everything
        Some(self.build_location(&rule.target, "", &req.query))
    }

    fn build_location(&self, target: &str, suffix: &str, query: &Option<String>) -> String {
        let mut location = format!("{target}{suffix}");
        if self.preserve_query {
            if let Some(qs) = query {
                if !qs.is_empty() {
                    location.push('?');
                    location.push_str(qs);
                }
            }
        }
        location
    }
}

fn redirect_response(status: u16, location: &str) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert("location".to_string(), location.to_string());
    headers.insert("content-type".to_string(), "text/plain".to_string());

    let body = match status {
        301 => "Moved Permanently",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        _ => "Found",
    };

    Response {
        status,
        headers,
        body: Some(body.as_bytes().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(path: &str, query: Option<&str>) -> Request {
        Request {
            method: "GET".to_string(),
            path: path.to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: query.map(String::from),
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn plugin_from_json(json: serde_json::Value) -> Redirect {
        serde_json::from_value(json).expect("valid config")
    }

    // --- Exact path matching ---

    #[test]
    fn exact_path_match_redirects() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "path": "/old", "target": "https://example.com/new" }]
        }));

        match plugin.on_request(make_request("/old", None)) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 302);
                assert_eq!(r.headers["location"], "https://example.com/new");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn exact_path_no_match_continues() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "path": "/old", "target": "/new" }]
        }));

        assert!(matches!(
            plugin.on_request(make_request("/other", None)),
            Action::Continue(_)
        ));
    }

    // --- Prefix matching ---

    #[test]
    fn prefix_match_strips_and_appends() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "prefix": "/api/v1", "target": "/api/v2" }]
        }));

        match plugin.on_request(make_request("/api/v1/users/42", None)) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.headers["location"], "/api/v2/users/42");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn prefix_no_match_continues() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "prefix": "/api/v1", "target": "/api/v2" }]
        }));

        assert!(matches!(
            plugin.on_request(make_request("/other/path", None)),
            Action::Continue(_)
        ));
    }

    // --- Catch-all rule ---

    #[test]
    fn catch_all_rule_matches_everything() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "status_code": 301,
            "rules": [{ "target": "https://new-domain.com" }]
        }));

        match plugin.on_request(make_request("/anything", None)) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 301);
                assert_eq!(r.headers["location"], "https://new-domain.com");
                assert_eq!(r.body_str(), Some("Moved Permanently"));
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- Query string preservation ---

    #[test]
    fn preserves_query_string_by_default() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "path": "/old", "target": "/new" }]
        }));

        match plugin.on_request(make_request("/old", Some("foo=bar&baz=1"))) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.headers["location"], "/new?foo=bar&baz=1");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn strips_query_string_when_disabled() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "preserve_query": false,
            "rules": [{ "path": "/old", "target": "/new" }]
        }));

        match plugin.on_request(make_request("/old", Some("foo=bar"))) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.headers["location"], "/new");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- Status code ---

    #[test]
    fn custom_status_code() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "status_code": 308,
            "rules": [{ "path": "/old", "target": "/new" }]
        }));

        match plugin.on_request(make_request("/old", None)) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 308);
                assert_eq!(r.body_str(), Some("Permanent Redirect"));
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn per_rule_status_code_overrides_default() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "status_code": 302,
            "rules": [
                { "path": "/permanent", "target": "/new", "status_code": 301 },
                { "path": "/temporary", "target": "/other" }
            ]
        }));

        match plugin.on_request(make_request("/permanent", None)) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 301),
            _ => panic!("expected ShortCircuit"),
        }

        match plugin.on_request(make_request("/temporary", None)) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 302),
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- Rule ordering ---

    #[test]
    fn first_matching_rule_wins() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [
                { "path": "/test", "target": "/first" },
                { "path": "/test", "target": "/second" }
            ]
        }));

        match plugin.on_request(make_request("/test", None)) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.headers["location"], "/first");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- Prefix + query string ---

    #[test]
    fn prefix_match_preserves_query_string() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "prefix": "/v1", "target": "/v2" }]
        }));

        match plugin.on_request(make_request("/v1/search", Some("q=hello"))) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.headers["location"], "/v2/search?q=hello");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- Config defaults ---

    #[test]
    fn config_defaults() {
        let plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "target": "/default" }]
        }));
        assert_eq!(plugin.status_code, 302);
        assert!(plugin.preserve_query);
    }

    // --- Response passthrough ---

    #[test]
    fn on_response_passthrough() {
        let mut plugin = plugin_from_json(serde_json::json!({
            "rules": [{ "target": "/x" }]
        }));
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"ok".to_vec()),
        };
        let result = plugin.on_response(resp);
        assert_eq!(result.status, 200);
    }
}
