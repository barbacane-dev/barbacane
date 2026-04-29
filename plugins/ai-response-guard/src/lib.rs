//! AI response-guard middleware plugin for Barbacane API gateway (ADR-0024).
//!
//! Runs in `on_response` and applies **named policy profiles** selected per
//! request from an upstream context key (typically written by `cel`). Same
//! composition pattern as `ai-proxy` named targets and `ai-prompt-guard`.
//!
//! Each profile carries:
//!
//! 1. **Redact rules** — regex → replacement applied to every
//!    `choices[].message.content` string (and `delta.content`).
//! 2. **Blocked patterns** — regexes scanned across the serialized response
//!    body (post-redaction). A match replaces the response with 502.
//!
//! Streamed responses (ADR-0023) arrive with `status == 0` and no body: the
//! client has already received the tokens. The plugin emits the
//! `redactions_skipped_streaming_total` counter and returns the response
//! unchanged. Operators who need strict redaction with streaming must
//! disable `"stream": true` on those routes.

use barbacane_plugin_sdk::prelude::*;
use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
struct RedactRuleConfig {
    pattern: String,
    #[serde(default = "default_replacement")]
    replacement: String,
}

fn default_replacement() -> String {
    "[REDACTED]".to_string()
}

fn default_context_key() -> String {
    "ai.policy".to_string()
}

#[derive(Deserialize, Default, Clone)]
struct GuardProfile {
    #[serde(default)]
    redact: Vec<RedactRuleConfig>,

    #[serde(default)]
    blocked_patterns: Vec<String>,
}

struct CompiledRedact {
    re: Regex,
    replacement: String,
}

#[derive(Default)]
struct CompiledProfile {
    redact: Vec<CompiledRedact>,
    blocked: Vec<Regex>,
    /// First regex-compile error, if any. Populated at compile time so
    /// subsequent calls fail fast without re-attempting compilation.
    compile_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Plugin struct
// ---------------------------------------------------------------------------

#[barbacane_middleware]
#[derive(Deserialize)]
pub struct AiResponseGuard {
    #[serde(default = "default_context_key")]
    context_key: String,

    default_profile: String,

    profiles: BTreeMap<String, GuardProfile>,

    /// Compiled cache keyed by profile name. Populated lazily.
    #[serde(skip)]
    compiled: BTreeMap<String, CompiledProfile>,
}

impl AiResponseGuard {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        let profile_name = self.resolve_profile_name();
        let Some(profile) = self.profiles.get(&profile_name).cloned() else {
            // Fail-closed: a PII-redaction plugin that silently lets
            // responses through on a config typo is a security downgrade.
            // A streamed response has already been delivered; we can't
            // replace it — record and return the sentinel so the host
            // surfaces the streamed result unchanged.
            log_message(
                0,
                &format!(
                    "ai-response-guard: default_profile '{}' not in profiles map",
                    profile_name
                ),
            );
            if resp.status == 0 {
                return resp;
            }
            return misconfig_response(&profile_name);
        };

        // Streamed responses can't be modified. Record the skip when the
        // *selected* profile actually had redaction work to do.
        if resp.status == 0 {
            if !profile.redact.is_empty() {
                metric_counter_inc("redactions_skipped_streaming_total", "{}", 1);
                log_message(
                    1,
                    "ai-response-guard: redaction skipped — response was streamed",
                );
            }
            return resp;
        }

        // Nothing configured for this profile → pass through without touching
        // the body. Avoids a JSON round-trip for "permissive" profiles.
        if profile.redact.is_empty() && profile.blocked_patterns.is_empty() {
            return resp;
        }

        self.ensure_compiled(&profile_name, &profile);
        let compiled = self
            .compiled
            .get(&profile_name)
            .expect("just compiled above");

        // Fail-closed on invalid regex: a typo that silently disables a PII
        // rule is the kind of bug operators only notice from an incident.
        if let Some(err) = &compiled.compile_error {
            return regex_compile_error_response(&profile_name, err);
        }

        let Some(body_bytes) = resp.body.as_deref() else {
            return resp;
        };

        let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(body_bytes) else {
            return resp;
        };

        if !compiled.redact.is_empty() {
            redact_choices_content(&mut json, &compiled.redact);
        }

        let serialized = match serde_json::to_vec(&json) {
            Ok(v) => v,
            Err(_) => return resp,
        };

        if !compiled.blocked.is_empty() {
            if let Ok(text) = std::str::from_utf8(&serialized) {
                for re in &compiled.blocked {
                    if re.is_match(text) {
                        log_message(
                            0,
                            &format!(
                                "ai-response-guard[{}]: blocked pattern '{}' matched; replacing with 502",
                                profile_name,
                                re.as_str()
                            ),
                        );
                        return blocked_response();
                    }
                }
            }
        }

        Response {
            status: resp.status,
            headers: resp.headers,
            body: Some(serialized),
        }
    }

    fn resolve_profile_name(&self) -> String {
        if let Some(name) = context_get(&self.context_key) {
            if self.profiles.contains_key(&name) {
                return name;
            }
            log_message(
                1,
                &format!(
                    "ai-response-guard: profile '{}' not found; falling back to '{}'",
                    name, self.default_profile
                ),
            );
        }
        self.default_profile.clone()
    }

    fn ensure_compiled(&mut self, profile_name: &str, profile: &GuardProfile) {
        if self.compiled.contains_key(profile_name) {
            return;
        }
        let mut state = CompiledProfile::default();
        for rule in &profile.redact {
            match Regex::new(&rule.pattern) {
                Ok(re) => state.redact.push(CompiledRedact {
                    re,
                    replacement: rule.replacement.clone(),
                }),
                Err(e) => {
                    let msg = format!("invalid redact regex '{}': {}", rule.pattern, e);
                    log_message(0, &format!("ai-response-guard[{}]: {}", profile_name, msg));
                    if state.compile_error.is_none() {
                        state.compile_error = Some(msg);
                    }
                }
            }
        }
        for pat in &profile.blocked_patterns {
            match Regex::new(pat) {
                Ok(re) => state.blocked.push(re),
                Err(e) => {
                    let msg = format!("invalid blocked regex '{}': {}", pat, e);
                    log_message(0, &format!("ai-response-guard[{}]: {}", profile_name, msg));
                    if state.compile_error.is_none() {
                        state.compile_error = Some(msg);
                    }
                }
            }
        }
        self.compiled.insert(profile_name.to_string(), state);
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
        "type": "urn:barbacane:error:ai-response-guard-misconfigured",
        "title": "Internal Server Error",
        "status": 500,
        "detail": format!(
            "ai-response-guard default_profile '{}' does not exist in the profiles map; fix the plugin configuration.",
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
        "type": "urn:barbacane:error:ai-response-guard-misconfigured",
        "title": "Internal Server Error",
        "status": 500,
        "detail": format!(
            "ai-response-guard profile '{}' has an invalid regex: {}",
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
// Redaction walker
// ---------------------------------------------------------------------------

fn redact_choices_content(json: &mut serde_json::Value, rules: &[CompiledRedact]) {
    let Some(choices) = json.get_mut("choices").and_then(|v| v.as_array_mut()) else {
        return;
    };

    for choice in choices.iter_mut() {
        if let Some(content) = choice.pointer_mut("/message/content") {
            if let Some(s) = content.as_str() {
                let redacted = apply_redactions(s, rules);
                *content = serde_json::Value::String(redacted);
            }
        }
        if let Some(content) = choice.pointer_mut("/delta/content") {
            if let Some(s) = content.as_str() {
                let redacted = apply_redactions(s, rules);
                *content = serde_json::Value::String(redacted);
            }
        }
    }
}

fn apply_redactions(input: &str, rules: &[CompiledRedact]) -> String {
    let mut current = input.to_string();
    for rule in rules {
        current = rule
            .re
            .replace_all(&current, rule.replacement.as_str())
            .into_owned();
    }
    current
}

// ---------------------------------------------------------------------------
// Blocked-pattern 502
// ---------------------------------------------------------------------------

fn blocked_response() -> Response {
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    let body = serde_json::json!({
        "type": "urn:barbacane:error:ai-response-blocked",
        "title": "Bad Gateway",
        "status": 502,
        "detail": "Upstream response was blocked by content policy.",
    });
    Response {
        status: 502,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
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
fn metric_counter_inc(name: &str, labels_json: &str, value: u64) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_metric_counter_inc(
            name_ptr: i32,
            name_len: i32,
            labels_ptr: i32,
            labels_len: i32,
            value: f64,
        );
    }
    unsafe {
        host_metric_counter_inc(
            name.as_ptr() as i32,
            name.len() as i32,
            labels_json.as_ptr() as i32,
            labels_json.len() as i32,
            value as f64,
        );
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
        pub(crate) static COUNTERS: RefCell<Vec<(String, String, u64)>> = const { RefCell::new(Vec::new()) };
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|m| m.borrow_mut().clear());
        COUNTERS.with(|m| m.borrow_mut().clear());
    }

    #[cfg(test)]
    pub fn set_context(k: &str, v: &str) {
        CONTEXT.with(|m| m.borrow_mut().insert(k.into(), v.into()));
    }

    #[cfg(test)]
    pub fn counters() -> Vec<(String, String, u64)> {
        COUNTERS.with(|m| m.borrow().clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::CONTEXT.with(|m| m.borrow().get(key).cloned())
}

#[cfg(not(target_arch = "wasm32"))]
fn metric_counter_inc(name: &str, labels: &str, value: u64) {
    mock_host::COUNTERS.with(|m| {
        m.borrow_mut()
            .push((name.to_string(), labels.to_string(), value))
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(redact: Vec<(&str, &str)>, blocked: Vec<&str>) -> GuardProfile {
        GuardProfile {
            redact: redact
                .into_iter()
                .map(|(p, r)| RedactRuleConfig {
                    pattern: p.to_string(),
                    replacement: r.to_string(),
                })
                .collect(),
            blocked_patterns: blocked.into_iter().map(String::from).collect(),
        }
    }

    fn plugin(default_profile: &str, profiles: Vec<(&str, GuardProfile)>) -> AiResponseGuard {
        AiResponseGuard {
            context_key: "ai.policy".into(),
            default_profile: default_profile.into(),
            profiles: profiles.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            compiled: BTreeMap::new(),
        }
    }

    fn single(p: GuardProfile) -> AiResponseGuard {
        plugin("default", vec![("default", p)])
    }

    fn response(body: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".into(), "application/json".into());
        Response {
            status: 200,
            headers,
            body: Some(body.as_bytes().to_vec()),
        }
    }

    // =======================================================================
    // Config shape
    // =======================================================================

    #[test]
    fn config_parses_profile_map() {
        let json = r#"{
            "default_profile": "default",
            "profiles": {
                "default": {
                    "redact": [{"pattern": "\\d+", "replacement": "[N]"}]
                },
                "strict": {
                    "redact": [{"pattern": "secret"}],
                    "blocked_patterns": ["CONFIDENTIAL"]
                }
            }
        }"#;
        let cfg: AiResponseGuard = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.context_key, "ai.policy");
        assert_eq!(cfg.default_profile, "default");
        assert_eq!(cfg.profiles.len(), 2);
        assert_eq!(cfg.profiles["default"].redact.len(), 1);
        assert_eq!(cfg.profiles["default"].redact[0].replacement, "[N]");
        // Default replacement applied
        assert_eq!(cfg.profiles["strict"].redact[0].replacement, "[REDACTED]");
        assert_eq!(cfg.profiles["strict"].blocked_patterns.len(), 1);
    }

    #[test]
    fn config_default_context_key_is_ai_policy() {
        let cfg: AiResponseGuard =
            serde_json::from_str(r#"{"default_profile":"d","profiles":{"d":{}}}"#).expect("parse");
        assert_eq!(cfg.context_key, "ai.policy");
    }

    #[test]
    fn config_custom_context_key_honored() {
        let cfg: AiResponseGuard = serde_json::from_str(
            r#"{"context_key":"tier","default_profile":"d","profiles":{"d":{}}}"#,
        )
        .expect("parse");
        assert_eq!(cfg.context_key, "tier");
    }

    #[test]
    fn config_rejects_missing_required_fields() {
        assert!(serde_json::from_str::<AiResponseGuard>(r#"{"profiles":{"d":{}}}"#).is_err());
        assert!(serde_json::from_str::<AiResponseGuard>(r#"{"default_profile":"d"}"#).is_err());
    }

    // =======================================================================
    // Profile selection
    // =======================================================================

    #[test]
    fn falls_back_to_default_when_context_key_absent() {
        mock_host::reset();
        let p = single(profile(vec![("x", "y")], vec![]));
        assert_eq!(p.resolve_profile_name(), "default");
    }

    #[test]
    fn uses_profile_named_by_context_key() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "strict");
        let p = plugin(
            "default",
            vec![
                ("default", profile(vec![], vec![])),
                ("strict", profile(vec![], vec![])),
            ],
        );
        assert_eq!(p.resolve_profile_name(), "strict");
    }

    #[test]
    fn falls_back_to_default_when_context_names_unknown_profile() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "nonexistent");
        let p = single(profile(vec![], vec![]));
        assert_eq!(p.resolve_profile_name(), "default");
    }

    #[test]
    fn honors_custom_context_key() {
        mock_host::reset();
        mock_host::set_context("tier", "premium");
        let mut p = plugin(
            "default",
            vec![
                ("default", profile(vec![], vec![])),
                ("premium", profile(vec![], vec![])),
            ],
        );
        p.context_key = "tier".into();
        assert_eq!(p.resolve_profile_name(), "premium");
    }

    // =======================================================================
    // Behaviour per profile
    // =======================================================================

    #[test]
    fn selected_profile_applies_redaction() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "strict");

        let mut p = plugin(
            "loose",
            vec![
                ("loose", profile(vec![], vec![])),
                ("strict", profile(vec![(r"\d+", "[N]")], vec![])),
            ],
        );
        let resp = response(r#"{"choices":[{"message":{"content":"call 911"}}]}"#);
        let out = p.on_response(resp);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["choices"][0]["message"]["content"].as_str(),
            Some("call [N]")
        );
    }

    #[test]
    fn default_profile_applies_when_context_unset() {
        mock_host::reset();
        let mut p = plugin(
            "strict",
            vec![
                ("strict", profile(vec![(r"secret", "[HIDDEN]")], vec![])),
                ("lax", profile(vec![], vec![])),
            ],
        );
        let resp = response(r#"{"choices":[{"message":{"content":"top secret"}}]}"#);
        let out = p.on_response(resp);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["choices"][0]["message"]["content"].as_str(),
            Some("top [HIDDEN]")
        );
    }

    #[test]
    fn different_profiles_have_independent_block_lists() {
        mock_host::reset();
        let mut p = plugin(
            "permissive",
            vec![
                ("permissive", profile(vec![], vec![])),
                ("strict", profile(vec![], vec!["(?i)confidential"])),
            ],
        );

        // Default (permissive) — response flows through untouched
        let resp1 = response(r#"{"choices":[{"message":{"content":"CONFIDENTIAL data"}}]}"#);
        assert_eq!(p.on_response(resp1).status, 200);

        // Switch to strict — response replaced with 502
        mock_host::set_context("ai.policy", "strict");
        let resp2 = response(r#"{"choices":[{"message":{"content":"CONFIDENTIAL data"}}]}"#);
        assert_eq!(p.on_response(resp2).status, 502);
    }

    #[test]
    fn empty_profile_passes_through_without_body_roundtrip() {
        // A profile with no rules returns the exact body bytes, not a
        // JSON-normalized reserialization.
        mock_host::reset();
        let raw = r#"{ "choices":[{"message":{"content":"x"}}] , "extra" : true }"#;
        let mut p = single(profile(vec![], vec![]));
        let out = p.on_response(response(raw));
        assert_eq!(out.body.expect("body"), raw.as_bytes());
    }

    #[test]
    fn blocked_scan_runs_after_redaction_per_profile() {
        mock_host::reset();
        let mut p = single(profile(
            vec![(r"sk-[a-z0-9]+", "[KEY]")],
            vec!["sk-[a-z0-9]+"],
        ));
        let resp = response(r#"{"choices":[{"message":{"content":"key: sk-abc123"}}]}"#);
        let out = p.on_response(resp);
        assert_eq!(out.status, 200);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["choices"][0]["message"]["content"].as_str(),
            Some("key: [KEY]")
        );
    }

    #[test]
    fn misconfigured_default_profile_fails_closed_with_500() {
        // Fail-closed: a PII-redaction plugin must NOT silently let upstream
        // responses through when the operator has mis-typed `default_profile`.
        mock_host::reset();
        let mut p = plugin(
            "missing",
            vec![("other", profile(vec![(r"\d+", "[N]")], vec![]))],
        );
        let resp = response(r#"{"choices":[{"message":{"content":"1234"}}]}"#);
        let out = p.on_response(resp);
        assert_eq!(out.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["type"].as_str(),
            Some("urn:barbacane:error:ai-response-guard-misconfigured")
        );
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("'missing'"));
    }

    #[test]
    fn misconfigured_default_profile_on_streamed_response_returns_sentinel() {
        // Streamed responses have already been sent; we can't overwrite with
        // 500. Return the sentinel unchanged but log the misconfig.
        mock_host::reset();
        let mut p = plugin("missing", vec![("other", profile(vec![], vec![]))]);
        let streamed = Response {
            status: 0,
            headers: BTreeMap::new(),
            body: None,
        };
        let out = p.on_response(streamed);
        assert_eq!(out.status, 0);
    }

    // =======================================================================
    // Streamed responses
    // =======================================================================

    #[test]
    fn streamed_response_records_counter_when_selected_profile_has_redact() {
        mock_host::reset();
        let mut p = single(profile(vec![(r"\d+", "[N]")], vec![]));
        let streamed = Response {
            status: 0,
            headers: BTreeMap::new(),
            body: None,
        };
        let out = p.on_response(streamed);
        assert_eq!(out.status, 0);

        let counters = mock_host::counters();
        assert_eq!(counters.len(), 1);
        assert_eq!(counters[0].0, "redactions_skipped_streaming_total");
    }

    #[test]
    fn streamed_response_no_counter_when_selected_profile_has_no_redact() {
        mock_host::reset();
        // Selected profile (default) has no redact; only blocked_patterns.
        let mut p = single(profile(vec![], vec!["anything"]));
        let streamed = Response {
            status: 0,
            headers: BTreeMap::new(),
            body: None,
        };
        let _ = p.on_response(streamed);
        assert!(mock_host::counters().is_empty());
    }

    // =======================================================================
    // Edge cases
    // =======================================================================

    #[test]
    fn non_json_body_passes_through() {
        mock_host::reset();
        let mut p = single(profile(vec![(r"\d+", "[N]")], vec![]));
        let resp = response("not json");
        let out = p.on_response(resp);
        assert_eq!(out.body.expect("body"), b"not json");
    }

    #[test]
    fn missing_choices_array_passes_through() {
        mock_host::reset();
        let mut p = single(profile(vec![(r"\d+", "[N]")], vec![]));
        let resp = response(r#"{"error":"oops 123"}"#);
        let out = p.on_response(resp);
        // JSON round-trip preserves the field
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(body["error"].as_str(), Some("oops 123"));
    }

    #[test]
    fn redact_applies_to_delta_content() {
        mock_host::reset();
        let mut p = single(profile(vec![("secret", "[HIDDEN]")], vec![]));
        let resp = response(r#"{"choices":[{"delta":{"content":"top secret"}}]}"#);
        let out = p.on_response(resp);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["choices"][0]["delta"]["content"].as_str(),
            Some("top [HIDDEN]")
        );
    }

    #[test]
    fn invalid_redact_regex_fails_closed_with_500() {
        // A typo in a redact pattern silently disabled that rule before —
        // which for a PII plugin is an incident waiting to happen. Fail-closed.
        mock_host::reset();
        let mut p = single(profile(vec![("[invalid", "x")], vec![]));
        let resp = response(r#"{"choices":[{"message":{"content":"hi"}}]}"#);
        let out = p.on_response(resp);
        assert_eq!(out.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert_eq!(
            body["type"].as_str(),
            Some("urn:barbacane:error:ai-response-guard-misconfigured")
        );
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid redact regex"));
    }

    #[test]
    fn invalid_blocked_pattern_fails_closed_with_500() {
        mock_host::reset();
        let mut p = single(profile(vec![], vec!["[also-invalid"]));
        let resp = response(r#"{"choices":[{"message":{"content":"hi"}}]}"#);
        let out = p.on_response(resp);
        assert_eq!(out.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(&out.body.expect("body")).expect("json");
        assert!(body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid blocked regex"));
    }

    #[test]
    fn compilation_cached_per_profile() {
        mock_host::reset();
        let mut p = plugin(
            "a",
            vec![
                ("a", profile(vec![(r"aaa", "x")], vec![])),
                ("b", profile(vec![(r"bbb", "y")], vec![])),
            ],
        );
        let _ = p.on_response(response(r#"{"choices":[]}"#));
        assert!(p.compiled.contains_key("a"));
        assert!(!p.compiled.contains_key("b"));

        mock_host::set_context("ai.policy", "b");
        let _ = p.on_response(response(r#"{"choices":[]}"#));
        assert!(p.compiled.contains_key("a"));
        assert!(p.compiled.contains_key("b"));
    }

    // =======================================================================
    // on_request
    // =======================================================================

    #[test]
    fn on_request_is_passthrough() {
        let mut p = single(profile(vec![], vec![]));
        let req = Request {
            method: "POST".into(),
            path: "/".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".into(),
            path_params: BTreeMap::new(),
        };
        let Action::Continue(_) = p.on_request(req) else {
            panic!("expected continue");
        };
    }
}
