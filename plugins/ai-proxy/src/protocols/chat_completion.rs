//! OpenAI Chat Completions protocol adapter.
//!
//! Path: `POST /v1/chat/completions`. For OpenAI-compatible providers this is
//! a passthrough; for Anthropic the request body is translated to the Messages
//! API and the response is translated back. Streaming is supported on
//! OpenAI-compatible providers; for Anthropic streaming, the dispatcher buffers
//! into a single response (true SSE translation is deferred — ADR-0024).

use crate::host;
use crate::{AiProxy, TargetConfig};
use barbacane_plugin_sdk::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

/// Per-protocol handler invoked by [`crate::dispatch`] after the orchestration
/// layer has resolved a target. Picks the OpenAI-compatible passthrough or the
/// Anthropic translation based on the resolved provider.
pub(crate) fn handle(
    plugin: &AiProxy,
    target: &TargetConfig,
    req: &Request,
    streaming: bool,
) -> Result<Response, String> {
    if target.provider.is_openai_compatible() {
        if streaming {
            plugin.openai_stream(target, req)
        } else {
            plugin.openai_call(target, req)
        }
    } else {
        // Anthropic: ADR-0024 SSE translation is future work; buffer the response.
        if streaming {
            host::log_warn("ai-proxy: Anthropic streaming not yet supported; buffering response");
        }
        plugin.anthropic_call(target, req, false)
    }
}

/// Translate an OpenAI chat completion request body to Anthropic Messages API format.
/// Pinned to Anthropic API version 2024-10-22 (ADR-0024).
pub(crate) fn translate_to_anthropic(
    body: &Option<Vec<u8>>,
    model: &str,
    stream: bool,
    default_max_tokens: Option<u32>,
) -> Result<String, String> {
    let raw = body.as_deref().unwrap_or(b"{}");
    let openai: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| format!("invalid request body: {}", e))?;

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
        model: openai["model"].as_str().unwrap_or(model).to_string(),
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
pub(crate) fn translate_from_anthropic(body: &str) -> Result<String, String> {
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
