use axum::body::Bytes;
use serde_json::Value;

pub(super) fn request_model_from_body(body: &Bytes) -> Option<String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn body_for_candidate(
    body: &Bytes,
    resolved_model: Option<&str>,
) -> anyhow::Result<Bytes> {
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return Ok(body.clone());
    };
    let Some(current_model) = value.get("model").and_then(Value::as_str) else {
        return Ok(body.clone());
    };
    let mut changed = false;
    if let Some(resolved_model) = resolved_model
        && resolved_model != current_model
    {
        replace_model(&mut value, resolved_model.to_string());
        changed = true;
    }
    if request_value_streams(&value) {
        ensure_stream_usage_requested(&mut value);
        changed = true;
    }
    if changed {
        Ok(Bytes::from(serde_json::to_vec(&value)?))
    } else {
        Ok(body.clone())
    }
}

pub(super) fn replace_model(value: &mut Value, model: String) {
    if let Some(object) = value.as_object_mut() {
        object.insert("model".to_string(), Value::String(model));
    }
}

pub(super) fn request_value_streams(value: &Value) -> bool {
    value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn ensure_stream_usage_requested(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let stream_options = object
        .entry("stream_options".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(options) = stream_options.as_object_mut() {
        options.insert("include_usage".to_string(), Value::Bool(true));
    }
}
