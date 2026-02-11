//! IP restriction middleware plugin for Barbacane API gateway.
//!
//! Allows or denies requests based on client IP address or CIDR ranges.
//! Supports both allowlist and denylist modes.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// IP restriction middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct IpRestriction {
    /// List of allowed IPs or CIDR ranges.
    /// If non-empty, only these IPs are allowed (allowlist mode).
    #[serde(default)]
    allow: Vec<String>,

    /// List of denied IPs or CIDR ranges.
    /// These IPs are blocked (denylist mode).
    #[serde(default)]
    deny: Vec<String>,

    /// Custom error message for denied requests.
    #[serde(default = "default_message")]
    message: String,

    /// HTTP status code for denied requests.
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

impl IpRestriction {
    /// Handle incoming request - check IP against allow/deny lists.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let client_ip = self.extract_client_ip(&req);

        // Check denylist first (takes precedence)
        if self.is_ip_in_list(&client_ip, &self.deny) {
            return Action::ShortCircuit(self.forbidden_response(&client_ip));
        }

        // Check allowlist if configured
        if !self.allow.is_empty() && !self.is_ip_in_list(&client_ip, &self.allow) {
            return Action::ShortCircuit(self.forbidden_response(&client_ip));
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Extract client IP from request headers or connection.
    fn extract_client_ip(&self, req: &Request) -> String {
        // Check X-Forwarded-For header (first IP in chain)
        if let Some(xff) = req.headers.get("x-forwarded-for") {
            if let Some(first_ip) = xff.split(',').next() {
                return first_ip.trim().to_string();
            }
        }

        // Check X-Real-IP header
        if let Some(real_ip) = req.headers.get("x-real-ip") {
            return real_ip.clone();
        }

        // Fall back to direct client IP
        req.client_ip.clone()
    }

    /// Check if an IP matches any entry in a list.
    fn is_ip_in_list(&self, client_ip: &str, list: &[String]) -> bool {
        let client_addr = match parse_ip(client_ip) {
            Some(addr) => addr,
            None => return false,
        };

        for entry in list {
            if entry.contains('/') {
                // CIDR notation
                if let Some((network, prefix_len)) = parse_cidr(entry) {
                    if ip_in_cidr(client_addr, network, prefix_len) {
                        return true;
                    }
                }
            } else {
                // Single IP
                if let Some(entry_addr) = parse_ip(entry) {
                    if client_addr == entry_addr {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Generate 403 Forbidden response.
    fn forbidden_response(&self, client_ip: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:ip-restricted",
            "title": "Forbidden",
            "status": self.status,
            "detail": self.message,
            "client_ip": client_ip
        });

        Response {
            status: self.status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

/// Parse an IPv4 address into a u32.
fn parse_ip(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.trim().split('.').collect();
    if parts.len() != 4 {
        return None;
    }

    let mut addr: u32 = 0;
    for (i, part) in parts.iter().enumerate() {
        let octet: u8 = part.parse().ok()?;
        addr |= (octet as u32) << (24 - i * 8);
    }

    Some(addr)
}

/// Parse a CIDR notation string into network address and prefix length.
fn parse_cidr(cidr: &str) -> Option<(u32, u8)> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return None;
    }

    let network = parse_ip(parts[0])?;
    let prefix_len: u8 = parts[1].parse().ok()?;

    if prefix_len > 32 {
        return None;
    }

    Some((network, prefix_len))
}

/// Check if an IP address is within a CIDR range.
fn ip_in_cidr(ip: u32, network: u32, prefix_len: u8) -> bool {
    if prefix_len == 0 {
        return true;
    }

    let mask = !0u32 << (32 - prefix_len);
    (ip & mask) == (network & mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plugin() -> IpRestriction {
        serde_json::from_value(serde_json::json!({
            "allow": ["10.0.0.0/8", "192.168.1.100"],
            "deny": ["10.0.0.5"]
        }))
        .unwrap()
    }

    fn request_with_ip(ip: &str) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: ip.to_string(),
        }
    }

    fn request_with_forwarded(xff: &str) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("x-forwarded-for".to_string(), xff.to_string());
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "0.0.0.0".to_string(),
        }
    }

    // --- parse_ip ---

    #[test]
    fn test_parse_ip_valid() {
        assert_eq!(parse_ip("192.168.1.1"), Some(0xC0A80101));
        assert_eq!(parse_ip("10.0.0.1"), Some(0x0A000001));
        assert_eq!(parse_ip("0.0.0.0"), Some(0));
        assert_eq!(parse_ip("255.255.255.255"), Some(0xFFFFFFFF));
    }

    #[test]
    fn test_parse_ip_invalid() {
        assert_eq!(parse_ip("not-an-ip"), None);
        assert_eq!(parse_ip("256.0.0.1"), None);
        assert_eq!(parse_ip("1.2.3"), None);
        assert_eq!(parse_ip(""), None);
    }

    // --- parse_cidr ---

    #[test]
    fn test_parse_cidr_valid() {
        let (network, prefix) = parse_cidr("10.0.0.0/8").unwrap();
        assert_eq!(network, 0x0A000000);
        assert_eq!(prefix, 8);
    }

    #[test]
    fn test_parse_cidr_host() {
        let (_, prefix) = parse_cidr("192.168.1.1/32").unwrap();
        assert_eq!(prefix, 32);
    }

    #[test]
    fn test_parse_cidr_invalid_prefix() {
        assert!(parse_cidr("10.0.0.0/33").is_none());
    }

    #[test]
    fn test_parse_cidr_invalid_format() {
        assert!(parse_cidr("10.0.0.0").is_none());
    }

    // --- ip_in_cidr ---

    #[test]
    fn test_ip_in_cidr_matches() {
        let ip = parse_ip("10.1.2.3").unwrap();
        let net = parse_ip("10.0.0.0").unwrap();
        assert!(ip_in_cidr(ip, net, 8));
    }

    #[test]
    fn test_ip_in_cidr_no_match() {
        let ip = parse_ip("11.0.0.1").unwrap();
        let net = parse_ip("10.0.0.0").unwrap();
        assert!(!ip_in_cidr(ip, net, 8));
    }

    #[test]
    fn test_ip_in_cidr_prefix_zero_matches_all() {
        let ip = parse_ip("1.2.3.4").unwrap();
        assert!(ip_in_cidr(ip, 0, 0));
    }

    #[test]
    fn test_ip_in_cidr_slash_24() {
        let ip = parse_ip("192.168.1.50").unwrap();
        let net = parse_ip("192.168.1.0").unwrap();
        assert!(ip_in_cidr(ip, net, 24));

        let outside = parse_ip("192.168.2.50").unwrap();
        assert!(!ip_in_cidr(outside, net, 24));
    }

    // --- extract_client_ip ---

    #[test]
    fn test_extract_client_ip_from_xff() {
        let plugin = test_plugin();
        let req = request_with_forwarded("10.0.0.1, 172.16.0.1");
        assert_eq!(plugin.extract_client_ip(&req), "10.0.0.1");
    }

    #[test]
    fn test_extract_client_ip_from_real_ip() {
        let plugin = test_plugin();
        let mut headers = BTreeMap::new();
        headers.insert("x-real-ip".to_string(), "10.0.0.2".to_string());
        let req = Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "0.0.0.0".to_string(),
        };
        assert_eq!(plugin.extract_client_ip(&req), "10.0.0.2");
    }

    #[test]
    fn test_extract_client_ip_direct() {
        let plugin = test_plugin();
        let req = request_with_ip("172.16.0.5");
        assert_eq!(plugin.extract_client_ip(&req), "172.16.0.5");
    }

    // --- is_ip_in_list ---

    #[test]
    fn test_is_ip_in_list_cidr_match() {
        let plugin = test_plugin();
        assert!(plugin.is_ip_in_list("10.1.2.3", &plugin.allow));
    }

    #[test]
    fn test_is_ip_in_list_exact_match() {
        let plugin = test_plugin();
        assert!(plugin.is_ip_in_list("192.168.1.100", &plugin.allow));
    }

    #[test]
    fn test_is_ip_in_list_no_match() {
        let plugin = test_plugin();
        assert!(!plugin.is_ip_in_list("172.16.0.1", &plugin.allow));
    }

    // --- on_request ---

    #[test]
    fn test_on_request_allowed_ip() {
        let mut plugin = test_plugin();
        let req = request_with_ip("10.0.0.1");
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn test_on_request_denied_ip_takes_precedence() {
        let mut plugin = test_plugin();
        let req = request_with_ip("10.0.0.5");
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_not_in_allowlist() {
        let mut plugin = test_plugin();
        let req = request_with_ip("172.16.0.1");
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn test_on_request_empty_allow_deny() {
        let mut plugin: IpRestriction =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let req = request_with_ip("1.2.3.4");
        assert!(matches!(plugin.on_request(req), Action::Continue(_)));
    }

    // --- forbidden_response ---

    #[test]
    fn test_forbidden_response_format() {
        let plugin = test_plugin();
        let resp = plugin.forbidden_response("10.0.0.5");
        assert_eq!(resp.status, 403);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:ip-restricted");
        assert_eq!(body["client_ip"], "10.0.0.5");
    }

    #[test]
    fn test_custom_status_code() {
        let mut plugin: IpRestriction = serde_json::from_value(serde_json::json!({
            "deny": ["1.2.3.4"],
            "status": 451,
            "message": "Blocked by policy"
        }))
        .unwrap();
        let req = request_with_ip("1.2.3.4");
        match plugin.on_request(req) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 451);
                let body: serde_json::Value =
                    serde_json::from_str(r.body.as_ref().unwrap()).unwrap();
                assert_eq!(body["detail"], "Blocked by policy");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    // --- config deserialization ---

    #[test]
    fn test_config_defaults() {
        let plugin: IpRestriction = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(plugin.allow.is_empty());
        assert!(plugin.deny.is_empty());
        assert_eq!(plugin.message, "Access denied");
        assert_eq!(plugin.status, 403);
    }

    // --- on_response passthrough ---

    #[test]
    fn test_on_response_passthrough() {
        let mut plugin = test_plugin();
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };
        let result = plugin.on_response(resp);
        assert_eq!(result.status, 200);
    }
}
