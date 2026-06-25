use super::*;
use serde_json::Value;

pub(super) fn usage_from_response(value: &Value) -> Option<CompatUsage> {
    usage_from_value(value.get("usage")?)
}

pub(super) fn usage_from_value(usage: &Value) -> Option<CompatUsage> {
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

pub(super) fn usage_i64(usage: &Value, fields: &[&str]) -> Option<i64> {
    fields
        .iter()
        .find_map(|field| usage.get(*field).and_then(Value::as_i64))
}
