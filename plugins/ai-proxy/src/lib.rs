//! AI proxy dispatcher plugin for Barbacane API gateway.
//!
//! Exposes a unified OpenAI-compatible API to clients and routes requests to
//! LLM providers (OpenAI, Anthropic, Ollama). Key features:
//!
//! - **Provider abstraction** — clients always send OpenAI format; the plugin
//!   translates for non-OpenAI providers (Anthropic Messages API).
//! - **Named targets** — operators define named provider profiles (`targets`);
//!   the `cel` middleware selects a target by writing `ai.target` into context.
//! - **Provider fallback** — on 5xx or connection failure, the plugin retries
//!   the next provider in the `fallback` chain before returning 502.
//! - **Token propagation** — after dispatch, `ai.provider`, `ai.model`,
//!   `ai.prompt_tokens`, and `ai.completion_tokens` are written into context
//!   for downstream middlewares (`ai-token-limit`, `ai-cost-tracker`).
//! - **Streaming** — OpenAI-compatible providers use `host_http_stream`; for
//!   Anthropic, streaming is forced non-streaming in this version (buffered
//!   response is returned; see ADR-0024 for the planned SSE translation).

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Provider type
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Provider {
    OpenAI,
    Anthropic,
    Ollama,
}

impl Provider {
    fn name(&self) -> &'static str {
        match self {
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
        }
    }

    fn default_base_url(&self) -> &'static str {
        match self {
            Provider::OpenAI => "https://api.openai.com",
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::Ollama => "http://localhost:11434",
        }
    }

    fn is_openai_compatible(&self) -> bool {
        matches!(self, Provider::OpenAI | Provider::Ollama)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// A single named provider target (provider + model + credentials).
#[derive(Deserialize, Clone)]
struct TargetConfig {
    provider: Provider,
    model: String,
    #[serde(default)]
    api_key: Option<String>,
    /// Custom base URL (Azure, self-hosted, Ollama remote, etc.).
    #[serde(default)]
    base_url: Option<String>,
}

impl TargetConfig {
    fn effective_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.provider.default_base_url())
    }
}

fn default_timeout() -> u64 {
    120
}

/// AI proxy dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct AiProxy {
    // --- Flat single-provider config (used when no `targets` map is defined) ---
    #[serde(default)]
    provider: Option<Provider>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    base_url: Option<String>,

    /// Request timeout in seconds. LLM calls can be slow; default is 120s.
    #[serde(default = "default_timeout")]
    timeout: u64,

    /// Default `max_tokens` applied when the client request omits it.
    #[serde(default)]
    max_tokens: Option<u32>,

    /// Provider fallback chain. Tried in order when the primary target returns
    /// a 5xx or a connection error. 4xx responses are returned directly.
    #[serde(default)]
    fallback: Vec<TargetConfig>,

    /// Named provider targets for policy-driven routing. The `cel` middleware
    /// selects a target by writing `ai.target` into the request context before
    /// this dispatcher runs.
    #[serde(default)]
    targets: BTreeMap<String, TargetConfig>,

    /// Target name to use when no `ai.target` context key is present.
    #[serde(default)]
    default_target: Option<String>,
}

// ---------------------------------------------------------------------------
// Wire types for host_http_call / host_http_stream
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// Anthropic wire types (for request/response translation)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

// ---------------------------------------------------------------------------
// Dispatcher implementation
// ---------------------------------------------------------------------------

impl AiProxy {
    pub fn dispatch(&mut self, req: Request) -> Response {
        let start_ms = host::time_now_ms();

        let primary = match self.resolve_target() {
            Some(t) => t,
            None => {
                return error_response(
                    500,
                    "ai-proxy misconfiguration: no provider configured (set `provider` or define `targets`)",
                )
            }
        };

        let streaming = is_streaming_request(&req.body);

        // Build the full try list: primary + fallback chain
        let mut targets: Vec<TargetConfig> = vec![primary];
        targets.extend(self.fallback.iter().cloned());

        let mut last_err = String::from("all providers failed");

        for (idx, target) in targets.iter().enumerate() {
            if idx > 0 {
                let prev = &targets[idx - 1];
                host::metric_counter_inc(
                    "fallback_total",
                    &labels2("from_provider", prev.provider.name(), "to_provider", target.provider.name()),
                    1,
                );
                host::log_warn(&format!(
                    "ai-proxy: falling back from {} to {}",
                    prev.provider.name(),
                    target.provider.name()
                ));
            }

            match self.try_dispatch(target, &req, streaming) {
                Ok(resp) => {
                    let elapsed_ms = host::time_now_ms().saturating_sub(start_ms);

                    // Streamed responses have status=0 — treat as success
                    let metric_status = if resp.status == 0 { 200 } else { resp.status };

                    host::metric_counter_inc(
                        "requests_total",
                        &labels2("provider", target.provider.name(), "status", &metric_status.to_string()),
                        1,
                    );
                    host::metric_histogram_observe(
                        "request_duration_seconds",
                        &labels1("provider", target.provider.name()),
                        elapsed_ms as f64 / 1000.0,
                    );

                    // Retry on 5xx (but not streamed responses — they already sent data)
                    if resp.status >= 500 && idx + 1 < targets.len() {
                        last_err = format!(
                            "provider {} returned {}",
                            target.provider.name(),
                            resp.status
                        );
                        continue;
                    }

                    // Propagate context: provider/model/tokens
                    propagate_context(target, &resp);

                    return resp;
                }
                Err(e) => {
                    last_err = e;
                    // Connection error — try next in chain
                }
            }
        }

        error_response(502, &format!("ai-proxy: {}", last_err))
    }

    /// Resolve the active target using the priority chain defined in ADR-0024:
    /// 1. `ai.target` context key (set by upstream middleware, e.g. `cel`)
    /// 2. `default_target` name
    /// 3. Flat `provider`/`model` config
    fn resolve_target(&self) -> Option<TargetConfig> {
        // 1. Context-set target name
        if let Some(name) = host::context_get("ai.target") {
            if let Some(t) = self.targets.get(&name) {
                return Some(t.clone());
            }
            host::log_warn(&format!(
                "ai-proxy: ai.target '{}' not found in targets map; falling through",
                name
            ));
        }

        // 2. Default target
        if let Some(ref name) = self.default_target {
            if let Some(t) = self.targets.get(name) {
                return Some(t.clone());
            }
        }

        // 3. Flat config
        self.provider.as_ref().map(|p| TargetConfig {
            provider: p.clone(),
            model: self.model.clone().unwrap_or_default(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
        })
    }

    fn try_dispatch(&self, target: &TargetConfig, req: &Request, streaming: bool) -> Result<Response, String> {
        if target.provider.is_openai_compatible() {
            if streaming {
                self.openai_stream(target, req)
            } else {
                self.openai_call(target, req)
            }
        } else {
            // Anthropic: ADR-0024 SSE translation is future work; buffer the response.
            if streaming {
                host::log_warn(
                    "ai-proxy: Anthropic streaming not yet supported; buffering response",
                );
            }
            self.anthropic_call(target, req, false)
        }
    }

    // --- OpenAI-compatible (passthrough) ---

    fn openai_call(&self, target: &TargetConfig, req: &Request) -> Result<Response, String> {
        let url = openai_url(target, &req.path);
        let headers = openai_headers(target);

        let body = self.maybe_inject_max_tokens(&req.body);

        let http_req = HttpRequest {
            method: req.method.clone(),
            url,
            headers,
            body,
            timeout_ms: Some(self.timeout * 1000),
        };

        let resp_bytes = http_call(&http_req)?;
        Ok(build_response(resp_bytes))
    }

    fn openai_stream(&self, target: &TargetConfig, req: &Request) -> Result<Response, String> {
        let url = openai_url(target, &req.path);
        let mut headers = openai_headers(target);
        // Ensure Accept header for SSE
        headers.insert("accept".to_string(), "text/event-stream".to_string());

        let body = self.maybe_inject_max_tokens(&req.body);

        let http_req = HttpRequest {
            method: req.method.clone(),
            url,
            headers,
            body,
            timeout_ms: Some(self.timeout * 1000),
        };

        let req_json = serde_json::to_vec(&http_req).map_err(|e| e.to_string())?;
        let result = unsafe { host_http_stream(req_json.as_ptr() as i32, req_json.len() as i32) };

        if result < 0 {
            return Err("upstream stream failed".to_string());
        }

        Ok(streamed_response())
    }

    // --- Anthropic (with request/response translation) ---

    fn anthropic_call(
        &self,
        target: &TargetConfig,
        req: &Request,
        stream: bool,
    ) -> Result<Response, String> {
        let base = target.effective_base_url().trim_end_matches('/');
        let url = format!("{}/v1/messages", base);

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("anthropic-version".to_string(), "2024-10-22".to_string());
        if let Some(key) = &target.api_key {
            headers.insert("x-api-key".to_string(), key.clone());
        }

        let body = translate_to_anthropic(&req.body, &target.model, stream, self.max_tokens)?;

        let http_req = HttpRequest {
            method: "POST".to_string(),
            url,
            headers,
            body: Some(body),
            timeout_ms: Some(self.timeout * 1000),
        };

        let resp_bytes = http_call(&http_req)?;
        let resp = build_response(resp_bytes);

        // Only translate 2xx responses; pass error responses through as-is
        if resp.status >= 200 && resp.status < 300 {
            let translated_body = resp
                .body
                .as_deref()
                .map(translate_from_anthropic)
                .transpose()?;
            Ok(Response {
                status: resp.status,
                headers: resp.headers,
                body: translated_body,
            })
        } else {
            Ok(resp)
        }
    }

    /// Inject a default `max_tokens` into the request body when the client
    /// didn't send one — required for Anthropic (field is mandatory) and
    /// useful as a cost guardrail for OpenAI.
    fn maybe_inject_max_tokens(&self, body: &Option<String>) -> Option<String> {
        let Some(max) = self.max_tokens else {
            return body.clone();
        };
        let Some(raw) = body.as_deref() else {
            return body.clone();
        };
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(raw) else {
            return body.clone();
        };
        if let Some(obj) = v.as_object_mut() {
            if !obj.contains_key("max_tokens") {
                obj.insert("max_tokens".to_string(), serde_json::json!(max));
                return Some(v.to_string());
            }
        }
        body.clone()
    }
}

// ---------------------------------------------------------------------------
// Translation helpers
// ---------------------------------------------------------------------------

/// Translate an OpenAI chat completion request body to Anthropic Messages API format.
/// Pinned to Anthropic API version 2024-10-22 (ADR-0024).
fn translate_to_anthropic(
    body: &Option<String>,
    model: &str,
    stream: bool,
    default_max_tokens: Option<u32>,
) -> Result<String, String> {
    let raw = body.as_deref().unwrap_or("{}");
    let openai: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid request body: {}", e))?;

    let messages = openai["messages"]
        .as_array()
        .ok_or("missing or invalid messages array")?;

    // Split system messages out; Anthropic takes them as a top-level field
    let mut system_parts: Vec<&str> = Vec::new();
    let mut chat_messages: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        if msg["role"].as_str() == Some("system") {
            if let Some(content) = msg["content"].as_str() {
                system_parts.push(content);
            }
        } else {
            chat_messages.push(msg.clone());
        }
    }

    let max_tokens = openai["max_tokens"]
        .as_u64()
        .map(|v| v as u32)
        .or(default_max_tokens)
        .unwrap_or(4096);

    let anthropic = AnthropicRequest {
        model: openai["model"]
            .as_str()
            .unwrap_or(model)
            .to_string(),
        messages: chat_messages,
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n"))
        },
        max_tokens,
        temperature: openai["temperature"].as_f64(),
        top_p: openai["top_p"].as_f64(),
        stream: if stream { Some(true) } else { None },
    };

    serde_json::to_string(&anthropic).map_err(|e| e.to_string())
}

/// Translate an Anthropic Messages API response body to OpenAI chat completion format.
/// Pinned to Anthropic API version 2024-10-22 (ADR-0024).
fn translate_from_anthropic(body: &str) -> Result<String, String> {
    let anthropic: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid Anthropic response: {}", e))?;

    // Extract text content from the first text block
    let content_text = anthropic["content"]
        .as_array()
        .and_then(|arr| arr.iter().find(|c| c["type"].as_str() == Some("text")))
        .and_then(|c| c["text"].as_str())
        .unwrap_or("");

    let input_tokens = anthropic["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = anthropic["usage"]["output_tokens"].as_u64().unwrap_or(0);

    // Map Anthropic stop reason to OpenAI finish_reason
    let finish_reason = match anthropic["stop_reason"].as_str() {
        Some("end_turn") => "stop",
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    };

    let openai = serde_json::json!({
        "id": anthropic["id"],
        "object": "chat.completion",
        "model": anthropic["model"],
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": content_text
            },
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    });

    serde_json::to_string(&openai).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Context propagation and metrics
// ---------------------------------------------------------------------------

/// Write AI context keys so downstream middlewares can read them.
/// For streamed responses (status=0), token counts are unavailable.
fn propagate_context(target: &TargetConfig, resp: &Response) {
    host::context_set("ai.provider", target.provider.name());
    host::context_set("ai.model", &target.model);

    // status=0 means streamed — token counts not available
    if resp.status == 0 {
        return;
    }

    if let Some(tokens) = extract_tokens(resp.body.as_deref()) {
        let prompt = tokens.0.to_string();
        let completion = tokens.1.to_string();

        host::context_set("ai.prompt_tokens", &prompt);
        host::context_set("ai.completion_tokens", &completion);

        host::metric_counter_inc(
            "tokens_total",
            &labels2("provider", target.provider.name(), "type", "prompt"),
            tokens.0,
        );
        host::metric_counter_inc(
            "tokens_total",
            &labels2("provider", target.provider.name(), "type", "completion"),
            tokens.1,
        );
    }
}

/// Extract (prompt_tokens, completion_tokens) from an OpenAI-format response body.
fn extract_tokens(body: Option<&str>) -> Option<(u64, u64)> {
    let body = body?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let prompt = v["usage"]["prompt_tokens"].as_u64()?;
    let completion = v["usage"]["completion_tokens"].as_u64()?;
    Some((prompt, completion))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn openai_url(target: &TargetConfig, req_path: &str) -> String {
    let base = target.effective_base_url().trim_end_matches('/');
    format!("{}{}", base, req_path)
}

fn openai_headers(target: &TargetConfig) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    if let Some(key) = &target.api_key {
        headers.insert("authorization".to_string(), format!("Bearer {}", key));
    }
    headers
}

fn is_streaming_request(body: &Option<String>) -> bool {
    body.as_ref()
        .and_then(|b| serde_json::from_str::<serde_json::Value>(b).ok())
        .and_then(|v| v["stream"].as_bool())
        .unwrap_or(false)
}

fn http_call(req: &HttpRequest) -> Result<HttpResponse, String> {
    let req_json = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    let result_len =
        unsafe { host_http_call(req_json.as_ptr() as i32, req_json.len() as i32) };

    if result_len < 0 {
        return Err("upstream connection failed".to_string());
    }

    let mut buf = vec![0u8; result_len as usize];
    let bytes_read =
        unsafe { host_http_read_result(buf.as_mut_ptr() as i32, result_len) };

    if bytes_read <= 0 {
        return Err("failed to read upstream response".to_string());
    }

    serde_json::from_slice(&buf[..bytes_read as usize])
        .map_err(|e| format!("invalid upstream response: {}", e))
}

fn build_response(http_resp: HttpResponse) -> Response {
    let mut headers = BTreeMap::new();
    for (k, v) in http_resp.headers {
        let k_lower = k.to_lowercase();
        if !matches!(
            k_lower.as_str(),
            "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
        ) {
            headers.insert(k, v);
        }
    }
    let body = http_resp
        .body
        .and_then(|b| String::from_utf8(b).ok());
    Response {
        status: http_resp.status,
        headers,
        body,
    }
}

fn error_response(status: u16, detail: &str) -> Response {
    let error_type = if status == 502 {
        "urn:barbacane:error:upstream-unavailable"
    } else {
        "urn:barbacane:error:internal"
    };
    let body = serde_json::json!({
        "type": error_type,
        "title": if status == 502 { "Bad Gateway" } else { "Internal Server Error" },
        "status": status,
        "detail": detail
    });
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/problem+json".to_string());
    Response {
        status,
        headers,
        body: Some(body.to_string()),
    }
}

/// Build a JSON labels string with one key-value pair.
fn labels1(k1: &str, v1: &str) -> String {
    format!("{{\"{}\":\"{}\"}}", k1, v1)
}

/// Build a JSON labels string with two key-value pairs.
fn labels2(k1: &str, v1: &str, k2: &str, v2: &str) -> String {
    format!("{{\"{}\":\"{}\",\"{}\":\"{}\"}}", k1, v1, k2, v2)
}

// ---------------------------------------------------------------------------
// Host functions
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    fn host_http_stream(req_ptr: i32, req_len: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_stream(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(target_arch = "wasm32")]
mod host {
    pub fn context_get(key: &str) -> Option<String> {
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

    pub fn context_set(key: &str, value: &str) {
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

    pub fn metric_counter_inc(name: &str, labels_json: &str, value: u64) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_metric_counter_inc(
                name_ptr: i32, name_len: i32,
                labels_ptr: i32, labels_len: i32,
                value: f64,
            );
        }
        unsafe {
            host_metric_counter_inc(
                name.as_ptr() as i32, name.len() as i32,
                labels_json.as_ptr() as i32, labels_json.len() as i32,
                value as f64,
            );
        }
    }

    pub fn metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_metric_histogram_observe(
                name_ptr: i32, name_len: i32,
                labels_ptr: i32, labels_len: i32,
                value: f64,
            );
        }
        unsafe {
            host_metric_histogram_observe(
                name.as_ptr() as i32, name.len() as i32,
                labels_json.as_ptr() as i32, labels_json.len() as i32,
                value,
            );
        }
    }

    pub fn log_warn(msg: &str) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
        }
        unsafe { host_log(2, msg.as_ptr() as i32, msg.len() as i32) }
    }

    pub fn time_now_ms() -> u64 {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_time_now() -> i64;
        }
        unsafe { host_time_now().max(0) as u64 }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static CONTEXT: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
        static COUNTERS: RefCell<Vec<(String, String, u64)>> = const { RefCell::new(Vec::new()) };
        static HISTOGRAMS: RefCell<Vec<(String, String, f64)>> = const { RefCell::new(Vec::new()) };
        static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        static TIME_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(1_000_000) };
    }

    pub fn context_get(key: &str) -> Option<String> {
        CONTEXT.with(|ctx| ctx.borrow().get(key).cloned())
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|ctx| {
            ctx.borrow_mut().insert(key.to_string(), value.to_string());
        });
    }

    pub fn metric_counter_inc(name: &str, labels_json: &str, value: u64) {
        COUNTERS.with(|c| c.borrow_mut().push((name.to_string(), labels_json.to_string(), value)));
    }

    pub fn metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
        HISTOGRAMS.with(|h| h.borrow_mut().push((name.to_string(), labels_json.to_string(), value)));
    }

    pub fn log_warn(msg: &str) {
        WARNINGS.with(|w| w.borrow_mut().push(msg.to_string()));
    }

    pub fn time_now_ms() -> u64 {
        TIME_MS.with(|t| t.get())
    }

    #[cfg(test)]
    pub fn set_context(key: &str, value: &str) {
        CONTEXT.with(|ctx| { ctx.borrow_mut().insert(key.to_string(), value.to_string()); });
    }

    #[cfg(test)]
    pub fn get_context() -> BTreeMap<String, String> {
        CONTEXT.with(|ctx| ctx.borrow().clone())
    }

    #[cfg(test)]
    pub fn get_counters() -> Vec<(String, String, u64)> {
        COUNTERS.with(|c| c.borrow().clone())
    }

    #[cfg(test)]
    pub fn get_warnings() -> Vec<String> {
        WARNINGS.with(|w| w.borrow().clone())
    }

    #[cfg(test)]
    pub fn reset() {
        CONTEXT.with(|c| c.borrow_mut().clear());
        COUNTERS.with(|c| c.borrow_mut().clear());
        HISTOGRAMS.with(|h| h.borrow_mut().clear());
        WARNINGS.with(|w| w.borrow_mut().clear());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(body: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Request {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            query: None,
            headers,
            body: body.map(|s| s.to_string()),
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    fn openai_plugin(provider: &str, model: &str) -> AiProxy {
        AiProxy {
            provider: Some(if provider == "anthropic" { Provider::Anthropic } else { Provider::OpenAI }),
            model: Some(model.to_string()),
            api_key: Some("test-key".to_string()),
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            targets: BTreeMap::new(),
            default_target: None,
        }
    }

    // --- Config deserialization ---

    #[test]
    fn config_flat_minimal() {
        let json = r#"{
            "provider": "openai",
            "model": "gpt-4o",
            "api_key": "sk-test"
        }"#;
        let cfg: AiProxy = serde_json::from_str(json).expect("should parse");
        assert!(matches!(cfg.provider, Some(Provider::OpenAI)));
        assert_eq!(cfg.model.as_deref(), Some("gpt-4o"));
        assert_eq!(cfg.timeout, 120);
        assert!(cfg.fallback.is_empty());
        assert!(cfg.targets.is_empty());
    }

    #[test]
    fn config_with_targets() {
        let json = r#"{
            "targets": {
                "local": { "provider": "ollama", "model": "mistral" },
                "premium": { "provider": "anthropic", "model": "claude-opus-4-6", "api_key": "sk-ant" }
            },
            "default_target": "local"
        }"#;
        let cfg: AiProxy = serde_json::from_str(json).expect("should parse");
        assert_eq!(cfg.targets.len(), 2);
        assert_eq!(cfg.default_target.as_deref(), Some("local"));
        assert!(cfg.provider.is_none());
    }

    #[test]
    fn config_with_fallback() {
        let json = r#"{
            "provider": "openai",
            "model": "gpt-4o",
            "api_key": "sk-openai",
            "fallback": [
                { "provider": "anthropic", "model": "claude-sonnet-4-20250514", "api_key": "sk-ant" }
            ]
        }"#;
        let cfg: AiProxy = serde_json::from_str(json).expect("should parse");
        assert_eq!(cfg.fallback.len(), 1);
        assert!(matches!(cfg.fallback[0].provider, Provider::Anthropic));
    }

    #[test]
    fn config_provider_variants() {
        for (s, expected) in &[
            (r#"{"provider":"openai","model":"m"}"#, "openai"),
            (r#"{"provider":"anthropic","model":"m"}"#, "anthropic"),
            (r#"{"provider":"ollama","model":"m"}"#, "ollama"),
        ] {
            let cfg: AiProxy = serde_json::from_str(s).expect("should parse");
            assert_eq!(cfg.provider.as_ref().expect("provider").name(), *expected);
        }
    }

    // --- Target resolution ---

    #[test]
    fn resolve_flat_config() {
        host::reset();
        let plugin = openai_plugin("openai", "gpt-4o");
        let target = plugin.resolve_target().expect("should resolve");
        assert!(matches!(target.provider, Provider::OpenAI));
        assert_eq!(target.model, "gpt-4o");
    }

    #[test]
    fn resolve_default_target() {
        host::reset();
        let mut targets = BTreeMap::new();
        targets.insert("local".to_string(), TargetConfig {
            provider: Provider::Ollama,
            model: "mistral".to_string(),
            api_key: None,
            base_url: None,
        });
        let plugin = AiProxy {
            provider: None,
            model: None,
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            targets,
            default_target: Some("local".to_string()),
        };
        let target = plugin.resolve_target().expect("should resolve");
        assert!(matches!(target.provider, Provider::Ollama));
        assert_eq!(target.model, "mistral");
    }

    #[test]
    fn resolve_context_target_overrides_default() {
        host::reset();
        host::set_context("ai.target", "premium");

        let mut targets = BTreeMap::new();
        targets.insert("local".to_string(), TargetConfig {
            provider: Provider::Ollama,
            model: "mistral".to_string(),
            api_key: None,
            base_url: None,
        });
        targets.insert("premium".to_string(), TargetConfig {
            provider: Provider::Anthropic,
            model: "claude-opus-4-6".to_string(),
            api_key: Some("sk-ant".to_string()),
            base_url: None,
        });

        let plugin = AiProxy {
            provider: None,
            model: None,
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            targets,
            default_target: Some("local".to_string()),
        };
        let target = plugin.resolve_target().expect("should resolve");
        assert!(matches!(target.provider, Provider::Anthropic));
        assert_eq!(target.model, "claude-opus-4-6");
    }

    #[test]
    fn resolve_none_when_no_config() {
        host::reset();
        let plugin = AiProxy {
            provider: None,
            model: None,
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            targets: BTreeMap::new(),
            default_target: None,
        };
        assert!(plugin.resolve_target().is_none());
    }

    // --- Streaming detection ---

    #[test]
    fn streaming_detection_true() {
        assert!(is_streaming_request(&Some(r#"{"stream":true,"messages":[]}"#.to_string())));
    }

    #[test]
    fn streaming_detection_false() {
        assert!(!is_streaming_request(&Some(r#"{"stream":false,"messages":[]}"#.to_string())));
    }

    #[test]
    fn streaming_detection_absent() {
        assert!(!is_streaming_request(&Some(r#"{"messages":[]}"#.to_string())));
    }

    #[test]
    fn streaming_detection_no_body() {
        assert!(!is_streaming_request(&None));
    }

    // --- Anthropic request translation ---

    #[test]
    fn translate_to_anthropic_basic() {
        let body = r#"{
            "model": "claude-opus-4-6",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1024
        }"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "claude-opus-4-6", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");

        assert_eq!(v["model"].as_str(), Some("claude-opus-4-6"));
        assert_eq!(v["max_tokens"].as_u64(), Some(1024));
        assert!(v["system"].is_null());
        let msgs = v["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn translate_to_anthropic_extracts_system_message() {
        let body = r#"{
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hi"}
            ]
        }"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "claude-opus-4-6", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");

        assert_eq!(v["system"].as_str(), Some("You are helpful."));
        let msgs = v["messages"].as_array().expect("messages");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"].as_str(), Some("user"));
    }

    #[test]
    fn translate_to_anthropic_multiple_system_messages_joined() {
        let body = r#"{
            "messages": [
                {"role": "system", "content": "Part one."},
                {"role": "system", "content": "Part two."},
                {"role": "user", "content": "Hello"}
            ]
        }"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "m", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["system"].as_str(), Some("Part one.\nPart two."));
    }

    #[test]
    fn translate_to_anthropic_uses_default_max_tokens() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "m", false, Some(2048))
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(2048));
    }

    #[test]
    fn translate_to_anthropic_fallback_max_tokens_4096() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "m", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(4096));
    }

    #[test]
    fn translate_to_anthropic_optional_params() {
        let body = r#"{
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.7,
            "top_p": 0.9
        }"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "m", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert!((v["temperature"].as_f64().unwrap() - 0.7).abs() < 0.001);
        assert!((v["top_p"].as_f64().unwrap() - 0.9).abs() < 0.001);
    }

    #[test]
    fn translate_to_anthropic_stream_flag() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result = translate_to_anthropic(&Some(body.to_string()), "m", true, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["stream"].as_bool(), Some(true));
    }

    // --- Anthropic response translation ---

    #[test]
    fn translate_from_anthropic_basic() {
        let body = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-opus-4-6",
            "content": [{"type": "text", "text": "Hello there!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let result = translate_from_anthropic(body).expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");

        assert_eq!(v["id"].as_str(), Some("msg_123"));
        assert_eq!(v["object"].as_str(), Some("chat.completion"));
        let choices = v["choices"].as_array().expect("choices");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0]["message"]["content"].as_str(), Some("Hello there!"));
        assert_eq!(choices[0]["message"]["role"].as_str(), Some("assistant"));
        assert_eq!(choices[0]["finish_reason"].as_str(), Some("stop"));
        assert_eq!(v["usage"]["prompt_tokens"].as_u64(), Some(10));
        assert_eq!(v["usage"]["completion_tokens"].as_u64(), Some(5));
        assert_eq!(v["usage"]["total_tokens"].as_u64(), Some(15));
    }

    #[test]
    fn translate_from_anthropic_max_tokens_stop_reason() {
        let body = r#"{
            "id": "msg_456",
            "model": "claude-opus-4-6",
            "content": [{"type": "text", "text": "truncated"}],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 5, "output_tokens": 100}
        }"#;
        let result = translate_from_anthropic(body).expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["choices"][0]["finish_reason"].as_str(), Some("length"));
    }

    #[test]
    fn translate_from_anthropic_invalid_json() {
        assert!(translate_from_anthropic("not json").is_err());
    }

    // --- Token extraction ---

    #[test]
    fn extract_tokens_from_openai_response() {
        let body = r#"{
            "usage": {"prompt_tokens": 20, "completion_tokens": 80, "total_tokens": 100}
        }"#;
        let (p, c) = extract_tokens(Some(body)).expect("should extract");
        assert_eq!(p, 20);
        assert_eq!(c, 80);
    }

    #[test]
    fn extract_tokens_missing_usage() {
        assert!(extract_tokens(Some(r#"{"choices":[]}"#)).is_none());
    }

    #[test]
    fn extract_tokens_none_body() {
        assert!(extract_tokens(None).is_none());
    }

    // --- max_tokens injection ---

    #[test]
    fn inject_max_tokens_when_missing() {
        let plugin = AiProxy {
            provider: Some(Provider::OpenAI),
            model: Some("gpt-4o".to_string()),
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: Some(2048),
            fallback: vec![],
            targets: BTreeMap::new(),
            default_target: None,
        };
        let body = Some(r#"{"messages":[]}"#.to_string());
        let result = plugin.maybe_inject_max_tokens(&body).expect("body");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(2048));
    }

    #[test]
    fn inject_max_tokens_skipped_when_present() {
        let plugin = AiProxy {
            provider: Some(Provider::OpenAI),
            model: Some("gpt-4o".to_string()),
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: Some(2048),
            fallback: vec![],
            targets: BTreeMap::new(),
            default_target: None,
        };
        let body = Some(r#"{"messages":[],"max_tokens":512}"#.to_string());
        let result = plugin.maybe_inject_max_tokens(&body).expect("body");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(512)); // client value preserved
    }

    // --- dispatch returns 500 when no provider configured ---

    #[test]
    fn dispatch_500_when_no_provider() {
        host::reset();
        let mut plugin = AiProxy {
            provider: None,
            model: None,
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            targets: BTreeMap::new(),
            default_target: None,
        };
        let req = make_request(Some(r#"{"messages":[]}"#));
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 500);
    }

    // --- dispatch returns 502 when host_http_call fails (native stub) ---

    #[test]
    fn dispatch_502_on_connection_failure() {
        host::reset();
        let mut plugin = openai_plugin("openai", "gpt-4o");
        let req = make_request(Some(r#"{"messages":[{"role":"user","content":"hi"}]}"#));
        let resp = plugin.dispatch(req);
        // Native stub returns -1, so all targets fail → 502
        assert_eq!(resp.status, 502);
    }

    #[test]
    fn dispatch_502_anthropic_on_connection_failure() {
        host::reset();
        let mut plugin = openai_plugin("anthropic", "claude-opus-4-6");
        let req = make_request(Some(r#"{"messages":[{"role":"user","content":"hi"}]}"#));
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 502);
    }

    // --- Anthropic streaming forces non-streaming and logs warning ---

    #[test]
    fn anthropic_streaming_logs_warning() {
        host::reset();
        let mut plugin = openai_plugin("anthropic", "claude-opus-4-6");
        let req = make_request(Some(r#"{"messages":[{"role":"user","content":"hi"}],"stream":true}"#));
        let _ = plugin.dispatch(req);
        let warnings = host::get_warnings();
        assert!(warnings.iter().any(|w| w.contains("buffering")));
    }

    // --- Error response format ---

    #[test]
    fn error_response_502_format() {
        let resp = error_response(502, "all providers failed");
        assert_eq!(resp.status, 502);
        assert_eq!(resp.headers.get("content-type").map(|s| s.as_str()), Some("application/problem+json"));
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"].as_str(), Some("urn:barbacane:error:upstream-unavailable"));
        assert_eq!(body["status"].as_u64(), Some(502));
    }

    #[test]
    fn error_response_500_format() {
        let resp = error_response(500, "misconfiguration");
        assert_eq!(resp.status, 500);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"].as_str(), Some("urn:barbacane:error:internal"));
    }

    // --- Labels helpers ---

    #[test]
    fn labels1_format() {
        assert_eq!(labels1("provider", "openai"), r#"{"provider":"openai"}"#);
    }

    #[test]
    fn labels2_format() {
        assert_eq!(labels2("provider", "openai", "status", "200"), r#"{"provider":"openai","status":"200"}"#);
    }

    // --- Context propagation ---

    #[test]
    fn propagate_context_sets_provider_and_model() {
        host::reset();
        let target = TargetConfig {
            provider: Provider::OpenAI,
            model: "gpt-4o".to_string(),
            api_key: None,
            base_url: None,
        };
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(r#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#.to_string()),
        };
        propagate_context(&target, &resp);

        let ctx = host::get_context();
        assert_eq!(ctx.get("ai.provider").map(|s| s.as_str()), Some("openai"));
        assert_eq!(ctx.get("ai.model").map(|s| s.as_str()), Some("gpt-4o"));
        assert_eq!(ctx.get("ai.prompt_tokens").map(|s| s.as_str()), Some("10"));
        assert_eq!(ctx.get("ai.completion_tokens").map(|s| s.as_str()), Some("20"));
    }

    #[test]
    fn propagate_context_skips_tokens_for_streamed_response() {
        host::reset();
        let target = TargetConfig {
            provider: Provider::Ollama,
            model: "mistral".to_string(),
            api_key: None,
            base_url: None,
        };
        let resp = streamed_response(); // status = 0
        propagate_context(&target, &resp);

        let ctx = host::get_context();
        assert_eq!(ctx.get("ai.provider").map(|s| s.as_str()), Some("ollama"));
        assert_eq!(ctx.get("ai.model").map(|s| s.as_str()), Some("mistral"));
        assert!(!ctx.contains_key("ai.prompt_tokens"));
        assert!(!ctx.contains_key("ai.completion_tokens"));
    }

    #[test]
    fn propagate_context_records_token_metrics() {
        host::reset();
        let target = TargetConfig {
            provider: Provider::Anthropic,
            model: "claude-opus-4-6".to_string(),
            api_key: None,
            base_url: None,
        };
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(r#"{"usage":{"prompt_tokens":5,"completion_tokens":15,"total_tokens":20}}"#.to_string()),
        };
        propagate_context(&target, &resp);

        let counters = host::get_counters();
        let prompt_counter = counters.iter().find(|(name, labels, _)| {
            name == "tokens_total" && labels.contains("prompt")
        });
        let completion_counter = counters.iter().find(|(name, labels, _)| {
            name == "tokens_total" && labels.contains("completion")
        });
        assert!(prompt_counter.is_some(), "prompt tokens counter should be recorded");
        assert_eq!(prompt_counter.unwrap().2, 5);
        assert!(completion_counter.is_some(), "completion tokens counter should be recorded");
        assert_eq!(completion_counter.unwrap().2, 15);
    }

    // --- Provider helpers ---

    #[test]
    fn provider_names() {
        assert_eq!(Provider::OpenAI.name(), "openai");
        assert_eq!(Provider::Anthropic.name(), "anthropic");
        assert_eq!(Provider::Ollama.name(), "ollama");
    }

    #[test]
    fn provider_default_base_urls() {
        assert_eq!(Provider::OpenAI.default_base_url(), "https://api.openai.com");
        assert_eq!(Provider::Anthropic.default_base_url(), "https://api.anthropic.com");
        assert_eq!(Provider::Ollama.default_base_url(), "http://localhost:11434");
    }

    #[test]
    fn provider_openai_compatible() {
        assert!(Provider::OpenAI.is_openai_compatible());
        assert!(Provider::Ollama.is_openai_compatible());
        assert!(!Provider::Anthropic.is_openai_compatible());
    }

    #[test]
    fn target_effective_base_url_custom() {
        let t = TargetConfig {
            provider: Provider::OpenAI,
            model: "gpt-4o".to_string(),
            api_key: None,
            base_url: Some("https://my-azure.openai.com".to_string()),
        };
        assert_eq!(t.effective_base_url(), "https://my-azure.openai.com");
    }

    #[test]
    fn target_effective_base_url_default() {
        let t = TargetConfig {
            provider: Provider::Anthropic,
            model: "claude-opus-4-6".to_string(),
            api_key: None,
            base_url: None,
        };
        assert_eq!(t.effective_base_url(), "https://api.anthropic.com");
    }
}
