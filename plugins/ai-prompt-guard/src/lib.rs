//! AI prompt guard middleware plugin for Barbacane API gateway (ADR-0024).
//!
//! Validates and constrains LLM chat-completion requests before they reach the
//! provider. Runs in the `on_request` phase; rejects violations with a 400 and
//! a problem+json body.
//!
//! # Policy composition
//!
//! The plugin exposes **named profiles** selected at request time from a
//! context key written by an upstream middleware (typically `cel`). The
//! pattern mirrors `ai-proxy`'s named targets:
//!
//! ```yaml
//! - name: cel
//!   config:
//!     expression: "request.claims.tier == 'premium'"
//!     on_match:
//!       set_context:
//!         ai.policy: premium
//!
//! - name: ai-prompt-guard
//!   config:
//!     default_profile: standard
//!     profiles:
//!       standard: { max_messages: 50, max_message_length: 32000 }
//!       premium:  { max_messages: 100 }
//!       trial:    { max_messages: 5, max_message_length: 2000, blocked_patterns: ["(?i)code"] }
//! ```
//!
//! The plugin reads `ai.policy` (overridable via `context_key`). When the key
//! is absent or names an unknown profile, `default_profile` applies.

use barbacane_plugin_sdk::prelude::*;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// A single named policy profile. Fields mirror the behaviour concerns listed
/// in ADR-0024 for `ai-prompt-guard` — length bounds, blocked patterns, and
/// managed system-template injection.
#[derive(Deserialize, Default, Clone)]
struct PromptProfile {
    #[serde(default)]
    max_messages: Option<usize>,

    #[serde(default)]
    max_message_length: Option<usize>,

    #[serde(default)]
    blocked_patterns: Vec<String>,

    #[serde(default)]
    system_template: Option<String>,

    #[serde(default)]
    template_vars: BTreeMap<String, String>,

    #[serde(default = "default_reject_status")]
    reject_status: u16,
}

fn default_reject_status() -> u16 {
    400
}

fn default_context_key() -> String {
    "ai.policy".to_string()
}

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

/// AI prompt-guard middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct AiPromptGuard {
    /// Context key read to select the active profile. Typically written by a
    /// `cel` middleware earlier in the chain (ADR-0024).
    #[serde(default = "default_context_key")]
    context_key: String,

    /// Profile name used when the context key is absent or names an unknown
    /// profile. Must appear in `profiles`.
    default_profile: String,

    /// Named profiles the operator can select between.
    profiles: BTreeMap<String, PromptProfile>,

    /// Compiled regex cache, keyed by profile name. Populated lazily.
    #[serde(skip)]
    compiled: BTreeMap<String, Vec<Regex>>,

    /// First regex-compile error per profile, if any. Surfaces misconfigs
    /// as 500 on the first request rather than silently dropping rules.
    #[serde(skip)]
    compile_errors: BTreeMap<String, Option<String>>,
}

impl AiPromptGuard {
    pub fn on_request(&mut self, mut req: Request) -> Action<Request> {
        let profile_name = self.resolve_profile_name();
        let Some(profile) = self.profiles.get(&profile_name).cloned() else {
            // Fail-closed: a guard plugin that lets requests through on a
            // misconfig is strictly weaker than one that errors loudly.
            log_message(
                0,
                &format!(
                    "ai-prompt-guard: default_profile '{}' not in profiles map",
                    profile_name
                ),
            );
            return Action::ShortCircuit(misconfig_response(&profile_name));
        };

        // Compile + validate regexes before body inspection. On invalid
        // patterns we 500 rather than silently skipping the rule.
        self.ensure_compiled(&profile_name, &profile);
        if let Some(err) = self
            .compile_errors
            .get(&profile_name)
            .cloned()
            .and_then(|e| e)
        {
            return Action::ShortCircuit(regex_compile_error_response(&profile_name, &err));
        }

        let Some(body_bytes) = req.body.as_deref() else {
            return Action::Continue(req);
        };

        let mut root: serde_json::Value = match serde_json::from_slice(body_bytes) {
            Ok(v) => v,
            Err(_) => return Action::Continue(req),
        };

        let Some(messages) = root.get("messages").and_then(|v| v.as_array()).cloned() else {
            return Action::Continue(req);
        };

        // --- Message count limit ---
        if let Some(max) = profile.max_messages {
            if messages.len() > max {
                return Action::ShortCircuit(reject(
                    &profile,
                    &format!(
                        "request has {} messages, max allowed is {}",
                        messages.len(),
                        max
                    ),
                ));
            }
        }

        let patterns = self
            .compiled
            .get(&profile_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        for (idx, msg) in messages.iter().enumerate() {
            let content = extract_message_text(msg);

            if let Some(max) = profile.max_message_length {
                if content.chars().count() > max {
                    return Action::ShortCircuit(reject(
                        &profile,
                        &format!(
                            "message[{}] exceeds max_message_length ({} chars)",
                            idx, max
                        ),
                    ));
                }
            }

            for pattern in patterns {
                if pattern.is_match(&content) {
                    log_message(
                        1,
                        &format!(
                            "ai-prompt-guard[{}]: blocked pattern '{}' matched in message[{}]",
                            profile_name,
                            pattern.as_str(),
                            idx
                        ),
                    );
                    return Action::ShortCircuit(reject(
                        &profile,
                        "prompt contains disallowed content",
                    ));
                }
            }
        }

        // --- System template injection ---
        if let Some(template) = &profile.system_template {
            let rendered = render_template(template, &profile.template_vars);
            let filtered: Vec<serde_json::Value> = messages
                .into_iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
                .collect();

            let mut new_messages = Vec::with_capacity(filtered.len() + 1);
            new_messages.push(serde_json::json!({
                "role": "system",
                "content": rendered,
            }));
            new_messages.extend(filtered);

            if let Some(obj) = root.as_object_mut() {
                obj.insert(
                    "messages".to_string(),
                    serde_json::Value::Array(new_messages),
                );
            }

            match serde_json::to_vec(&root) {
                Ok(new_body) => req.body = Some(new_body),
                Err(e) => log_message(
                    0,
                    &format!("ai-prompt-guard: failed to serialize rewritten body: {}", e),
                ),
            }
        }

        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    fn resolve_profile_name(&self) -> String {
        if let Some(name) = context_get(&self.context_key) {
            if self.profiles.contains_key(&name) {
                return name;
            }
            log_message(
                1,
                &format!(
                    "ai-prompt-guard: profile '{}' not found; falling back to '{}'",
                    name, self.default_profile
                ),
            );
        }
        self.default_profile.clone()
    }

    fn ensure_compiled(&mut self, profile_name: &str, profile: &PromptProfile) {
        if self.compiled.contains_key(profile_name) {
            return;
        }
        let mut out = Vec::with_capacity(profile.blocked_patterns.len());
        let mut first_error: Option<String> = None;
        for pat in &profile.blocked_patterns {
            match Regex::new(pat) {
                Ok(re) => out.push(re),
                Err(e) => {
                    let msg = format!("invalid blocked_patterns regex '{}': {}", pat, e);
                    log_message(0, &format!("ai-prompt-guard[{}]: {}", profile_name, msg));
                    if first_error.is_none() {
                        first_error = Some(msg);
                    }
                }
            }
        }
        self.compiled.insert(profile_name.to_string(), out);
        self.compile_errors
            .insert(profile_name.to_string(), first_error);
    }
}

// ---------------------------------------------------------------------------
// Fail-closed error responses
// ---------------------------------------------------------------------------

fn misconfig_response(default_profile: &str) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    let body = serde_json::json!({
        "type": "urn:barbacane:error:ai-prompt-guard-misconfigured",
        "title": "Internal Server Error",
        "status": 500,
        "detail": format!(
            "ai-prompt-guard default_profile '{}' does not exist in the profiles map; fix the plugin configuration.",
            default_profile
        ),
    });
    Response {
        status: 500,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
}

fn regex_compile_error_response(profile_name: &str, detail: &str) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    let body = serde_json::json!({
        "type": "urn:barbacane:error:ai-prompt-guard-misconfigured",
        "title": "Internal Server Error",
        "status": 500,
        "detail": format!(
            "ai-prompt-guard profile '{}' has an invalid regex: {}",
            profile_name, detail
        ),
    });
    Response {
        status: 500,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn reject(profile: &PromptProfile, detail: &str) -> Response {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    let body = serde_json::json!({
        "type": "urn:barbacane:error:ai-prompt-guard",
        "title": "Bad Request",
        "status": profile.reject_status,
        "detail": detail,
    });
    Response {
        status: profile.reject_status,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
}

/// Extract a string representation of a message's `content` field.
///
/// Accepts the classic OpenAI form `"content": "text"` and the multimodal form
/// `"content": [{"type":"text","text":"..."}]`. For multimodal, all `text`
/// parts are concatenated with newlines.
fn extract_message_text(msg: &serde_json::Value) -> String {
    let Some(content) = msg.get("content") else {
        return String::new();
    };

    if let Some(s) = content.as_str() {
        return s.to_string();
    }

    if let Some(parts) = content.as_array() {
        let mut out = String::new();
        for part in parts {
            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
        }
        return out;
    }

    String::new()
}

/// Replace `{name}` placeholders. Unknown placeholders are left in place.
fn render_template(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        let mut name = String::new();
        let mut closed = false;
        for nc in chars.by_ref() {
            if nc == '}' {
                closed = true;
                break;
            }
            name.push(nc);
        }
        if !closed {
            out.push('{');
            out.push_str(&name);
            continue;
        }
        if let Some(value) = vars.get(&name) {
            out.push_str(value);
        } else {
            out.push('{');
            out.push_str(&name);
            out.push('}');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Host bindings
// ---------------------------------------------------------------------------

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
        let read = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read != len {
            return None;
        }
        String::from_utf8(buf).ok()
    }
}

#[cfg(target_arch = "wasm32")]
fn log_message(level: i32, msg: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
    }
    unsafe { host_log(level, msg.as_ptr() as i32, msg.len() as i32) }
}

// ---------------------------------------------------------------------------
// Native stubs
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        pub(crate) static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|m| m.borrow_mut().clear());
    }

    #[cfg(test)]
    pub fn set_context(k: &str, v: &str) {
        CONTEXT.with(|m| m.borrow_mut().insert(k.into(), v.into()));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::CONTEXT.with(|m| m.borrow().get(key).cloned())
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(default_profile: &str, profiles: Vec<(&str, PromptProfile)>) -> AiPromptGuard {
        AiPromptGuard {
            context_key: "ai.policy".to_string(),
            default_profile: default_profile.to_string(),
            profiles: profiles
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            compiled: BTreeMap::new(),
            compile_errors: BTreeMap::new(),
        }
    }

    fn profile_with(
        max_messages: Option<usize>,
        max_message_length: Option<usize>,
        blocked_patterns: Vec<&str>,
    ) -> PromptProfile {
        PromptProfile {
            max_messages,
            max_message_length,
            blocked_patterns: blocked_patterns.into_iter().map(String::from).collect(),
            system_template: None,
            template_vars: BTreeMap::new(),
            reject_status: 400,
        }
    }

    fn single_profile_plugin(p: PromptProfile) -> AiPromptGuard {
        plugin("default", vec![("default", p)])
    }

    fn req(body: &str) -> Request {
        Request {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            query: None,
            headers: BTreeMap::new(),
            body: Some(body.as_bytes().to_vec()),
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        }
    }

    // =======================================================================
    // Config shape
    // =======================================================================

    #[test]
    fn config_parses_profile_map() {
        let json = r#"{
            "default_profile": "standard",
            "profiles": {
                "standard": { "max_messages": 50, "max_message_length": 32000 },
                "strict": {
                    "max_messages": 5,
                    "blocked_patterns": ["(?i)ignore previous"],
                    "system_template": "You are {company}.",
                    "template_vars": { "company": "Acme" }
                }
            }
        }"#;
        let cfg: AiPromptGuard = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.context_key, "ai.policy");
        assert_eq!(cfg.default_profile, "standard");
        assert_eq!(cfg.profiles.len(), 2);
        assert_eq!(cfg.profiles["standard"].max_messages, Some(50));
        assert_eq!(cfg.profiles["strict"].blocked_patterns.len(), 1);
        assert_eq!(cfg.profiles["strict"].reject_status, 400); // default
    }

    #[test]
    fn config_default_context_key_is_ai_policy() {
        let cfg: AiPromptGuard =
            serde_json::from_str(r#"{"default_profile":"d","profiles":{"d":{}}}"#).expect("parse");
        assert_eq!(cfg.context_key, "ai.policy");
    }

    #[test]
    fn config_custom_context_key_honored() {
        let cfg: AiPromptGuard = serde_json::from_str(
            r#"{"context_key":"x.y","default_profile":"d","profiles":{"d":{}}}"#,
        )
        .expect("parse");
        assert_eq!(cfg.context_key, "x.y");
    }

    #[test]
    fn config_rejects_missing_required_fields() {
        assert!(serde_json::from_str::<AiPromptGuard>(r#"{"profiles":{}}"#).is_err());
        assert!(serde_json::from_str::<AiPromptGuard>(r#"{"default_profile":"d"}"#).is_err());
    }

    // =======================================================================
    // Profile selection
    // =======================================================================

    #[test]
    fn falls_back_to_default_when_context_key_absent() {
        mock_host::reset();
        let p = single_profile_plugin(profile_with(Some(1), None, vec![]));
        assert_eq!(p.resolve_profile_name(), "default");
    }

    #[test]
    fn uses_profile_named_by_context_key() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "strict");
        let p = plugin(
            "default",
            vec![
                ("default", profile_with(Some(50), None, vec![])),
                ("strict", profile_with(Some(5), None, vec![])),
            ],
        );
        assert_eq!(p.resolve_profile_name(), "strict");
    }

    #[test]
    fn falls_back_to_default_when_context_names_unknown_profile() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "nonexistent");
        let p = plugin(
            "default",
            vec![("default", profile_with(Some(50), None, vec![]))],
        );
        assert_eq!(p.resolve_profile_name(), "default");
    }

    #[test]
    fn honors_custom_context_key() {
        mock_host::reset();
        mock_host::set_context("tier", "premium");
        let mut p = plugin(
            "default",
            vec![
                ("default", profile_with(None, None, vec![])),
                ("premium", profile_with(None, None, vec![])),
            ],
        );
        p.context_key = "tier".to_string();
        assert_eq!(p.resolve_profile_name(), "premium");
    }

    // =======================================================================
    // Behaviour scoped to selected profile
    // =======================================================================

    #[test]
    fn active_profile_applies_message_count_limit() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "strict");
        let mut p = plugin(
            "default",
            vec![
                ("default", profile_with(Some(50), None, vec![])),
                ("strict", profile_with(Some(1), None, vec![])),
            ],
        );
        let r = req(r#"{"messages":[
            {"role":"user","content":"a"},
            {"role":"user","content":"b"}
        ]}"#);
        match p.on_request(r) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 400);
                let body = String::from_utf8(resp.body.expect("body")).expect("utf8");
                assert!(body.contains("max allowed is 1"));
            }
            _ => panic!("expected short-circuit"),
        }
    }

    #[test]
    fn default_profile_applies_when_context_unset() {
        mock_host::reset();
        let mut p = plugin(
            "default",
            vec![
                ("default", profile_with(Some(1), None, vec![])),
                ("premium", profile_with(Some(100), None, vec![])),
            ],
        );
        let r = req(r#"{"messages":[
            {"role":"user","content":"a"},
            {"role":"user","content":"b"}
        ]}"#);
        match p.on_request(r) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 400),
            _ => panic!("expected short-circuit under default profile"),
        }
    }

    #[test]
    fn different_profiles_have_independent_pattern_lists() {
        mock_host::reset();
        // premium → strict list; trial → lax (no patterns)
        let mut p = plugin(
            "trial",
            vec![
                ("trial", profile_with(None, None, vec![])),
                ("premium", profile_with(None, None, vec!["(?i)secret"])),
            ],
        );

        // First call under "trial" (default) — "secret" passes.
        let r1 = req(r#"{"messages":[{"role":"user","content":"top secret"}]}"#);
        assert!(matches!(p.on_request(r1), Action::Continue(_)));

        // Flip to "premium" — same content now rejected.
        mock_host::set_context("ai.policy", "premium");
        let r2 = req(r#"{"messages":[{"role":"user","content":"top secret"}]}"#);
        assert!(matches!(p.on_request(r2), Action::ShortCircuit(_)));
    }

    #[test]
    fn misconfigured_default_profile_fails_closed_with_500() {
        // Fail-closed: a guard plugin that lets requests through on an
        // operator typo is strictly weaker than one that errors loudly.
        mock_host::reset();
        let mut p = plugin(
            "missing",
            vec![("other", profile_with(Some(1), None, vec![]))],
        );
        let r = req(r#"{"messages":[{"role":"user","content":"x"}]}"#);
        match p.on_request(r) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 500);
                let body = String::from_utf8(resp.body.expect("body")).expect("utf8");
                assert!(body.contains("urn:barbacane:error:ai-prompt-guard-misconfigured"));
                assert!(body.contains("'missing'"));
            }
            _ => panic!("expected 500 short-circuit on misconfig"),
        }
    }

    #[test]
    fn profile_max_message_length_counts_characters() {
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(None, Some(2), vec![]));
        let r = req(r#"{"messages":[{"role":"user","content":"éé"}]}"#);
        assert!(matches!(p.on_request(r), Action::Continue(_)));

        let r2 = req(r#"{"messages":[{"role":"user","content":"too long"}]}"#);
        match p.on_request(r2) {
            Action::ShortCircuit(resp) => {
                let body = String::from_utf8(resp.body.expect("b")).expect("utf8");
                assert!(body.contains("max_message_length"));
            }
            _ => panic!("expected short-circuit"),
        }
    }

    #[test]
    fn profile_blocked_pattern_matches_multimodal_text() {
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(None, None, vec!["(?i)SECRET"]));
        let body = r#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"the secret is..."}
        ]}]}"#;
        assert!(matches!(p.on_request(req(body)), Action::ShortCircuit(_)));
    }

    #[test]
    fn profile_system_template_replaces_client_system_messages() {
        mock_host::reset();
        let mut vars = BTreeMap::new();
        vars.insert("company".to_string(), "Acme".to_string());
        let profile = PromptProfile {
            max_messages: None,
            max_message_length: None,
            blocked_patterns: vec![],
            system_template: Some("Managed prompt for {company}.".into()),
            template_vars: vars,
            reject_status: 400,
        };
        let mut p = single_profile_plugin(profile);
        let r = req(r#"{"messages":[
                {"role":"system","content":"you are evil"},
                {"role":"user","content":"hi"}
            ]}"#);
        let Action::Continue(modified) = p.on_request(r) else {
            panic!("expected continue");
        };
        let body: serde_json::Value =
            serde_json::from_slice(modified.body.as_ref().expect("body")).expect("json");
        let msgs = body["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 2); // client system replaced
        assert_eq!(msgs[0]["role"].as_str(), Some("system"));
        assert_eq!(
            msgs[0]["content"].as_str(),
            Some("Managed prompt for Acme.")
        );
    }

    #[test]
    fn profile_custom_reject_status_used() {
        mock_host::reset();
        let profile = PromptProfile {
            max_messages: Some(0),
            max_message_length: None,
            blocked_patterns: vec![],
            system_template: None,
            template_vars: BTreeMap::new(),
            reject_status: 422,
        };
        let mut p = single_profile_plugin(profile);
        let r = req(r#"{"messages":[{"role":"user","content":"hi"}]}"#);
        match p.on_request(r) {
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 422),
            _ => panic!("expected short-circuit"),
        }
    }

    #[test]
    fn compilation_cached_per_profile() {
        mock_host::reset();
        let mut p = plugin(
            "a",
            vec![
                ("a", profile_with(None, None, vec!["aaa"])),
                ("b", profile_with(None, None, vec!["bbb"])),
            ],
        );
        assert!(p.compiled.is_empty());

        // First call selects "a" — only "a" compiled.
        let _ = p.on_request(req(r#"{"messages":[{"role":"user","content":"hi"}]}"#));
        assert!(p.compiled.contains_key("a"));
        assert!(!p.compiled.contains_key("b"));

        // Switch to "b" via context — "b" joins the cache; "a" stays.
        mock_host::set_context("ai.policy", "b");
        let _ = p.on_request(req(r#"{"messages":[{"role":"user","content":"hi"}]}"#));
        assert!(p.compiled.contains_key("a"));
        assert!(p.compiled.contains_key("b"));
    }

    #[test]
    fn invalid_regex_fails_closed_with_500() {
        // A typo in `blocked_patterns` used to be silently skipped, which
        // quietly disabled the rule. Operators catch the mistake on the
        // first request now instead of in a post-incident review.
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(None, None, vec!["[invalid"]));
        let r = req(r#"{"messages":[{"role":"user","content":"hi"}]}"#);
        match p.on_request(r) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 500);
                let body = String::from_utf8(resp.body.expect("body")).expect("utf8");
                assert!(body.contains("urn:barbacane:error:ai-prompt-guard-misconfigured"));
                assert!(body.contains("invalid blocked_patterns regex"));
            }
            _ => panic!("expected 500 on invalid regex"),
        }
    }

    // =======================================================================
    // Pass-through cases
    // =======================================================================

    #[test]
    fn no_body_continues() {
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(Some(5), None, vec![]));
        let mut r = req("");
        r.body = None;
        assert!(matches!(p.on_request(r), Action::Continue(_)));
    }

    #[test]
    fn non_json_body_continues() {
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(Some(5), None, vec![]));
        assert!(matches!(p.on_request(req("not json")), Action::Continue(_)));
    }

    #[test]
    fn body_without_messages_continues() {
        mock_host::reset();
        let mut p = single_profile_plugin(profile_with(Some(5), None, vec![]));
        assert!(matches!(
            p.on_request(req(r#"{"input":"hello"}"#)),
            Action::Continue(_)
        ));
    }

    #[test]
    fn on_response_is_passthrough() {
        let mut p = single_profile_plugin(profile_with(None, None, vec![]));
        let mut headers = BTreeMap::new();
        headers.insert("content-type".into(), "application/json".into());
        let resp = Response {
            status: 200,
            headers: headers.clone(),
            body: Some(b"{}".to_vec()),
        };
        let out = p.on_response(resp);
        assert_eq!(out.status, 200);
        assert_eq!(out.headers, headers);
        assert_eq!(out.body.as_deref(), Some(b"{}".as_ref()));
    }

    // =======================================================================
    // Pure helpers
    // =======================================================================

    #[test]
    fn render_template_no_vars() {
        assert_eq!(
            render_template("hello world", &BTreeMap::new()),
            "hello world"
        );
    }

    #[test]
    fn render_template_unclosed_brace_kept() {
        assert_eq!(
            render_template("hello {name", &BTreeMap::new()),
            "hello {name"
        );
    }

    #[test]
    fn render_template_unknown_placeholder_kept() {
        assert_eq!(render_template("x {y} z", &BTreeMap::new()), "x {y} z");
    }

    #[test]
    fn extract_missing_content() {
        let msg = serde_json::json!({"role": "user"});
        assert_eq!(extract_message_text(&msg), "");
    }

    #[test]
    fn extract_multimodal_joins_text_parts() {
        let msg = serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "first"},
                {"type": "image_url"},
                {"type": "text", "text": "second"}
            ]
        });
        assert_eq!(extract_message_text(&msg), "first\nsecond");
    }
}
