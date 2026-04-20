//! AI token-limit middleware plugin for Barbacane API gateway (ADR-0024).
//!
//! Enforces a token budget per consumer per sliding window. Budget is charged
//! against the token counts reported by the `ai-proxy` dispatcher via context
//! keys `ai.prompt_tokens` / `ai.completion_tokens`.
//!
//! # Policy composition
//!
//! Each profile carries its own `quota` + `window`. The active profile is
//! selected from a context key written by an upstream middleware (typically
//! `cel`) — the same composition pattern used by `ai-proxy` named targets
//! and `ai-prompt-guard` / `ai-response-guard`.
//!
//! Consumer partitioning stays top-level (not per-profile): one operator
//! policy names a budget tier; a separate top-level `partition_key` names
//! *whose* budget is being charged.
//!
//! # Enforcement model
//!
//! - **on_request** asks the host rate limiter whether the current bucket has
//!   capacity. Each call records one unit of usage; if exhausted the request
//!   is rejected with 429 plus standard `ratelimit-*` headers.
//! - **on_response** reads the real token count from context and charges the
//!   remainder (`tokens_used - 1`) against the same bucket. A streamed
//!   response that already left the gateway cannot be interrupted
//!   retroactively — the overshoot is absorbed and the *next* request 429s.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which token counts charge against the budget.
#[derive(Deserialize, Clone, Copy, PartialEq, Debug, Default)]
#[serde(rename_all = "lowercase")]
enum CountMode {
    Prompt,
    Completion,
    #[default]
    Total,
}

#[derive(Deserialize, Clone)]
struct TokenProfile {
    /// Maximum tokens allowed per sliding window.
    quota: u32,
    /// Sliding-window duration in seconds.
    window: u32,
}

fn default_context_key() -> String {
    "ai.policy".to_string()
}

fn default_partition_key() -> String {
    "client_ip".to_string()
}

fn default_policy_name() -> String {
    "ai-tokens".to_string()
}

/// AI token-limit middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct AiTokenLimit {
    /// Context key read to select the active profile.
    #[serde(default = "default_context_key")]
    context_key: String,

    /// Profile used when the context key is absent or names an unknown
    /// profile. Must be a key of `profiles`.
    default_profile: String,

    /// Named token-budget profiles. Each profile owns a `quota` + `window`.
    profiles: BTreeMap<String, TokenProfile>,

    /// Identifier used in `ratelimit-policy` headers and as the rate-limit
    /// bucket-key prefix. Shared across all profiles of a single instance.
    #[serde(default = "default_policy_name")]
    policy_name: String,

    /// Per-consumer partition source. Same semantics as `rate-limit` plugin:
    /// `client_ip`, `header:<name>`, `context:<key>`, or a literal string.
    #[serde(default = "default_partition_key")]
    partition_key: String,

    /// Which tokens charge against the budget.
    #[serde(default)]
    count: CountMode,
}

/// Result from `host_rate_limit_check`. Only the fields consulted below are
/// materialized; `remaining` is ignored on the wire.
#[derive(Debug, Deserialize)]
struct RateLimitResult {
    allowed: bool,
    reset: u64,
    limit: u32,
    #[serde(default)]
    retry_after: Option<u64>,
}

// ---------------------------------------------------------------------------
// Plugin impl
// ---------------------------------------------------------------------------

impl AiTokenLimit {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        let (profile_name, profile) = match self.resolve_profile() {
            Some(p) => p,
            None => return Action::ShortCircuit(misconfig_response(&self.default_profile)),
        };

        let partition = extract_partition(&req, &self.partition_key);

        // Persist the resolved partition so on_response charges the same
        // bucket — on_response has no Request in scope and header/IP sources
        // would otherwise degrade to the shared "unknown" bucket.
        host_context_set(&self.partition_context_key(), &partition);

        let key = self.bucket_key(&profile_name, &partition);

        let Some(result) = check_rate_limit(&key, profile.quota, profile.window) else {
            log_message(
                1,
                "ai-token-limit: rate limiter unavailable, allowing request",
            );
            return Action::Continue(req);
        };

        if result.allowed {
            Action::Continue(req)
        } else {
            Action::ShortCircuit(self.too_many_requests_response(&profile_name, &profile, &result))
        }
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        let Some((profile_name, profile)) = self.resolve_profile() else {
            // on_request already short-circuited with 500 in this case;
            // on_response for that request won't run. Defensive: pass through.
            return resp;
        };

        let tokens = self.tokens_from_context();
        if tokens == 0 {
            return resp;
        }
        // One unit was already charged on_request; charge the rest.
        let extra = tokens.saturating_sub(1);
        if extra == 0 {
            return resp;
        }

        // Prefer the partition persisted by on_request; fall back to
        // context-derivable sources only if the key is missing (e.g. when
        // this instance is invoked on_response without a matching on_request,
        // which shouldn't happen in normal flows).
        let partition = context_get(&self.partition_context_key())
            .unwrap_or_else(|| partition_from_context_only(&self.partition_key));
        let key = self.bucket_key(&profile_name, &partition);

        for _ in 0..extra {
            let Some(result) = check_rate_limit(&key, profile.quota, profile.window) else {
                break;
            };
            if !result.allowed {
                break;
            }
        }

        resp
    }

    /// Context key used to carry the resolved partition from on_request to
    /// on_response. Scoped by `policy_name` so stacked instances don't
    /// overwrite each other.
    fn partition_context_key(&self) -> String {
        format!("__ai_token_limit.{}.partition", self.policy_name)
    }

    /// Pick the active profile, or `None` if `default_profile` isn't even in
    /// the map (misconfiguration — caller should pass-through).
    fn resolve_profile(&self) -> Option<(String, TokenProfile)> {
        let name = self.resolve_profile_name();
        let profile = self.profiles.get(&name)?.clone();
        Some((name, profile))
    }

    fn resolve_profile_name(&self) -> String {
        if let Some(name) = context_get(&self.context_key) {
            if self.profiles.contains_key(&name) {
                return name;
            }
            log_message(
                1,
                &format!(
                    "ai-token-limit: profile '{}' not found; falling back to '{}'",
                    name, self.default_profile
                ),
            );
        }
        self.default_profile.clone()
    }

    fn bucket_key(&self, profile_name: &str, partition: &str) -> String {
        format!("{}:{}:{}", self.policy_name, profile_name, partition)
    }

    fn tokens_from_context(&self) -> u32 {
        let prompt = context_get("ai.prompt_tokens")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let completion = context_get("ai.completion_tokens")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);

        match self.count {
            CountMode::Prompt => prompt,
            CountMode::Completion => completion,
            CountMode::Total => prompt.saturating_add(completion),
        }
    }

    fn too_many_requests_response(
        &self,
        profile_name: &str,
        profile: &TokenProfile,
        result: &RateLimitResult,
    ) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        headers.insert(
            "ratelimit-policy".to_string(),
            format!(
                "{}-{};q={};w={}",
                self.policy_name, profile_name, profile.quota, profile.window
            ),
        );
        headers.insert(
            "ratelimit".to_string(),
            format!(
                "limit={}, remaining=0, reset={}",
                result.limit, result.reset
            ),
        );
        if let Some(retry_after) = result.retry_after {
            headers.insert("retry-after".to_string(), retry_after.to_string());
        }

        let body = serde_json::json!({
            "type": "urn:barbacane:error:ai-token-limit-exceeded",
            "title": "Too Many Requests",
            "status": 429,
            "detail": format!(
                "Token budget exhausted under profile '{}' (quota: {} tokens per {} seconds).",
                profile_name, profile.quota, profile.window
            ),
            "profile": profile_name,
        });

        Response {
            status: 429,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }
}

// ---------------------------------------------------------------------------
// Misconfiguration response (fail-closed)
// ---------------------------------------------------------------------------

/// 500 response returned when `default_profile` isn't in the `profiles` map.
/// Fail-closed: a rate-limit plugin that silently allows traffic on misconfig
/// is worse than one that errors loudly — operators catch the typo in CI /
/// first-request telemetry rather than weeks later when a bill arrives.
fn misconfig_response(default_profile: &str) -> Response {
    log_message(
        0,
        &format!(
            "ai-token-limit: default_profile '{}' not in profiles map; returning 500",
            default_profile
        ),
    );
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    let body = serde_json::json!({
        "type": "urn:barbacane:error:ai-token-limit-misconfigured",
        "title": "Internal Server Error",
        "status": 500,
        "detail": format!(
            "ai-token-limit default_profile '{}' does not exist in the profiles map; fix the plugin configuration.",
            default_profile
        ),
    });
    Response {
        status: 500,
        headers,
        body: Some(body.to_string().into_bytes()),
    }
}

// ---------------------------------------------------------------------------
// Partition-key extraction
// ---------------------------------------------------------------------------

fn extract_partition(req: &Request, source: &str) -> String {
    if source == "client_ip" {
        if let Some(v) = req
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        {
            return v;
        }
        if let Some(v) = req.headers.get("x-real-ip") {
            return v.clone();
        }
        if !req.client_ip.is_empty() {
            return req.client_ip.clone();
        }
        return "unknown".to_string();
    }

    if let Some(header_name) = source.strip_prefix("header:") {
        return req
            .headers
            .get(header_name)
            .or_else(|| req.headers.get(&header_name.to_lowercase()))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
    }

    if let Some(key) = source.strip_prefix("context:") {
        return context_get(key).unwrap_or_else(|| "unknown".to_string());
    }

    source.to_string()
}

/// `on_response` has no `Request` in scope, so the partition key can only be
/// resolved from context-based sources. Header/IP sources degrade to the
/// shared `"unknown"` bucket — acceptable under the advisory-only model.
fn partition_from_context_only(source: &str) -> String {
    if let Some(key) = source.strip_prefix("context:") {
        return context_get(key).unwrap_or_else(|| "unknown".to_string());
    }
    if source.starts_with("header:") || source == "client_ip" {
        return "unknown".to_string();
    }
    source.to_string()
}

// ---------------------------------------------------------------------------
// Host bindings
// ---------------------------------------------------------------------------

fn check_rate_limit(key: &str, quota: u32, window_secs: u32) -> Option<RateLimitResult> {
    let len = call_rate_limit_check(key, quota, window_secs);
    if len <= 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let read = call_rate_limit_read_result(&mut buf);
    if read <= 0 {
        return None;
    }
    serde_json::from_slice(&buf[..read as usize]).ok()
}

#[cfg(target_arch = "wasm32")]
fn call_rate_limit_check(key: &str, quota: u32, window_secs: u32) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_check(key_ptr: i32, key_len: i32, quota: u32, window_secs: u32) -> i32;
    }
    unsafe { host_rate_limit_check(key.as_ptr() as i32, key.len() as i32, quota, window_secs) }
}

#[cfg(target_arch = "wasm32")]
fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_rate_limit_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    }
    unsafe { host_rate_limit_read_result(buf.as_mut_ptr() as i32, buf.len() as i32) }
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
        let read = host_context_read_result(buf.as_mut_ptr() as i32, len);
        if read != len {
            return None;
        }
        String::from_utf8(buf).ok()
    }
}

#[cfg(target_arch = "wasm32")]
fn host_context_set(key: &str, value: &str) {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
    }
    unsafe {
        host_context_set(
            key.as_ptr() as i32,
            key.len() as i32,
            value.as_ptr() as i32,
            value.len() as i32,
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
// Native stubs (tests)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod mock_host {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        pub(crate) static BUDGETS: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
        pub(crate) static CONTEXT: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
        pub(crate) static UNAVAILABLE: RefCell<bool> = const { RefCell::new(false) };
    }

    #[cfg(test)]
    pub fn reset() {
        BUDGETS.with(|m| m.borrow_mut().clear());
        CONTEXT.with(|m| m.borrow_mut().clear());
        UNAVAILABLE.with(|u| *u.borrow_mut() = false);
    }

    #[cfg(test)]
    pub fn set_context(key: &str, value: &str) {
        CONTEXT.with(|m| m.borrow_mut().insert(key.into(), value.into()));
    }

    #[cfg(test)]
    pub fn set_rate_limiter_unavailable() {
        UNAVAILABLE.with(|u| *u.borrow_mut() = true);
    }

    #[cfg(test)]
    pub fn remaining(key: &str) -> Option<u32> {
        BUDGETS.with(|m| m.borrow().get(key).copied())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn call_rate_limit_check(key: &str, quota: u32, _window_secs: u32) -> i32 {
    use mock_host::*;
    if UNAVAILABLE.with(|u| *u.borrow()) {
        return -1;
    }
    let result_json = BUDGETS.with(|m| {
        let mut m = m.borrow_mut();
        let remaining = m.entry(key.to_string()).or_insert(quota);
        if *remaining == 0 {
            serde_json::json!({
                "allowed": false,
                "remaining": 0,
                "reset": 0,
                "limit": quota,
                "retry_after": 60,
            })
            .to_string()
        } else {
            *remaining -= 1;
            serde_json::json!({
                "allowed": true,
                "remaining": *remaining,
                "reset": 0,
                "limit": quota,
            })
            .to_string()
        }
    });
    LAST_RESULT.with(|r| *r.borrow_mut() = Some(result_json.into_bytes()));
    LAST_RESULT.with(|r| r.borrow().as_ref().map(|v| v.len() as i32).unwrap_or(-1))
}

#[cfg(not(target_arch = "wasm32"))]
fn call_rate_limit_read_result(buf: &mut [u8]) -> i32 {
    LAST_RESULT.with(|r| {
        if let Some(data) = r.borrow_mut().take() {
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            len as i32
        } else {
            -1
        }
    })
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static LAST_RESULT: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::CONTEXT.with(|m| m.borrow().get(key).cloned())
}

#[cfg(not(target_arch = "wasm32"))]
fn host_context_set(key: &str, value: &str) {
    mock_host::CONTEXT.with(|m| m.borrow_mut().insert(key.into(), value.into()));
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(_level: i32, _msg: &str) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::mock_host;
    use super::*;

    fn plugin(
        default_profile: &str,
        profiles: Vec<(&str, u32, u32)>,
        partition_key: &str,
        count: CountMode,
    ) -> AiTokenLimit {
        AiTokenLimit {
            context_key: "ai.policy".into(),
            default_profile: default_profile.into(),
            profiles: profiles
                .into_iter()
                .map(|(name, quota, window)| (name.to_string(), TokenProfile { quota, window }))
                .collect(),
            policy_name: "ai-tokens".into(),
            partition_key: partition_key.into(),
            count,
        }
    }

    fn simple(quota: u32, window: u32) -> AiTokenLimit {
        plugin(
            "default",
            vec![("default", quota, window)],
            "context:auth.sub",
            CountMode::Total,
        )
    }

    fn make_request() -> Request {
        Request {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
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
                "standard": { "quota": 10000, "window": 60 },
                "premium":  { "quota": 100000, "window": 60 },
                "trial":    { "quota": 1000, "window": 3600 }
            },
            "partition_key": "context:auth.sub"
        }"#;
        let cfg: AiTokenLimit = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.default_profile, "standard");
        assert_eq!(cfg.profiles.len(), 3);
        assert_eq!(cfg.profiles["premium"].quota, 100000);
        assert_eq!(cfg.profiles["trial"].window, 3600);
        assert_eq!(cfg.partition_key, "context:auth.sub");
        assert_eq!(cfg.policy_name, "ai-tokens");
        assert_eq!(cfg.context_key, "ai.policy");
        assert_eq!(cfg.count, CountMode::Total);
    }

    #[test]
    fn config_count_variants() {
        for variant in ["prompt", "completion", "total"] {
            let cfg: AiTokenLimit = serde_json::from_str(&format!(
                r#"{{"default_profile":"d","profiles":{{"d":{{"quota":1,"window":60}}}},"count":"{}"}}"#,
                variant
            ))
            .expect("parse");
            let expected = match variant {
                "prompt" => CountMode::Prompt,
                "completion" => CountMode::Completion,
                _ => CountMode::Total,
            };
            assert_eq!(cfg.count, expected);
        }
    }

    #[test]
    fn config_rejects_missing_required_fields() {
        assert!(serde_json::from_str::<AiTokenLimit>(r#"{"profiles":{}}"#).is_err());
        assert!(serde_json::from_str::<AiTokenLimit>(r#"{"default_profile":"d"}"#).is_err());
        // Profile missing quota
        assert!(serde_json::from_str::<AiTokenLimit>(
            r#"{"default_profile":"d","profiles":{"d":{"window":60}}}"#
        )
        .is_err());
    }

    // =======================================================================
    // Profile selection
    // =======================================================================

    #[test]
    fn falls_back_to_default_when_context_key_absent() {
        mock_host::reset();
        let p = simple(100, 60);
        let (name, _) = p.resolve_profile().expect("resolved");
        assert_eq!(name, "default");
    }

    #[test]
    fn uses_profile_named_by_context_key() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "premium");
        let p = plugin(
            "default",
            vec![("default", 10, 60), ("premium", 1000, 60)],
            "context:auth.sub",
            CountMode::Total,
        );
        let (name, profile) = p.resolve_profile().expect("resolved");
        assert_eq!(name, "premium");
        assert_eq!(profile.quota, 1000);
    }

    #[test]
    fn falls_back_to_default_when_context_names_unknown_profile() {
        mock_host::reset();
        mock_host::set_context("ai.policy", "ghost");
        let p = plugin(
            "default",
            vec![("default", 10, 60)],
            "context:auth.sub",
            CountMode::Total,
        );
        let (name, _) = p.resolve_profile().expect("resolved");
        assert_eq!(name, "default");
    }

    // =======================================================================
    // on_request enforcement
    // =======================================================================

    #[test]
    fn on_request_continues_within_budget() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        let mut p = simple(100, 60);
        assert!(matches!(p.on_request(make_request()), Action::Continue(_)));
    }

    #[test]
    fn on_request_fails_open_when_limiter_unavailable() {
        mock_host::reset();
        mock_host::set_rate_limiter_unavailable();
        let mut p = simple(100, 60);
        assert!(matches!(p.on_request(make_request()), Action::Continue(_)));
    }

    #[test]
    fn on_request_blocks_when_budget_exhausted() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        let mut p = simple(1, 60);

        assert!(matches!(p.on_request(make_request()), Action::Continue(_)));

        match p.on_request(make_request()) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 429);
                let body = String::from_utf8(resp.body.expect("body")).expect("utf8");
                assert!(body.contains("urn:barbacane:error:ai-token-limit-exceeded"));
                assert!(body.contains("\"profile\":\"default\""));
                assert_eq!(
                    resp.headers.get("ratelimit-policy").map(|s| s.as_str()),
                    Some("ai-tokens-default;q=1;w=60")
                );
                assert!(resp.headers.contains_key("ratelimit"));
                assert!(resp.headers.contains_key("retry-after"));
            }
            _ => panic!("expected 429"),
        }
    }

    #[test]
    fn misconfigured_default_profile_fails_closed_with_500() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        let mut p = plugin(
            "missing",
            vec![("other", 10, 60)],
            "context:auth.sub",
            CountMode::Total,
        );
        // Fail-closed: a rate limiter that silently lets traffic through on
        // an operator typo is worse than a loud 500.
        match p.on_request(make_request()) {
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 500);
                let body = String::from_utf8(resp.body.expect("body")).expect("utf8");
                assert!(body.contains("urn:barbacane:error:ai-token-limit-misconfigured"));
                assert!(body.contains("'missing'"));
            }
            _ => panic!("expected 500 short-circuit on misconfig"),
        }
    }

    // =======================================================================
    // Profile separation
    // =======================================================================

    #[test]
    fn different_profiles_use_distinct_buckets() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");

        let mut p = plugin(
            "default",
            vec![("default", 5, 60), ("premium", 1000, 60)],
            "context:auth.sub",
            CountMode::Total,
        );

        // Default bucket charged once
        let _ = p.on_request(make_request());
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            4
        );

        // Switch profile — premium bucket is separate
        mock_host::set_context("ai.policy", "premium");
        let _ = p.on_request(make_request());
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            4
        );
        assert_eq!(
            mock_host::remaining("ai-tokens:premium:alice").expect("bucket"),
            999
        );
    }

    #[test]
    fn per_consumer_buckets_within_same_profile() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        let mut p = simple(5, 60);
        let _ = p.on_request(make_request());
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            4
        );

        mock_host::set_context("auth.sub", "bob");
        let _ = p.on_request(make_request());
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            4
        );
        assert_eq!(
            mock_host::remaining("ai-tokens:default:bob").expect("bucket"),
            4
        );
    }

    // =======================================================================
    // on_response charging
    // =======================================================================

    #[test]
    fn on_response_charges_tokens_against_selected_profile() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        mock_host::set_context("ai.policy", "premium");
        mock_host::set_context("ai.prompt_tokens", "20");
        mock_host::set_context("ai.completion_tokens", "80");

        let mut p = plugin(
            "default",
            vec![("default", 100, 60), ("premium", 10000, 60)],
            "context:auth.sub",
            CountMode::Total,
        );
        let _ = p.on_request(make_request());
        let _ = p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });

        assert_eq!(
            mock_host::remaining("ai-tokens:premium:alice").expect("bucket"),
            10000 - 100
        );
    }

    #[test]
    fn on_response_count_prompt_only() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        mock_host::set_context("ai.prompt_tokens", "30");
        mock_host::set_context("ai.completion_tokens", "70");
        let mut p = plugin(
            "default",
            vec![("default", 1000, 60)],
            "context:auth.sub",
            CountMode::Prompt,
        );
        let _ = p.on_request(make_request());
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            1000 - 30
        );
    }

    #[test]
    fn on_response_count_completion_only() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        mock_host::set_context("ai.prompt_tokens", "30");
        mock_host::set_context("ai.completion_tokens", "70");
        let mut p = plugin(
            "default",
            vec![("default", 1000, 60)],
            "context:auth.sub",
            CountMode::Completion,
        );
        let _ = p.on_request(make_request());
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            1000 - 70
        );
    }

    #[test]
    fn on_response_without_token_context_is_noop() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        let mut p = simple(100, 60);
        let _ = p.on_request(make_request());
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            99
        );
    }

    #[test]
    fn on_response_stops_charging_once_saturated() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        mock_host::set_context("ai.prompt_tokens", "500");
        mock_host::set_context("ai.completion_tokens", "500");
        let mut p = simple(5, 60);
        let _ = p.on_request(make_request());
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert_eq!(
            mock_host::remaining("ai-tokens:default:alice").expect("bucket"),
            0
        );
    }

    #[test]
    fn on_response_noop_when_default_profile_missing() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "alice");
        mock_host::set_context("ai.prompt_tokens", "10");
        let mut p = plugin(
            "missing",
            vec![("other", 100, 60)],
            "context:auth.sub",
            CountMode::Total,
        );
        // No panic, no bucket created.
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });
        assert!(mock_host::remaining("ai-tokens:other:alice").is_none());
    }

    // =======================================================================
    // Partition persistence (regression: on_response must charge the same
    // bucket on_request charged, regardless of partition source)
    // =======================================================================

    #[test]
    fn partition_persists_from_on_request_to_on_response_for_client_ip() {
        // Regression: `partition_key: client_ip` used to degrade to the
        // shared "unknown" bucket on_response. The persisted context key
        // now keeps the same consumer bucket across both phases.
        mock_host::reset();
        mock_host::set_context("ai.prompt_tokens", "50");
        mock_host::set_context("ai.completion_tokens", "50");

        let mut p = plugin(
            "default",
            vec![("default", 1000, 60)],
            "client_ip",
            CountMode::Total,
        );
        let mut req = make_request();
        req.client_ip = "203.0.113.9".into();

        let _ = p.on_request(req);
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });

        // All 100 tokens charged to the IP's bucket, not to "unknown".
        assert_eq!(
            mock_host::remaining("ai-tokens:default:203.0.113.9").expect("ip bucket"),
            1000 - 100
        );
        assert!(
            mock_host::remaining("ai-tokens:default:unknown").is_none(),
            "no charges should leak to the shared 'unknown' bucket"
        );
    }

    #[test]
    fn partition_persists_for_header_source() {
        mock_host::reset();
        mock_host::set_context("ai.prompt_tokens", "40");
        mock_host::set_context("ai.completion_tokens", "60");

        let mut p = plugin(
            "default",
            vec![("default", 1000, 60)],
            "header:x-api-key",
            CountMode::Total,
        );
        let mut req = make_request();
        req.headers.insert("x-api-key".into(), "abc123".into());

        let _ = p.on_request(req);
        p.on_response(Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        });

        assert_eq!(
            mock_host::remaining("ai-tokens:default:abc123").expect("header bucket"),
            1000 - 100
        );
        assert!(mock_host::remaining("ai-tokens:default:unknown").is_none());
    }

    #[test]
    fn partition_context_key_scoped_by_policy_name() {
        // Two stacked instances with distinct policy_names must not
        // overwrite each other's persisted partition.
        let mut p1 = plugin(
            "default",
            vec![("default", 10, 60)],
            "client_ip",
            CountMode::Total,
        );
        p1.policy_name = "minute".into();
        let mut p2 = plugin(
            "default",
            vec![("default", 10, 3600)],
            "client_ip",
            CountMode::Total,
        );
        p2.policy_name = "hour".into();

        assert_ne!(p1.partition_context_key(), p2.partition_context_key());
    }

    // =======================================================================
    // Partition extraction
    // =======================================================================

    #[test]
    fn partition_from_client_ip_forwarded_for() {
        let mut req = make_request();
        req.headers
            .insert("x-forwarded-for".into(), "1.2.3.4, 5.6.7.8".into());
        assert_eq!(extract_partition(&req, "client_ip"), "1.2.3.4");
    }

    #[test]
    fn partition_from_client_ip_real_ip() {
        let mut req = make_request();
        req.headers.insert("x-real-ip".into(), "9.9.9.9".into());
        assert_eq!(extract_partition(&req, "client_ip"), "9.9.9.9");
    }

    #[test]
    fn partition_from_client_ip_fallback_field() {
        let req = make_request();
        assert_eq!(extract_partition(&req, "client_ip"), "127.0.0.1");
    }

    #[test]
    fn partition_from_header() {
        let mut req = make_request();
        req.headers.insert("x-api-key".into(), "abc123".into());
        assert_eq!(extract_partition(&req, "header:x-api-key"), "abc123");
    }

    #[test]
    fn partition_from_context() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "bob");
        let req = make_request();
        assert_eq!(extract_partition(&req, "context:auth.sub"), "bob");
    }

    #[test]
    fn partition_literal() {
        let req = make_request();
        assert_eq!(extract_partition(&req, "global"), "global");
    }

    #[test]
    fn partition_context_missing_defaults_to_unknown() {
        mock_host::reset();
        let req = make_request();
        assert_eq!(extract_partition(&req, "context:missing"), "unknown");
    }

    #[test]
    fn partition_from_context_only_handles_all_sources() {
        mock_host::reset();
        mock_host::set_context("auth.sub", "bob");
        assert_eq!(partition_from_context_only("context:auth.sub"), "bob");
        assert_eq!(partition_from_context_only("client_ip"), "unknown");
        assert_eq!(partition_from_context_only("header:x-api-key"), "unknown");
        assert_eq!(partition_from_context_only("literal"), "literal");
    }
}
