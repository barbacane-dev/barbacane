//! Shared OpenAI ↔ Anthropic tool-schema translation, used by both the Chat
//! Completions and Responses protocol adapters (ADR-0024 / ADR-0030).
//!
//! `Provider` selects the wire protocol; this module handles the *tool*
//! surface that both OpenAI-shaped protocols expose and that must be mapped
//! onto Anthropic's Messages `tools` / `tool_choice` fields. Without this the
//! Anthropic upstream never learns the client's tools exist, so it can never
//! call them (see the gap ADR-0024/0030 left open).

use serde_json::{json, Value};

/// A tool type the Anthropic Messages API has no representation for — Codex
/// freeform `custom` tools (e.g. `apply_patch`), `local_shell`, or a hosted
/// server tool. Carries the offending `type` so the caller can surface a
/// precise 400 instead of silently dropping the tool.
#[derive(Debug)]
pub(crate) struct UnsupportedTool {
    pub tool_type: String,
}

/// Build one Anthropic tool object from a name / description / JSON-schema
/// `parameters` triple. Anthropic requires `input_schema`; default to an empty
/// object schema when the client omitted `parameters`.
fn anthropic_tool(name: &Value, description: Option<Value>, parameters: Option<Value>) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".to_string(), name.clone());
    if let Some(d) = description {
        obj.insert("description".to_string(), d);
    }
    obj.insert(
        "input_schema".to_string(),
        parameters.unwrap_or_else(|| json!({ "type": "object" })),
    );
    Value::Object(obj)
}

/// Translate a Chat Completions `tools` array
/// (`[{type:"function", function:{name, description, parameters}}]`) into an
/// Anthropic `tools` array. Chat Completions only defines function tools; a
/// non-function entry is forward-compatibly skipped rather than rejected.
/// Returns `None` when there are no usable tools.
pub(crate) fn chat_tools_to_anthropic(openai: &Value) -> Option<Value> {
    let arr = openai.get("tools")?.as_array()?;
    let tools: Vec<Value> = arr
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            let name = f.get("name")?;
            Some(anthropic_tool(
                name,
                f.get("description").cloned(),
                f.get("parameters").cloned(),
            ))
        })
        .collect();
    (!tools.is_empty()).then_some(Value::Array(tools))
}

/// Translate a Responses `tools` array (flat
/// `[{type:"function", name, description, parameters}]`) into Anthropic tools.
/// Rejects tool types Anthropic can't represent (`custom` freeform such as
/// Codex `apply_patch`, `local_shell`, hosted server tools) with an
/// [`UnsupportedTool`] so the dispatcher returns a precise 400 rather than
/// dropping the tool and leaving the model unable to call it.
pub(crate) fn responses_tools_to_anthropic(
    responses: &Value,
) -> Result<Option<Value>, UnsupportedTool> {
    let arr = match responses.get("tools").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Ok(None),
    };
    let mut out = Vec::with_capacity(arr.len());
    for t in arr {
        // Responses defaults an entry with no `type` to a function tool.
        let ty = t.get("type").and_then(|v| v.as_str()).unwrap_or("function");
        if ty != "function" {
            return Err(UnsupportedTool {
                tool_type: ty.to_string(),
            });
        }
        let Some(name) = t.get("name") else { continue };
        out.push(anthropic_tool(
            name,
            t.get("description").cloned(),
            t.get("parameters").cloned(),
        ));
    }
    Ok((!out.is_empty()).then_some(Value::Array(out)))
}

/// Map an OpenAI `tool_choice` (the shape is shared across Chat Completions and
/// Responses) plus the `parallel_tool_calls` flag into an Anthropic
/// `tool_choice` object. Returns `None` when the client left the choice
/// implicit and parallel calls aren't disabled — let Anthropic apply its
/// default rather than forcing one.
pub(crate) fn tool_choice_to_anthropic(
    tool_choice: Option<&Value>,
    parallel_tool_calls: Option<bool>,
) -> Option<Value> {
    let mut base = match tool_choice {
        Some(Value::String(s)) => match s.as_str() {
            "auto" => Some(json!({ "type": "auto" })),
            "none" => Some(json!({ "type": "none" })),
            // OpenAI "required" == Anthropic "any" (must call some tool).
            "required" | "any" => Some(json!({ "type": "any" })),
            _ => None,
        },
        // Named-tool form. Chat Completions nests the name under `function`;
        // Responses puts it at the top level.
        Some(Value::Object(o)) => o
            .get("function")
            .and_then(|f| f.get("name"))
            .or_else(|| o.get("name"))
            .cloned()
            .map(|name| json!({ "type": "tool", "name": name })),
        _ => None,
    };

    if parallel_tool_calls == Some(false) {
        let mut choice = base.unwrap_or_else(|| json!({ "type": "auto" }));
        if let Some(obj) = choice.as_object_mut() {
            obj.insert("disable_parallel_tool_use".to_string(), json!(true));
        }
        base = Some(choice);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_tools_map_function_shape_to_input_schema() {
        let openai = json!({
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
                }
            }]
        });
        let tools = chat_tools_to_anthropic(&openai).expect("tools");
        let t = &tools.as_array().unwrap()[0];
        assert_eq!(t["name"], "get_weather");
        assert_eq!(t["description"], "Get weather");
        assert_eq!(t["input_schema"]["properties"]["city"]["type"], "string");
        assert!(t.get("function").is_none(), "must be flattened for Anthropic");
    }

    #[test]
    fn chat_tools_default_input_schema_when_parameters_missing() {
        let openai = json!({ "tools": [{ "type": "function", "function": { "name": "ping" } }] });
        let tools = chat_tools_to_anthropic(&openai).unwrap();
        assert_eq!(tools[0]["input_schema"], json!({ "type": "object" }));
    }

    #[test]
    fn chat_tools_none_when_absent() {
        assert!(chat_tools_to_anthropic(&json!({})).is_none());
        assert!(chat_tools_to_anthropic(&json!({ "tools": [] })).is_none());
    }

    #[test]
    fn responses_tools_map_flat_function_shape() {
        let responses = json!({
            "tools": [{ "type": "function", "name": "get_time", "parameters": { "type": "object" } }]
        });
        let tools = responses_tools_to_anthropic(&responses).unwrap().unwrap();
        assert_eq!(tools[0]["name"], "get_time");
        assert_eq!(tools[0]["input_schema"], json!({ "type": "object" }));
    }

    #[test]
    fn responses_tools_reject_custom_freeform() {
        // Codex apply_patch is a custom/freeform tool — Anthropic can't
        // represent it, so we reject explicitly instead of dropping it.
        let responses = json!({ "tools": [{ "type": "custom", "name": "apply_patch" }] });
        let err = responses_tools_to_anthropic(&responses).unwrap_err();
        assert_eq!(err.tool_type, "custom");
    }

    #[test]
    fn responses_tools_reject_local_shell() {
        let responses = json!({ "tools": [{ "type": "local_shell" }] });
        assert_eq!(
            responses_tools_to_anthropic(&responses).unwrap_err().tool_type,
            "local_shell"
        );
    }

    #[test]
    fn tool_choice_string_forms() {
        assert_eq!(
            tool_choice_to_anthropic(Some(&json!("auto")), None),
            Some(json!({ "type": "auto" }))
        );
        assert_eq!(
            tool_choice_to_anthropic(Some(&json!("required")), None),
            Some(json!({ "type": "any" }))
        );
        assert_eq!(
            tool_choice_to_anthropic(Some(&json!("none")), None),
            Some(json!({ "type": "none" }))
        );
    }

    #[test]
    fn tool_choice_named_forms_both_protocols() {
        // Chat Completions nesting
        assert_eq!(
            tool_choice_to_anthropic(
                Some(&json!({ "type": "function", "function": { "name": "f" } })),
                None
            ),
            Some(json!({ "type": "tool", "name": "f" }))
        );
        // Responses flat naming
        assert_eq!(
            tool_choice_to_anthropic(Some(&json!({ "type": "function", "name": "f" })), None),
            Some(json!({ "type": "tool", "name": "f" }))
        );
    }

    #[test]
    fn tool_choice_parallel_disabled_adds_flag() {
        // No explicit choice, parallel disabled → default auto + flag.
        assert_eq!(
            tool_choice_to_anthropic(None, Some(false)),
            Some(json!({ "type": "auto", "disable_parallel_tool_use": true }))
        );
        // Explicit choice keeps its type and gains the flag.
        assert_eq!(
            tool_choice_to_anthropic(Some(&json!("required")), Some(false)),
            Some(json!({ "type": "any", "disable_parallel_tool_use": true }))
        );
    }

    #[test]
    fn tool_choice_none_when_implicit() {
        assert!(tool_choice_to_anthropic(None, None).is_none());
        assert!(tool_choice_to_anthropic(None, Some(true)).is_none());
    }
}
