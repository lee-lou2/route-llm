mod ids;
mod input;
mod json_response;
mod stream;
mod tools;
mod usage;

use ids::*;
use input::*;
pub(crate) use json_response::convert_json_response;
#[cfg(test)]
use stream::ChatStreamAdapter;
pub(crate) use stream::convert_streaming_response;
use tools::*;

use crate::{db, http_proxy::StreamUsageAuditHandle};
use axum::body::Bytes;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy)]
pub(crate) struct CompatUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedResponseRequest {
    pub response_id: String,
    pub message_id: String,
    pub created_at: i64,
    pub model: String,
    pub instructions: Option<String>,
    pub previous_response_id: Option<String>,
    pub stream: bool,
    pub chat_messages: Vec<Value>,
    chat_body: Value,
    tool_map: ResponseToolMap,
    response_tools: Value,
    response_tool_choice: Value,
    parallel_tool_calls: Value,
    max_output_tokens: Value,
    temperature: Value,
    top_p: Value,
    store: Value,
    reasoning: Value,
    text: Value,
    truncation: Value,
    metadata: Value,
    user: Value,
}

impl PreparedResponseRequest {
    pub(crate) fn body_for_candidate(&self, resolved_model: Option<&str>) -> anyhow::Result<Bytes> {
        let mut body = self.chat_body.clone();
        if let Some(resolved_model) = resolved_model {
            if let Some(object) = body.as_object_mut() {
                object.insert(
                    "model".to_string(),
                    Value::String(resolved_model.to_string()),
                );
            }
        }
        Ok(Bytes::from(serde_json::to_vec(&body)?))
    }
}

pub(crate) async fn prepare_request(
    pool: &SqlitePool,
    client_id: Option<i64>,
    body: &Bytes,
) -> anyhow::Result<PreparedResponseRequest> {
    let value = serde_json::from_slice::<Value>(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("responses request body must be a JSON object"))?;
    let model = string_field(object, "model")
        .ok_or_else(|| anyhow::anyhow!("responses request requires string field `model`"))?;
    let previous_response_id = string_field(object, "previous_response_id");
    let instructions = string_field(object, "instructions");
    let stream = bool_field(object, "stream").unwrap_or(false);

    let mut chat_messages = if let Some(previous_response_id) = previous_response_id.as_deref() {
        let state = db::get_response_state(pool, previous_response_id, client_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "previous_response_id not found for this client: {previous_response_id}"
                )
            })?;
        serde_json::from_str::<Vec<Value>>(&state.chat_messages_json)?
    } else {
        Vec::new()
    };

    if let Some(instructions) = instructions.as_deref() {
        chat_messages.push(json!({
            "role": "system",
            "content": instructions,
        }));
    }

    let input = object
        .get("input")
        .ok_or_else(|| anyhow::anyhow!("responses request requires `input`"))?;
    append_input_messages(&mut chat_messages, input)?;
    if chat_messages.is_empty() {
        anyhow::bail!("responses request did not produce any chat messages");
    }

    let mut chat_body = Map::new();
    chat_body.insert("model".to_string(), Value::String(model.clone()));
    chat_body.insert("messages".to_string(), Value::Array(chat_messages.clone()));
    if stream {
        chat_body.insert("stream".to_string(), Value::Bool(true));
        chat_body.insert(
            "stream_options".to_string(),
            json!({
                "include_usage": true
            }),
        );
    }

    copy_if_present(object, &mut chat_body, "temperature");
    copy_if_present(object, &mut chat_body, "top_p");
    copy_if_present(object, &mut chat_body, "stop");
    copy_if_present(object, &mut chat_body, "user");
    copy_if_present(object, &mut chat_body, "metadata");
    if let Some(max_output_tokens) = object.get("max_output_tokens") {
        chat_body.insert("max_tokens".to_string(), max_output_tokens.clone());
    }
    if let Some(parallel_tool_calls) = object.get("parallel_tool_calls") {
        chat_body.insert(
            "parallel_tool_calls".to_string(),
            parallel_tool_calls.clone(),
        );
    }

    let mut tool_map = ResponseToolMap::default();
    let response_tools = object.get("tools").cloned().unwrap_or_else(|| json!([]));
    if let Some(tools) = object.get("tools") {
        let mapped_tools = map_tools(tools, &mut tool_map)?;
        if !mapped_tools.is_empty() {
            chat_body.insert("tools".to_string(), Value::Array(mapped_tools));
        }
    }
    let response_tool_choice = object
        .get("tool_choice")
        .cloned()
        .unwrap_or_else(|| Value::String("auto".to_string()));
    if let Some(tool_choice) = object.get("tool_choice") {
        if let Some(mapped_tool_choice) = map_tool_choice(tool_choice, &tool_map)? {
            chat_body.insert("tool_choice".to_string(), mapped_tool_choice);
        }
    }

    Ok(PreparedResponseRequest {
        response_id: prefixed_id("resp"),
        message_id: prefixed_id("msg"),
        created_at: db::now_epoch(),
        model,
        instructions,
        previous_response_id,
        stream,
        chat_messages,
        chat_body: Value::Object(chat_body),
        tool_map,
        response_tools,
        response_tool_choice,
        parallel_tool_calls: object
            .get("parallel_tool_calls")
            .cloned()
            .unwrap_or(Value::Bool(true)),
        max_output_tokens: object
            .get("max_output_tokens")
            .cloned()
            .unwrap_or(Value::Null),
        temperature: object.get("temperature").cloned().unwrap_or(Value::Null),
        top_p: object.get("top_p").cloned().unwrap_or(Value::Null),
        store: object.get("store").cloned().unwrap_or(Value::Bool(true)),
        reasoning: object.get("reasoning").cloned().unwrap_or_else(|| {
            json!({
                "effort": Value::Null,
                "summary": Value::Null,
            })
        }),
        text: object.get("text").cloned().unwrap_or_else(|| {
            json!({
                "format": {
                    "type": "text",
                }
            })
        }),
        truncation: object
            .get("truncation")
            .cloned()
            .unwrap_or_else(|| Value::String("disabled".to_string())),
        metadata: object.get("metadata").cloned().unwrap_or_else(|| json!({})),
        user: object.get("user").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests;
