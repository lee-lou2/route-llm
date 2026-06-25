use super::usage::usage_from_response;
use super::*;
use axum::body::Bytes;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

pub(crate) async fn convert_json_response(
    pool: &SqlitePool,
    client_id: Option<i64>,
    request: &PreparedResponseRequest,
    body: &Bytes,
) -> anyhow::Result<(Bytes, Option<CompatUsage>)> {
    let value = serde_json::from_slice::<Value>(body)?;
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| anyhow::anyhow!("chat completion response missing choices[0].message"))?;
    let output_text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let output = output_from_chat_message(request, message, &output_text);
    let usage = usage_from_response(&value);
    let response = response_json(request, "completed", Some(output.clone()), usage, None);
    store_response_state(
        pool,
        client_id,
        request,
        output.clone(),
        output_text,
        usage,
        assistant_message_for_state(message),
    )
    .await?;
    Ok((Bytes::from(serde_json::to_vec(&response)?), usage))
}

pub(super) fn output_from_chat_message(
    request: &PreparedResponseRequest,
    message: &Value,
    output_text: &str,
) -> Vec<Value> {
    let mut output = Vec::new();
    if !output_text.is_empty() || message.get("tool_calls").is_none() {
        output.push(message_output_item(
            &request.message_id,
            "completed",
            output_text,
        ));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            output.push(tool_output_item(request, tool_call, "completed", None));
        }
    }
    output
}

pub(super) fn message_output_item(id: &str, status: &str, text: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": text,
            "annotations": [],
        }],
    })
}

pub(super) fn tool_output_item(
    request: &PreparedResponseRequest,
    tool_call: &Value,
    status: &str,
    item_id: Option<&str>,
) -> Value {
    let call_id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let chat_name = tool_call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = tool_call
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mapping = request.tool_map.get(chat_name);
    match mapping.map(|mapping| &mapping.kind) {
        Some(ResponseToolKind::Custom) => {
            let mut object = Map::new();
            object.insert(
                "id".to_string(),
                Value::String(
                    item_id
                        .map(str::to_string)
                        .unwrap_or_else(|| prefixed_id("ctc")),
                ),
            );
            object.insert(
                "type".to_string(),
                Value::String("custom_tool_call".to_string()),
            );
            object.insert("status".to_string(), Value::String(status.to_string()));
            object.insert("call_id".to_string(), Value::String(call_id));
            if let Some(mapping) = mapping {
                if let Some(namespace) = &mapping.namespace {
                    object.insert("namespace".to_string(), Value::String(namespace.clone()));
                }
                object.insert("name".to_string(), Value::String(mapping.name.clone()));
            } else {
                object.insert("name".to_string(), Value::String(chat_name.to_string()));
            }
            object.insert(
                "input".to_string(),
                Value::String(custom_input_from_chat_arguments(arguments)),
            );
            Value::Object(object)
        }
        _ => {
            let mut object = Map::new();
            object.insert(
                "id".to_string(),
                Value::String(
                    item_id
                        .map(str::to_string)
                        .unwrap_or_else(|| prefixed_id("fc")),
                ),
            );
            object.insert(
                "type".to_string(),
                Value::String("function_call".to_string()),
            );
            object.insert("status".to_string(), Value::String(status.to_string()));
            object.insert("call_id".to_string(), Value::String(call_id));
            if let Some(mapping) = mapping {
                if let Some(namespace) = &mapping.namespace {
                    object.insert("namespace".to_string(), Value::String(namespace.clone()));
                }
                object.insert("name".to_string(), Value::String(mapping.name.clone()));
            } else {
                object.insert("name".to_string(), Value::String(chat_name.to_string()));
            }
            object.insert(
                "arguments".to_string(),
                Value::String(arguments.to_string()),
            );
            Value::Object(object)
        }
    }
}

pub(super) fn assistant_message_for_state(message: &Value) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String("assistant".to_string()));
    object.insert(
        "content".to_string(),
        message.get("content").cloned().unwrap_or(Value::Null),
    );
    if let Some(tool_calls) = message.get("tool_calls") {
        object.insert("tool_calls".to_string(), tool_calls.clone());
    }
    Value::Object(object)
}

pub(super) fn response_json(
    request: &PreparedResponseRequest,
    status: &str,
    output: Option<Vec<Value>>,
    usage: Option<CompatUsage>,
    error: Option<Value>,
) -> Value {
    let output = output.unwrap_or_default();
    let output_text = output
        .iter()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flat_map(|parts| parts.iter())
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    json!({
        "id": request.response_id.clone(),
        "object": "response",
        "created_at": request.created_at,
        "status": status,
        "error": error,
        "incomplete_details": Value::Null,
        "instructions": request.instructions.clone(),
        "max_output_tokens": request.max_output_tokens.clone(),
        "model": request.model.clone(),
        "output": output,
        "output_text": output_text,
        "parallel_tool_calls": request.parallel_tool_calls.clone(),
        "previous_response_id": request.previous_response_id.clone(),
        "reasoning": request.reasoning.clone(),
        "store": request.store.clone(),
        "temperature": request.temperature.clone(),
        "text": request.text.clone(),
        "tool_choice": request.response_tool_choice.clone(),
        "tools": request.response_tools.clone(),
        "top_p": request.top_p.clone(),
        "truncation": request.truncation.clone(),
        "usage": usage.map(usage_json).unwrap_or(Value::Null),
        "user": request.user.clone(),
        "metadata": request.metadata.clone(),
    })
}

pub(super) fn usage_json(usage: CompatUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "output_tokens_details": {
            "reasoning_tokens": 0,
        },
        "total_tokens": usage.total_tokens,
    })
}

pub(super) async fn store_response_state(
    pool: &SqlitePool,
    client_id: Option<i64>,
    request: &PreparedResponseRequest,
    output: Vec<Value>,
    output_text: String,
    usage: Option<CompatUsage>,
    assistant_message: Value,
) -> anyhow::Result<()> {
    let mut chat_messages = request.chat_messages.clone();
    chat_messages.push(assistant_message);
    db::insert_response_state(
        pool,
        &db::ResponseState {
            id: request.response_id.clone(),
            previous_response_id: request.previous_response_id.clone(),
            client_id,
            model: request.model.clone(),
            chat_messages_json: serde_json::to_string(&chat_messages)?,
            output_json: serde_json::to_string(&output)?,
            output_text,
            input_tokens: usage.and_then(|usage| usage.input_tokens),
            output_tokens: usage.and_then(|usage| usage.output_tokens),
            total_tokens: usage.and_then(|usage| usage.total_tokens),
        },
    )
    .await
}
