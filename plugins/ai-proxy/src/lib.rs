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
//!
//! ## Source layout
//!
//! - [`mod@protocols`] — per-protocol translation adapters (Chat Completions
//!   today; ADR-0030 will add `/v1/responses` here).
//! - [`mod@providers`] — per-provider transport (OpenAI passthrough, Anthropic
//!   Messages, Ollama via OpenAI passthrough).
//! - This file — orchestration: target resolution, fallback chain, metrics,
//!   context propagation. Path-based dispatch picks the protocol handler.

use barbacane_plugin_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod protocols;
pub mod providers;

// ---------------------------------------------------------------------------
// Provider type
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Provider {
    OpenAI,
    Anthropic,
    Ollama,
}

impl Provider {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Provider::OpenAI => "openai",
            Provider::Anthropic => "anthropic",
            Provider::Ollama => "ollama",
        }
    }

    pub(crate) fn default_base_url(&self) -> &'static str {
        match self {
            Provider::OpenAI => "https://api.openai.com",
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::Ollama => "http://localhost:11434",
        }
    }

    pub(crate) fn is_openai_compatible(&self) -> bool {
        matches!(self, Provider::OpenAI | Provider::Ollama)
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// A single named provider target (provider + credentials). The model is
/// always the client-supplied value (ADR-0030 §0 — caller-owned model).
///
/// `allow` / `deny` are catalog-policy glob lists evaluated against the
/// client's `model` after target resolution. They apply on every resolution
/// path that produces a target carrying them (ADR-0030 §3) — including
/// context-driven dispatch via `ai.target` — so a `cel` misconfig cannot
/// leak a denied model.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetConfig {
    pub provider: Provider,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Custom base URL (Azure, self-hosted, Ollama remote, etc.).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Allow-list of glob patterns. When set, the client's `model` must match
    /// at least one entry; otherwise 403 `model_not_permitted`.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Deny-list of glob patterns, evaluated after `allow`. When set, the
    /// client's `model` must not match any entry; otherwise 403.
    #[serde(default)]
    pub deny: Vec<String>,
}

impl TargetConfig {
    pub(crate) fn effective_base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or_else(|| self.provider.default_base_url())
    }
}

/// A `routes` entry: dispatch to `provider` when the client's `model` field
/// matches `pattern`. ADR-0030 §3 — dynamic model routing.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Route {
    pub pattern: String,
    pub provider: Provider,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

impl Route {
    /// Build the equivalent `TargetConfig` so the rest of the dispatcher (which
    /// thinks in targets) doesn't need to know about routes.
    fn to_target(&self) -> TargetConfig {
        TargetConfig {
            provider: self.provider.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            allow: self.allow.clone(),
            deny: self.deny.clone(),
        }
    }
}

fn default_timeout() -> u64 {
    120
}

/// AI proxy dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiProxy {
    // --- Flat single-provider config (used when no `targets` map is defined) ---
    // The model is always the client-supplied value (ADR-0030 §0).
    #[serde(default)]
    pub(crate) provider: Option<Provider>,
    #[serde(default)]
    pub(crate) api_key: Option<String>,
    #[serde(default)]
    pub(crate) base_url: Option<String>,

    /// Request timeout in seconds. LLM calls can be slow; default is 120s.
    #[serde(default = "default_timeout")]
    pub(crate) timeout: u64,

    /// Default `max_tokens` applied when the client request omits it.
    #[serde(default)]
    pub(crate) max_tokens: Option<u32>,

    /// Provider fallback chain. Tried in order when the primary target returns
    /// a 5xx or a connection error. 4xx responses are returned directly.
    #[serde(default)]
    pub(crate) fallback: Vec<TargetConfig>,

    /// Glob-pattern routing rules evaluated against the client's `model`
    /// (ADR-0030 §3). First match wins; a denied model does not fall through.
    #[serde(default)]
    pub(crate) routes: Vec<Route>,

    /// Lazy-compiled glob set built from `routes` patterns and per-route
    /// allow/deny lists. Populated on first dispatch.
    #[serde(skip)]
    pub(crate) compiled_routes: Option<Vec<CompiledRoute>>,

    /// Named provider targets for policy-driven routing. The `cel` middleware
    /// selects a target by writing `ai.target` into the request context before
    /// this dispatcher runs.
    #[serde(default)]
    pub(crate) targets: BTreeMap<String, TargetConfig>,

    /// Target name to use when no `ai.target` context key is present and no
    /// route matched.
    #[serde(default)]
    pub(crate) default_target: Option<String>,
}

/// Compiled form of a [`Route`] — the pattern turned into a
/// [`globset::GlobMatcher`] for fast case-sensitive matching, paired with
/// the resolved target. Per-route `allow` / `deny` ride along on the target
/// (see [`Route::to_target`]) and are evaluated by [`evaluate_catalog_policy`]
/// after resolution. Recompiling tiny per-target lists per request is cheap
/// and lets the same code path serve `targets`-driven and routes-driven
/// resolutions without a parallel "precompiled set" cache.
pub(crate) struct CompiledRoute {
    pub pattern: globset::GlobMatcher,
    pub target: TargetConfig,
}

// ---------------------------------------------------------------------------
// Wire types for host_http_call / host_http_stream
// ---------------------------------------------------------------------------

/// Body travels via side-channel (`set_http_request_body`), not in JSON.
#[derive(Serialize)]
pub(crate) struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub timeout_ms: Option<u64>,
}

/// Body is read separately via `read_http_response_body()`.
#[derive(Deserialize)]
pub(crate) struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// Dispatcher implementation
// ---------------------------------------------------------------------------

/// Function signature every protocol handler implements: given a resolved
/// target, the request, and the client-supplied model, return a `Response`
/// or a connection-level error the orchestration loop can fall back on.
///
/// The `client_model` is the value of the `model` field on the request body —
/// extracted upstream by `dispatch()` and passed through verbatim. The
/// caller-owned-model principle (ADR-0030 §0) means the gateway never picks
/// a model for the client; this argument is the only model identifier any
/// downstream component (Anthropic translation, context propagation, metrics)
/// is allowed to use.
type ProtocolHandler =
    fn(&AiProxy, &TargetConfig, &Request, &str, bool) -> Result<Response, String>;

impl AiProxy {
    pub fn dispatch(&mut self, req: Request) -> Response {
        match req.path.as_str() {
            "/v1/chat/completions" => self.dispatch_chat_completion(req),
            // ADR-0030 §2 will add: "/v1/responses" — same model-required check
            //                        + protocols::responses::handle.
            // ADR-0030 §4 will add: "/v1/models" — no client body, separate
            //                        path that does not go through dispatch_with_handler.
            other => error_response(404, &format!("ai-proxy: no handler for path {}", other)),
        }
    }

    fn dispatch_chat_completion(&mut self, req: Request) -> Response {
        let client_model = match extract_client_model(&req.body) {
            Some(m) => m,
            None => return model_required_response(),
        };
        self.dispatch_with_handler(req, &client_model, protocols::chat_completion::handle)
    }

    /// Shared orchestration loop: resolve target, run the protocol handler,
    /// fall back on 5xx / connection error, emit metrics, propagate context.
    fn dispatch_with_handler(
        &mut self,
        req: Request,
        client_model: &str,
        handler: ProtocolHandler,
    ) -> Response {
        let start_ms = host::time_now_ms();

        // Compile route patterns lazily on first dispatch. Surface bad globs
        // as 500 problem+json — this is operator misconfiguration, not a
        // request-level error, and it's stable across requests.
        if let Err(msg) = self.ensure_compiled_routes() {
            return error_response(500, &format!("ai-proxy misconfiguration: {}", msg));
        }

        let (primary, resolution) = match self.resolve_target(client_model) {
            ResolveOutcome::Resolved(t, kind) => (t, kind),
            ResolveOutcome::NoRouteMatch => {
                return no_route_response(client_model);
            }
            ResolveOutcome::NotConfigured => {
                return error_response(
                    500,
                    "ai-proxy misconfiguration: no provider configured (set `provider`, `routes`, or define `targets`)",
                );
            }
        };

        host::metric_counter_inc(
            "resolution_total",
            &labels1("resolution", resolution.as_str()),
            1,
        );
        host::log_warn(&format!(
            "ai-proxy: resolved provider={} via={}",
            primary.provider.name(),
            resolution.as_str()
        ));

        // Catalog allow/deny — applies on every resolution path that produces
        // a target carrying those rules (ADR-0030 §3). A denied model returns
        // 403 and does not fall through to fallback or to another route.
        if let Some(reason) = evaluate_catalog_policy(&primary, client_model) {
            return model_not_permitted_response(client_model, reason);
        }

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
                    &labels2(
                        "from_provider",
                        prev.provider.name(),
                        "to_provider",
                        target.provider.name(),
                    ),
                    1,
                );
                host::log_warn(&format!(
                    "ai-proxy: falling back from {} to {}",
                    prev.provider.name(),
                    target.provider.name()
                ));
            }

            match handler(self, target, &req, client_model, streaming) {
                Ok(resp) => {
                    let elapsed_ms = host::time_now_ms().saturating_sub(start_ms);

                    // Streamed responses have status=0 — treat as success
                    let metric_status = if resp.status == 0 { 200 } else { resp.status };

                    host::metric_counter_inc(
                        "requests_total",
                        &labels2(
                            "provider",
                            target.provider.name(),
                            "status",
                            &metric_status.to_string(),
                        ),
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
                    propagate_context(target, client_model, &resp);

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

    /// Resolve the active target using the four-step priority chain defined
    /// in ADR-0030 §3:
    /// 1. `ai.target` context key (set by upstream middleware, e.g. `cel`)
    /// 2. `routes` glob match against the client's `model`
    /// 3. `default_target` name
    /// 4. Flat `provider` config
    ///
    /// Returns `NoRouteMatch` when the operator configured `routes` but none
    /// matched and there is no `default_target` / flat fallthrough — this is a
    /// 400 (client supplied a model the operator's catalog doesn't cover).
    /// Returns `NotConfigured` when nothing is configured at all — 500.
    pub(crate) fn resolve_target(&self, client_model: &str) -> ResolveOutcome {
        // 1. Context-set target name (ai.target written by upstream cel)
        if let Some(name) = host::context_get("ai.target") {
            if let Some(t) = self.targets.get(&name) {
                return ResolveOutcome::Resolved(t.clone(), ResolutionKind::Context);
            }
            host::log_warn(&format!(
                "ai-proxy: ai.target '{}' not found in targets map; falling through",
                name
            ));
        }

        // 2. Routes — first glob match wins
        let mut routes_configured = false;
        if let Some(compiled) = &self.compiled_routes {
            routes_configured = !compiled.is_empty();
            for route in compiled {
                if route.pattern.is_match(client_model) {
                    return ResolveOutcome::Resolved(route.target.clone(), ResolutionKind::Routes);
                }
            }
        }

        // 3. Default target
        if let Some(ref name) = self.default_target {
            if let Some(t) = self.targets.get(name) {
                return ResolveOutcome::Resolved(t.clone(), ResolutionKind::Default);
            }
        }

        // 4. Flat config
        if let Some(p) = self.provider.as_ref() {
            return ResolveOutcome::Resolved(
                TargetConfig {
                    provider: p.clone(),
                    api_key: self.api_key.clone(),
                    base_url: self.base_url.clone(),
                    allow: Vec::new(),
                    deny: Vec::new(),
                },
                ResolutionKind::Flat,
            );
        }

        // Nothing matched. Distinguish "operator declared routes but the
        // client's model doesn't fit" from "operator declared nothing" —
        // the first is a 400 (client error), the second a 500 (operator).
        if routes_configured {
            ResolveOutcome::NoRouteMatch
        } else {
            ResolveOutcome::NotConfigured
        }
    }

    /// Lazy-compile route globs and per-route allow/deny `GlobSet`s. Idempotent
    /// — only runs once per plugin instance. Returns the first compile error
    /// in human-readable form so the dispatch layer can surface it as 500
    /// problem+json.
    pub(crate) fn ensure_compiled_routes(&mut self) -> Result<(), String> {
        if self.compiled_routes.is_some() {
            return Ok(());
        }
        let mut out = Vec::with_capacity(self.routes.len());
        for (idx, route) in self.routes.iter().enumerate() {
            let pattern = compile_glob(&route.pattern)
                .map_err(|e| format!("routes[{}].pattern: {}", idx, e))?
                .compile_matcher();
            // Pre-validate allow/deny globs at first dispatch so a bad pattern
            // surfaces as a stable 500 instead of as a per-request 403.
            compile_glob_set(&route.allow).map_err(|e| format!("routes[{}].allow: {}", idx, e))?;
            compile_glob_set(&route.deny).map_err(|e| format!("routes[{}].deny: {}", idx, e))?;
            out.push(CompiledRoute {
                pattern,
                target: route.to_target(),
            });
        }
        self.compiled_routes = Some(out);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Resolution outcomes
// ---------------------------------------------------------------------------

pub(crate) enum ResolveOutcome {
    Resolved(TargetConfig, ResolutionKind),
    /// `routes` was configured but no entry matched, and there's no
    /// `default_target`/flat fallthrough. → 400 `no_route`.
    NoRouteMatch,
    /// Nothing configured: no `routes`, no `default_target`, no flat. → 500.
    NotConfigured,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ResolutionKind {
    Context,
    Routes,
    Default,
    Flat,
}

impl ResolutionKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            ResolutionKind::Context => "context",
            ResolutionKind::Routes => "routes",
            ResolutionKind::Default => "default",
            ResolutionKind::Flat => "flat",
        }
    }
}

// ---------------------------------------------------------------------------
// Context propagation and metrics
// ---------------------------------------------------------------------------

/// Write AI context keys so downstream middlewares can read them. The
/// `client_model` is the model string the client supplied on the request
/// body (ADR-0030 §0 — caller-owned model); the gateway never substitutes
/// its own. For streamed responses (status=0), token counts are unavailable.
pub(crate) fn propagate_context(target: &TargetConfig, client_model: &str, resp: &Response) {
    host::context_set("ai.provider", target.provider.name());
    host::context_set("ai.model", client_model);

    // status=0 means streamed — token counts not available
    if resp.status == 0 {
        return;
    }

    if let Some(tokens) = extract_tokens(
        resp.body
            .as_deref()
            .and_then(|b| std::str::from_utf8(b).ok()),
    ) {
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
pub(crate) fn extract_tokens(body: Option<&str>) -> Option<(u64, u64)> {
    let body = body?;
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let prompt = v["usage"]["prompt_tokens"].as_u64()?;
    let completion = v["usage"]["completion_tokens"].as_u64()?;
    Some((prompt, completion))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn is_streaming_request(body: &Option<Vec<u8>>) -> bool {
    body.as_ref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v["stream"].as_bool())
        .unwrap_or(false)
}

/// Extract the client-supplied `model` field from an OpenAI-format request
/// body. Returns `None` for an absent body, malformed JSON, missing field,
/// non-string value, or empty string. Caller-owned model (ADR-0030 §0) — the
/// gateway never substitutes a default.
pub(crate) fn extract_client_model(body: &Option<Vec<u8>>) -> Option<String> {
    let raw = body.as_deref()?;
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let s = v.get("model")?.as_str()?;
    if s.is_empty() {
        return None;
    }
    Some(s.to_string())
}

/// 400 problem+json for a request that omits `model`. Matches both upstream
/// provider contracts (OpenAI Chat Completions and Responses both require
/// `model`) and ADR-0030 §0's caller-owned-model principle.
pub(crate) fn model_required_response() -> Response {
    let body = serde_json::json!({
        "type": "urn:barbacane:error:model_required",
        "title": "Bad Request",
        "status": 400,
        "code": "model_required",
        "detail": "ai-proxy: request body is missing a non-empty `model` field. \
                   The gateway does not pick a default model — see ADR-0030 §0.",
    });
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    Response {
        status: 400,
        headers,
        body: Some(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

/// 400 problem+json when `routes` was declared but no entry matches and there
/// is no `default_target` / flat fallthrough — the operator's catalog doesn't
/// cover the model the client requested. ADR-0030 §3.
pub(crate) fn no_route_response(client_model: &str) -> Response {
    let body = serde_json::json!({
        "type": "urn:barbacane:error:no_route",
        "title": "Bad Request",
        "status": 400,
        "code": "no_route",
        "detail": format!(
            "ai-proxy: no route matched model {:?}. Add a `routes` entry, set `default_target`, \
             or configure a flat `provider`. See ADR-0030 §3.",
            client_model
        ),
    });
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    Response {
        status: 400,
        headers,
        body: Some(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

/// 403 problem+json when a resolved target's `allow` / `deny` rules reject
/// the client's `model` (catalog policy denial). ADR-0030 §3.
pub(crate) fn model_not_permitted_response(client_model: &str, reason: PolicyDenial) -> Response {
    let detail = match reason {
        PolicyDenial::NotInAllow => format!(
            "ai-proxy: model {:?} is not in the resolved target's `allow` list. \
             See ADR-0030 §3 (catalog policy).",
            client_model
        ),
        PolicyDenial::Denied => format!(
            "ai-proxy: model {:?} matches the resolved target's `deny` list. \
             See ADR-0030 §3 (catalog policy).",
            client_model
        ),
    };
    let body = serde_json::json!({
        "type": "urn:barbacane:error:model_not_permitted",
        "title": "Forbidden",
        "status": 403,
        "code": "model_not_permitted",
        "detail": detail,
    });
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    Response {
        status: 403,
        headers,
        body: Some(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

/// Reason a target's catalog policy rejected the client's model.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PolicyDenial {
    /// `allow` is set and the model didn't match any entry.
    NotInAllow,
    /// `deny` matched at least one entry.
    Denied,
}

/// Evaluate a target's `allow` / `deny` glob lists against the client's model.
/// `deny` is evaluated after `allow`. Compiles the lists on the fly — these
/// lists are typically tiny (single digits) and exposed only on resolved
/// targets, so compiling once per request is cheap. The schema's
/// `^[A-Za-z0-9_*?\[\]\-:.+/]+$` constraint should keep us out of the
/// failure path here, but if a glob does fail to compile we treat it as a
/// deny to fail closed rather than silently bypass the policy.
pub(crate) fn evaluate_catalog_policy(
    target: &TargetConfig,
    client_model: &str,
) -> Option<PolicyDenial> {
    if !target.allow.is_empty() {
        let set = match compile_glob_set(&target.allow) {
            Ok(Some(s)) => s,
            // compile_glob_set returns None only for empty input; we just
            // checked .is_empty() so this arm is unreachable in practice.
            Ok(None) => return Some(PolicyDenial::NotInAllow),
            Err(_) => return Some(PolicyDenial::NotInAllow),
        };
        if !set.is_match(client_model) {
            return Some(PolicyDenial::NotInAllow);
        }
    }
    if !target.deny.is_empty() {
        let set = match compile_glob_set(&target.deny) {
            Ok(Some(s)) => s,
            Ok(None) => return Some(PolicyDenial::Denied),
            Err(_) => return Some(PolicyDenial::Denied),
        };
        if set.is_match(client_model) {
            return Some(PolicyDenial::Denied);
        }
    }
    None
}

/// Compile a single glob pattern with case-sensitive, anchored matching —
/// the semantics ADR-0030 §3 pins for `routes` patterns and allow/deny entries.
fn compile_glob(pattern: &str) -> Result<globset::Glob, globset::Error> {
    globset::GlobBuilder::new(pattern)
        .case_insensitive(false)
        .literal_separator(false)
        .build()
}

/// Compile a list of glob patterns into a single `GlobSet`. Returns `Ok(None)`
/// for an empty list so callers can `if let Some(set) = compile_glob_set(...)`
/// to skip the match path entirely.
fn compile_glob_set(patterns: &[String]) -> Result<Option<globset::GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = globset::GlobSetBuilder::new();
    for p in patterns {
        builder.add(compile_glob(p).map_err(|e| e.to_string())?);
    }
    builder.build().map(Some).map_err(|e| e.to_string())
}

pub(crate) fn http_call(req: &HttpRequest) -> Result<HttpResponse, String> {
    let req_json = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    let result_len = unsafe { host_http_call(req_json.as_ptr() as i32, req_json.len() as i32) };

    if result_len < 0 {
        return Err("upstream connection failed".to_string());
    }

    let mut buf = vec![0u8; result_len as usize];
    let bytes_read = unsafe { host_http_read_result(buf.as_mut_ptr() as i32, result_len) };

    if bytes_read <= 0 {
        return Err("failed to read upstream response".to_string());
    }

    serde_json::from_slice(&buf[..bytes_read as usize])
        .map_err(|e| format!("invalid upstream response: {}", e))
}

pub(crate) fn build_response(http_resp: HttpResponse) -> Response {
    let response_body = read_http_response_body();
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
    Response {
        status: http_resp.status,
        headers,
        body: response_body,
    }
}

pub(crate) fn error_response(status: u16, detail: &str) -> Response {
    let (error_type, title) = match status {
        404 => ("urn:barbacane:error:not-found", "Not Found"),
        500 => ("urn:barbacane:error:internal", "Internal Server Error"),
        502 => ("urn:barbacane:error:upstream-unavailable", "Bad Gateway"),
        _ => ("urn:barbacane:error:internal", "Internal Server Error"),
    };
    let body = serde_json::json!({
        "type": error_type,
        "title": title,
        "status": status,
        "detail": detail
    });
    let mut headers = BTreeMap::new();
    headers.insert(
        "content-type".to_string(),
        "application/problem+json".to_string(),
    );
    Response {
        status,
        headers,
        body: Some(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

/// Build a JSON labels string with one key-value pair.
pub(crate) fn labels1(k1: &str, v1: &str) -> String {
    format!("{{\"{}\":\"{}\"}}", k1, v1)
}

/// Build a JSON labels string with two key-value pairs.
pub(crate) fn labels2(k1: &str, v1: &str, k2: &str, v2: &str) -> String {
    format!("{{\"{}\":\"{}\",\"{}\":\"{}\"}}", k1, v1, k2, v2)
}

// ---------------------------------------------------------------------------
// Host functions
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    pub(crate) fn host_http_call(req_ptr: i32, req_len: i32) -> i32;
    pub(crate) fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
    pub(crate) fn host_http_stream(req_ptr: i32, req_len: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn host_http_stream(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(target_arch = "wasm32")]
pub(crate) mod host {
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

    pub fn metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_metric_histogram_observe(
                name_ptr: i32,
                name_len: i32,
                labels_ptr: i32,
                labels_len: i32,
                value: f64,
            );
        }
        unsafe {
            host_metric_histogram_observe(
                name.as_ptr() as i32,
                name.len() as i32,
                labels_json.as_ptr() as i32,
                labels_json.len() as i32,
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
pub(crate) mod host {
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
        COUNTERS.with(|c| {
            c.borrow_mut()
                .push((name.to_string(), labels_json.to_string(), value))
        });
    }

    pub fn metric_histogram_observe(name: &str, labels_json: &str, value: f64) {
        HISTOGRAMS.with(|h| {
            h.borrow_mut()
                .push((name.to_string(), labels_json.to_string(), value))
        });
    }

    pub fn log_warn(msg: &str) {
        WARNINGS.with(|w| w.borrow_mut().push(msg.to_string()));
    }

    pub fn time_now_ms() -> u64 {
        TIME_MS.with(|t| t.get())
    }

    #[cfg(test)]
    pub fn set_context(key: &str, value: &str) {
        CONTEXT.with(|ctx| {
            ctx.borrow_mut().insert(key.to_string(), value.to_string());
        });
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
    use super::protocols::chat_completion::{translate_from_anthropic, translate_to_anthropic};
    use super::*;

    fn make_request(body: Option<&str>) -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        Request {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            query: None,
            headers,
            body: body.map(|s| s.as_bytes().to_vec()),
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        }
    }

    fn openai_plugin(provider: &str) -> AiProxy {
        AiProxy {
            provider: Some(if provider == "anthropic" {
                Provider::Anthropic
            } else {
                Provider::OpenAI
            }),
            api_key: Some("test-key".to_string()),
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            routes: vec![],
            compiled_routes: None,
            targets: BTreeMap::new(),
            default_target: None,
        }
    }

    /// Test-only AiProxy builder that fills the routes/compiled_routes slots
    /// added in PR-3. Use `..plugin_with()` to start from a zeroed plugin and
    /// override what each test cares about.
    fn plugin_with() -> AiProxy {
        AiProxy {
            provider: None,
            api_key: None,
            base_url: None,
            timeout: 120,
            max_tokens: None,
            fallback: vec![],
            routes: vec![],
            compiled_routes: None,
            targets: BTreeMap::new(),
            default_target: None,
        }
    }

    /// Test-only TargetConfig with all PR-3 fields zeroed.
    fn target_with(provider: Provider) -> TargetConfig {
        TargetConfig {
            provider,
            api_key: None,
            base_url: None,
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }

    fn make_chat_request(model: &str) -> Request {
        let body = format!(
            r#"{{"model":"{}","messages":[{{"role":"user","content":"hi"}}]}}"#,
            model
        );
        make_request(Some(&body))
    }

    // --- Config deserialization ---

    #[test]
    fn config_flat_minimal() {
        let json = r#"{
            "provider": "openai",
            "api_key": "sk-test"
        }"#;
        let cfg: AiProxy = serde_json::from_str(json).expect("should parse");
        assert!(matches!(cfg.provider, Some(Provider::OpenAI)));
        assert_eq!(cfg.timeout, 120);
        assert!(cfg.fallback.is_empty());
        assert!(cfg.targets.is_empty());
    }

    #[test]
    fn config_rejects_legacy_model_at_top_level() {
        // ADR-0030 §0: model is removed from gateway config; serde
        // deny_unknown_fields surfaces leftover `model:` at config-load time.
        let json = r#"{
            "provider": "openai",
            "model": "gpt-4o"
        }"#;
        match serde_json::from_str::<AiProxy>(json) {
            Ok(_) => panic!("legacy model: at top level must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("model"),
                "error should name the offending field: {}",
                e
            ),
        }
    }

    #[test]
    fn config_rejects_legacy_model_on_target() {
        // Same migration check applied to nested targets — the runtime safety
        // net for the schema gap that vacuum doesn't recurse into yet.
        let json = r#"{
            "targets": {
                "premium": { "provider": "anthropic", "model": "claude-opus-4-6" }
            }
        }"#;
        match serde_json::from_str::<AiProxy>(json) {
            Ok(_) => panic!("legacy model: on a target must be rejected"),
            Err(e) => assert!(
                e.to_string().contains("model"),
                "error should name the offending field: {}",
                e
            ),
        }
    }

    #[test]
    fn config_with_targets() {
        let json = r#"{
            "targets": {
                "local": { "provider": "ollama" },
                "premium": { "provider": "anthropic", "api_key": "sk-ant" }
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
            "api_key": "sk-openai",
            "fallback": [
                { "provider": "anthropic", "api_key": "sk-ant" }
            ]
        }"#;
        let cfg: AiProxy = serde_json::from_str(json).expect("should parse");
        assert_eq!(cfg.fallback.len(), 1);
        assert!(matches!(cfg.fallback[0].provider, Provider::Anthropic));
    }

    #[test]
    fn config_provider_variants() {
        for (s, expected) in &[
            (r#"{"provider":"openai"}"#, "openai"),
            (r#"{"provider":"anthropic"}"#, "anthropic"),
            (r#"{"provider":"ollama"}"#, "ollama"),
        ] {
            let cfg: AiProxy = serde_json::from_str(s).expect("should parse");
            assert_eq!(cfg.provider.as_ref().expect("provider").name(), *expected);
        }
    }

    // --- Target resolution ---

    fn assert_resolved(outcome: ResolveOutcome) -> (TargetConfig, ResolutionKind) {
        match outcome {
            ResolveOutcome::Resolved(t, k) => (t, k),
            other => panic!("expected Resolved, got {:?}", outcome_kind(&other)),
        }
    }

    fn outcome_kind(o: &ResolveOutcome) -> &'static str {
        match o {
            ResolveOutcome::Resolved(_, _) => "Resolved",
            ResolveOutcome::NoRouteMatch => "NoRouteMatch",
            ResolveOutcome::NotConfigured => "NotConfigured",
        }
    }

    #[test]
    fn resolve_flat_config() {
        host::reset();
        let plugin = openai_plugin("openai");
        let (target, kind) = assert_resolved(plugin.resolve_target("gpt-4o"));
        assert!(matches!(target.provider, Provider::OpenAI));
        assert_eq!(kind, ResolutionKind::Flat);
    }

    #[test]
    fn resolve_default_target() {
        host::reset();
        let mut targets = BTreeMap::new();
        targets.insert("local".to_string(), target_with(Provider::Ollama));
        let plugin = AiProxy {
            targets,
            default_target: Some("local".to_string()),
            ..plugin_with()
        };
        let (target, kind) = assert_resolved(plugin.resolve_target("mistral"));
        assert!(matches!(target.provider, Provider::Ollama));
        assert_eq!(kind, ResolutionKind::Default);
    }

    #[test]
    fn resolve_context_target_overrides_default() {
        host::reset();
        host::set_context("ai.target", "premium");

        let mut targets = BTreeMap::new();
        targets.insert("local".to_string(), target_with(Provider::Ollama));
        targets.insert(
            "premium".to_string(),
            TargetConfig {
                api_key: Some("sk-ant".to_string()),
                ..target_with(Provider::Anthropic)
            },
        );

        let plugin = AiProxy {
            targets,
            default_target: Some("local".to_string()),
            ..plugin_with()
        };
        let (target, kind) = assert_resolved(plugin.resolve_target("claude-opus-4-6"));
        assert!(matches!(target.provider, Provider::Anthropic));
        assert_eq!(kind, ResolutionKind::Context);
    }

    #[test]
    fn resolve_none_when_no_config() {
        host::reset();
        let plugin = plugin_with();
        assert!(matches!(
            plugin.resolve_target("anything"),
            ResolveOutcome::NotConfigured
        ));
    }

    // --- routes resolution (ADR-0030 §3) ---

    fn route(pattern: &str, provider: Provider) -> Route {
        Route {
            pattern: pattern.to_string(),
            provider,
            api_key: None,
            base_url: None,
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }

    fn plugin_with_routes(routes: Vec<Route>) -> AiProxy {
        let mut p = AiProxy {
            routes,
            ..plugin_with()
        };
        p.ensure_compiled_routes().expect("globs compile");
        p
    }

    #[test]
    fn resolve_routes_first_match_wins() {
        host::reset();
        let plugin = plugin_with_routes(vec![
            route("claude-*", Provider::Anthropic),
            route("gpt-*", Provider::OpenAI),
            route("*", Provider::Ollama),
        ]);
        let (target, kind) = assert_resolved(plugin.resolve_target("claude-opus-4-6"));
        assert!(matches!(target.provider, Provider::Anthropic));
        assert_eq!(kind, ResolutionKind::Routes);
    }

    #[test]
    fn resolve_routes_catch_all_wins_when_specific_does_not_match() {
        host::reset();
        let plugin = plugin_with_routes(vec![
            route("claude-*", Provider::Anthropic),
            route("*", Provider::Ollama),
        ]);
        let (target, kind) = assert_resolved(plugin.resolve_target("mistral"));
        assert!(matches!(target.provider, Provider::Ollama));
        assert_eq!(kind, ResolutionKind::Routes);
    }

    #[test]
    fn resolve_routes_no_match_returns_no_route_when_no_fallthrough() {
        host::reset();
        let plugin = plugin_with_routes(vec![
            route("claude-*", Provider::Anthropic),
            route("gpt-*", Provider::OpenAI),
        ]);
        // No catch-all route, no default_target, no flat → 400 no_route.
        assert!(matches!(
            plugin.resolve_target("mistral"),
            ResolveOutcome::NoRouteMatch
        ));
    }

    #[test]
    fn resolve_routes_falls_through_to_default_when_unmatched() {
        host::reset();
        let mut targets = BTreeMap::new();
        targets.insert("local".to_string(), target_with(Provider::Ollama));
        let mut plugin = AiProxy {
            routes: vec![route("claude-*", Provider::Anthropic)],
            targets,
            default_target: Some("local".to_string()),
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let (target, kind) = assert_resolved(plugin.resolve_target("mistral"));
        assert!(matches!(target.provider, Provider::Ollama));
        assert_eq!(kind, ResolutionKind::Default);
    }

    #[test]
    fn resolve_routes_falls_through_to_flat_when_unmatched() {
        host::reset();
        let mut plugin = AiProxy {
            provider: Some(Provider::Ollama),
            routes: vec![route("claude-*", Provider::Anthropic)],
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let (target, kind) = assert_resolved(plugin.resolve_target("mistral"));
        assert!(matches!(target.provider, Provider::Ollama));
        assert_eq!(kind, ResolutionKind::Flat);
    }

    #[test]
    fn resolve_context_target_wins_over_routes() {
        // Resolution precedence: ai.target context > routes > default > flat.
        host::reset();
        host::set_context("ai.target", "named");
        let mut targets = BTreeMap::new();
        targets.insert("named".to_string(), target_with(Provider::Anthropic));
        let mut plugin = AiProxy {
            routes: vec![route("*", Provider::Ollama)],
            targets,
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let (target, kind) = assert_resolved(plugin.resolve_target("anything"));
        assert!(matches!(target.provider, Provider::Anthropic));
        assert_eq!(kind, ResolutionKind::Context);
    }

    #[test]
    fn ensure_compiled_routes_surfaces_invalid_glob() {
        let mut plugin = AiProxy {
            routes: vec![route("[unclosed", Provider::OpenAI)],
            ..plugin_with()
        };
        let err = plugin.ensure_compiled_routes().unwrap_err();
        assert!(
            err.contains("routes[0].pattern"),
            "error should name the bad route: {}",
            err
        );
    }

    // --- catalog policy: allow / deny on the resolved target ---

    fn target_with_allow_deny(
        provider: Provider,
        allow: Vec<&str>,
        deny: Vec<&str>,
    ) -> TargetConfig {
        TargetConfig {
            allow: allow.into_iter().map(String::from).collect(),
            deny: deny.into_iter().map(String::from).collect(),
            ..target_with(provider)
        }
    }

    #[test]
    fn catalog_policy_allow_pass() {
        let t = target_with_allow_deny(Provider::OpenAI, vec!["gpt-4o", "gpt-4o-mini"], vec![]);
        assert!(evaluate_catalog_policy(&t, "gpt-4o").is_none());
    }

    #[test]
    fn catalog_policy_allow_reject() {
        let t = target_with_allow_deny(Provider::OpenAI, vec!["gpt-4o", "gpt-4o-mini"], vec![]);
        assert!(matches!(
            evaluate_catalog_policy(&t, "gpt-3.5-turbo"),
            Some(PolicyDenial::NotInAllow)
        ));
    }

    #[test]
    fn catalog_policy_deny_pass() {
        let t = target_with_allow_deny(Provider::Anthropic, vec![], vec!["claude-opus-*"]);
        assert!(evaluate_catalog_policy(&t, "claude-sonnet-4-6").is_none());
    }

    #[test]
    fn catalog_policy_deny_match() {
        let t = target_with_allow_deny(Provider::Anthropic, vec![], vec!["claude-opus-*"]);
        assert!(matches!(
            evaluate_catalog_policy(&t, "claude-opus-4-6"),
            Some(PolicyDenial::Denied)
        ));
    }

    #[test]
    fn catalog_policy_deny_evaluated_after_allow() {
        // ADR-0030 §3: combine allow + deny; deny is evaluated after allow.
        let t =
            target_with_allow_deny(Provider::Anthropic, vec!["claude-*"], vec!["claude-opus-*"]);
        // claude-sonnet-* passes allow, doesn't hit deny → ok.
        assert!(evaluate_catalog_policy(&t, "claude-sonnet-4-6").is_none());
        // claude-opus-* passes allow but is then denied → 403.
        assert!(matches!(
            evaluate_catalog_policy(&t, "claude-opus-4-6"),
            Some(PolicyDenial::Denied)
        ));
        // Doesn't even pass allow → NotInAllow.
        assert!(matches!(
            evaluate_catalog_policy(&t, "gpt-4o"),
            Some(PolicyDenial::NotInAllow)
        ));
    }

    // --- end-to-end dispatch tests for the new responses ---

    #[test]
    fn dispatch_400_no_route_when_routes_set_but_no_match() {
        host::reset();
        let mut plugin = plugin_with_routes(vec![route("claude-*", Provider::Anthropic)]);
        let resp = plugin.dispatch(make_chat_request("mistral"));
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"].as_str(), Some("no_route"));
        assert_eq!(body["type"].as_str(), Some("urn:barbacane:error:no_route"));
    }

    #[test]
    fn dispatch_403_when_route_denies_model() {
        host::reset();
        let mut plugin = AiProxy {
            routes: vec![Route {
                deny: vec!["claude-opus-*".to_string()],
                ..route("claude-*", Provider::Anthropic)
            }],
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let resp = plugin.dispatch(make_chat_request("claude-opus-4-6"));
        assert_eq!(resp.status, 403);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"].as_str(), Some("model_not_permitted"));
        assert_eq!(
            body["type"].as_str(),
            Some("urn:barbacane:error:model_not_permitted")
        );
    }

    #[test]
    fn dispatch_403_does_not_fall_through_to_next_route() {
        // ADR-0030 §3: a denied model returns 403 and does NOT fall through
        // to the next route — that would silently escalate to a different
        // provider. The escape hatch is to tighten the route's pattern.
        host::reset();
        let mut plugin = AiProxy {
            routes: vec![
                Route {
                    allow: vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
                    ..route("gpt-*", Provider::OpenAI)
                },
                route("*", Provider::Ollama),
            ],
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let resp = plugin.dispatch(make_chat_request("gpt-3.5-turbo"));
        assert_eq!(resp.status, 403);
        // Did NOT fall through to the catch-all ollama route.
    }

    #[test]
    fn dispatch_403_when_ai_target_resolves_to_a_target_whose_deny_fires() {
        // ADR-0030 §3 subtlety: catalog policy is attached to the target, not
        // to the resolution path. A `cel` misconfig that sets ai.target to a
        // target whose `deny` covers the request's model still gets 403 —
        // catalog policy applies on every resolution path.
        host::reset();
        host::set_context("ai.target", "anthropic-tier");

        let mut targets = BTreeMap::new();
        targets.insert(
            "anthropic-tier".to_string(),
            target_with_allow_deny(Provider::Anthropic, vec![], vec!["claude-opus-*"]),
        );
        let mut plugin = AiProxy {
            targets,
            ..plugin_with()
        };
        plugin.ensure_compiled_routes().expect("globs compile");
        let resp = plugin.dispatch(make_chat_request("claude-opus-4-6"));
        assert_eq!(resp.status, 403);
    }

    #[test]
    fn dispatch_resolution_metric_label_set_on_each_path() {
        // Quick spot-check that the metric is emitted on a successful resolution.
        host::reset();
        let mut plugin = openai_plugin("openai");
        let _ = plugin.dispatch(make_chat_request("gpt-4o"));
        let counters = host::get_counters();
        let label = counters
            .iter()
            .find(|(name, _, _)| name == "resolution_total")
            .expect("resolution_total counter recorded");
        assert!(
            label.1.contains("\"resolution\":\"flat\""),
            "expected resolution=flat label, got {}",
            label.1
        );
    }

    // --- Streaming detection ---

    #[test]
    fn streaming_detection_true() {
        assert!(is_streaming_request(&Some(
            br#"{"stream":true,"messages":[]}"#.to_vec()
        )));
    }

    #[test]
    fn streaming_detection_false() {
        assert!(!is_streaming_request(&Some(
            br#"{"stream":false,"messages":[]}"#.to_vec()
        )));
    }

    #[test]
    fn streaming_detection_absent() {
        assert!(!is_streaming_request(&Some(br#"{"messages":[]}"#.to_vec())));
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
        let result = translate_to_anthropic(
            &Some(body.as_bytes().to_vec()),
            "claude-opus-4-6",
            false,
            None,
        )
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
        let result = translate_to_anthropic(
            &Some(body.as_bytes().to_vec()),
            "claude-opus-4-6",
            false,
            None,
        )
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
        let result = translate_to_anthropic(&Some(body.as_bytes().to_vec()), "m", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["system"].as_str(), Some("Part one.\nPart two."));
    }

    #[test]
    fn translate_to_anthropic_uses_default_max_tokens() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result =
            translate_to_anthropic(&Some(body.as_bytes().to_vec()), "m", false, Some(2048))
                .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(2048));
    }

    #[test]
    fn translate_to_anthropic_fallback_max_tokens_4096() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result = translate_to_anthropic(&Some(body.as_bytes().to_vec()), "m", false, None)
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
        let result = translate_to_anthropic(&Some(body.as_bytes().to_vec()), "m", false, None)
            .expect("should translate");
        let v: serde_json::Value = serde_json::from_str(&result).expect("valid json");
        assert!((v["temperature"].as_f64().unwrap() - 0.7).abs() < 0.001);
        assert!((v["top_p"].as_f64().unwrap() - 0.9).abs() < 0.001);
    }

    #[test]
    fn translate_to_anthropic_stream_flag() {
        let body = r#"{"messages":[{"role":"user","content":"hi"}]}"#;
        let result = translate_to_anthropic(&Some(body.as_bytes().to_vec()), "m", true, None)
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
        assert_eq!(
            choices[0]["message"]["content"].as_str(),
            Some("Hello there!")
        );
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
            max_tokens: Some(2048),
            ..plugin_with()
        };
        let body = Some(br#"{"messages":[]}"#.to_vec());
        let result = plugin.maybe_inject_max_tokens(&body).expect("body");
        let v: serde_json::Value = serde_json::from_slice(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(2048));
    }

    #[test]
    fn inject_max_tokens_skipped_when_present() {
        let plugin = AiProxy {
            provider: Some(Provider::OpenAI),
            max_tokens: Some(2048),
            ..plugin_with()
        };
        let body = Some(br#"{"messages":[],"max_tokens":512}"#.to_vec());
        let result = plugin.maybe_inject_max_tokens(&body).expect("body");
        let v: serde_json::Value = serde_json::from_slice(&result).expect("valid json");
        assert_eq!(v["max_tokens"].as_u64(), Some(512)); // client value preserved
    }

    // --- dispatch returns 500 when no provider configured ---

    #[test]
    fn dispatch_500_when_no_provider() {
        host::reset();
        let mut plugin = plugin_with();
        let resp = plugin.dispatch(make_chat_request("gpt-4o"));
        assert_eq!(resp.status, 500);
    }

    // --- dispatch returns 502 when host_http_call fails (native stub) ---

    #[test]
    fn dispatch_502_on_connection_failure() {
        host::reset();
        let mut plugin = openai_plugin("openai");
        let resp = plugin.dispatch(make_chat_request("gpt-4o"));
        // Native stub returns -1, so all targets fail → 502
        assert_eq!(resp.status, 502);
    }

    #[test]
    fn dispatch_502_anthropic_on_connection_failure() {
        host::reset();
        let mut plugin = openai_plugin("anthropic");
        let resp = plugin.dispatch(make_chat_request("claude-opus-4-6"));
        assert_eq!(resp.status, 502);
    }

    // --- Anthropic streaming forces non-streaming and logs warning ---

    #[test]
    fn anthropic_streaming_logs_warning() {
        host::reset();
        let mut plugin = openai_plugin("anthropic");
        let req = make_request(Some(
            r#"{"model":"claude-opus-4-6","messages":[{"role":"user","content":"hi"}],"stream":true}"#,
        ));
        let _ = plugin.dispatch(req);
        let warnings = host::get_warnings();
        assert!(warnings.iter().any(|w| w.contains("buffering")));
    }

    // --- Caller-owned model (ADR-0030 §0) ---

    #[test]
    fn dispatch_400_when_body_missing_model() {
        host::reset();
        let mut plugin = openai_plugin("openai");
        // No `model` field — the gateway never picks a default.
        let req = make_request(Some(r#"{"messages":[{"role":"user","content":"hi"}]}"#));
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"].as_str(), Some("model_required"));
        assert_eq!(
            body["type"].as_str(),
            Some("urn:barbacane:error:model_required")
        );
    }

    #[test]
    fn dispatch_400_when_model_is_empty_string() {
        host::reset();
        let mut plugin = openai_plugin("openai");
        let req = make_request(Some(r#"{"model":"","messages":[]}"#));
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"].as_str(), Some("model_required"));
    }

    #[test]
    fn dispatch_400_when_body_is_missing_entirely() {
        host::reset();
        let mut plugin = openai_plugin("openai");
        let req = make_request(None);
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 400);
    }

    #[test]
    fn extract_client_model_helper() {
        assert_eq!(
            extract_client_model(&Some(br#"{"model":"gpt-4o"}"#.to_vec())),
            Some("gpt-4o".to_string())
        );
        assert_eq!(
            extract_client_model(&Some(br#"{"model":""}"#.to_vec())),
            None
        );
        assert_eq!(
            extract_client_model(&Some(br#"{"model":42}"#.to_vec())),
            None
        );
        assert_eq!(
            extract_client_model(&Some(br#"{"messages":[]}"#.to_vec())),
            None
        );
        assert_eq!(extract_client_model(&Some(b"not-json".to_vec())), None);
        assert_eq!(extract_client_model(&None), None);
    }

    // --- Path-based dispatch (PR-1: only /v1/chat/completions; others 404) ---

    #[test]
    fn dispatch_unknown_path_returns_404() {
        host::reset();
        let mut plugin = openai_plugin("openai");
        let mut req = make_chat_request("gpt-4o");
        req.path = "/v1/something-else".to_string();
        let resp = plugin.dispatch(req);
        assert_eq!(resp.status, 404);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"].as_str(), Some("urn:barbacane:error:not-found"));
    }

    // --- Error response format ---

    #[test]
    fn error_response_502_format() {
        let resp = error_response(502, "all providers failed");
        assert_eq!(resp.status, 502);
        assert_eq!(
            resp.headers.get("content-type").map(|s| s.as_str()),
            Some("application/problem+json")
        );
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(
            body["type"].as_str(),
            Some("urn:barbacane:error:upstream-unavailable")
        );
        assert_eq!(body["status"].as_u64(), Some(502));
    }

    #[test]
    fn error_response_500_format() {
        let resp = error_response(500, "misconfiguration");
        assert_eq!(resp.status, 500);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["type"].as_str(), Some("urn:barbacane:error:internal"));
    }

    // --- Labels helpers ---

    #[test]
    fn labels1_format() {
        assert_eq!(labels1("provider", "openai"), r#"{"provider":"openai"}"#);
    }

    #[test]
    fn labels2_format() {
        assert_eq!(
            labels2("provider", "openai", "status", "200"),
            r#"{"provider":"openai","status":"200"}"#
        );
    }

    // --- Context propagation ---

    #[test]
    fn propagate_context_sets_provider_and_model() {
        host::reset();
        let target = target_with(Provider::OpenAI);
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(
                br#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#
                    .to_vec(),
            ),
        };
        propagate_context(&target, "gpt-4o", &resp);

        let ctx = host::get_context();
        assert_eq!(ctx.get("ai.provider").map(|s| s.as_str()), Some("openai"));
        // ai.model is the client-supplied model (ADR-0030 §0), not target-derived.
        assert_eq!(ctx.get("ai.model").map(|s| s.as_str()), Some("gpt-4o"));
        assert_eq!(ctx.get("ai.prompt_tokens").map(|s| s.as_str()), Some("10"));
        assert_eq!(
            ctx.get("ai.completion_tokens").map(|s| s.as_str()),
            Some("20")
        );
    }

    #[test]
    fn propagate_context_skips_tokens_for_streamed_response() {
        host::reset();
        let target = target_with(Provider::Ollama);
        let resp = streamed_response(); // status = 0
        propagate_context(&target, "mistral", &resp);

        let ctx = host::get_context();
        assert_eq!(ctx.get("ai.provider").map(|s| s.as_str()), Some("ollama"));
        assert_eq!(ctx.get("ai.model").map(|s| s.as_str()), Some("mistral"));
        assert!(!ctx.contains_key("ai.prompt_tokens"));
        assert!(!ctx.contains_key("ai.completion_tokens"));
    }

    #[test]
    fn propagate_context_records_token_metrics() {
        host::reset();
        let target = target_with(Provider::Anthropic);
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(
                br#"{"usage":{"prompt_tokens":5,"completion_tokens":15,"total_tokens":20}}"#
                    .to_vec(),
            ),
        };
        propagate_context(&target, "claude-opus-4-6", &resp);

        let counters = host::get_counters();
        let prompt_counter = counters
            .iter()
            .find(|(name, labels, _)| name == "tokens_total" && labels.contains("prompt"));
        let completion_counter = counters
            .iter()
            .find(|(name, labels, _)| name == "tokens_total" && labels.contains("completion"));
        assert!(
            prompt_counter.is_some(),
            "prompt tokens counter should be recorded"
        );
        assert_eq!(prompt_counter.unwrap().2, 5);
        assert!(
            completion_counter.is_some(),
            "completion tokens counter should be recorded"
        );
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
        assert_eq!(
            Provider::OpenAI.default_base_url(),
            "https://api.openai.com"
        );
        assert_eq!(
            Provider::Anthropic.default_base_url(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            Provider::Ollama.default_base_url(),
            "http://localhost:11434"
        );
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
            base_url: Some("https://my-azure.openai.com".to_string()),
            ..target_with(Provider::OpenAI)
        };
        assert_eq!(t.effective_base_url(), "https://my-azure.openai.com");
    }

    #[test]
    fn target_effective_base_url_default() {
        let t = target_with(Provider::Anthropic);
        assert_eq!(t.effective_base_url(), "https://api.anthropic.com");
    }
}
