use super::*;
use axum::{body::Bytes, http::request::Parts};

pub(super) async fn save_audit_best_effort(
    state: &AppState,
    started_at: Instant,
    mut audit: db::RequestAudit,
    attempts: &[db::AttemptAudit],
) -> Option<i64> {
    audit.completed_at = db::now_epoch();
    audit.duration_ms = elapsed_ms(started_at);
    audit.attempts = attempts.len() as i64;
    match db::insert_request_audit(&state.pool, &audit, attempts).await {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!(error = %error, "failed to write request audit");
            None
        }
    }
}

pub(super) fn new_request_audit(parts: &Parts) -> db::RequestAudit {
    let headers = &parts.headers;
    let (client_ip, client_ip_source) = extract_client_ip(headers);
    let query = parts.uri.query();
    db::RequestAudit {
        completed_at: 0,
        duration_ms: 0,
        client_id: None,
        client_name: None,
        client_token_id: None,
        client_token_name: None,
        client_key_hash: None,
        client_ip,
        client_ip_source,
        cf_ray: header_string(headers, "cf-ray", 100),
        cf_country: header_string(headers, "cf-ipcountry", 10),
        method: parts.method.as_str().to_string(),
        path: sanitize_audit_text(parts.uri.path(), 500),
        route_kind: route_kind(parts.uri.path()).to_string(),
        has_query: query.is_some(),
        query_hash: query.map(db::hash_secret),
        model: None,
        stream: None,
        content_type: header_string(headers, "content-type", 200),
        request_body_bytes: content_length(headers),
        user_agent_hash: header_string(headers, "user-agent", 1000)
            .map(|value| db::hash_secret(&value)),
        upstream_id: None,
        upstream_name: None,
        upstream_key_id: None,
        upstream_key_name: None,
        status: None,
        outcome: "unknown".to_string(),
        error_class: None,
        error_message: None,
        attempts: 0,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
    }
}

pub(super) fn should_capture_usage(audit: &db::RequestAudit) -> bool {
    matches!(
        audit.route_kind.as_str(),
        "chat_completions" | "responses" | "completions" | "embeddings"
    )
}

pub(super) fn is_json_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("json"))
        .unwrap_or(false)
}

pub(super) fn is_event_stream_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

pub(super) fn extract_token_usage(body: &Bytes) -> Option<TokenUsage> {
    if body.len() > MAX_AUDIT_BODY_PARSE_BYTES {
        return None;
    }
    let value = serde_json::from_slice::<Value>(body).ok()?;
    extract_token_usage_from_value(&value)
}

pub(super) fn extract_token_usage_from_value(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    let input_tokens = usage_i64(usage, &["input_tokens", "prompt_tokens"]);
    let output_tokens = usage_i64(usage, &["output_tokens", "completion_tokens"]);
    let total_tokens = usage_i64(usage, &["total_tokens"]);
    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }
    Some(TokenUsage {
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

pub(super) fn apply_token_usage(audit: &mut db::RequestAudit, usage: Option<TokenUsage>) {
    if let Some(usage) = usage {
        audit.input_tokens = usage.input_tokens;
        audit.output_tokens = usage.output_tokens;
        audit.total_tokens = usage.total_tokens;
    }
}

pub(super) fn enrich_request_audit_from_body(audit: &mut db::RequestAudit, body: &Bytes) {
    audit.request_body_bytes = Some(body.len().min(i64::MAX as usize) as i64);
    if body.len() > MAX_AUDIT_BODY_PARSE_BYTES {
        return;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return;
    };
    audit.model = value
        .get("model")
        .and_then(Value::as_str)
        .map(|value| sanitize_audit_text(value, 200));
    audit.stream = value.get("stream").and_then(Value::as_bool);
}

pub(super) struct AttemptAuditInput<'a> {
    pub(super) attempt_index: i64,
    pub(super) status: Option<u16>,
    pub(super) outcome: &'a str,
    pub(super) retriable: bool,
    pub(super) duration_ms: i64,
    pub(super) retry_after_secs: Option<i64>,
    pub(super) disabled_until: Option<i64>,
    pub(super) error_class: Option<&'a str>,
    pub(super) error_message: Option<&'a str>,
    pub(super) upstream_response: Option<&'a UpstreamResponseAudit>,
}

pub(super) fn new_attempt_audit(
    candidate: &Candidate,
    input: AttemptAuditInput<'_>,
) -> db::AttemptAudit {
    db::AttemptAudit {
        attempt_index: input.attempt_index,
        upstream_id: candidate.upstream_id,
        upstream_name: candidate.upstream_name.clone(),
        upstream_key_id: candidate.key_id,
        upstream_key_name: candidate.key_name.clone(),
        status: input.status.map(i64::from),
        outcome: input.outcome.to_string(),
        retriable: input.retriable,
        duration_ms: input.duration_ms,
        retry_after_secs: input.retry_after_secs,
        disabled_until: input.disabled_until,
        error_class: input.error_class.map(str::to_string),
        error_message: input
            .error_message
            .map(|value| sanitize_audit_text(value, 500)),
        upstream_content_type: input
            .upstream_response
            .and_then(|response| response.content_type.clone()),
        upstream_body_bytes: input
            .upstream_response
            .and_then(|response| response.body_bytes),
        upstream_body_hash: input
            .upstream_response
            .and_then(|response| response.body_hash.clone()),
        upstream_body_kind: input
            .upstream_response
            .and_then(|response| response.body_kind.clone()),
    }
}
