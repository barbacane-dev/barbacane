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
use serde_json::{json, Value};

use super::tools;

#[derive(Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Tools translated from the client's `tools` array (ADR-0024 tool-use
    /// gap). Absent when the client sent none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
}

/// Per-protocol handler invoked by [`crate::dispatch`] after the orchestration
/// layer has resolved a target. Picks the OpenAI-compatible passthrough or the
/// Anthropic translation based on the resolved provider. The `client_model`
/// is the caller-supplied model identifier (ADR-0030 §0); it travels with the
/// request body for the OpenAI passthrough, and is plumbed into the Anthropic
/// translation explicitly.
pub(crate) fn handle(
    plugin: &AiProxy,
    target: &TargetConfig,
    req: &Request,
    client_model: &str,
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
        plugin.anthropic_call(target, req, client_model, false)
    }
}

/// Translate an OpenAI chat completion request body to Anthropic Messages API format.
/// Pinned to Anthropic API version 2024-10-22 (ADR-0024).
///
/// `client_model` is the caller-supplied model identifier (ADR-0030 §0).
/// `dispatch()` validates it is non-empty before this function is called, so
/// no fallback is needed here — passing it explicitly keeps the caller-owned
/// invariant local to the call site.
pub(crate) fn translate_to_anthropic(
    body: &Option<Vec<u8>>,
    client_model: &str,
    stream: bool,
    default_max_tokens: Option<u32>,
) -> Result<String, String> {
    let raw = body.as_deref().unwrap_or(b"{}");
    let openai: Value =
        serde_json::from_slice(raw).map_err(|e| format!("invalid request body: {}", e))?;

    let msgs = openai["messages"]
        .as_array()
        .ok_or("missing or invalid messages array")?;

    let (messages, system_parts) = translate_chat_messages(msgs);

    let max_tokens = openai["max_tokens"]
        .as_u64()
        .map(|v| v as u32)
        .or(default_max_tokens)
        .unwrap_or(4096);

    let anthropic = AnthropicRequest {
        model: client_model.to_string(),
        messages,
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n"))
        },
        max_tokens,
        temperature: openai["temperature"].as_f64(),
        top_p: openai["top_p"].as_f64(),
        stream: if stream { Some(true) } else { None },
        tools: tools::chat_tools_to_anthropic(&openai),
        tool_choice: tools::tool_choice_to_anthropic(
            openai.get("tool_choice"),
            openai.get("parallel_tool_calls").and_then(|v| v.as_bool()),
        ),
    };

    serde_json::to_string(&anthropic).map_err(|e| e.to_string())
}

/// Translate the OpenAI `messages` array into Anthropic `messages` +
/// hoisted `system` parts. Beyond splitting system out, this maps the tool-use
/// wire shapes Anthropic needs (the ADR-0024 gap): an assistant `tool_calls`
/// array becomes `tool_use` content blocks, and each `role:"tool"` message
/// becomes a `tool_result` block. Consecutive tool messages are merged into a
/// single user turn, as Anthropic expects all results for one assistant turn
/// grouped together.
fn translate_chat_messages(messages: &[Value]) -> (Vec<Value>, Vec<String>) {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    let mut system_parts: Vec<String> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();

    let flush = |pending: &mut Vec<Value>, out: &mut Vec<Value>| {
        if !pending.is_empty() {
            out.push(json!({ "role": "user", "content": std::mem::take(pending) }));
        }
    };

    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");
        if role != "tool" {
            flush(&mut pending_tool_results, &mut out);
        }

        match role {
            "system" => collect_text_into(&msg["content"], &mut system_parts),
            "tool" => {
                // OpenAI `role:"tool"` → Anthropic `tool_result` block. The
                // `tool_call_id` maps to Anthropic's `tool_use_id`.
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": msg.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "content": stringify_tool_content(&msg["content"]),
                }));
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = msg["content"].as_str() {
                    if !text.is_empty() {
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                } else if let Some(parts) = msg["content"].as_array() {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                            blocks.push(json!({ "type": "text", "text": t }));
                        }
                    }
                }
                if let Some(calls) = msg["tool_calls"].as_array() {
                    for c in calls {
                        let input = c["function"]["arguments"]
                            .as_str()
                            .and_then(|s| serde_json::from_str::<Value>(s).ok())
                            .unwrap_or_else(|| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": c.get("id").cloned().unwrap_or(Value::Null),
                            "name": c["function"].get("name").cloned().unwrap_or(Value::Null),
                            "input": input,
                        }));
                    }
                }
                // Skip an assistant turn with neither text nor tool calls —
                // Anthropic rejects an empty content array.
                if !blocks.is_empty() {
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            // "user" and any unknown role pass through as a user turn.
            _ => out.push(json!({ "role": "user", "content": normalize_user_content(&msg["content"]) })),
        }
    }
    flush(&mut pending_tool_results, &mut out);
    (out, system_parts)
}

/// Append text from an OpenAI `content` field (string or array-of-parts) to
/// `parts`. Used to hoist `system` messages into Anthropic's `system` field.
fn collect_text_into(content: &Value, parts: &mut Vec<String>) {
    match content {
        Value::String(s) => parts.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    parts.push(t.to_string());
                }
            }
        }
        _ => {}
    }
}

/// Normalize an OpenAI user `content` into an Anthropic-acceptable content
/// value: a plain string passes through; an array of parts maps `text` and
/// `image_url` parts to Anthropic content blocks.
fn normalize_user_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => {
            let blocks: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p.get("type").and_then(|v| v.as_str()) {
                    Some("text") | None => p
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|t| json!({ "type": "text", "text": t })),
                    Some("image_url") => p
                        .get("image_url")
                        .and_then(|u| u.get("url"))
                        .and_then(|v| v.as_str())
                        .map(|url| json!({ "type": "image", "source": { "type": "url", "url": url } })),
                    _ => None,
                })
                .collect();
            Value::Array(blocks)
        }
        Value::Null => Value::String(String::new()),
        other => other.clone(),
    }
}

/// Coerce an OpenAI tool-message `content` into the string Anthropic's
/// `tool_result` block expects. Non-string content is JSON-serialized.
fn stringify_tool_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Null => Value::String(String::new()),
        other => Value::String(other.to_string()),
    }
}

/// Translate an Anthropic Messages API response body to OpenAI chat completion format.
/// Pinned to Anthropic API version 2024-10-22 (ADR-0024).
pub(crate) fn translate_from_anthropic(body: &str) -> Result<String, String> {
    let anthropic: Value =
        serde_json::from_str(body).map_err(|e| format!("invalid Anthropic response: {}", e))?;

    // Walk every content block: concatenate text blocks and turn each
    // `tool_use` block into an OpenAI `tool_calls` entry. The previous
    // implementation kept only the first text block and dropped tool calls
    // entirely, so a tool-calling turn was returned malformed.
    let mut content_text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(blocks) = anthropic["content"].as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = block["text"].as_str() {
                        content_text.push_str(t);
                    }
                }
                Some("tool_use") => {
                    // OpenAI carries the arguments as a JSON *string*.
                    let arguments = block
                        .get("input")
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(json!({
                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                        "type": "function",
                        "function": {
                            "name": block.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": arguments,
                        },
                    }));
                }
                _ => {}
            }
        }
    }

    let input_tokens = anthropic["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = anthropic["usage"]["output_tokens"].as_u64().unwrap_or(0);

    // Map Anthropic stop reason to OpenAI finish_reason
    let finish_reason = match anthropic["stop_reason"].as_str() {
        Some("end_turn") => "stop",
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ if !tool_calls.is_empty() => "tool_calls",
        _ => "stop",
    };

    // OpenAI convention: `content` is null when the turn is only tool calls.
    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert(
        "content".to_string(),
        if content_text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(content_text)
        },
    );
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }

    let openai = json!({
        "id": anthropic["id"],
        "object": "chat.completion",
        "model": anthropic["model"],
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn to_anthropic(body: &str) -> Value {
        let out = translate_to_anthropic(&Some(body.as_bytes().to_vec()), "claude-sonnet-4-6", false, Some(1024))
            .expect("translate_to_anthropic");
        serde_json::from_str(&out).unwrap()
    }

    #[test]
    fn to_anthropic_forwards_tools_and_tool_choice() {
        let body = to_anthropic(
            r#"{
              "messages":[{"role":"user","content":"weather?"}],
              "tools":[{"type":"function","function":{"name":"get_weather","description":"d","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}],
              "tool_choice":{"type":"function","function":{"name":"get_weather"}},
              "parallel_tool_calls": false
            }"#,
        );
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["input_schema"]["properties"]["city"]["type"], "string");
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], "get_weather");
        assert_eq!(body["tool_choice"]["disable_parallel_tool_use"], true);
    }

    #[test]
    fn to_anthropic_translates_assistant_tool_calls_and_tool_result() {
        let body = to_anthropic(
            r#"{
              "messages":[
                {"role":"user","content":"weather in Paris?"},
                {"role":"assistant","content":null,"tool_calls":[
                  {"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call_1","content":"18C"}
              ]
            }"#,
        );
        let messages = body["messages"].as_array().unwrap();
        // user, assistant(tool_use), user(tool_result)
        assert_eq!(messages.len(), 3);
        let tool_use = &messages[1]["content"][0];
        assert_eq!(tool_use["type"], "tool_use");
        assert_eq!(tool_use["id"], "call_1");
        assert_eq!(tool_use["name"], "get_weather");
        assert_eq!(tool_use["input"]["city"], "Paris");
        let tool_result = &messages[2]["content"][0];
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(tool_result["type"], "tool_result");
        assert_eq!(tool_result["tool_use_id"], "call_1");
        assert_eq!(tool_result["content"], "18C");
    }

    #[test]
    fn to_anthropic_merges_consecutive_tool_results() {
        let body = to_anthropic(
            r#"{"messages":[
              {"role":"assistant","content":null,"tool_calls":[
                {"id":"a","type":"function","function":{"name":"f","arguments":"{}"}},
                {"id":"b","type":"function","function":{"name":"g","arguments":"{}"}}
              ]},
              {"role":"tool","tool_call_id":"a","content":"ra"},
              {"role":"tool","tool_call_id":"b","content":"rb"}
            ]}"#,
        );
        let messages = body["messages"].as_array().unwrap();
        // assistant, then a single user turn holding both tool_results.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn from_anthropic_emits_tool_calls_with_stringified_args() {
        let out = translate_from_anthropic(
            r#"{"id":"msg_1","model":"claude","stop_reason":"tool_use",
                "content":[{"type":"tool_use","id":"tu_1","name":"get_weather","input":{"city":"Paris"}}],
                "usage":{"input_tokens":5,"output_tokens":3}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let msg = &v["choices"][0]["message"];
        assert!(msg["content"].is_null(), "content is null on a tool-only turn");
        let call = &msg["tool_calls"][0];
        assert_eq!(call["id"], "tu_1");
        assert_eq!(call["type"], "function");
        assert_eq!(call["function"]["name"], "get_weather");
        // arguments is a JSON *string*, not an object.
        assert_eq!(call["function"]["arguments"], "{\"city\":\"Paris\"}");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn from_anthropic_plain_text_unchanged() {
        let out = translate_from_anthropic(
            r#"{"id":"m","model":"c","stop_reason":"end_turn",
                "content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "hi");
        assert!(v["choices"][0]["message"].get("tool_calls").is_none());
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
    }
}
