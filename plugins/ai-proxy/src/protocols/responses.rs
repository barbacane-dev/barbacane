//! OpenAI Responses API protocol adapter (ADR-0030 §2).
//!
//! Path: `POST /v1/responses`. Stateless only — the gateway never persists
//! conversation state, so:
//! - `previous_response_id` is rejected with 400 `previous_response_id_not_supported`.
//! - `store` is permissive: `true` / `false` / absent all flow through.
//!   `store ≠ false` adds a `Warning: 299` header and increments a counter
//!   (`barbacane_plugin_ai_proxy_responses_store_downgrades_total`) so
//!   operators can quantify stateful-API usage.
//! - The synthetic `id` on the response is `resp_<uuid-v7>` — time-ordered
//!   so logs grep chronologically without a separate sort key.
//!
//! Per-provider behavior:
//! - **OpenAI**: passthrough at `/v1/responses` upstream.
//! - **Anthropic**: translate `input[]` items ↔ Messages API `content` blocks.
//! - **Ollama**: 400 `responses_not_supported_for_provider` (Ollama's
//!   OpenAI-compat surface is Chat Completions only as of 2026-04).
//!
//! Streaming: SSE is buffered into a single terminal event for the Anthropic
//! path (mirrors ADR-0024 Chat Completions until true SSE translation lands).
//! The OpenAI passthrough streams normally via `host_http_stream`.

use crate::providers::openai::{openai_base_headers, openai_url};
use crate::{
    error_response, host, host_http_stream, http_call, AiProxy, HttpRequest, Provider, Response,
    TargetConfig,
};
use barbacane_plugin_sdk::prelude::*;
use std::collections::BTreeMap;

/// Context key that carries the preflight `store_downgrade` flag from
/// `dispatch_responses` (where the body was first parsed) into [`handle`]
/// (where the response is built). Avoids a second parse of the inbound body
/// solely to recover this one boolean.
pub(crate) const CTX_STORE_DOWNGRADE: &str = "ai.responses.store_downgrade";

// ---------------------------------------------------------------------------
// Preflight: the cheap, pre-target-resolution checks
// ---------------------------------------------------------------------------

/// Spec-level validation that runs before target resolution. Reject
/// `previous_response_id` (the only genuinely stateful feature a client can
/// invoke) so operators see 400 at the surface where the client is trying to
/// use state the gateway can't provide.
pub(crate) struct ResponsesPreflight {
    /// True when the client sent `store: true` (or omitted it — server-side
    /// default per OpenAI). Used downstream to attach a `Warning: 299` header
    /// and increment the downgrade counter.
    pub store_downgrade: bool,
}

impl ResponsesPreflight {
    pub(crate) fn from_body(body: &Option<Vec<u8>>) -> Result<Self, Response> {
        let raw = body.as_deref().unwrap_or(b"{}");
        let v: serde_json::Value = match serde_json::from_slice(raw) {
            Ok(v) => v,
            // Malformed JSON — let downstream handlers handle their own
            // shape errors (the dispatch layer doesn't try to parse here).
            Err(_) => {
                return Ok(Self {
                    store_downgrade: false,
                })
            }
        };

        if v.get("previous_response_id").is_some()
            && !matches!(v.get("previous_response_id"), Some(serde_json::Value::Null))
        {
            return Err(previous_response_id_not_supported_response());
        }

        // OpenAI defaults `store` to true server-side. Treat `Some(true)` and
        // a missing field as downgrade-required; only an explicit `store: false`
        // skips the warning.
        let store_downgrade = !matches!(v.get("store"), Some(serde_json::Value::Bool(false)));

        Ok(Self { store_downgrade })
    }
}

// ---------------------------------------------------------------------------
// Per-protocol handler invoked by `dispatch_with_handler`
// ---------------------------------------------------------------------------

pub(crate) fn handle(
    plugin: &AiProxy,
    target: &TargetConfig,
    req: &Request,
    client_model: &str,
    streaming: bool,
) -> Result<Response, String> {
    match target.provider {
        Provider::OpenAI => openai_passthrough(plugin, target, req, streaming),
        Provider::Ollama => Ok(responses_not_supported_for_provider_response(
            Provider::Ollama,
        )),
        Provider::Anthropic => {
            // Parse the body for translation. The preflight already confirmed
            // `previous_response_id` is absent and stashed `store_downgrade`
            // on context (read back below), so this is the only parse we
            // need on the Anthropic path.
            let raw = req.body.as_deref().unwrap_or(b"{}");
            let body: serde_json::Value = serde_json::from_slice(raw)
                .map_err(|e| format!("invalid Responses request body: {}", e))?;

            let store_downgrade = host::context_get(CTX_STORE_DOWNGRADE)
                .map(|v| v == "true")
                .unwrap_or(true);

            let translation =
                ResponsesToAnthropic::translate(&body, client_model, streaming, plugin.max_tokens)?;

            // Buffered Anthropic call — true SSE translation deferred per
            // ADR-0030 §2; mirror the Chat Completions buffering behavior.
            if streaming {
                host::log_warn(
                    "ai-proxy: Anthropic streaming for Responses not yet supported; buffering response",
                );
            }
            let raw_resp =
                plugin.anthropic_messages_call_raw(target, translation.body.as_bytes())?;

            // 4xx/5xx pass through as-is (don't mangle upstream errors).
            if !(200..300).contains(&raw_resp.status) {
                return Ok(raw_resp);
            }

            let body_str = raw_resp.body_str().unwrap_or("").to_string();
            let translated = AnthropicToResponses::translate(&body_str, client_model)?;

            // Annotate the response with Warning headers + emit counters so
            // operators can quantify both "this client sends store: true" and
            // "this client sends reasoning items we dropped".
            let mut headers = raw_resp.headers;
            attach_warnings(
                &mut headers,
                store_downgrade,
                translation.dropped_reasoning_count > 0,
            );

            if store_downgrade {
                host::metric_counter_inc(
                    "responses_store_downgrades_total",
                    &crate::labels1("provider", target.provider.name()),
                    1,
                );
            }
            if translation.dropped_reasoning_count > 0 {
                host::metric_counter_inc(
                    "responses_reasoning_dropped_total",
                    &crate::labels1("provider", target.provider.name()),
                    translation.dropped_reasoning_count as u64,
                );
            }

            Ok(Response {
                status: raw_resp.status,
                headers,
                body: Some(translated.into_bytes()),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// OpenAI passthrough — the wire format already matches `/v1/responses`,
// so this is just an HTTP call against `${base_url}/v1/responses`.
// Streaming is delegated to `host_http_stream` like Chat Completions.
// ---------------------------------------------------------------------------

fn openai_passthrough(
    plugin: &AiProxy,
    target: &TargetConfig,
    req: &Request,
    streaming: bool,
) -> Result<Response, String> {
    let mut url = openai_url(target, &req.path);
    let mut headers = openai_base_headers();
    if let Some(key) = &target.api_key {
        crate::providers::apply_auth(&target.effective_auth(), key, &mut headers, &mut url);
    }
    if streaming {
        headers.insert("accept".to_string(), "text/event-stream".to_string());
    }

    if let Some(b) = req.body.as_ref() {
        set_http_request_body(b);
    }

    let http_req = HttpRequest {
        method: req.method.clone(),
        url,
        headers,
        timeout_ms: Some(plugin.timeout * 1000),
    };

    if streaming {
        // Known gap: ADR-0030 §2 requires the response `id` to be a synthetic
        // `resp_<uuid-v7>` so the gateway's stateless contract holds uniformly.
        // For non-streaming we rewrite the id post-call (below); for streaming
        // SSE the id is buried in `response.created` SSE event payloads which
        // we'd need to parse and rewrite mid-stream. True SSE handling is
        // already deferred for both protocols — see ADR-0030 §2 "Streaming".
        let req_json = serde_json::to_vec(&http_req).map_err(|e| e.to_string())?;
        let result = unsafe { host_http_stream(req_json.as_ptr() as i32, req_json.len() as i32) };
        if result < 0 {
            return Err("upstream stream failed".to_string());
        }
        Ok(streamed_response())
    } else {
        let resp_bytes = http_call(&http_req)?;
        let resp = crate::build_response(resp_bytes);
        // Stateless contract: rewrite the upstream `id` to a synthetic
        // `resp_<uuid-v7>`. Without this, OpenAI's real id leaks to the
        // client, who could then send it back as `previous_response_id`
        // and get 400 — the rejection lands consistently for every provider.
        Ok(rewrite_response_id_if_2xx(resp))
    }
}

/// On a 2xx upstream Responses payload, replace the `id` field with a
/// synthetic `resp_<uuid-v7>`. Non-2xx and unparseable bodies pass through
/// untouched (errors carry their own shape; we don't risk mangling them).
fn rewrite_response_id_if_2xx(resp: Response) -> Response {
    if !(200..300).contains(&resp.status) {
        return resp;
    }
    let body_bytes = match resp.body.as_ref() {
        Some(b) => b,
        None => return resp,
    };
    let mut v: serde_json::Value = match serde_json::from_slice(body_bytes) {
        Ok(v) => v,
        Err(_) => return resp,
    };
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "id".to_string(),
            serde_json::Value::String(format!("resp_{}", make_uuid_v7())),
        );
    } else {
        return resp;
    }
    Response {
        status: resp.status,
        headers: resp.headers,
        body: Some(serde_json::to_vec(&v).unwrap_or(body_bytes.clone())),
    }
}

// ---------------------------------------------------------------------------
// Responses → Anthropic translation
// ---------------------------------------------------------------------------

/// Result of translating a Responses-format request body into the equivalent
/// Anthropic Messages body.
pub(crate) struct ResponsesToAnthropic {
    /// JSON body to send to Anthropic's `/v1/messages`.
    pub body: String,
    /// Number of `reasoning` items that were dropped during translation.
    /// Anthropic doesn't accept client-supplied reasoning input — silently
    /// dropping them can degrade output quality on multi-turn agent flows,
    /// so the dispatcher emits a `Warning: 299` header and a counter
    /// whenever this is non-zero.
    pub dropped_reasoning_count: usize,
}

impl ResponsesToAnthropic {
    pub(crate) fn translate(
        responses: &serde_json::Value,
        client_model: &str,
        stream: bool,
        default_max_tokens: Option<u32>,
    ) -> Result<Self, String> {
        let input_items = responses
            .get("input")
            .and_then(|v| v.as_array())
            .ok_or("Responses request must include `input` array")?;

        let mut system_parts: Vec<String> = Vec::new();
        if let Some(instructions) = responses.get("instructions").and_then(|v| v.as_str()) {
            // Responses API's `instructions` field maps to Anthropic's
            // top-level `system` field.
            system_parts.push(instructions.to_string());
        }

        let mut messages: Vec<serde_json::Value> = Vec::new();
        let mut current_role: Option<String> = None;
        let mut current_blocks: Vec<serde_json::Value> = Vec::new();
        let mut dropped_reasoning_count = 0usize;

        for item in input_items {
            let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");

            match item_type {
                "input_text" | "" => {
                    // Plain message (no `type`) or explicitly `input_text`.
                    let content = item.get("content").or_else(|| item.get("text"));
                    let blocks = build_text_or_array_blocks(content);
                    if role == "system" {
                        // Hoist the text into Anthropic's system field.
                        for block in blocks {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                system_parts.push(t.to_string());
                            }
                        }
                    } else {
                        if current_role.as_deref() != Some(role) {
                            flush_message(
                                current_role.as_deref(),
                                &mut current_blocks,
                                &mut messages,
                            );
                            current_role = Some(role.to_string());
                        }
                        current_blocks.extend(blocks);
                    }
                }
                "input_image" => {
                    // `input_image` carries either `image_url` or base64
                    // bytes. Anthropic accepts both via the `image` block;
                    // we forward whatever we got and let the upstream tell
                    // the client if the format is wrong.
                    let block = build_image_block(item);
                    if current_role.as_deref() != Some(role) {
                        flush_message(current_role.as_deref(), &mut current_blocks, &mut messages);
                        current_role = Some(role.to_string());
                    }
                    current_blocks.push(block);
                }
                "function_call" => {
                    // Assistant-issued tool call → Anthropic `tool_use` block.
                    let tool_use = serde_json::json!({
                        "type": "tool_use",
                        "id": item.get("call_id").or_else(|| item.get("id")).cloned().unwrap_or(serde_json::Value::Null),
                        "name": item.get("name").cloned().unwrap_or(serde_json::Value::Null),
                        "input": item
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                            .unwrap_or(serde_json::Value::Object(Default::default())),
                    });
                    if current_role.as_deref() != Some("assistant") {
                        flush_message(current_role.as_deref(), &mut current_blocks, &mut messages);
                        current_role = Some("assistant".to_string());
                    }
                    current_blocks.push(tool_use);
                }
                "function_call_output" => {
                    // Client-provided tool result → Anthropic `tool_result` block.
                    let tool_result = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": item.get("call_id").cloned().unwrap_or(serde_json::Value::Null),
                        "content": item.get("output").cloned().unwrap_or(serde_json::Value::String(String::new())),
                    });
                    if current_role.as_deref() != Some("user") {
                        flush_message(current_role.as_deref(), &mut current_blocks, &mut messages);
                        current_role = Some("user".to_string());
                    }
                    current_blocks.push(tool_result);
                }
                "reasoning" => {
                    // Anthropic does not accept client-supplied reasoning input.
                    // Drop, count, and signal via Warning header + metric upstack.
                    dropped_reasoning_count += 1;
                }
                other => {
                    // Unknown item type — drop silently rather than fail.
                    // Forward-compatible: a future OpenAI item type that
                    // Barbacane doesn't know yet shouldn't break translation.
                    host::log_warn(&format!(
                        "ai-proxy: unknown Responses input item type {:?}; dropping",
                        other
                    ));
                }
            }
        }
        flush_message(current_role.as_deref(), &mut current_blocks, &mut messages);

        // Anthropic requires `max_tokens`. The translator falls back to the
        // dispatcher's default; if that's also unset, we use 4096 — same
        // floor as the Chat Completions translator.
        let max_tokens = responses
            .get("max_output_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or(default_max_tokens)
            .unwrap_or(4096);

        let mut anthropic = serde_json::Map::new();
        anthropic.insert(
            "model".to_string(),
            serde_json::Value::String(client_model.to_string()),
        );
        anthropic.insert("messages".to_string(), serde_json::Value::Array(messages));
        if !system_parts.is_empty() {
            anthropic.insert(
                "system".to_string(),
                serde_json::Value::String(system_parts.join("\n")),
            );
        }
        anthropic.insert(
            "max_tokens".to_string(),
            serde_json::Value::Number(max_tokens.into()),
        );
        if let Some(t) = responses.get("temperature").cloned() {
            anthropic.insert("temperature".to_string(), t);
        }
        if let Some(t) = responses.get("top_p").cloned() {
            anthropic.insert("top_p".to_string(), t);
        }
        if stream {
            anthropic.insert("stream".to_string(), serde_json::Value::Bool(true));
        }

        let body = serde_json::to_string(&serde_json::Value::Object(anthropic))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            body,
            dropped_reasoning_count,
        })
    }
}

/// Push a `{role, content}` message into `messages` if there are any blocks
/// buffered for the current role, and clear the buffer. Used by the
/// translator to flush out a message when the role changes.
fn flush_message(
    role: Option<&str>,
    blocks: &mut Vec<serde_json::Value>,
    messages: &mut Vec<serde_json::Value>,
) {
    let r = match role {
        Some(r) => r,
        None => return,
    };
    if blocks.is_empty() {
        return;
    }
    messages.push(serde_json::json!({
        "role": r,
        "content": std::mem::take(blocks),
    }));
}

/// Convert `input_text`'s `content` field into a vec of Anthropic text blocks.
/// Accepts either a plain string or the OpenAI array-of-parts shape.
fn build_text_or_array_blocks(value: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    match value {
        Some(serde_json::Value::String(s)) => {
            vec![serde_json::json!({ "type": "text", "text": s })]
        }
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                let pt = part
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("input_text");
                match pt {
                    "input_text" | "text" => part
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|t| serde_json::json!({ "type": "text", "text": t })),
                    "input_image" => Some(build_image_block(part)),
                    other => {
                        // Match the top-level item handler — unknown part-types
                        // log a warning rather than dropping silently. Future
                        // OpenAI part-types we don't know yet stay diagnosable
                        // without breaking translation.
                        host::log_warn(&format!(
                            "ai-proxy: unknown Responses content part type {:?}; dropping",
                            other
                        ));
                        None
                    }
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Build an Anthropic `image` content block from an OpenAI Responses
/// `input_image` item (or part). Forwards whatever URL/base64 form the client
/// sent — Anthropic returns its own error if the source isn't accepted.
fn build_image_block(part: &serde_json::Value) -> serde_json::Value {
    if let Some(url) = part.get("image_url").and_then(|v| v.as_str()) {
        return serde_json::json!({
            "type": "image",
            "source": { "type": "url", "url": url }
        });
    }
    serde_json::json!({
        "type": "image",
        "source": part.get("image").cloned().unwrap_or(serde_json::Value::Null),
    })
}

// ---------------------------------------------------------------------------
// Anthropic → Responses translation
// ---------------------------------------------------------------------------

struct AnthropicToResponses;

impl AnthropicToResponses {
    /// Translate a 2xx Anthropic Messages-format response body into an
    /// equivalent OpenAI Responses payload. Warning headers and counters for
    /// `store_downgrade` and dropped reasoning items are surfaced by the
    /// caller via [`attach_warnings`] / `metric_counter_inc`; nothing about
    /// them flows into the response body.
    fn translate(body: &str, client_model: &str) -> Result<String, String> {
        let anthropic: serde_json::Value =
            serde_json::from_str(body).map_err(|e| format!("invalid Anthropic response: {}", e))?;

        let mut output_items: Vec<serde_json::Value> = Vec::new();
        if let Some(content) = anthropic.get("content").and_then(|v| v.as_array()) {
            for block in content {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        output_items.push(serde_json::json!({
                            "type": "output_text",
                            "text": text,
                            "annotations": [],
                        }));
                    }
                    "tool_use" => {
                        output_items.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": block.get("id"),
                            "name": block.get("name"),
                            "arguments": block.get("input").map(|v| v.to_string()),
                        }));
                    }
                    other => {
                        // Anthropic-introduced block types we don't recognize:
                        // pass through under a generic `unknown` shape so the
                        // client at least sees something rather than dropping.
                        output_items.push(serde_json::json!({
                            "type": format!("unknown:{}", other),
                            "raw": block,
                        }));
                    }
                }
            }
        }

        let input_tokens = anthropic
            .get("usage")
            .and_then(|u| u.get("input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = anthropic
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        // ADR-0030 §2: synthetic id is uuid-v7 — time-ordered, opaque, non-
        // retrievable (the gateway is stateless). Built manually from the
        // host clock + a per-instance counter so we don't drag a wasm32-
        // unsupported RNG dep in for a non-cryptographic id.
        let id = format!("resp_{}", make_uuid_v7());

        let response = serde_json::json!({
            "id": id,
            "object": "response",
            "created_at": now_secs(),
            "status": "completed",
            "model": client_model,
            "output": output_items,
            "usage": {
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens,
            },
        });

        serde_json::to_string(&response).map_err(|e| e.to_string())
    }
}

fn now_secs() -> u64 {
    host::time_now_ms() / 1000
}

/// Build a UUID v7 (RFC 9562) using the host clock for the upper 48 timestamp
/// bits and a per-instance counter for the rand_a / rand_b portions.
///
/// The wasm32-unknown-unknown target has no system RNG, so a true random tail
/// would force pulling in `getrandom`'s JS backend (which doesn't exist in
/// the Barbacane runtime) or adding a `host_random_bytes` capability. We
/// don't need cryptographic randomness for a non-retrievable opaque
/// identifier — the v7 spec only requires monotonicity within a node, which
/// the counter satisfies.
///
/// **Scope.** The counter is per-plugin-instance and resets to 0 on instance
/// restart; it wraps with `wrapping_add` at 2^64. Two ids generated in the
/// same millisecond **by the same plugin instance** differ in their counter
/// portion. Two instances starting in the same millisecond can theoretically
/// collide on early ids, and a single instance running 1.8×10^19 requests
/// would collide on wrap — both inconsequential for a non-retrievable
/// opaque tracking handle, but worth knowing.
fn make_uuid_v7() -> uuid::Uuid {
    use std::cell::Cell;
    thread_local! {
        static COUNTER: Cell<u64> = const { Cell::new(0) };
    }

    let timestamp_ms = host::time_now_ms();
    let counter = COUNTER.with(|c| {
        let next = c.get().wrapping_add(1);
        c.set(next);
        next
    });

    let mut bytes = [0u8; 16];
    // bits 0..47 — timestamp in milliseconds (big-endian, 48 bits).
    bytes[0..6].copy_from_slice(&timestamp_ms.to_be_bytes()[2..8]);
    // bits 48..51 — version = 0b0111
    // bits 52..63 — rand_a (12 bits) — top 12 bits of the counter.
    bytes[6] = 0x70 | ((counter >> 8) & 0x0F) as u8;
    bytes[7] = (counter & 0xFF) as u8;
    // bits 64..65 — variant = 0b10
    // bits 66..127 — rand_b (62 bits) — remaining counter bytes + a fixed
    // suffix (we don't have entropy; suffix can be a hash/marker but is
    // not security-critical).
    bytes[8] = 0x80 | ((counter >> 56) & 0x3F) as u8;
    bytes[9] = (counter >> 48) as u8;
    bytes[10] = (counter >> 40) as u8;
    bytes[11] = (counter >> 32) as u8;
    bytes[12] = (counter >> 24) as u8;
    bytes[13] = (counter >> 16) as u8;
    bytes[14] = (counter >> 8) as u8;
    bytes[15] = counter as u8;

    uuid::Uuid::from_bytes(bytes)
}

// ---------------------------------------------------------------------------
// Warning header construction
// ---------------------------------------------------------------------------

/// Append `Warning: 299` values to the response headers per ADR-0030 §2.
/// HTTP allows multiple Warning entries; we comma-join into a single value
/// since the SDK's `Response.headers` is `BTreeMap<String, String>`.
fn attach_warnings(
    headers: &mut BTreeMap<String, String>,
    store_downgrade: bool,
    reasoning_dropped: bool,
) {
    let mut parts: Vec<&str> = Vec::new();
    if store_downgrade {
        parts.push(r#"299 - "store ignored; gateway is stateless, see ADR-0030""#);
    }
    if reasoning_dropped {
        parts.push(
            r#"299 - "reasoning items dropped; Anthropic upstream does not accept client-supplied reasoning input""#,
        );
    }
    if parts.is_empty() {
        return;
    }
    let merged = parts.join(", ");
    headers
        .entry("warning".to_string())
        .and_modify(|existing| {
            existing.push_str(", ");
            existing.push_str(&merged);
        })
        .or_insert(merged);
}

// ---------------------------------------------------------------------------
// problem+json error helpers
// ---------------------------------------------------------------------------

fn previous_response_id_not_supported_response() -> Response {
    ProblemDetails::new(
        400,
        "urn:barbacane:error:previous_response_id_not_supported",
        "Bad Request",
    )
    .detail(
        "ai-proxy: this gateway is stateless. \
                   `previous_response_id` requires server-side conversation \
                   storage that ADR-0030 §2 explicitly defers. Resend the \
                   full `input[]` each turn.",
    )
    .with("code", "previous_response_id_not_supported")
    .into_response()
}

fn responses_not_supported_for_provider_response(provider: Provider) -> Response {
    ProblemDetails::new(
        400,
        "urn:barbacane:error:responses_not_supported_for_provider",
        "Bad Request",
    )
    .detail(format!(
        "ai-proxy: provider {:?} does not implement the OpenAI Responses API \
             (no upstream surface to translate to or passthrough). Use \
             `/v1/chat/completions` instead, or route to OpenAI/Anthropic.",
        provider.name()
    ))
    .with("code", "responses_not_supported_for_provider")
    .into_response()
}

// Re-export so dispatch can use the same helper without duplicating the
// problem+json shape (the orchestration layer already has error_response
// for generic 5xx — these are domain-specific 400 cases).
#[allow(dead_code)]
fn _domain_errors_module_is_used(_: ()) {
    // Keep the error_response import alive even when no caller uses it
    // directly within this file (dispatch layer constructs the responses).
    let _ = error_response;
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Preflight ---

    #[test]
    fn preflight_passes_when_previous_response_id_absent() {
        let body = Some(br#"{"input":[]}"#.to_vec());
        let pre = ResponsesPreflight::from_body(&body).expect("ok");
        assert!(pre.store_downgrade); // store omitted → downgrade
    }

    #[test]
    fn preflight_rejects_previous_response_id_with_400() {
        let body = Some(br#"{"input":[],"previous_response_id":"resp_abc"}"#.to_vec());
        let resp = match ResponsesPreflight::from_body(&body) {
            Ok(_) => panic!("must reject"),
            Err(r) => r,
        };
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"], "previous_response_id_not_supported");
        assert_eq!(
            body["type"],
            "urn:barbacane:error:previous_response_id_not_supported"
        );
    }

    #[test]
    fn preflight_treats_explicit_null_previous_response_id_as_absent() {
        // OpenAI clients sometimes serialize Optional fields as null. Don't
        // 400 those — treat them as if the field weren't there.
        let body = Some(br#"{"input":[],"previous_response_id":null}"#.to_vec());
        ResponsesPreflight::from_body(&body).expect("null pri must be allowed");
    }

    #[test]
    fn preflight_store_true_triggers_downgrade() {
        let body = Some(br#"{"input":[],"store":true}"#.to_vec());
        assert!(
            ResponsesPreflight::from_body(&body)
                .unwrap()
                .store_downgrade
        );
    }

    #[test]
    fn preflight_store_false_skips_downgrade() {
        let body = Some(br#"{"input":[],"store":false}"#.to_vec());
        assert!(
            !ResponsesPreflight::from_body(&body)
                .unwrap()
                .store_downgrade
        );
    }

    #[test]
    fn preflight_store_omitted_treated_as_default_true() {
        // OpenAI server-side default for store is true; clients that don't
        // send it expect server-side persistence. We downgrade.
        let body = Some(br#"{"input":[]}"#.to_vec());
        assert!(
            ResponsesPreflight::from_body(&body)
                .unwrap()
                .store_downgrade
        );
    }

    // --- Responses → Anthropic translation ---

    fn translate_in(json: &str) -> ResponsesToAnthropic {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        ResponsesToAnthropic::translate(&v, "claude-sonnet-4-6", false, Some(1024))
            .expect("translate")
    }

    #[test]
    fn translate_in_input_text_becomes_text_block() {
        let res =
            translate_in(r#"{"input":[{"type":"input_text","role":"user","content":"Hello"}]}"#);
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let blocks = messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "Hello");
    }

    #[test]
    fn translate_in_instructions_hoist_to_system() {
        let res = translate_in(
            r#"{"instructions":"Be concise.","input":[{"type":"input_text","role":"user","content":"hi"}]}"#,
        );
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        assert_eq!(body["system"], "Be concise.");
    }

    #[test]
    fn translate_in_function_call_becomes_tool_use() {
        let res = translate_in(
            r#"{"input":[
                {"type":"function_call","role":"assistant","call_id":"call_1","name":"get_time","arguments":"{\"tz\":\"UTC\"}"}
            ]}"#,
        );
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_use");
        assert_eq!(block["id"], "call_1");
        assert_eq!(block["name"], "get_time");
        assert_eq!(block["input"]["tz"], "UTC");
    }

    #[test]
    fn translate_in_function_call_output_becomes_tool_result() {
        let res = translate_in(
            r#"{"input":[
                {"type":"function_call_output","call_id":"call_1","output":"2026-04-30T12:00:00Z"}
            ]}"#,
        );
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "call_1");
        assert_eq!(block["content"], "2026-04-30T12:00:00Z");
    }

    #[test]
    fn translate_in_reasoning_items_dropped_and_counted() {
        let res = translate_in(
            r#"{"input":[
                {"type":"reasoning","summary":"thinking..."},
                {"type":"input_text","role":"user","content":"hi"},
                {"type":"reasoning","summary":"more thinking"}
            ]}"#,
        );
        assert_eq!(res.dropped_reasoning_count, 2);
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        // Only the input_text item should remain in messages.
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn translate_in_max_output_tokens_used_or_default() {
        let with_explicit = translate_in(
            r#"{"max_output_tokens":42,"input":[{"type":"input_text","role":"user","content":"hi"}]}"#,
        );
        let v: serde_json::Value = serde_json::from_str(&with_explicit.body).unwrap();
        assert_eq!(v["max_tokens"], 42);

        let without =
            translate_in(r#"{"input":[{"type":"input_text","role":"user","content":"hi"}]}"#);
        let v: serde_json::Value = serde_json::from_str(&without.body).unwrap();
        // Falls back to the dispatcher's default_max_tokens=1024 (test config).
        assert_eq!(v["max_tokens"], 1024);
    }

    #[test]
    fn translate_in_uses_client_model_not_request_field() {
        // Responses requests carry `model` in the body; the translator must
        // use the caller-owned value plumbed in (ADR-0030 §0), not parse
        // the body field separately.
        let res = translate_in(
            r#"{"model":"some-other-model","input":[{"type":"input_text","role":"user","content":"hi"}]}"#,
        );
        let v: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        assert_eq!(v["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn translate_in_interleaved_roles_preserve_order() {
        // R8: the role-switching `flush_message` logic is the most plausible
        // regression site. Send an interleaved [user, assistant, user]
        // sequence and verify each segment becomes its own message in order
        // — not collapsed or reordered.
        let res = translate_in(
            r#"{"input":[
                {"type":"input_text","role":"user","content":"first user"},
                {"type":"input_text","role":"assistant","content":"first assistant"},
                {"type":"input_text","role":"user","content":"second user"}
            ]}"#,
        );
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["text"], "first user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][0]["text"], "first assistant");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["text"], "second user");
    }

    #[test]
    fn translate_in_consecutive_same_role_items_coalesce() {
        // The flip side of the role-switch test: two `user` items in a row
        // should land in the same message's `content` array, not produce two
        // separate messages.
        let res = translate_in(
            r#"{"input":[
                {"type":"input_text","role":"user","content":"part one"},
                {"type":"input_text","role":"user","content":"part two"}
            ]}"#,
        );
        let body: serde_json::Value = serde_json::from_str(&res.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let blocks = messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "part one");
        assert_eq!(blocks[1]["text"], "part two");
    }

    #[test]
    fn flush_message_helper_is_a_no_op_with_empty_buffer() {
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        let mut messages: Vec<serde_json::Value> = Vec::new();
        flush_message(Some("user"), &mut blocks, &mut messages);
        assert!(messages.is_empty());
    }

    #[test]
    fn flush_message_helper_is_a_no_op_with_no_role() {
        let mut blocks = vec![serde_json::json!({"type":"text","text":"orphaned"})];
        let mut messages: Vec<serde_json::Value> = Vec::new();
        flush_message(None, &mut blocks, &mut messages);
        assert!(messages.is_empty());
    }

    // --- R1: id rewriter on the OpenAI passthrough path ---

    #[test]
    fn rewrite_response_id_replaces_2xx_id_with_synthetic() {
        let upstream = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(br#"{"id":"resp_real_openai","object":"response"}"#.to_vec()),
        };
        let out = rewrite_response_id_if_2xx(upstream);
        let body: serde_json::Value = serde_json::from_slice(out.body.as_ref().unwrap()).unwrap();
        let id = body["id"].as_str().unwrap();
        assert!(id.starts_with("resp_"), "{}", id);
        assert_ne!(id, "resp_real_openai");
        assert_eq!(body["object"], "response");
    }

    #[test]
    fn rewrite_response_id_passes_4xx_through_unchanged() {
        // Don't mangle upstream errors — they have their own shape.
        let upstream = Response {
            status: 400,
            headers: BTreeMap::new(),
            body: Some(br#"{"error":{"code":"invalid_request","message":"bad"}}"#.to_vec()),
        };
        let out = rewrite_response_id_if_2xx(upstream);
        let body: serde_json::Value = serde_json::from_slice(out.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "invalid_request");
        assert!(body.get("id").is_none());
    }

    #[test]
    fn rewrite_response_id_passes_unparseable_body_through_unchanged() {
        // Some upstreams (or middleboxes) return non-JSON. Don't fail —
        // pass through and let the client see what we saw.
        let upstream = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"not even json".to_vec()),
        };
        let out = rewrite_response_id_if_2xx(upstream);
        assert_eq!(out.body.as_ref().unwrap(), b"not even json");
    }

    // --- Anthropic → Responses translation ---

    fn translate_out(body: &str) -> serde_json::Value {
        let s = AnthropicToResponses::translate(body, "claude-sonnet-4-6").unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn translate_out_text_block_becomes_output_text() {
        let v = translate_out(
            r#"{
                "id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-6",
                "content":[{"type":"text","text":"Hello"}],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":3,"output_tokens":1}
            }"#,
        );
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        let item = &v["output"][0];
        assert_eq!(item["type"], "output_text");
        assert_eq!(item["text"], "Hello");
        assert!(item["annotations"].is_array());
    }

    #[test]
    fn translate_out_tool_use_block_becomes_function_call() {
        let v = translate_out(
            r#"{
                "id":"msg_2","model":"claude-sonnet-4-6",
                "content":[{"type":"tool_use","id":"call_42","name":"get_time","input":{"tz":"UTC"}}],
                "usage":{"input_tokens":1,"output_tokens":1}
            }"#,
        );
        let item = &v["output"][0];
        assert_eq!(item["type"], "function_call");
        assert_eq!(item["call_id"], "call_42");
        assert_eq!(item["name"], "get_time");
        // Arguments are serialized as a JSON string per the OpenAI Responses
        // contract — clients re-parse them.
        assert!(item["arguments"].as_str().unwrap().contains("UTC"));
    }

    #[test]
    fn translate_out_synthetic_id_is_resp_uuid_v7() {
        let v = translate_out(
            r#"{"id":"msg","model":"x","content":[],"usage":{"input_tokens":0,"output_tokens":0}}"#,
        );
        let id = v["id"].as_str().unwrap();
        assert!(
            id.starts_with("resp_"),
            "id should start with resp_, got {}",
            id
        );
        // After the prefix is a 36-character UUID dashed-hex form.
        let uuid_part = id.trim_start_matches("resp_");
        assert_eq!(uuid_part.len(), 36, "{}", uuid_part);
        // Version-7 UUIDs have the version nibble (13th hex char of the
        // dashed form, position 14 due to the dashes) equal to '7'.
        assert_eq!(
            uuid_part.as_bytes()[14],
            b'7',
            "expected v7 nibble, got {}",
            uuid_part
        );
    }

    #[test]
    fn translate_out_uses_caller_supplied_model_not_anthropic_model() {
        // ADR-0030 §0: ai.model and the response.model are the client's
        // requested model. Even if Anthropic echoes a different model
        // string, the gateway returns the caller-owned value.
        let v = translate_out(
            r#"{"id":"msg","model":"claude-anything-anthropic-says","content":[],"usage":{"input_tokens":0,"output_tokens":0}}"#,
        );
        assert_eq!(v["model"], "claude-sonnet-4-6");
    }

    #[test]
    fn translate_out_usage_maps_directly() {
        let v = translate_out(
            r#"{"id":"msg","model":"x","content":[],"usage":{"input_tokens":42,"output_tokens":10}}"#,
        );
        assert_eq!(v["usage"]["input_tokens"], 42);
        assert_eq!(v["usage"]["output_tokens"], 10);
        assert_eq!(v["usage"]["total_tokens"], 52);
    }

    // --- Domain error responses ---

    #[test]
    fn responses_not_supported_for_provider_shape() {
        let resp = responses_not_supported_for_provider_response(Provider::Ollama);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_slice(resp.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["code"], "responses_not_supported_for_provider");
        assert_eq!(
            body["type"],
            "urn:barbacane:error:responses_not_supported_for_provider"
        );
        assert!(body["detail"].as_str().unwrap().contains("ollama"));
    }

    // --- Warning header ---

    #[test]
    fn warnings_attached_for_store_only() {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        attach_warnings(&mut headers, true, false);
        let warning = headers.get("warning").expect("warning set");
        assert!(warning.contains("store ignored"));
        assert!(!warning.contains("reasoning items dropped"));
    }

    #[test]
    fn warnings_attached_for_reasoning_only() {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        attach_warnings(&mut headers, false, true);
        let warning = headers.get("warning").expect("warning set");
        assert!(warning.contains("reasoning items dropped"));
        assert!(!warning.contains("store ignored"));
    }

    #[test]
    fn warnings_merged_when_both_fire() {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        attach_warnings(&mut headers, true, true);
        let warning = headers.get("warning").expect("warning set");
        assert!(warning.contains("store ignored"));
        assert!(warning.contains("reasoning items dropped"));
    }

    #[test]
    fn warnings_skipped_when_neither_fires() {
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        attach_warnings(&mut headers, false, false);
        assert!(headers.get("warning").is_none());
    }
}
