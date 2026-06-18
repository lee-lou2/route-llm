use crate::{db, http_proxy::StreamUsageAuditHandle};
use axum::body::{Body, Bytes};
use futures_util::StreamExt;
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
    response_tools: Value,
    response_tool_choice: Value,
    parallel_tool_calls: Value,
    max_output_tokens: Value,
    temperature: Value,
    top_p: Value,
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

    let response_tools = object.get("tools").cloned().unwrap_or_else(|| json!([]));
    if let Some(tools) = object.get("tools") {
        chat_body.insert("tools".to_string(), map_tools(tools)?);
    }
    let response_tool_choice = object
        .get("tool_choice")
        .cloned()
        .unwrap_or_else(|| Value::String("auto".to_string()));
    if let Some(tool_choice) = object.get("tool_choice") {
        chat_body.insert("tool_choice".to_string(), map_tool_choice(tool_choice)?);
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
    })
}

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

pub(crate) fn convert_streaming_response(
    pool: SqlitePool,
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    request: PreparedResponseRequest,
    client_id: Option<i64>,
    audit_handle: StreamUsageAuditHandle,
) -> Body {
    let mut stream = Box::pin(stream);
    let body_stream = async_stream::stream! {
        let mut adapter = ChatStreamAdapter::new(request);
        for frame in adapter.start_frames() {
            yield Ok::<Bytes, std::io::Error>(frame);
        }

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    match adapter.push_bytes(&chunk) {
                        Ok(frames) => {
                            for frame in frames {
                                yield Ok::<Bytes, std::io::Error>(frame);
                            }
                        }
                        Err(error) => {
                            yield Ok::<Bytes, std::io::Error>(sse_event("error", &json!({
                                "type": "error",
                                "error": {
                                    "message": error.to_string(),
                                    "type": "route_llm_response_compat_error"
                                }
                            })));
                            return;
                        }
                    }
                }
                Err(error) => {
                    yield Err(std::io::Error::other(error.to_string()));
                    return;
                }
            }
        }

        match adapter.finish() {
            Ok(finalized) => {
                for frame in finalized.frames {
                    yield Ok::<Bytes, std::io::Error>(frame);
                }
                if let (Some(audit_id), Some(usage)) = (audit_handle.audit_id(), finalized.usage)
                    && let Err(error) = db::update_request_audit_usage(
                        &pool,
                        audit_id,
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.total_tokens,
                    )
                    .await
                {
                    tracing::warn!(error = %error, audit_id, "failed to update streaming responses token usage");
                }
                if let Err(error) = store_response_state(
                    &pool,
                    client_id,
                    &finalized.request,
                    finalized.output,
                    finalized.output_text,
                    finalized.usage,
                    finalized.assistant_message,
                )
                .await
                {
                    tracing::warn!(error = %error, response_id = finalized.request.response_id.as_str(), "failed to store responses compatibility state");
                }
            }
            Err(error) => {
                yield Ok::<Bytes, std::io::Error>(sse_event("error", &json!({
                    "type": "error",
                    "error": {
                        "message": error.to_string(),
                        "type": "route_llm_response_compat_error"
                    }
                })));
            }
        }
    };
    Body::from_stream(body_stream)
}

fn string_field(object: &Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bool_field(object: &Map<String, Value>, field: &str) -> Option<bool> {
    object.get(field).and_then(Value::as_bool)
}

fn copy_if_present(from: &Map<String, Value>, to: &mut Map<String, Value>, field: &str) {
    if let Some(value) = from.get(field) {
        to.insert(field.to_string(), value.clone());
    }
}

fn append_input_messages(messages: &mut Vec<Value>, input: &Value) -> anyhow::Result<()> {
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

fn append_input_item(messages: &mut Vec<Value>, item: &Value) -> anyhow::Result<()> {
    let Some(object) = item.as_object() else {
        anyhow::bail!("responses input array items must be objects");
    };
    let item_type = object.get("type").and_then(Value::as_str);
    match item_type {
        Some("function_call_output") => {
            let call_id = string_field(object, "call_id")
                .or_else(|| string_field(object, "tool_call_id"))
                .ok_or_else(|| anyhow::anyhow!("function_call_output requires call_id"))?;
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
                        "name": name,
                        "arguments": arguments,
                    }
                }]
            }));
        }
        Some("message") | None => {
            let role = object.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = object
                .get("content")
                .map(content_to_chat)
                .unwrap_or_else(|| Value::String(String::new()));
            messages.push(json!({
                "role": role,
                "content": content,
            }));
        }
        Some(other) => anyhow::bail!("unsupported responses input item type: {other}"),
    }
    Ok(())
}

fn content_to_chat(content: &Value) -> Value {
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

fn map_tools(tools: &Value) -> anyhow::Result<Value> {
    let Some(items) = tools.as_array() else {
        anyhow::bail!("responses `tools` must be an array");
    };
    let mut mapped = Vec::with_capacity(items.len());
    for item in items {
        let object = item
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("tool definitions must be objects"))?;
        match object.get("type").and_then(Value::as_str) {
            Some("function") => {
                if let Some(function) = object.get("function") {
                    mapped.push(json!({
                        "type": "function",
                        "function": function,
                    }));
                    continue;
                }
                let name = string_field(object, "name")
                    .ok_or_else(|| anyhow::anyhow!("function tool requires name"))?;
                let mut function = Map::new();
                function.insert("name".to_string(), Value::String(name));
                if let Some(description) = object.get("description") {
                    function.insert("description".to_string(), description.clone());
                }
                if let Some(parameters) = object.get("parameters") {
                    function.insert("parameters".to_string(), parameters.clone());
                }
                if let Some(strict) = object.get("strict") {
                    function.insert("strict".to_string(), strict.clone());
                }
                mapped.push(json!({
                    "type": "function",
                    "function": Value::Object(function),
                }));
            }
            Some(other) => {
                anyhow::bail!("unsupported responses tool type for chat adapter: {other}")
            }
            None => anyhow::bail!("tool definition requires type"),
        }
    }
    Ok(Value::Array(mapped))
}

fn map_tool_choice(tool_choice: &Value) -> anyhow::Result<Value> {
    if tool_choice.is_string() {
        return Ok(tool_choice.clone());
    }
    let Some(object) = tool_choice.as_object() else {
        anyhow::bail!("tool_choice must be a string or object");
    };
    if object.get("function").is_some() {
        return Ok(tool_choice.clone());
    }
    if object.get("type").and_then(Value::as_str) == Some("function") {
        let name = string_field(object, "name")
            .ok_or_else(|| anyhow::anyhow!("function tool_choice requires name"))?;
        return Ok(json!({
            "type": "function",
            "function": {
                "name": name,
            }
        }));
    }
    Ok(tool_choice.clone())
}

fn usage_from_response(value: &Value) -> Option<CompatUsage> {
    usage_from_value(value.get("usage")?)
}

fn usage_from_value(usage: &Value) -> Option<CompatUsage> {
    let input_tokens = usage_i64(usage, &["input_tokens", "prompt_tokens"]);
    let output_tokens = usage_i64(usage, &["output_tokens", "completion_tokens"]);
    let total_tokens = usage_i64(usage, &["total_tokens"]);
    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }
    Some(CompatUsage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}

fn usage_i64(usage: &Value, fields: &[&str]) -> Option<i64> {
    fields
        .iter()
        .find_map(|field| usage.get(*field).and_then(Value::as_i64))
}

fn output_from_chat_message(
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
            output.push(function_output_item(tool_call, "completed"));
        }
    }
    output
}

fn message_output_item(id: &str, status: &str, text: &str) -> Value {
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

fn function_output_item(tool_call: &Value, status: &str) -> Value {
    let id = tool_call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    json!({
        "id": prefixed_id("fc"),
        "type": "function_call",
        "status": status,
        "call_id": id,
        "name": tool_call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
        "arguments": tool_call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or(""),
    })
}

fn assistant_message_for_state(message: &Value) -> Value {
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

fn response_json(
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
        "reasoning": {
            "effort": Value::Null,
            "summary": Value::Null,
        },
        "store": true,
        "temperature": request.temperature.clone(),
        "text": {
            "format": {
                "type": "text",
            }
        },
        "tool_choice": request.response_tool_choice.clone(),
        "tools": request.response_tools.clone(),
        "top_p": request.top_p.clone(),
        "truncation": "disabled",
        "usage": usage.map(usage_json).unwrap_or(Value::Null),
        "user": Value::Null,
        "metadata": {},
    })
}

fn usage_json(usage: CompatUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "output_tokens_details": {
            "reasoning_tokens": 0,
        },
        "total_tokens": usage.total_tokens,
    })
}

async fn store_response_state(
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

#[derive(Debug, Clone)]
struct StreamToolCall {
    output_index: usize,
    id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
}

struct ChatStreamAdapter {
    request: PreparedResponseRequest,
    buffer: String,
    output_text: String,
    text_started: bool,
    usage: Option<CompatUsage>,
    tool_calls: Vec<StreamToolCall>,
}

struct FinalizedStream {
    request: PreparedResponseRequest,
    frames: Vec<Bytes>,
    output: Vec<Value>,
    output_text: String,
    usage: Option<CompatUsage>,
    assistant_message: Value,
}

impl ChatStreamAdapter {
    fn new(request: PreparedResponseRequest) -> Self {
        Self {
            request,
            buffer: String::new(),
            output_text: String::new(),
            text_started: false,
            usage: None,
            tool_calls: Vec::new(),
        }
    }

    fn start_frames(&self) -> Vec<Bytes> {
        let response = response_json(&self.request, "in_progress", Some(Vec::new()), None, None);
        vec![
            sse_event(
                "response.created",
                &json!({
                    "type": "response.created",
                    "response": response,
                }),
            ),
            sse_event(
                "response.in_progress",
                &json!({
                    "type": "response.in_progress",
                    "response": response,
                }),
            ),
        ]
    }

    fn push_bytes(&mut self, chunk: &Bytes) -> anyhow::Result<Vec<Bytes>> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut frames = Vec::new();
        while let Some(index) = self.buffer.find('\n') {
            let mut line = self.buffer.drain(..=index).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            frames.extend(self.observe_line(&line)?);
        }
        Ok(frames)
    }

    fn finish(mut self) -> anyhow::Result<FinalizedStream> {
        let mut frames = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            frames.extend(self.observe_line(&line)?);
        }
        if self.text_started {
            frames.push(sse_event(
                "response.output_text.done",
                &json!({
                    "type": "response.output_text.done",
                    "item_id": self.request.message_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": self.output_text,
                }),
            ));
            frames.push(sse_event(
                "response.content_part.done",
                &json!({
                    "type": "response.content_part.done",
                    "item_id": self.request.message_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": self.output_text,
                        "annotations": [],
                    }
                }),
            ));
            frames.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": message_output_item(&self.request.message_id, "completed", &self.output_text),
                }),
            ));
        }

        for tool_call in &self.tool_calls {
            frames.push(sse_event(
                "response.function_call_arguments.done",
                &json!({
                    "type": "response.function_call_arguments.done",
                    "item_id": tool_call.id,
                    "output_index": tool_call.output_index,
                    "arguments": tool_call.arguments,
                }),
            ));
            frames.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": tool_call.output_index,
                    "item": stream_function_output_item(tool_call, "completed"),
                }),
            ));
        }

        let output = self.output_items();
        let response = response_json(
            &self.request,
            "completed",
            Some(output.clone()),
            self.usage,
            None,
        );
        frames.push(sse_event(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": response,
            }),
        ));
        let assistant_message = self.assistant_message();
        Ok(FinalizedStream {
            request: self.request,
            frames,
            output,
            output_text: self.output_text,
            usage: self.usage,
            assistant_message,
        })
    }

    fn observe_line(&mut self, line: &str) -> anyhow::Result<Vec<Bytes>> {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(Vec::new());
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return Ok(Vec::new());
        }
        let value = serde_json::from_str::<Value>(data)?;
        if let Some(usage) = usage_from_response(&value) {
            self.usage = Some(usage);
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(Vec::new());
        };
        let Some(delta) = choice.get("delta") else {
            return Ok(Vec::new());
        };
        let mut frames = Vec::new();
        if let Some(content) = delta.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            frames.extend(self.observe_text_delta(content));
        }
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                frames.extend(self.observe_tool_call_delta(tool_call)?);
            }
        }
        Ok(frames)
    }

    fn observe_text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut frames = Vec::new();
        if !self.text_started {
            self.text_started = true;
            frames.push(sse_event(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "output_index": 0,
                    "item": message_output_item(&self.request.message_id, "in_progress", ""),
                }),
            ));
            frames.push(sse_event(
                "response.content_part.added",
                &json!({
                    "type": "response.content_part.added",
                    "item_id": self.request.message_id,
                    "output_index": 0,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "",
                        "annotations": [],
                    }
                }),
            ));
        }
        self.output_text.push_str(delta);
        frames.push(sse_event(
            "response.output_text.delta",
            &json!({
                "type": "response.output_text.delta",
                "item_id": self.request.message_id,
                "output_index": 0,
                "content_index": 0,
                "delta": delta,
            }),
        ));
        frames
    }

    fn observe_tool_call_delta(&mut self, delta: &Value) -> anyhow::Result<Vec<Bytes>> {
        let index = delta.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let output_index = if self.text_started { index + 1 } else { index };
        if self.tool_calls.len() <= index {
            self.tool_calls.resize_with(index + 1, || StreamToolCall {
                output_index,
                id: prefixed_id("fc"),
                call_id: String::new(),
                name: String::new(),
                arguments: String::new(),
                added: false,
            });
        }
        let tool_call = &mut self.tool_calls[index];
        if let Some(id) = delta.get("id").and_then(Value::as_str) {
            tool_call.call_id = id.to_string();
        }
        if let Some(function) = delta.get("function") {
            if let Some(name) = function.get("name").and_then(Value::as_str) {
                tool_call.name.push_str(name);
            }
            if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                tool_call.arguments.push_str(arguments);
            }
        }
        let mut frames = Vec::new();
        if !tool_call.added {
            tool_call.added = true;
            frames.push(sse_event(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "output_index": tool_call.output_index,
                    "item": stream_function_output_item(tool_call, "in_progress"),
                }),
            ));
        }
        if let Some(function) = delta.get("function")
            && let Some(arguments) = function.get("arguments").and_then(Value::as_str)
            && !arguments.is_empty()
        {
            frames.push(sse_event(
                "response.function_call_arguments.delta",
                &json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": tool_call.id,
                    "output_index": tool_call.output_index,
                    "delta": arguments,
                }),
            ));
        }
        Ok(frames)
    }

    fn output_items(&self) -> Vec<Value> {
        let mut output = Vec::new();
        if self.text_started || self.tool_calls.is_empty() {
            output.push(message_output_item(
                &self.request.message_id,
                "completed",
                &self.output_text,
            ));
        }
        output.extend(
            self.tool_calls
                .iter()
                .map(|tool_call| stream_function_output_item(tool_call, "completed")),
        );
        output
    }

    fn assistant_message(&self) -> Value {
        let mut object = Map::new();
        object.insert("role".to_string(), Value::String("assistant".to_string()));
        if self.output_text.is_empty() && !self.tool_calls.is_empty() {
            object.insert("content".to_string(), Value::Null);
        } else {
            object.insert(
                "content".to_string(),
                Value::String(self.output_text.clone()),
            );
        }
        if !self.tool_calls.is_empty() {
            object.insert(
                "tool_calls".to_string(),
                Value::Array(
                    self.tool_calls
                        .iter()
                        .map(|tool_call| {
                            json!({
                                "id": tool_call.call_id,
                                "type": "function",
                                "function": {
                                    "name": tool_call.name,
                                    "arguments": tool_call.arguments,
                                }
                            })
                        })
                        .collect(),
                ),
            );
        }
        Value::Object(object)
    }
}

fn stream_function_output_item(tool_call: &StreamToolCall, status: &str) -> Value {
    json!({
        "id": tool_call.id,
        "type": "function_call",
        "status": status,
        "call_id": tool_call.call_id,
        "name": tool_call.name,
        "arguments": tool_call.arguments,
    })
}

fn sse_event(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn prefixed_id(prefix: &str) -> String {
    match db::generate_client_api_key() {
        Ok(value) => format!("{prefix}_{}", &value[..32]),
        Err(_) => format!("{prefix}_{}", db::now_epoch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn responses_request_maps_text_tools_and_tool_choice_to_chat() {
        let path = std::env::temp_dir().join(format!(
            "route-llm-responses-compat-{}.sqlite",
            std::process::id()
        ));
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let body = Bytes::from_static(
            br#"{
                "model":"llm-model",
                "instructions":"Be terse.",
                "input":"ping",
                "stream":true,
                "tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}],
                "tool_choice":{"type":"function","name":"lookup"},
                "max_output_tokens":12
            }"#,
        );

        let prepared = prepare_request(&pool, Some(1), &body).await.unwrap();
        let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
        let value: Value = serde_json::from_slice(&outbound).unwrap();

        assert_eq!(value["model"], "provider-llm");
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(value["messages"][1]["content"], "ping");
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert_eq!(value["tools"][0]["function"]["name"], "lookup");
        assert_eq!(value["tool_choice"]["function"]["name"], "lookup");
        assert_eq!(value["max_tokens"], 12);
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn json_response_stores_state_for_previous_response_id() {
        let path = std::env::temp_dir().join(format!(
            "route-llm-responses-state-{}.sqlite",
            std::process::id()
        ));
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let client_id = db::upsert_client(&pool, "client", "client-test-token", true)
            .await
            .unwrap();
        let request = prepare_request(
            &pool,
            Some(client_id),
            &Bytes::from_static(br#"{"model":"llm-model","input":"ping"}"#),
        )
        .await
        .unwrap();
        let chat_response = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        );

        let (body, usage) = convert_json_response(&pool, Some(client_id), &request, &chat_response)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(value["object"], "response");
        assert_eq!(value["output_text"], "pong");
        assert_eq!(value["usage"]["input_tokens"], 3);
        assert_eq!(usage.unwrap().total_tokens, Some(5));

        let follow_up = prepare_request(
            &pool,
            Some(client_id),
            &Bytes::from(
                serde_json::json!({
                    "model": "llm-model",
                    "previous_response_id": request.response_id,
                    "input": "again"
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(follow_up.chat_messages.len(), 3);
        assert_eq!(follow_up.chat_messages[1]["content"], "pong");
        assert_eq!(follow_up.chat_messages[2]["content"], "again");
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[test]
    fn stream_adapter_emits_response_events_and_usage() {
        let request = PreparedResponseRequest {
            response_id: "resp_test".to_string(),
            message_id: "msg_test".to_string(),
            created_at: 1,
            model: "llm-model".to_string(),
            instructions: None,
            previous_response_id: None,
            stream: true,
            chat_messages: vec![json!({"role":"user","content":"ping"})],
            chat_body: json!({}),
            response_tools: json!([]),
            response_tool_choice: Value::String("auto".to_string()),
            parallel_tool_calls: Value::Bool(true),
            max_output_tokens: Value::Null,
            temperature: Value::Null,
            top_p: Value::Null,
        };
        let mut adapter = ChatStreamAdapter::new(request);
        let mut frames = adapter.start_frames();
        frames.extend(
            adapter
                .push_bytes(&Bytes::from_static(
                    br#"data: {"choices":[{"delta":{"content":"hel"}}]}
data: {"choices":[{"delta":{"content":"lo"}}]}
data: {"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}}
data: [DONE]

"#,
                ))
                .unwrap(),
        );
        let finalized = adapter.finish().unwrap();
        frames.extend(finalized.frames);
        let text = frames
            .iter()
            .map(|frame| String::from_utf8_lossy(frame).to_string())
            .collect::<String>();

        assert!(text.contains("event: response.created"));
        assert!(text.contains("event: response.output_text.delta"));
        assert!(text.contains("\"delta\":\"hel\""));
        assert!(text.contains("event: response.completed"));
        assert_eq!(finalized.output_text, "hello");
        assert_eq!(finalized.usage.unwrap().total_tokens, Some(6));
    }

    #[test]
    fn stream_adapter_emits_tool_call_events() {
        let request = PreparedResponseRequest {
            response_id: "resp_test".to_string(),
            message_id: "msg_test".to_string(),
            created_at: 1,
            model: "llm-model".to_string(),
            instructions: None,
            previous_response_id: None,
            stream: true,
            chat_messages: vec![json!({"role":"user","content":"ping"})],
            chat_body: json!({}),
            response_tools: json!([]),
            response_tool_choice: Value::String("auto".to_string()),
            parallel_tool_calls: Value::Bool(true),
            max_output_tokens: Value::Null,
            temperature: Value::Null,
            top_p: Value::Null,
        };
        let mut adapter = ChatStreamAdapter::new(request);
        let frames = adapter
            .push_bytes(&Bytes::from_static(
                br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"x\"}"}}]}}]}

"#,
            ))
            .unwrap();
        let finalized = adapter.finish().unwrap();
        let text = frames
            .into_iter()
            .chain(finalized.frames)
            .map(|frame| String::from_utf8_lossy(&frame).to_string())
            .collect::<String>();

        assert!(text.contains("response.function_call_arguments.delta"));
        assert!(text.contains("response.function_call_arguments.done"));
        assert_eq!(finalized.output[0]["type"], "function_call");
        assert_eq!(finalized.output[0]["name"], "lookup");
        assert_eq!(finalized.output[0]["arguments"], "{\"q\":\"x\"}");
    }

    fn remove_sqlite_files(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    }
}
