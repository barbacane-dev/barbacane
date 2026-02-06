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
