use crate::{db, http_proxy::StreamUsageAuditHandle};
use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;
use std::collections::HashMap;

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResponseToolKind {
    Function,
    Custom,
}

#[derive(Debug, Clone)]
struct ResponseToolMapping {
    kind: ResponseToolKind,
    namespace: Option<String>,
    name: String,
}

#[derive(Debug, Clone, Default)]
struct ResponseToolMap {
    by_chat_name: HashMap<String, ResponseToolMapping>,
}

impl ResponseToolMap {
    fn insert(
        &mut self,
        chat_name: String,
        kind: ResponseToolKind,
        namespace: Option<String>,
        name: String,
    ) {
        self.by_chat_name.insert(
            chat_name,
            ResponseToolMapping {
                kind,
                namespace,
                name,
            },
        );
    }

    fn get(&self, chat_name: &str) -> Option<&ResponseToolMapping> {
        self.by_chat_name.get(chat_name)
    }
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
                            record_stream_audit_error(
                                &pool,
                                &audit_handle,
                                "response_stream_conversion_error",
                                &error.to_string(),
                            )
                            .await;
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
                    record_stream_audit_error(
                        &pool,
                        &audit_handle,
                        "response_stream_transport_error",
                        &error.to_string(),
                    )
                    .await;
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
                    record_stream_audit_error(
                        &pool,
                        &audit_handle,
                        "response_state_store_error",
                        &error.to_string(),
                    )
                    .await;
                    tracing::warn!(error = %error, response_id = finalized.request.response_id.as_str(), "failed to store responses compatibility state");
                }
            }
            Err(error) => {
                record_stream_audit_error(
                    &pool,
                    &audit_handle,
                    "response_stream_conversion_error",
                    &error.to_string(),
                )
                .await;
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

async fn record_stream_audit_error(
    pool: &SqlitePool,
    audit_handle: &StreamUsageAuditHandle,
    error_class: &str,
    error_message: &str,
) {
    if let Some(audit_id) = audit_handle.audit_id()
        && let Err(error) =
            db::update_request_audit_stream_error(pool, audit_id, error_class, error_message).await
    {
        tracing::warn!(error = %error, audit_id, "failed to update streaming responses audit error");
    }
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

fn chat_role(role: &str) -> &str {
    match role {
        "developer" | "system" => "system",
        "assistant" => "assistant",
        "tool" => "tool",
        _ => "user",
    }
}

fn is_context_response_item(item_type: &str) -> bool {
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

fn map_tools(tools: &Value, tool_map: &mut ResponseToolMap) -> anyhow::Result<Vec<Value>> {
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
                let name = response_tool_name(object, "function tool")?;
                mapped.push(function_tool_for_chat(object, &name, None)?);
                tool_map.insert(name.clone(), ResponseToolKind::Function, None, name);
            }
            Some("custom") => {
                let name = response_tool_name(object, "custom tool")?;
                mapped.push(custom_tool_for_chat(object, &name, None)?);
                tool_map.insert(name.clone(), ResponseToolKind::Custom, None, name);
            }
            Some("namespace") => {
                let namespace = string_field(object, "name")
                    .ok_or_else(|| anyhow::anyhow!("namespace tool requires name"))?;
                let namespace_description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let Some(namespace_tools) = object.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for namespace_tool in namespace_tools {
                    let namespace_tool = namespace_tool.as_object().ok_or_else(|| {
                        anyhow::anyhow!("namespace tool definitions must be objects")
                    })?;
                    let name = response_tool_name(namespace_tool, "namespace tool")?;
                    let chat_name = namespaced_tool_name(&namespace, &name);
                    match namespace_tool.get("type").and_then(Value::as_str) {
                        Some("function") => {
                            mapped.push(function_tool_for_chat(
                                namespace_tool,
                                &chat_name,
                                namespace_description.as_deref(),
                            )?);
                            tool_map.insert(
                                chat_name,
                                ResponseToolKind::Function,
                                Some(namespace.clone()),
                                name,
                            );
                        }
                        Some("custom") => {
                            mapped.push(custom_tool_for_chat(
                                namespace_tool,
                                &chat_name,
                                namespace_description.as_deref(),
                            )?);
                            tool_map.insert(
                                chat_name,
                                ResponseToolKind::Custom,
                                Some(namespace.clone()),
                                name,
                            );
                        }
                        Some(other) => {
                            anyhow::bail!("unsupported namespace tool type: {other}");
                        }
                        None => anyhow::bail!("namespace tool definition requires type"),
                    }
                }
            }
            Some(_) => {}
            None => anyhow::bail!("tool definition requires type"),
        }
    }
    Ok(mapped)
}

fn map_tool_choice(
    tool_choice: &Value,
    _tool_map: &ResponseToolMap,
) -> anyhow::Result<Option<Value>> {
    if tool_choice.is_string() {
        return Ok(Some(tool_choice.clone()));
    }
    let Some(object) = tool_choice.as_object() else {
        anyhow::bail!("tool_choice must be a string or object");
    };
    if object.get("function").is_some() {
        return Ok(Some(tool_choice.clone()));
    }
    match object.get("type").and_then(Value::as_str) {
        Some("function" | "custom") => {
            let name = string_field(object, "name")
                .ok_or_else(|| anyhow::anyhow!("tool_choice requires name"))?;
            let chat_name = match string_field(object, "namespace") {
                Some(namespace) => namespaced_tool_name(&namespace, &name),
                None => name,
            };
            return Ok(Some(json!({
                "type": "function",
                "function": {
                    "name": chat_name,
                }
            })));
        }
        Some("namespace") => {
            return Ok(None);
        }
        _ => {}
    }
    Ok(None)
}

fn response_tool_name(object: &Map<String, Value>, context: &str) -> anyhow::Result<String> {
    if let Some(name) = object
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        return Ok(name);
    }
    string_field(object, "name").ok_or_else(|| anyhow::anyhow!("{context} requires name"))
}

fn function_tool_for_chat(
    object: &Map<String, Value>,
    chat_name: &str,
    namespace_description: Option<&str>,
) -> anyhow::Result<Value> {
    let mut function = object
        .get("function")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    function.insert("name".to_string(), Value::String(chat_name.to_string()));
    if !function.contains_key("description") {
        if let Some(description) = object.get("description") {
            function.insert("description".to_string(), description.clone());
        }
    }
    if let Some(namespace_description) = namespace_description {
        prepend_description(&mut function, namespace_description);
    }
    if !function.contains_key("parameters") {
        if let Some(parameters) = object.get("parameters") {
            function.insert("parameters".to_string(), parameters.clone());
        }
    }
    if !function.contains_key("strict") {
        if let Some(strict) = object.get("strict") {
            function.insert("strict".to_string(), strict.clone());
        }
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

fn custom_tool_for_chat(
    object: &Map<String, Value>,
    chat_name: &str,
    namespace_description: Option<&str>,
) -> anyhow::Result<Value> {
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(chat_name.to_string()));
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Free-form custom tool input.");
    function.insert(
        "description".to_string(),
        Value::String(format!(
            "{description}\nPut the exact custom tool input in the `input` string."
        )),
    );
    if let Some(namespace_description) = namespace_description {
        prepend_description(&mut function, namespace_description);
    }
    function.insert(
        "parameters".to_string(),
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Exact free-form input for this custom tool."
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    );
    if let Some(strict) = object.get("strict") {
        function.insert("strict".to_string(), strict.clone());
    }
    Ok(json!({
        "type": "function",
        "function": Value::Object(function),
    }))
}

fn prepend_description(function: &mut Map<String, Value>, namespace_description: &str) {
    if namespace_description.trim().is_empty() {
        return;
    }
    let existing = function
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let description = if existing.is_empty() {
        namespace_description.to_string()
    } else {
        format!("{namespace_description}\n{existing}")
    };
    function.insert("description".to_string(), Value::String(description));
}

fn namespaced_tool_name(namespace: &str, name: &str) -> String {
    format!("{namespace}.{name}")
}

fn custom_input_to_chat_arguments(input: &str) -> String {
    json!({ "input": input }).to_string()
}

fn custom_input_from_chat_arguments(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(object)) => object
            .get("input")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| Value::Object(object).to_string()),
        Ok(Value::String(input)) => input,
        Ok(value) => value.to_string(),
        Err(_) => arguments.to_string(),
    }
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
            output.push(tool_output_item(request, tool_call, "completed", None));
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

fn tool_output_item(
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
            if self.is_custom_tool_call(tool_call) {
                frames.push(sse_event(
                    "response.custom_tool_call_input.done",
                    &json!({
                        "type": "response.custom_tool_call_input.done",
                        "item_id": tool_call.id,
                        "output_index": tool_call.output_index,
                        "input": custom_input_from_chat_arguments(&tool_call.arguments),
                    }),
                ));
            } else {
                frames.push(sse_event(
                    "response.function_call_arguments.done",
                    &json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": tool_call.id,
                        "output_index": tool_call.output_index,
                        "arguments": tool_call.arguments,
                    }),
                ));
            }
            frames.push(sse_event(
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done",
                    "output_index": tool_call.output_index,
                    "item": self.stream_tool_output_item(tool_call, "completed"),
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
        let added = {
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
            if !tool_call.added {
                tool_call.added = true;
                true
            } else {
                false
            }
        };
        let tool_call = self.tool_calls[index].clone();
        let mut frames = Vec::new();
        if added {
            frames.push(sse_event(
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added",
                    "output_index": tool_call.output_index,
                    "item": self.stream_tool_output_item(&tool_call, "in_progress"),
                }),
            ));
        }
        if let Some(function) = delta.get("function")
            && let Some(arguments) = function.get("arguments").and_then(Value::as_str)
            && !arguments.is_empty()
        {
            if self.is_custom_tool_call(&tool_call) {
                frames.push(sse_event(
                    "response.custom_tool_call_input.delta",
                    &json!({
                        "type": "response.custom_tool_call_input.delta",
                        "item_id": tool_call.id,
                        "output_index": tool_call.output_index,
                        "delta": arguments,
                    }),
                ));
            } else {
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
                .map(|tool_call| self.stream_tool_output_item(tool_call, "completed")),
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

    fn is_custom_tool_call(&self, tool_call: &StreamToolCall) -> bool {
        self.request
            .tool_map
            .get(&tool_call.name)
            .map(|mapping| mapping.kind == ResponseToolKind::Custom)
            .unwrap_or(false)
    }

    fn stream_tool_output_item(&self, tool_call: &StreamToolCall, status: &str) -> Value {
        let chat_tool_call = json!({
            "id": tool_call.call_id,
            "type": "function",
            "function": {
                "name": tool_call.name,
                "arguments": tool_call.arguments,
            }
        });
        tool_output_item(&self.request, &chat_tool_call, status, Some(&tool_call.id))
    }
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
    use sqlx::Row;

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
    async fn responses_request_maps_codex_namespace_and_custom_tools_for_chat_adapter() {
        let path = temp_sqlite_path("route-llm-responses-codex-tools");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let body = Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":"ping",
                "stream":true,
                "parallel_tool_calls":false,
                "tools":[
                    {"type":"namespace","name":"mcp","tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}]},
                    {"type":"web_search","search_context_size":"low"},
                    {"type":"custom","name":"shell","description":"Run shell"},
                    {"type":"image_generation","quality":"low"}
                ],
                "tool_choice":{"type":"web_search"}
            }"#,
        );

        let prepared = prepare_request(&pool, None, &body).await.unwrap();
        let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
        let outbound_value: Value = serde_json::from_slice(&outbound).unwrap();

        assert_eq!(outbound_value["model"], "provider-llm");
        assert_eq!(outbound_value["messages"][0]["content"], "ping");
        assert_eq!(outbound_value["stream_options"]["include_usage"], true);
        assert_eq!(outbound_value["parallel_tool_calls"], false);
        let tools = outbound_value["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "mcp.lookup");
        assert_eq!(tools[1]["function"]["name"], "shell");
        assert_eq!(
            tools[1]["function"]["parameters"]["properties"]["input"]["type"],
            "string"
        );
        assert!(outbound_value.get("tool_choice").is_none());

        let chat_response = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        );
        let (body, usage) = convert_json_response(&pool, None, &prepared, &chat_response)
            .await
            .unwrap();
        let response_value: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(response_value["output_text"], "pong");
        assert_eq!(response_value["tools"].as_array().unwrap().len(), 4);
        assert_eq!(response_value["tool_choice"]["type"], "web_search");
        assert_eq!(usage.unwrap().total_tokens, Some(5));
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn json_response_restores_namespace_and_custom_tool_calls() {
        let path = temp_sqlite_path("route-llm-responses-tool-output-map");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let request = prepare_request(
            &pool,
            None,
            &Bytes::from_static(
                br#"{
                    "model":"llm-model",
                    "input":"ping",
                    "tools":[
                        {"type":"namespace","name":"mcp","tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}]},
                        {"type":"custom","name":"terminal","description":"Terminal"}
                    ]
                }"#,
            ),
        )
        .await
        .unwrap();
        let chat_response = Bytes::from_static(
            br#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_lookup","type":"function","function":{"name":"mcp.lookup","arguments":"{\"q\":\"x\"}"}},{"id":"call_terminal","type":"function","function":{"name":"terminal","arguments":"{\"input\":\"echo hi\"}"}}]}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
        );

        let (body, usage) = convert_json_response(&pool, None, &request, &chat_response)
            .await
            .unwrap();
        let response_value: Value = serde_json::from_slice(&body).unwrap();
        let output = response_value["output"].as_array().unwrap();

        assert_eq!(output[0]["type"], "function_call");
        assert_eq!(output[0]["namespace"], "mcp");
        assert_eq!(output[0]["name"], "lookup");
        assert_eq!(output[0]["arguments"], "{\"q\":\"x\"}");
        assert_eq!(output[1]["type"], "custom_tool_call");
        assert_eq!(output[1]["name"], "terminal");
        assert_eq!(output[1]["input"], "echo hi");
        assert_eq!(usage.unwrap().total_tokens, Some(5));
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn responses_input_restores_tool_calls_for_chat_history() {
        let path = temp_sqlite_path("route-llm-responses-input-tool-calls");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let request = prepare_request(
            &pool,
            None,
            &Bytes::from_static(
                br#"{
                    "model":"llm-model",
                    "input":[
                        {"type":"message","role":"developer","content":[{"type":"input_text","text":"Be terse."}]},
                        {"type":"message","role":"user","content":[{"type":"input_text","text":"ping"}]},
                        {"type":"function_call","namespace":"mcp","name":"lookup","call_id":"call_lookup","arguments":"{\"q\":\"x\"}"},
                        {"type":"function_call_output","call_id":"call_lookup","output":"found"},
                        {"type":"custom_tool_call","name":"terminal","call_id":"call_terminal","input":"echo hi"},
                        {"type":"custom_tool_call_output","call_id":"call_terminal","output":"hi"}
                    ]
                }"#,
            ),
        )
        .await
        .unwrap();

        assert_eq!(request.chat_messages[0]["role"], "system");
        assert_eq!(request.chat_messages[1]["role"], "user");
        assert_eq!(
            request.chat_messages[2]["tool_calls"][0]["function"]["name"],
            "mcp.lookup"
        );
        assert_eq!(request.chat_messages[3]["role"], "tool");
        assert_eq!(
            request.chat_messages[4]["tool_calls"][0]["function"]["arguments"],
            "{\"input\":\"echo hi\"}"
        );
        assert_eq!(request.chat_messages[5]["content"], "hi");
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn responses_request_keeps_function_tools_when_ignoring_builtin_tools() {
        let path = temp_sqlite_path("route-llm-responses-mixed-tools");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let body = Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":"ping",
                "tools":[
                    {"type":"namespace","name":"mcp"},
                    {"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}},
                    {"type":"web_search"}
                ],
                "tool_choice":{"type":"function","name":"lookup"}
            }"#,
        );

        let prepared = prepare_request(&pool, None, &body).await.unwrap();
        let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
        let value: Value = serde_json::from_slice(&outbound).unwrap();
        let tools = value["tools"].as_array().unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "lookup");
        assert_eq!(value["tool_choice"]["function"]["name"], "lookup");
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn stream_adapter_completes_when_request_includes_ignored_tools() {
        let path = temp_sqlite_path("route-llm-responses-stream-ignored-tools");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let request = prepare_request(
            &pool,
            None,
            &Bytes::from_static(
                br#"{
                    "model":"llm-model",
                    "input":"ping",
                    "stream":true,
                    "tools":[
                        {"type":"namespace","name":"mcp"},
                        {"type":"web_search"},
                        {"type":"custom","name":"terminal"},
                        {"type":"image_generation"}
                    ],
                    "tool_choice":{"type":"custom","name":"terminal"}
                }"#,
            ),
        )
        .await
        .unwrap();
        let mut adapter = ChatStreamAdapter::new(request);
        let mut frames = adapter.start_frames();
        frames.extend(
            adapter
                .push_bytes(&Bytes::from_static(
                    br#"data: {"choices":[{"delta":{"content":"po"}}]}
data: {"choices":[{"delta":{"content":"ng"}}]}
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
        assert!(text.contains("event: response.completed"));
        assert_eq!(finalized.output_text, "pong");
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
            tool_map: ResponseToolMap::default(),
            response_tools: json!([]),
            response_tool_choice: Value::String("auto".to_string()),
            parallel_tool_calls: Value::Bool(true),
            max_output_tokens: Value::Null,
            temperature: Value::Null,
            top_p: Value::Null,
            store: Value::Bool(true),
            reasoning: json!({"effort": Value::Null, "summary": Value::Null}),
            text: json!({"format": {"type": "text"}}),
            truncation: Value::String("disabled".to_string()),
            metadata: json!({}),
            user: Value::Null,
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
            tool_map: ResponseToolMap::default(),
            response_tools: json!([]),
            response_tool_choice: Value::String("auto".to_string()),
            parallel_tool_calls: Value::Bool(true),
            max_output_tokens: Value::Null,
            temperature: Value::Null,
            top_p: Value::Null,
            store: Value::Bool(true),
            reasoning: json!({"effort": Value::Null, "summary": Value::Null}),
            text: json!({"format": {"type": "text"}}),
            truncation: Value::String("disabled".to_string()),
            metadata: json!({}),
            user: Value::Null,
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

    #[tokio::test]
    async fn stream_adapter_restores_namespace_custom_tool_call_events() {
        let path = temp_sqlite_path("route-llm-responses-stream-custom-tool");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let request = prepare_request(
            &pool,
            None,
            &Bytes::from_static(
                br#"{
                    "model":"llm-model",
                    "input":"ping",
                    "stream":true,
                    "tools":[
                        {"type":"namespace","name":"mcp","tools":[{"type":"custom","name":"terminal","description":"Terminal"}]}
                    ]
                }"#,
            ),
        )
        .await
        .unwrap();
        let mut adapter = ChatStreamAdapter::new(request);
        let frames = adapter
            .push_bytes(&Bytes::from_static(
                br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"mcp.terminal","arguments":"{\"input\":\"echo"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" hi\"}"}}]}}]}

"#,
            ))
            .unwrap();
        let finalized = adapter.finish().unwrap();
        let text = frames
            .into_iter()
            .chain(finalized.frames)
            .map(|frame| String::from_utf8_lossy(&frame).to_string())
            .collect::<String>();

        assert!(text.contains("response.custom_tool_call_input.delta"));
        assert!(text.contains("response.custom_tool_call_input.done"));
        assert_eq!(finalized.output[0]["type"], "custom_tool_call");
        assert_eq!(finalized.output[0]["namespace"], "mcp");
        assert_eq!(finalized.output[0]["name"], "terminal");
        assert_eq!(finalized.output[0]["input"], "echo hi");
        pool.close().await;
        remove_sqlite_files(path);
    }

    #[tokio::test]
    async fn streaming_conversion_error_updates_request_audit_outcome() {
        let path = temp_sqlite_path("route-llm-responses-stream-audit-error");
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let request = prepare_request(
            &pool,
            None,
            &Bytes::from_static(br#"{"model":"llm-model","input":"ping","stream":true}"#),
        )
        .await
        .unwrap();
        let audit_id = db::insert_request_audit(
            &pool,
            &db::RequestAudit {
                completed_at: db::now_epoch(),
                duration_ms: 1,
                client_id: None,
                client_name: None,
                client_token_id: None,
                client_token_name: None,
                client_key_hash: None,
                client_ip: None,
                client_ip_source: None,
                cf_ray: None,
                cf_country: None,
                method: "POST".to_string(),
                path: "/v1/responses".to_string(),
                route_kind: "responses".to_string(),
                has_query: false,
                query_hash: None,
                model: Some("llm-model".to_string()),
                stream: Some(true),
                content_type: Some("application/json".to_string()),
                request_body_bytes: Some(64),
                user_agent_hash: None,
                upstream_id: None,
                upstream_name: Some("provider".to_string()),
                upstream_key_id: None,
                upstream_key_name: Some("key".to_string()),
                status: Some(200),
                outcome: "success".to_string(),
                error_class: None,
                error_message: None,
                attempts: 1,
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            },
            &[],
        )
        .await
        .unwrap();
        let audit_handle = StreamUsageAuditHandle::new();
        audit_handle.set_audit_id(audit_id);
        let upstream =
            futures_util::stream::iter(vec![Ok(Bytes::from_static(b"data: {not-json}\n\n"))]);

        let body = convert_streaming_response(pool.clone(), upstream, request, None, audit_handle);
        let _ = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();

        let row = sqlx::query(
            r#"
            SELECT outcome, error_class, error_message
            FROM request_audits
            WHERE id = ?;
            "#,
        )
        .bind(audit_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("outcome"), "response_stream_error");
        assert_eq!(
            row.get::<String, _>("error_class"),
            "response_stream_conversion_error"
        );
        assert!(!row.get::<String, _>("error_message").is_empty());

        pool.close().await;
        remove_sqlite_files(path);
    }

    fn temp_sqlite_path(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{name}-{}-{unique}.sqlite", std::process::id()))
    }

    fn remove_sqlite_files(path: std::path::PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    }
}
