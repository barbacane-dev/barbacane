//! IP restriction middleware plugin for Barbacane API gateway.
//!
//! Allows or denies requests based on client IP address or CIDR ranges.
//! Supports both allowlist and denylist modes.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::net::IpAddr;

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

    /// Exact IPs of trusted reverse proxies. `X-Forwarded-For` / `X-Real-IP` are
    /// honored only when the immediate peer is one of these; otherwise the
    /// observed peer IP is used. Empty (default) means forwarded headers are
    /// never trusted, so a client cannot spoof its address.
    #[serde(default)]
    trusted_proxies: Vec<String>,

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
        let client_ip_str = resolve_client_ip(&req, &self.trusted_proxies);

        // Parse the client IP (IPv4 or IPv6). If it can't be parsed we cannot
        // evaluate the rules, so fail closed whenever any restriction exists.
        let client_ip = match client_ip_str.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => {
                if self.allow.is_empty() && self.deny.is_empty() {
                    return Action::Continue(req);
                }
                return Action::ShortCircuit(self.forbidden_response(&client_ip_str));
            }
        };

        // Check denylist first (takes precedence)
        if self.is_ip_in_list(client_ip, &self.deny) {
            return Action::ShortCircuit(self.forbidden_response(&client_ip_str));
        }

        // Check allowlist if configured
        if !self.allow.is_empty() && !self.is_ip_in_list(client_ip, &self.allow) {
            return Action::ShortCircuit(self.forbidden_response(&client_ip_str));
        }

        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Check if an IP matches any entry in a list (single IPs and CIDR ranges,
    /// IPv4 and IPv6).
    fn is_ip_in_list(&self, client_ip: IpAddr, list: &[String]) -> bool {
        list.iter().any(|entry| ip_matches(client_ip, entry))
    }

    /// Generate 403 Forbidden response.
    fn forbidden_response(&self, client_ip: &str) -> Response {
        ProblemDetails::new(
            self.status,
            "urn:barbacane:error:ip-restricted",
            "Forbidden",
        )
        .detail(self.message.clone())
        .with("client_ip", client_ip)
        .into_response()
    }
}

/// Whether `client` matches a list entry: either a single IP (`10.0.0.5`,
/// `2001:db8::1`) or a CIDR range (`10.0.0.0/8`, `2001:db8::/32`). IPv4 and IPv6
/// are both supported; a family mismatch never matches.
fn ip_matches(client: IpAddr, entry: &str) -> bool {
    match entry.split_once('/') {
        Some((network, prefix)) => match (
            network.trim().parse::<IpAddr>(),
            prefix.trim().parse::<u8>(),
        ) {
            (Ok(network), Ok(prefix)) => ip_in_cidr(client, network, prefix),
            _ => false,
        },
        None => entry
            .trim()
            .parse::<IpAddr>()
            .map(|e| e == client)
            .unwrap_or(false),
    }
}

/// Check if an IP address is within a CIDR range (IPv4 or IPv6).
fn ip_in_cidr(ip: IpAddr, network: IpAddr, prefix_len: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            bits_match(&ip.octets(), &net.octets(), prefix_len, 32)
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            bits_match(&ip.octets(), &net.octets(), prefix_len, 128)
        }
        // Mismatched families (e.g. IPv4 client vs IPv6 range) never match.
        _ => false,
    }
}

/// Compare the first `prefix_len` bits of two address byte arrays.
fn bits_match(a: &[u8], b: &[u8], prefix_len: u8, max_bits: u8) -> bool {
    if prefix_len > max_bits {
        return false;
    }
    let mut remaining = prefix_len as usize;
    for (x, y) in a.iter().zip(b.iter()) {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(8);
        let mask: u8 = if take == 8 { 0xFF } else { !0u8 << (8 - take) };
        if (x & mask) != (y & mask) {
            return false;
        }
        remaining -= take;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn test_plugin() -> IpRestriction {
        serde_json::from_value(serde_json::json!({
            "allow": ["10.0.0.0/8", "192.168.1.100", "2001:db8::/32"],
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

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn matches_exact_ipv4_and_ipv6() {
        assert!(ip_matches(ip("192.168.1.100"), "192.168.1.100"));
        assert!(!ip_matches(ip("192.168.1.101"), "192.168.1.100"));
        assert!(ip_matches(ip("2001:db8::1"), "2001:db8::1"));
        assert!(!ip_matches(ip("2001:db8::2"), "2001:db8::1"));
    }

    #[test]
    fn matches_cidr_ipv4_and_ipv6() {
        assert!(ip_matches(ip("10.1.2.3"), "10.0.0.0/8"));
        assert!(!ip_matches(ip("11.0.0.1"), "10.0.0.0/8"));
        assert!(ip_matches(ip("192.168.1.50"), "192.168.1.0/24"));
        assert!(!ip_matches(ip("192.168.2.50"), "192.168.1.0/24"));
        assert!(ip_matches(ip("2001:db8::abcd"), "2001:db8::/32"));
        assert!(!ip_matches(ip("2001:db9::1"), "2001:db8::/32"));
    }

    #[test]
    fn cidr_prefix_zero_matches_all() {
        assert!(ip_matches(ip("1.2.3.4"), "0.0.0.0/0"));
    }

    #[test]
    fn family_mismatch_never_matches() {
        assert!(!ip_matches(ip("10.0.0.1"), "2001:db8::/32"));
        assert!(!ip_matches(ip("2001:db8::1"), "10.0.0.0/8"));
    }

    #[test]
    fn invalid_entry_never_matches() {
        assert!(!ip_matches(ip("10.0.0.1"), "not-an-ip"));
        assert!(!ip_matches(ip("10.0.0.1"), "10.0.0.0/99"));
    }

    #[test]
    fn allowed_ip_continues() {
        let mut p = test_plugin();
        assert!(matches!(
            p.on_request(request_with_ip("10.0.0.1")),
            Action::Continue(_)
        ));
    }

    #[test]
    fn denied_ip_takes_precedence() {
        let mut p = test_plugin();
        match p.on_request(request_with_ip("10.0.0.5")) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn not_in_allowlist_denied() {
        let mut p = test_plugin();
        match p.on_request(request_with_ip("172.16.0.1")) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn ipv6_client_in_allowlist_continues() {
        let mut p = test_plugin();
        assert!(matches!(
            p.on_request(request_with_ip("2001:db8::5")),
            Action::Continue(_)
        ));
    }

    // PL-2 regression: an IPv6 client must be evaluated against a denylist (the
    // old IPv4-only parser silently let every IPv6 client through).
    #[test]
    fn ipv6_client_denylist_does_not_fail_open() {
        let mut p: IpRestriction =
            serde_json::from_value(serde_json::json!({ "deny": ["2001:db8::/32"] })).unwrap();
        match p.on_request(request_with_ip("2001:db8::99")) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("IPv6 client must be denied, not fail open"),
        }
    }

    #[test]
    fn empty_allow_deny_continues() {
        let mut p: IpRestriction = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(matches!(
            p.on_request(request_with_ip("1.2.3.4")),
            Action::Continue(_)
        ));
    }

    // PL-2 regression: a spoofed X-Forwarded-For from an untrusted peer must not
    // bypass the denylist.
    #[test]
    fn spoofed_xff_does_not_bypass_denylist() {
        let mut p: IpRestriction =
            serde_json::from_value(serde_json::json!({ "deny": ["203.0.113.7"] })).unwrap();
        let mut req = request_with_ip("203.0.113.7");
        req.headers
            .insert("x-forwarded-for".to_string(), "10.0.0.1".to_string());
        match p.on_request(req) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("spoofed XFF must not bypass the denylist"),
        }
    }

    #[test]
    fn xff_honored_behind_trusted_proxy() {
        let mut p: IpRestriction = serde_json::from_value(serde_json::json!({
            "allow": ["198.51.100.0/24"],
            "trusted_proxies": ["203.0.113.7"]
        }))
        .unwrap();
        let mut req = request_with_ip("203.0.113.7");
        req.headers
            .insert("x-forwarded-for".to_string(), "198.51.100.42".to_string());
        assert!(matches!(p.on_request(req), Action::Continue(_)));
    }

    #[test]
    fn unparseable_client_ip_fails_closed_when_restricted() {
        let mut p: IpRestriction =
            serde_json::from_value(serde_json::json!({ "deny": ["10.0.0.5"] })).unwrap();
        match p.on_request(request_with_ip("garbage")) {
            Action::ShortCircuit(r) => assert_eq!(r.status, 403),
            _ => panic!("unparseable IP with restrictions must fail closed"),
        }
    }

    #[test]
    fn forbidden_response_format() {
        let p = test_plugin();
        let resp = p.forbidden_response("10.0.0.5");
        assert_eq!(resp.status, 403);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:ip-restricted");
        assert_eq!(body["client_ip"], "10.0.0.5");
    }

    #[test]
    fn custom_status_code() {
        let mut p: IpRestriction = serde_json::from_value(serde_json::json!({
            "deny": ["1.2.3.4"], "status": 451, "message": "Blocked by policy"
        }))
        .unwrap();
        match p.on_request(request_with_ip("1.2.3.4")) {
            Action::ShortCircuit(r) => {
                assert_eq!(r.status, 451);
                let body: serde_json::Value =
                    serde_json::from_slice(r.body.as_ref().unwrap()).unwrap();
                assert_eq!(body["detail"], "Blocked by policy");
            }
            _ => panic!("expected ShortCircuit"),
        }
    }

    #[test]
    fn config_defaults() {
        let p: IpRestriction = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(p.allow.is_empty());
        assert!(p.deny.is_empty());
        assert!(p.trusted_proxies.is_empty());
        assert_eq!(p.message, "Access denied");
        assert_eq!(p.status, 403);
    }

    #[test]
    fn on_response_passthrough() {
        let mut p = test_plugin();
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };
        assert_eq!(p.on_response(resp).status, 200);
    }
}
