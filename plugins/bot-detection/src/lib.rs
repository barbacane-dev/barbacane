//! Bot detection middleware plugin for Barbacane API gateway.
//!
//! Blocks requests from known bots and scrapers by matching the `User-Agent`
//! header against configurable deny patterns.  An optional allow list lets
//! trusted crawlers (e.g. Googlebot) bypass the deny list.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Bot detection middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct BotDetection {
    /// User-Agent substrings to block (case-insensitive).
    ///
    /// A request is blocked when its `User-Agent` header contains any of these
    /// substrings.  Matching is case-insensitive.
    #[serde(default)]
    deny: Vec<String>,

    /// User-Agent substrings that are explicitly allowed (case-insensitive).
    ///
    /// An allow match takes precedence over any deny match.  Use this to
    /// whitelist trusted crawlers such as `"Googlebot"` or `"Bingbot"`.
    #[serde(default)]
    allow: Vec<String>,

    /// Block requests that carry no `User-Agent` header at all.
    ///
    /// Default: `false` (missing UA is allowed through).
    #[serde(default)]
    block_empty_ua: bool,

    /// Custom error message included in the problem response body.
    #[serde(default = "default_message")]
    message: String,

    /// HTTP status code returned for blocked requests.
    /// Default: 403
    #[serde(default = "default_status")]
    status: u16,
}

fn default_message() -> String {
    "Access denied".to_string()
}

fn default_status() -> u16 {
    403
}

impl BotDetection {
    /// Inspect the incoming request and block known bots.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let ua = req.headers.get("user-agent").cloned();

        match ua {
            None if self.block_empty_ua => {
                return Action::ShortCircuit(self.blocked_response(None));
            }
            None => return Action::Continue(req),
            Some(ref ua_value) => {
                // Allow list takes precedence.
                if self.matches_any(ua_value, &self.allow) {
                    return Action::Continue(req);
                }

                // Check deny list.
                if self.matches_any(ua_value, &self.deny) {
                    return Action::ShortCircuit(self.blocked_response(Some(ua_value)));
                }
            }
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Return true if `value` contains any of the patterns as a substring.
    ///
    /// Comparison is case-insensitive.
    fn matches_any(&self, value: &str, patterns: &[String]) -> bool {
        let lower = value.to_lowercase();
        patterns
            .iter()
            .any(|p| lower.contains(p.to_lowercase().as_str()))
    }

    /// Build the blocked response.
    fn blocked_response(&self, user_agent: Option<&str>) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let mut body = serde_json::json!({
            "type": "urn:barbacane:error:bot-detected",
            "title": "Forbidden",
            "status": self.status,
            "detail": self.message,
        });

        if let Some(ua) = user_agent {
            body["user_agent"] = serde_json::Value::String(ua.to_string());
        }

        Response {
            status: self.status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(ua: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        if let Some(ua) = ua {
            headers.insert("user-agent".to_string(), ua.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "1.2.3.4".to_string(),
        }
    }

    // --- config defaults ---

    #[test]
    fn test_config_defaults() {
        let plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(plugin.deny.is_empty());
        assert!(plugin.allow.is_empty());
        assert!(!plugin.block_empty_ua);
        assert_eq!(plugin.message, "Access denied");
        assert_eq!(plugin.status, 403);
    }

    // --- empty config allows everything ---

    #[test]
    fn test_no_config_allows_all() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(matches!(
            plugin.on_request(make_request(Some("Mozilla/5.0"))),
            Action::Continue(_)
        ));
    }

    #[test]
    fn test_no_config_allows_empty_ua() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(matches!(
            plugin.on_request(make_request(None)),
            Action::Continue(_)
        ));
    }

    // --- block_empty_ua ---

    #[test]
    fn test_block_empty_ua_enabled() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "block_empty_ua": true
        }))
        .unwrap();
        match plugin.on_request(make_request(None)) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_block_empty_ua_disabled_allows_no_ua() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "block_empty_ua": false
        }))
        .unwrap();
        assert!(matches!(
            plugin.on_request(make_request(None)),
            Action::Continue(_)
        ));
    }

    // --- deny patterns ---

    #[test]
    fn test_deny_blocks_matching_ua() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["scrapy", "ahrefsbot"]
        }))
        .unwrap();
        match plugin.on_request(make_request(Some("Scrapy/2.11"))) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_deny_allows_non_matching_ua() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["scrapy"]
        }))
        .unwrap();
        assert!(matches!(
            plugin.on_request(make_request(Some("Mozilla/5.0 Chrome"))),
            Action::Continue(_)
        ));
    }

    #[test]
    fn test_deny_case_insensitive() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["SemrushBot"]
        }))
        .unwrap();
        // Pattern "SemrushBot" should match lowercase ua "semrushbot/..."
        match plugin.on_request(make_request(Some("semrushbot/2.1"))) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_deny_substring_match() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["bot"]
        }))
        .unwrap();
        // Any UA containing "bot" is blocked
        match plugin.on_request(make_request(Some("AhrefsBot/7.0"))) {
            Action::ShortCircuit(_) => {}
            _ => panic!("expected ShortCircuit"),
        }
        match plugin.on_request(make_request(Some("DotBot/1.1"))) {
            Action::ShortCircuit(_) => {}
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- allow overrides deny ---

    #[test]
    fn test_allow_overrides_deny() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["bot"],
            "allow": ["Googlebot"]
        }))
        .unwrap();
        // "Googlebot" contains "bot" (deny) but also matches allow → should continue
        assert!(matches!(
            plugin.on_request(make_request(Some("Googlebot/2.1"))),
            Action::Continue(_)
        ));
    }

    #[test]
    fn test_allow_does_not_protect_non_matching() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["bot"],
            "allow": ["Googlebot"]
        }))
        .unwrap();
        // AhrefsBot matches deny but not allow → blocked
        match plugin.on_request(make_request(Some("AhrefsBot/7.0"))) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- custom status and message ---

    #[test]
    fn test_custom_status_and_message() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["curl"],
            "status": 429,
            "message": "Rate limited"
        }))
        .unwrap();
        match plugin.on_request(make_request(Some("curl/8.5.0"))) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 429);
                let body: serde_json::Value =
                    serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
                assert_eq!(body["detail"], "Rate limited");
                assert_eq!(body["status"], 429);
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- blocked_response format ---

    #[test]
    fn test_blocked_response_with_ua() {
        let plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        let resp = plugin.blocked_response(Some("scrapy/2.11"));
        assert_eq!(resp.status, 403);
        assert_eq!(
            resp.headers.get("content-type").map(String::as_str),
            Some("application/problem+json")
        );
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:bot-detected");
        assert_eq!(body["user_agent"], "scrapy/2.11");
    }

    #[test]
    fn test_blocked_response_without_ua() {
        let plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        let resp = plugin.blocked_response(None);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert!(body.get("user_agent").is_none());
    }

    // --- on_response passthrough ---

    #[test]
    fn test_on_response_passthrough() {
        let mut plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };
        assert_eq!(plugin.on_response(resp).status, 200);
    }

    // --- matches_any ---

    #[test]
    fn test_matches_any_empty_list() {
        let plugin: BotDetection = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!plugin.matches_any("scrapy/2.11", &[]));
    }

    #[test]
    fn test_matches_any_multiple_patterns() {
        let plugin: BotDetection = serde_json::from_value(serde_json::json!({
            "deny": ["wget", "curl", "scrapy"]
        }))
        .unwrap();
        assert!(plugin.matches_any("wget/1.21", &plugin.deny));
        assert!(plugin.matches_any("curl/8.0", &plugin.deny));
        assert!(plugin.matches_any("Scrapy/2.11", &plugin.deny));
        assert!(!plugin.matches_any("Mozilla/5.0", &plugin.deny));
    }
}
