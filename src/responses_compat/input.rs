use super::*;
use serde_json::{Map, Value, json};

pub(super) fn string_field(object: &Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn bool_field(object: &Map<String, Value>, field: &str) -> Option<bool> {
    object.get(field).and_then(Value::as_bool)
}

pub(super) fn copy_if_present(from: &Map<String, Value>, to: &mut Map<String, Value>, field: &str) {
    if let Some(value) = from.get(field) {
        to.insert(field.to_string(), value.clone());
    }
}

pub(super) fn append_input_messages(
    messages: &mut Vec<Value>,
    input: &Value,
) -> anyhow::Result<()> {
    match input {
        Value::String(text) => {
            messages.push(json!({
                "role": "user",
                "content": text,
            }));
        }
        Value::Array(items) => {
            for item in items {
                append_input_item(messages, item)?;
            }
        }
        Value::Object(_) => append_input_item(messages, input)?,
        _ => anyhow::bail!("responses `input` must be a string, object, or array"),
    }
    Ok(())
}

pub(super) fn append_input_item(messages: &mut Vec<Value>, item: &Value) -> anyhow::Result<()> {
    let Some(object) = item.as_object() else {
        anyhow::bail!("responses input array items must be objects");
    };
    let item_type = object.get("type").and_then(Value::as_str);
    match item_type {
        Some("function_call_output") | Some("custom_tool_call_output") => {
            let call_id = string_field(object, "call_id")
                .or_else(|| string_field(object, "tool_call_id"))
                .ok_or_else(|| anyhow::anyhow!("{:?} requires call_id", item_type))?;
            let output = object
                .get("output")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    object
                        .get("output")
                        .map(Value::to_string)
                        .unwrap_or_default()
                });
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output,
            }));
        }
        Some("function_call") => {
            let call_id = string_field(object, "call_id")
                .or_else(|| string_field(object, "id"))
                .ok_or_else(|| anyhow::anyhow!("function_call requires call_id or id"))?;
            let name = string_field(object, "name")
                .ok_or_else(|| anyhow::anyhow!("function_call requires name"))?;
            let chat_name = match string_field(object, "namespace") {
                Some(namespace) => namespaced_tool_name(&namespace, &name),
                None => name,
            };
            let arguments = object
                .get("arguments")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    object
                        .get("arguments")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_string())
                });
            messages.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": chat_name,
                        "arguments": arguments,
                    }
                }]
            }));
        }
        Some("custom_tool_call") => {
            let call_id = string_field(object, "call_id")
                .or_else(|| string_field(object, "id"))
                .ok_or_else(|| anyhow::anyhow!("custom_tool_call requires call_id or id"))?;
            let name = string_field(object, "name")
                .ok_or_else(|| anyhow::anyhow!("custom_tool_call requires name"))?;
            let chat_name = match string_field(object, "namespace") {
                Some(namespace) => namespaced_tool_name(&namespace, &name),
                None => name,
            };
            let input = object
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    object
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_default()
                });
            messages.push(json!({
                "role": "assistant",
                "content": Value::Null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": chat_name,
                        "arguments": custom_input_to_chat_arguments(&input),
                    }
                }]
            }));
        }
        Some("message") | None => {
            let role = chat_role(object.get("role").and_then(Value::as_str).unwrap_or("user"));
            let content = object
                .get("content")
                .map(content_to_chat)
                .unwrap_or_else(|| Value::String(String::new()));
            messages.push(json!({
                "role": role,
                "content": content,
            }));
        }
        Some(other) if is_context_response_item(other) => {
            messages.push(json!({
                "role": "assistant",
                "content": format!("[{other} output item preserved by Responses compatibility adapter]"),
            }));
        }
        Some(other) => anyhow::bail!("unsupported responses input item type: {other}"),
    }
    Ok(())
}

pub(super) fn chat_role(role: &str) -> &str {
    match role {
        "developer" | "system" => "system",
        "assistant" => "assistant",
        "tool" => "tool",
        _ => "user",
    }
}

pub(super) fn is_context_response_item(item_type: &str) -> bool {
    matches!(
        item_type,
        "web_search_call"
            | "image_generation_call"
            | "file_search_call"
            | "code_interpreter_call"
            | "computer_call"
            | "mcp_call"
            | "mcp_list_tools"
            | "local_shell_call"
            | "local_shell_call_output"
            | "shell_call"
            | "shell_call_output"
            | "reasoning"
    )
}

pub(super) fn content_to_chat(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => {
            let mut chat_parts = Vec::new();
            let mut all_text = true;
            let mut text = String::new();
            for part in parts {
                let Some(object) = part.as_object() else {
                    continue;
                };
                match object.get("type").and_then(Value::as_str) {
                    Some("input_text" | "output_text" | "text") => {
                        if let Some(value) = object.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                            chat_parts.push(json!({
                                "type": "text",
                                "text": value,
                            }));
                        }
                    }
                    Some("input_image") => {
                        all_text = false;
                        if let Some(url) = object.get("image_url").and_then(Value::as_str) {
                            chat_parts.push(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": url,
                                }
                            }));
                        } else if let Some(image_url) = object.get("image_url") {
                            chat_parts.push(json!({
                                "type": "image_url",
                                "image_url": image_url,
                            }));
                        }
                    }
                    _ => {
                        if let Some(value) = object.get("text").and_then(Value::as_str) {
                            text.push_str(value);
                            chat_parts.push(json!({
                                "type": "text",
                                "text": value,
                            }));
                        }
                    }
                }
            }
            if all_text {
                Value::String(text)
            } else {
                Value::Array(chat_parts)
            }
        }
        _ => Value::String(content.to_string()),
    }
}
