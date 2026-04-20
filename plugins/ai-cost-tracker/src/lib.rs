//! AI cost-tracker middleware plugin for Barbacane API gateway (ADR-0024).
//!
//! Records per-request LLM cost in USD based on the tokens reported by the
//! `ai-proxy` dispatcher (context keys `ai.provider`, `ai.model`,
//! `ai.prompt_tokens`, `ai.completion_tokens`) and a configurable price table.
//! Emits the Prometheus counter `cost_dollars` labelled by provider and model;
//! the host auto-prefixes it as `barbacane_plugin_ai_cost_tracker_cost_dollars`.
//!
//! Prices are expressed in USD per 1,000 tokens — the industry-standard
//! notation used by OpenAI, Anthropic, and most vendors.

use barbacane_plugin_sdk::prelude::*;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Per-model price entry.
#[derive(Deserialize, Default, Clone, Debug)]
struct ModelPrice {
    #[serde(default)]
    prompt: f64,
    #[serde(default)]
    completion: f64,
}

/// AI cost-tracker middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct AiCostTracker {
    /// `provider/model` → price entry (USD per 1,000 tokens).
    prices: BTreeMap<String, ModelPrice>,

    #[serde(default = "default_warn_unknown_model")]
    warn_unknown_model: bool,
}

fn default_warn_unknown_model() -> bool {
    true
}

impl AiCostTracker {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        Action::Continue(req)
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        let Some(provider) = context_get("ai.provider") else {
            return resp;
        };
        let Some(model) = context_get("ai.model") else {
            return resp;
        };

        let key = format!("{}/{}", provider, model);
        let Some(price) = self.prices.get(&key) else {
            if self.warn_unknown_model {
                log_message(
                    1,
                    &format!("ai-cost-tracker: no price configured for '{}'", key),
                );
            }
            return resp;
        };

        let prompt_tokens = context_get("ai.prompt_tokens")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let completion_tokens = context_get("ai.completion_tokens")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        if prompt_tokens == 0 && completion_tokens == 0 {
            return resp;
        }

        let cost = compute_cost(prompt_tokens, completion_tokens, price);
        if cost <= 0.0 {
            return resp;
        }

        let labels = labels_provider_model(&provider, &model);
        metric_counter_add("cost_dollars", &labels, cost);

        resp
    }
}

// ---------------------------------------------------------------------------
// Pricing math
// ---------------------------------------------------------------------------

/// Cost in USD = (prompt / 1000) * price.prompt + (completion / 1000) * price.completion
fn compute_cost(prompt_tokens: u64, completion_tokens: u64, price: &ModelPrice) -> f64 {
    (prompt_tokens as f64 / 1000.0) * price.prompt
        + (completion_tokens as f64 / 1000.0) * price.completion
}

// ---------------------------------------------------------------------------
// Labels helper
// ---------------------------------------------------------------------------

fn labels_provider_model(provider: &str, model: &str) -> String {
    format!(
        "{{\"provider\":\"{}\",\"model\":\"{}\"}}",
        escape_label(provider),
        escape_label(model)
    )
}

fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
fn metric_counter_add(name: &str, labels_json: &str, value: f64) {
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
            value,
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
        pub(crate) static COUNTERS: RefCell<Vec<(String, String, f64)>> = const { RefCell::new(Vec::new()) };
        pub(crate) static LOGS: RefCell<Vec<(i32, String)>> = const { RefCell::new(Vec::new()) };
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|m| m.borrow_mut().clear());
        COUNTERS.with(|m| m.borrow_mut().clear());
        LOGS.with(|m| m.borrow_mut().clear());
    }

    #[cfg(test)]
    pub fn set_context(k: &str, v: &str) {
        CONTEXT.with(|m| m.borrow_mut().insert(k.into(), v.into()));
    }

    #[cfg(test)]
    pub fn counters() -> Vec<(String, String, f64)> {
        COUNTERS.with(|m| m.borrow().clone())
    }

    #[cfg(test)]
    pub fn logs() -> Vec<(i32, String)> {
        LOGS.with(|m| m.borrow().clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn context_get(key: &str) -> Option<String> {
    mock_host::CONTEXT.with(|m| m.borrow().get(key).cloned())
}

#[cfg(not(target_arch = "wasm32"))]
fn metric_counter_add(name: &str, labels: &str, value: f64) {
    mock_host::COUNTERS.with(|m| {
        m.borrow_mut()
            .push((name.to_string(), labels.to_string(), value))
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn log_message(level: i32, msg: &str) {
    mock_host::LOGS.with(|m| m.borrow_mut().push((level, msg.to_string())));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plugin(prices: &[(&str, f64, f64)]) -> AiCostTracker {
        let map = prices
            .iter()
            .map(|(k, p, c)| {
                (
                    k.to_string(),
                    ModelPrice {
                        prompt: *p,
                        completion: *c,
                    },
                )
            })
            .collect();
        AiCostTracker {
            prices: map,
            warn_unknown_model: true,
        }
    }

    fn resp() -> Response {
        Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        }
    }

    // --- Config ---

    #[test]
    fn config_parses() {
        let json = r#"{
            "prices": {
                "openai/gpt-4o": {"prompt": 0.0025, "completion": 0.01},
                "anthropic/claude-opus-4-6": {"prompt": 0.015, "completion": 0.075}
            }
        }"#;
        let cfg: AiCostTracker = serde_json::from_str(json).expect("parse");
        assert_eq!(cfg.prices.len(), 2);
        assert_eq!(cfg.prices["openai/gpt-4o"].prompt, 0.0025);
        assert_eq!(cfg.prices["anthropic/claude-opus-4-6"].completion, 0.075);
        assert!(cfg.warn_unknown_model);
    }

    #[test]
    fn config_requires_prices() {
        let result: Result<AiCostTracker, _> = serde_json::from_str("{}");
        assert!(result.is_err());
    }

    // --- compute_cost ---

    #[test]
    fn compute_cost_basic() {
        let price = ModelPrice {
            prompt: 0.0025,
            completion: 0.01,
        };
        // 1000 prompt + 1000 completion tokens → 0.0025 + 0.01 = 0.0125
        assert!((compute_cost(1000, 1000, &price) - 0.0125).abs() < 1e-9);
    }

    #[test]
    fn compute_cost_zero_for_free_model() {
        let price = ModelPrice {
            prompt: 0.0,
            completion: 0.0,
        };
        assert_eq!(compute_cost(100_000, 100_000, &price), 0.0);
    }

    // --- on_response: happy path emits metric ---

    #[test]
    fn on_response_emits_cost_metric() {
        mock_host::reset();
        mock_host::set_context("ai.provider", "openai");
        mock_host::set_context("ai.model", "gpt-4o");
        mock_host::set_context("ai.prompt_tokens", "2000");
        mock_host::set_context("ai.completion_tokens", "500");

        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        p.on_response(resp());

        let counters = mock_host::counters();
        assert_eq!(counters.len(), 1);
        let (name, labels, value) = &counters[0];
        assert_eq!(name, "cost_dollars");
        assert!(labels.contains("\"provider\":\"openai\""));
        assert!(labels.contains("\"model\":\"gpt-4o\""));
        // 2000/1000 * 0.0025 + 500/1000 * 0.01 = 0.005 + 0.005 = 0.01
        assert!((value - 0.01).abs() < 1e-9);
    }

    #[test]
    fn on_response_noop_without_provider_context() {
        mock_host::reset();
        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        p.on_response(resp());
        assert!(mock_host::counters().is_empty());
    }

    #[test]
    fn on_response_noop_without_model_context() {
        mock_host::reset();
        mock_host::set_context("ai.provider", "openai");
        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        p.on_response(resp());
        assert!(mock_host::counters().is_empty());
    }

    #[test]
    fn on_response_unknown_model_is_noop_with_warning() {
        mock_host::reset();
        mock_host::set_context("ai.provider", "openai");
        mock_host::set_context("ai.model", "gpt-5-turbo");
        mock_host::set_context("ai.prompt_tokens", "100");
        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        p.on_response(resp());
        assert!(mock_host::counters().is_empty());
        let logs = mock_host::logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].1.contains("openai/gpt-5-turbo"));
    }

    #[test]
    fn on_response_unknown_model_warning_can_be_suppressed() {
        mock_host::reset();
        mock_host::set_context("ai.provider", "openai");
        mock_host::set_context("ai.model", "gpt-5-turbo");
        mock_host::set_context("ai.prompt_tokens", "100");
        let mut p = AiCostTracker {
            prices: BTreeMap::new(),
            warn_unknown_model: false,
        };
        p.on_response(resp());
        assert!(mock_host::logs().is_empty());
    }

    #[test]
    fn on_response_noop_when_tokens_missing() {
        mock_host::reset();
        mock_host::set_context("ai.provider", "openai");
        mock_host::set_context("ai.model", "gpt-4o");
        // No token context (streamed response case).
        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        p.on_response(resp());
        assert!(mock_host::counters().is_empty());
    }

    #[test]
    fn on_response_noop_when_free_model_tokens_set() {
        // Ollama with zero-priced model: still a no-op, no metric emitted.
        mock_host::reset();
        mock_host::set_context("ai.provider", "ollama");
        mock_host::set_context("ai.model", "mistral");
        mock_host::set_context("ai.prompt_tokens", "100");
        mock_host::set_context("ai.completion_tokens", "200");
        let mut p = make_plugin(&[("ollama/mistral", 0.0, 0.0)]);
        p.on_response(resp());
        assert!(mock_host::counters().is_empty());
    }

    // --- on_request passthrough ---

    #[test]
    fn on_request_is_passthrough() {
        let mut p = make_plugin(&[("openai/gpt-4o", 0.0025, 0.01)]);
        let req = Request {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
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

    // --- Label escaping ---

    #[test]
    fn labels_escape_quotes_and_backslashes() {
        let labels = labels_provider_model("a\"b", "c\\d");
        assert_eq!(labels, r#"{"provider":"a\"b","model":"c\\d"}"#);
    }
}
