//! OpenAI Models API protocol adapter (ADR-0030 §4).
//!
//! Path: `GET /v1/models`. Aggregates the model catalog across every unique
//! provider declared in the dispatcher's config — `routes` entries, the
//! `targets` map, and the flat `provider`. Each upstream's `/v1/models` is
//! queried (or `/api/tags` for Ollama, then translated). Returns an
//! OpenAI-compatible `{ object: "list", data: [...] }` payload.
//!
//! On per-provider failure (5xx, timeout, connection error), the dispatcher
//! returns a **partial response**: 200 OK with the available providers'
//! models in `data[]`, plus `partial: true` and a `warnings: [{provider,
//! status}]` array describing each failure. A 4xx aggregator-wide response
//! would force a discovery client to retry instead of degrading gracefully —
//! ADR-0030 §4 calls this out explicitly. Each failure also increments the
//! `barbacane_plugin_ai_proxy_models_provider_failures_total{provider}`
//! counter so operators see degradations without polling clients.
//!
//! ## Out of scope (deferred)
//!
//! - **Caching.** ADR-0030 §4 specifies a per-instance cache via the
//!   `host_cache_*` capability (default TTL 5 min) plus single-flight against
//!   thundering-herd. v1 hits upstream on every call — the endpoint is not
//!   on the data plane critical path (clients don't call `/v1/models`
//!   per-request), so cache adds complexity without immediate value.
//! - **Filter by route `allow`/`deny`.** v1 returns whatever the upstream
//!   returns; filtering is ergonomic, not security (denied models still 403
//!   on actual dispatch). Future PR.
//! - **`schemas/ai-gateway.yaml` spec fragment** that operators drop into
//!   their `specs/` folder. Tracked as PR-6 in the implementation plan.

use crate::providers::openai::openai_base_headers;
use crate::{build_response, host, http_call, AiProxy, Auth, HttpRequest, Provider, Response};
use barbacane_plugin_sdk::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Per-protocol entry point
// ---------------------------------------------------------------------------

pub(crate) fn handle(plugin: &AiProxy, req: &Request) -> Response {
    if !req.method.eq_ignore_ascii_case("GET") {
        return method_not_allowed_response();
    }

    let providers = collect_unique_providers(plugin);
    if providers.is_empty() {
        return crate::error_response(
            500,
            "ai-proxy misconfiguration: no provider configured (set `provider`, `routes`, or define `targets`)",
        );
    }

    let mut data: Vec<serde_json::Value> = Vec::new();
    let mut warnings: Vec<serde_json::Value> = Vec::new();

    for upstream in providers {
        match fetch_provider_models(plugin, &upstream) {
            Ok(items) => data.extend(items),
            Err(err) => {
                host::metric_counter_inc(
                    "models_provider_failures_total",
                    &crate::labels1("provider", upstream.provider.name()),
                    1,
                );
                host::log_warn(&format!(
                    "ai-proxy: /v1/models failed for {} at {} ({})",
                    upstream.provider.name(),
                    upstream.base_url,
                    err.detail
                ));
                warnings.push(serde_json::json!({
                    "provider": upstream.provider.name(),
                    "status": err.status,
                    "detail": err.detail,
                }));
            }
        }
    }

    let mut body = serde_json::json!({
        "object": "list",
        "data": data,
    });
    if !warnings.is_empty() {
        let obj = body.as_object_mut().expect("object literal");
        obj.insert("partial".to_string(), serde_json::Value::Bool(true));
        obj.insert("warnings".to_string(), serde_json::Value::Array(warnings));
    }

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    Response {
        status: 200,
        headers,
        body: Some(serde_json::to_vec(&body).unwrap_or_default()),
    }
}

// ---------------------------------------------------------------------------
// Provider collection — dedupe across routes + targets + flat
// ---------------------------------------------------------------------------

/// One upstream the aggregator will query. Two routes pointing at the same
/// `(provider, base_url)` produce one [`UpstreamProvider`].
///
/// The dedup key is `(provider, base_url)` only — `api_key` is intentionally
/// excluded. A model list isn't sensitive to which key called it (the same
/// upstream account returns the same catalog regardless of which key
/// authenticates). The trade-off: in multi-tenant configurations where
/// different routes deliberately use different keys against the same
/// upstream account (e.g. for billing splits), the aggregator picks
/// whichever key sorted first into the dedup. If that key is revoked or
/// rate-limited and the other isn't, `/v1/models` warnings will name the
/// provider but not the offending key — operators have to correlate via
/// the upstream's own logs. Acceptable for v1; the partial-response shape
/// makes the failure visible without breaking the aggregator.
#[derive(Clone, Debug)]
struct UpstreamProvider {
    provider: Provider,
    base_url: String,
    api_key: Option<String>,
    auth: Option<Auth>,
}

fn collect_unique_providers(plugin: &AiProxy) -> Vec<UpstreamProvider> {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out: Vec<UpstreamProvider> = Vec::new();

    let push = |provider: &Provider,
                api_key: Option<&str>,
                base_url: Option<&str>,
                auth: Option<&Auth>,
                seen: &mut BTreeSet<(String, String)>,
                out: &mut Vec<UpstreamProvider>| {
        let resolved_base = base_url
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| provider.default_base_url().to_string());
        let key = (provider.name().to_string(), resolved_base.clone());
        if seen.insert(key) {
            out.push(UpstreamProvider {
                provider: provider.clone(),
                base_url: resolved_base,
                api_key: api_key.map(String::from),
                auth: auth.cloned(),
            });
        }
    };

    for route in &plugin.routes {
        push(
            &route.provider,
            route.api_key.as_deref(),
            route.base_url.as_deref(),
            route.auth.as_ref(),
            &mut seen,
            &mut out,
        );
    }
    for target in plugin.targets.values() {
        push(
            &target.provider,
            target.api_key.as_deref(),
            target.base_url.as_deref(),
            target.auth.as_ref(),
            &mut seen,
            &mut out,
        );
    }
    if let Some(p) = plugin.provider.as_ref() {
        push(
            p,
            plugin.api_key.as_deref(),
            plugin.base_url.as_deref(),
            plugin.auth.as_ref(),
            &mut seen,
            &mut out,
        );
    }

    out
}

// ---------------------------------------------------------------------------
// Per-provider HTTP fetch
// ---------------------------------------------------------------------------

/// Failure surfaced into the aggregator's `warnings[]` array. `status: 0`
/// means the connection itself failed (no HTTP response received).
struct UpstreamFailure {
    status: u16,
    detail: String,
}

fn fetch_provider_models(
    plugin: &AiProxy,
    upstream: &UpstreamProvider,
) -> Result<Vec<serde_json::Value>, UpstreamFailure> {
    let base = upstream.base_url.trim_end_matches('/');
    let (mut url, mut headers) = match upstream.provider {
        Provider::OpenAI => (format!("{}/v1/models", base), openai_base_headers()),
        Provider::Anthropic => {
            let mut h = BTreeMap::new();
            h.insert("content-type".to_string(), "application/json".to_string());
            h.insert(
                "anthropic-version".to_string(),
                crate::providers::anthropic::ANTHROPIC_API_VERSION.to_string(),
            );
            (format!("{}/v1/models", base), h)
        }
        // Ollama has no `/v1/models`; the OpenAI-compat surface uses
        // `/api/tags`, which we translate to OpenAI list shape below. It's an
        // unauthenticated local endpoint, so no credential is attached.
        Provider::Ollama => (format!("{}/api/tags", base), BTreeMap::new()),
    };
    if !matches!(upstream.provider, Provider::Ollama) {
        if let Some(key) = upstream.api_key.as_deref() {
            crate::providers::apply_auth(
                &synthetic_target(upstream).effective_auth(),
                key,
                &mut headers,
                &mut url,
            );
        }
    }

    let req = HttpRequest {
        method: "GET".to_string(),
        url,
        headers,
        // Use the dedicated discovery timeout — `plugin.timeout` is sized
        // for LLM completions (default 120 s), which is far too patient
        // for a model-catalog GET. With the default 5 s here, a single
        // hung upstream caps its contribution to the aggregate latency
        // at 5 s instead of two minutes.
        timeout_ms: Some(plugin.models_timeout_ms),
    };

    let raw = http_call(&req).map_err(|e| UpstreamFailure {
        status: 0,
        detail: e,
    })?;
    let resp = build_response(raw);

    if !(200..300).contains(&resp.status) {
        return Err(UpstreamFailure {
            status: resp.status,
            detail: format!("upstream returned {}", resp.status),
        });
    }

    let body_str = resp.body_str().unwrap_or("");
    let body: serde_json::Value = serde_json::from_str(body_str).map_err(|e| UpstreamFailure {
        status: resp.status,
        detail: format!("invalid JSON from upstream: {}", e),
    })?;

    Ok(translate_models_response(upstream.provider.clone(), &body))
}

/// Build a synthetic [`crate::TargetConfig`] from an [`UpstreamProvider`] so
/// we can reuse [`crate::providers::apply_auth`] without duplicating the
/// auth-attachment logic.
fn synthetic_target(upstream: &UpstreamProvider) -> crate::TargetConfig {
    crate::TargetConfig {
        provider: upstream.provider.clone(),
        api_key: upstream.api_key.clone(),
        base_url: Some(upstream.base_url.clone()),
        auth: upstream.auth.clone(),
        allow: Vec::new(),
        deny: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Response translation — Ollama needs reshaping; OpenAI/Anthropic are native
// ---------------------------------------------------------------------------

pub(crate) fn translate_models_response(
    provider: Provider,
    body: &serde_json::Value,
) -> Vec<serde_json::Value> {
    match provider {
        Provider::OpenAI | Provider::Anthropic => body
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|item| {
                // Stamp owned_by from the provider name — Anthropic's own
                // `owned_by` is sometimes "anthropic", sometimes elided.
                // Normalize so clients can group consistently.
                let mut o = item;
                if let Some(obj) = o.as_object_mut() {
                    obj.entry("owned_by".to_string())
                        .or_insert_with(|| serde_json::Value::String(provider.name().to_string()));
                }
                o
            })
            .collect(),
        Provider::Ollama => body
            .get("models")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| {
                let name = m.get("name").and_then(|v| v.as_str())?;
                Some(serde_json::json!({
                    "id": name,
                    "object": "model",
                    "owned_by": "ollama",
                }))
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// problem+json error helpers
// ---------------------------------------------------------------------------

fn method_not_allowed_response() -> Response {
    let mut resp = ProblemDetails::new(
        405,
        "urn:barbacane:error:method_not_allowed",
        "Method Not Allowed",
    )
    .detail("ai-proxy: /v1/models accepts GET only.")
    .with("code", "method_not_allowed")
    .into_response();
    resp.headers.insert("allow".to_string(), "GET".to_string());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Route, TargetConfig};

    fn empty_plugin() -> AiProxy {
        AiProxy {
            provider: None,
            api_key: None,
            base_url: None,
            auth: None,
            timeout: 120,
            models_timeout_ms: 5_000,
            max_tokens: None,
            fallback: vec![],
            routes: vec![],
            compiled_routes: None,
            targets: BTreeMap::new(),
            default_target: None,
        }
    }

    fn target_with(provider: Provider, base_url: Option<&str>) -> TargetConfig {
        TargetConfig {
            provider,
            api_key: None,
            base_url: base_url.map(String::from),
            auth: None,
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }

    fn route_with(provider: Provider, base_url: &str) -> Route {
        Route {
            pattern: "*".to_string(),
            provider,
            api_key: None,
            base_url: Some(base_url.to_string()),
            auth: None,
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }

    // --- collect_unique_providers ---

    #[test]
    fn collect_dedupes_by_provider_and_base_url() {
        let mut p = empty_plugin();
        p.routes = vec![
            route_with(Provider::OpenAI, "https://api.openai.com"),
            route_with(Provider::OpenAI, "https://api.openai.com"), // duplicate
        ];
        let out = collect_unique_providers(&p);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0].provider, Provider::OpenAI));
    }

    #[test]
    fn collect_keeps_separate_when_base_urls_differ() {
        let mut p = empty_plugin();
        p.routes = vec![
            route_with(Provider::OpenAI, "https://api.openai.com"),
            route_with(Provider::OpenAI, "https://my-azure.openai.com"),
        ];
        let out = collect_unique_providers(&p);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn collect_walks_routes_targets_flat_in_order() {
        let mut p = empty_plugin();
        p.routes = vec![route_with(Provider::Anthropic, "https://api.anthropic.com")];
        p.targets.insert(
            "ollama".to_string(),
            target_with(Provider::Ollama, Some("http://ollama.internal:11434")),
        );
        p.provider = Some(Provider::OpenAI);
        let out = collect_unique_providers(&p);
        assert_eq!(out.len(), 3);
        // Routes first, then targets, then flat.
        assert!(matches!(out[0].provider, Provider::Anthropic));
        assert!(matches!(out[1].provider, Provider::Ollama));
        assert!(matches!(out[2].provider, Provider::OpenAI));
    }

    #[test]
    fn collect_uses_default_base_url_when_unset() {
        let mut p = empty_plugin();
        p.provider = Some(Provider::OpenAI);
        let out = collect_unique_providers(&p);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base_url, "https://api.openai.com");
    }

    #[test]
    fn collect_returns_empty_when_no_providers() {
        let p = empty_plugin();
        assert!(collect_unique_providers(&p).is_empty());
    }

    // --- translate_models_response ---

    #[test]
    fn translate_openai_passes_through_data_array() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-4o", "object": "model"},
                {"id": "gpt-4o-mini", "object": "model"},
            ]
        });
        let out = translate_models_response(Provider::OpenAI, &body);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], "gpt-4o");
        assert_eq!(out[0]["owned_by"], "openai");
    }

    #[test]
    fn translate_anthropic_stamps_owned_by_from_provider_name() {
        let body = serde_json::json!({
            "data": [{"id": "claude-sonnet-4-6"}]
        });
        let out = translate_models_response(Provider::Anthropic, &body);
        assert_eq!(out[0]["owned_by"], "anthropic");
    }

    #[test]
    fn translate_anthropic_preserves_existing_owned_by_if_present() {
        let body = serde_json::json!({
            "data": [{"id": "claude", "owned_by": "anthropic-research"}]
        });
        let out = translate_models_response(Provider::Anthropic, &body);
        assert_eq!(out[0]["owned_by"], "anthropic-research");
    }

    #[test]
    fn translate_ollama_reshapes_api_tags_to_openai_list() {
        let body = serde_json::json!({
            "models": [
                {"name": "llama3", "size": 1234567},
                {"name": "mistral", "size": 7654321},
            ]
        });
        let out = translate_models_response(Provider::Ollama, &body);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], "llama3");
        assert_eq!(out[0]["object"], "model");
        assert_eq!(out[0]["owned_by"], "ollama");
        assert!(out[0].get("size").is_none(), "size should be dropped");
    }

    #[test]
    fn translate_handles_missing_data_array_gracefully() {
        let body = serde_json::json!({});
        assert!(translate_models_response(Provider::OpenAI, &body).is_empty());
        assert!(translate_models_response(Provider::Anthropic, &body).is_empty());
        assert!(translate_models_response(Provider::Ollama, &body).is_empty());
    }

    #[test]
    fn translate_handles_empty_arrays() {
        // Edge case: an operator just installed Ollama with no models pulled,
        // or an OpenAI account with `data: []`. The aggregator must handle
        // the empty list cleanly (no models contributed, but also no warning
        // — the upstream succeeded, it just has nothing to advertise).
        let openai_empty = serde_json::json!({"object": "list", "data": []});
        assert!(translate_models_response(Provider::OpenAI, &openai_empty).is_empty());

        let ollama_empty = serde_json::json!({"models": []});
        assert!(translate_models_response(Provider::Ollama, &ollama_empty).is_empty());
    }

    // --- handle / dispatch shape ---

    #[test]
    fn handle_returns_405_for_non_get_methods() {
        let plugin = empty_plugin();
        let req = Request {
            method: "POST".to_string(),
            path: "/v1/models".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };
        let resp = handle(&plugin, &req);
        assert_eq!(resp.status, 405);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"], "method_not_allowed");
        assert_eq!(resp.headers.get("allow").map(|s| s.as_str()), Some("GET"));
    }

    #[test]
    fn handle_returns_500_when_no_providers_configured() {
        let plugin = empty_plugin();
        let req = Request {
            method: "GET".to_string(),
            path: "/v1/models".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };
        let resp = handle(&plugin, &req);
        assert_eq!(resp.status, 500);
    }
}
