use super::*;
use super::{
    json_response::{message_output_item, response_json, store_response_state, tool_output_item},
    usage::usage_from_response,
};
use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

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

pub(super) async fn record_stream_audit_error(
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

#[derive(Debug, Clone)]
pub(super) struct StreamToolCall {
    output_index: usize,
    id: String,
    call_id: String,
    name: String,
    arguments: String,
    added: bool,
}

pub(super) struct ChatStreamAdapter {
    request: PreparedResponseRequest,
    buffer: String,
    output_text: String,
    text_started: bool,
    usage: Option<CompatUsage>,
    tool_calls: Vec<StreamToolCall>,
}

pub(super) struct FinalizedStream {
    pub(super) request: PreparedResponseRequest,
    pub(super) frames: Vec<Bytes>,
    pub(super) output: Vec<Value>,
    pub(super) output_text: String,
    pub(super) usage: Option<CompatUsage>,
    pub(super) assistant_message: Value,
}

impl ChatStreamAdapter {
    pub(super) fn new(request: PreparedResponseRequest) -> Self {
        Self {
            request,
            buffer: String::new(),
            output_text: String::new(),
            text_started: false,
            usage: None,
            tool_calls: Vec::new(),
        }
    }

    pub(super) fn start_frames(&self) -> Vec<Bytes> {
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

    pub(super) fn push_bytes(&mut self, chunk: &Bytes) -> anyhow::Result<Vec<Bytes>> {
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

    pub(super) fn finish(mut self) -> anyhow::Result<FinalizedStream> {
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

pub(super) fn sse_event(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}
