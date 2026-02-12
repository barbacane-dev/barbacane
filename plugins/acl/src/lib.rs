//! Access control list middleware plugin for Barbacane API gateway.
//!
//! Enforces access control based on consumer identity and group membership.
//! Reads `x-auth-consumer` and `x-auth-consumer-groups` headers set by
//! upstream auth plugins (basic-auth, jwt-auth, oidc-auth, etc.).

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// ACL middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct Acl {
    /// Group names that are allowed access.
    /// If non-empty, consumer must belong to at least one.
    #[serde(default)]
    allow: Vec<String>,

    /// Group names that are denied access (takes precedence over allow).
    #[serde(default)]
    deny: Vec<String>,

    /// Specific consumer IDs allowed (bypasses group checks).
    #[serde(default)]
    allow_consumers: Vec<String>,

    /// Specific consumer IDs denied (highest precedence).
    #[serde(default)]
    deny_consumers: Vec<String>,

    /// Optional static consumer-to-groups mapping.
    /// Merged with `x-auth-consumer-groups` header (no duplicates).
    #[serde(default)]
    consumer_groups: BTreeMap<String, Vec<String>>,

    /// Custom 403 error message.
    #[serde(default = "default_message")]
    message: String,

    /// Suppress consumer identity in 403 error body.
    #[serde(default)]
    hide_consumer_in_errors: bool,
}

fn default_message() -> String {
    "Access denied by ACL policy".to_string()
}

impl Acl {
    /// Handle incoming request — enforce ACL policy.
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Step 1: Extract consumer identity
        let consumer = match req.headers.get("x-auth-consumer") {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return Action::ShortCircuit(self.forbidden_response(None)),
        };

        // Step 2: deny_consumers — highest precedence
        if self.deny_consumers.contains(&consumer) {
            return Action::ShortCircuit(self.forbidden_response(Some(&consumer)));
        }

        // Step 3: allow_consumers — bypasses group checks
        if self.allow_consumers.contains(&consumer) {
            return Action::Continue(req);
        }

        // Step 4: Resolve groups
        let groups = self.resolve_groups(&consumer, &req);

        // Step 5: deny group match — precedence over allow
        for group in &self.deny {
            if groups.contains(group) {
                return Action::ShortCircuit(self.forbidden_response(Some(&consumer)));
            }
        }

        // Step 6+7: allow group match
        if !self.allow.is_empty() {
            let has_match = self.allow.iter().any(|g| groups.contains(g));
            if !has_match {
                return Action::ShortCircuit(self.forbidden_response(Some(&consumer)));
            }
        }

        // Step 8: allow empty → only deny rules active → pass
        Action::Continue(req)
    }

    /// Pass through responses unchanged.
    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Resolve all groups for a consumer.
    /// Merges `x-auth-consumer-groups` header with static `consumer_groups` config.
    fn resolve_groups(&self, consumer: &str, req: &Request) -> Vec<String> {
        let mut groups = Vec::new();

        // Groups from header (comma-separated)
        if let Some(header_val) = req.headers.get("x-auth-consumer-groups") {
            for g in header_val.split(',') {
                let trimmed = g.trim().to_string();
                if !trimmed.is_empty() && !groups.contains(&trimmed) {
                    groups.push(trimmed);
                }
            }
        }

        // Groups from static config
        if let Some(static_groups) = self.consumer_groups.get(consumer) {
            for g in static_groups {
                if !groups.contains(g) {
                    groups.push(g.clone());
                }
            }
        }

        groups
    }

    /// Generate a 403 Forbidden response in RFC 9457 problem+json format.
    fn forbidden_response(&self, consumer: Option<&str>) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let mut body = serde_json::json!({
            "type": "urn:barbacane:error:acl-denied",
            "title": "Forbidden",
            "status": 403,
            "detail": self.message
        });

        if !self.hide_consumer_in_errors {
            if let Some(consumer) = consumer {
                body["consumer"] = serde_json::Value::String(consumer.to_string());
            }
        }

        Response {
            status: 403,
            headers,
            body: Some(body.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_acl() -> Acl {
        Acl {
            allow: Vec::new(),
            deny: Vec::new(),
            allow_consumers: Vec::new(),
            deny_consumers: Vec::new(),
            consumer_groups: BTreeMap::new(),
            message: "Access denied by ACL policy".to_string(),
            hide_consumer_in_errors: false,
        }
    }

    fn request_with_consumer(consumer: &str, groups: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("x-auth-consumer".to_string(), consumer.to_string());
        if let Some(g) = groups {
            headers.insert("x-auth-consumer-groups".to_string(), g.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers,
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    fn request_without_consumer() -> Request {
        Request {
            method: "GET".to_string(),
            path: "/test".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params: BTreeMap::new(),
            client_ip: "127.0.0.1".to_string(),
        }
    }

    // --- Config tests ---

    #[test]
    fn config_minimal() {
        let json = r#"{}"#;
        let config: Acl = serde_json::from_str(json).unwrap();
        assert!(config.allow.is_empty());
        assert!(config.deny.is_empty());
        assert!(config.allow_consumers.is_empty());
        assert!(config.deny_consumers.is_empty());
        assert!(config.consumer_groups.is_empty());
        assert_eq!(config.message, "Access denied by ACL policy");
        assert!(!config.hide_consumer_in_errors);
    }

    #[test]
    fn config_full() {
        let json = r#"{
            "allow": ["admin", "editor"],
            "deny": ["banned"],
            "allow_consumers": ["superadmin"],
            "deny_consumers": ["attacker"],
            "consumer_groups": {"alice": ["premium"]},
            "message": "Not allowed",
            "hide_consumer_in_errors": true
        }"#;
        let config: Acl = serde_json::from_str(json).unwrap();
        assert_eq!(config.allow, vec!["admin", "editor"]);
        assert_eq!(config.deny, vec!["banned"]);
        assert_eq!(config.allow_consumers, vec!["superadmin"]);
        assert_eq!(config.deny_consumers, vec!["attacker"]);
        assert_eq!(
            config.consumer_groups.get("alice"),
            Some(&vec!["premium".to_string()])
        );
        assert_eq!(config.message, "Not allowed");
        assert!(config.hide_consumer_in_errors);
    }

    // --- resolve_groups tests ---

    #[test]
    fn resolve_groups_from_header() {
        let acl = default_acl();
        let req = request_with_consumer("alice", Some("admin,editor"));
        let groups = acl.resolve_groups("alice", &req);
        assert_eq!(groups, vec!["admin", "editor"]);
    }

    #[test]
    fn resolve_groups_from_static_config() {
        let mut acl = default_acl();
        acl.consumer_groups
            .insert("bob".to_string(), vec!["premium".to_string()]);
        let req = request_with_consumer("bob", None);
        let groups = acl.resolve_groups("bob", &req);
        assert_eq!(groups, vec!["premium"]);
    }

    #[test]
    fn resolve_groups_merged_no_duplicates() {
        let mut acl = default_acl();
        acl.consumer_groups.insert(
            "alice".to_string(),
            vec!["admin".to_string(), "premium".to_string()],
        );
        let req = request_with_consumer("alice", Some("admin,editor"));
        let groups = acl.resolve_groups("alice", &req);
        // admin appears in both header and config, should only appear once
        assert_eq!(groups, vec!["admin", "editor", "premium"]);
    }

    // --- Consumer identification tests ---

    #[test]
    fn missing_consumer_header_returns_403() {
        let mut acl = default_acl();
        let req = request_without_consumer();
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403"),
        }
    }

    #[test]
    fn empty_consumer_header_returns_403() {
        let mut acl = default_acl();
        let mut req = request_without_consumer();
        req.headers
            .insert("x-auth-consumer".to_string(), "".to_string());
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403"),
        }
    }

    // --- Consumer-level tests ---

    #[test]
    fn deny_consumers_takes_highest_precedence() {
        let mut acl = default_acl();
        acl.allow.push("admin".to_string());
        acl.allow_consumers.push("attacker".to_string());
        acl.deny_consumers.push("attacker".to_string());
        let req = request_with_consumer("attacker", Some("admin"));
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403 — deny_consumers should win"),
        }
    }

    #[test]
    fn allow_consumers_bypasses_groups() {
        let mut acl = default_acl();
        acl.allow.push("admin".to_string());
        acl.allow_consumers.push("superadmin".to_string());
        // superadmin has no groups at all, but is in allow_consumers
        let req = request_with_consumer("superadmin", None);
        match acl.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(_) => panic!("Expected allow — allow_consumers should bypass"),
        }
    }

    // --- Group-level tests ---

    #[test]
    fn allow_group_match_passes() {
        let mut acl = default_acl();
        acl.allow.push("editor".to_string());
        let req = request_with_consumer("alice", Some("editor,viewer"));
        match acl.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(_) => panic!("Expected allow — editor matches"),
        }
    }

    #[test]
    fn allow_group_no_match_denies() {
        let mut acl = default_acl();
        acl.allow.push("admin".to_string());
        let req = request_with_consumer("alice", Some("editor,viewer"));
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403 — no matching allow group"),
        }
    }

    #[test]
    fn deny_group_takes_precedence_over_allow() {
        let mut acl = default_acl();
        acl.allow.push("editor".to_string());
        acl.deny.push("banned".to_string());
        let req = request_with_consumer("alice", Some("editor,banned"));
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403 — deny takes precedence"),
        }
    }

    #[test]
    fn deny_only_allows_non_matching() {
        let mut acl = default_acl();
        acl.deny.push("banned".to_string());
        let req = request_with_consumer("alice", Some("editor"));
        match acl.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(_) => panic!("Expected allow — not in deny list"),
        }
    }

    #[test]
    fn deny_only_blocks_matching() {
        let mut acl = default_acl();
        acl.deny.push("banned".to_string());
        let req = request_with_consumer("alice", Some("banned"));
        match acl.on_request(req) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
            Action::Continue(_) => panic!("Expected 403 — banned group matched"),
        }
    }

    // --- Default (no rules) test ---

    #[test]
    fn no_rules_allows_all() {
        let mut acl = default_acl();
        let req = request_with_consumer("anyone", Some("whatever"));
        match acl.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(_) => panic!("Expected allow — no rules configured"),
        }
    }

    // --- Error format tests ---

    #[test]
    fn error_response_problem_json() {
        let acl = default_acl();
        let resp = acl.forbidden_response(Some("alice"));
        assert_eq!(resp.status, 403);
        assert_eq!(
            resp.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"], "urn:barbacane:error:acl-denied");
        assert_eq!(body["title"], "Forbidden");
        assert_eq!(body["status"], 403);
        assert_eq!(body["detail"], "Access denied by ACL policy");
        assert_eq!(body["consumer"], "alice");
    }

    #[test]
    fn error_response_hides_consumer() {
        let mut acl = default_acl();
        acl.hide_consumer_in_errors = true;
        let resp = acl.forbidden_response(Some("alice"));
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert!(body.get("consumer").is_none());
    }

    // --- Response passthrough test ---

    #[test]
    fn on_response_passthrough() {
        let mut acl = default_acl();
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some("ok".to_string()),
        };
        let result = acl.on_response(response.clone());
        assert_eq!(result.status, 200);
        assert_eq!(result.body, response.body);
    }
}
