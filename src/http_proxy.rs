use crate::{
    db::{self, Candidate},
    responses_compat,
    server::AppState,
};
use axum::{
    Json,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{
        HeaderMap, HeaderName, Method, Request, Response, StatusCode, Uri,
        header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST},
        request::Parts,
    },
    response::IntoResponse,
};
use futures_util::{StreamExt, TryStreamExt};
use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_AUDIT_BODY_PARSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct TokenUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

impl From<responses_compat::CompatUsage> for TokenUsage {
    fn from(usage: responses_compat::CompatUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
}

pub async fn health() -> impl IntoResponse {
    Json(HealthResponse { status: "ok" })
}

pub async fn proxy(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
) -> impl IntoResponse {
    match proxy_inner(state, request).await {
        Ok(response) => response,
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

async fn proxy_inner(
    state: Arc<AppState>,
    request: Request<Body>,
) -> anyhow::Result<Response<Body>> {
    let started_at = Instant::now();
    let (parts, body) = request.into_parts();
    if should_ignore_proxy_request(parts.uri.path(), &state.config.public_prefix) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let mut audit = new_request_audit(&parts);
    let mut attempts = Vec::new();

    let Some(client_key) = bearer_token(&parts.headers) else {
        let status = StatusCode::UNAUTHORIZED;
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "unauthorized".to_string();
        audit.error_class = Some("missing_bearer_token".to_string());
        audit.error_message = Some("missing bearer token".to_string());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        return Ok(json_error(status, "missing bearer token".to_string()));
    };
    audit.client_key_hash = Some(db::hash_secret(client_key));
    let Some(client) = db::authenticate_client(&state.pool, client_key).await? else {
        let status = StatusCode::UNAUTHORIZED;
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "unauthorized".to_string();
        audit.error_class = Some("invalid_bearer_token".to_string());
        audit.error_message = Some("invalid bearer token".to_string());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        return Ok(json_error(status, "invalid bearer token".to_string()));
    };
    audit.client_id = Some(client.id);
    audit.client_name = Some(client.name);
    audit.client_token_id = Some(client.token_id);
    audit.client_token_name = Some(client.token_name);

    if is_models_list_request(&parts, &state.config.public_prefix) {
        audit.status = Some(i64::from(StatusCode::OK.as_u16()));
        audit.outcome = "success".to_string();
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        return models_list_response(&state).await;
    }

    let body = match to_bytes(body, state.config.max_body_bytes).await {
        Ok(body) => body,
        Err(_) => {
            let status = StatusCode::PAYLOAD_TOO_LARGE;
            audit.status = Some(i64::from(status.as_u16()));
            audit.outcome = "request_too_large".to_string();
            audit.error_class = Some("body_too_large".to_string());
            audit.error_message = Some("request body exceeds configured limit".to_string());
            save_audit_best_effort(&state, started_at, audit, &attempts).await;
            return Ok(json_error(
                status,
                "request body exceeds configured limit".to_string(),
            ));
        }
    };
    enrich_request_audit_from_body(&mut audit, &body);
    let response_request = if is_responses_create_request(&parts, &state.config.public_prefix) {
        match responses_compat::prepare_request(&state.pool, Some(client.id), &body).await {
            Ok(request) => Some(request),
            Err(error) => {
                let status = StatusCode::BAD_REQUEST;
                audit.status = Some(i64::from(status.as_u16()));
                audit.outcome = "invalid_request".to_string();
                audit.error_class = Some("invalid_responses_request".to_string());
                audit.error_message = Some(sanitize_audit_text(&error.to_string(), 500));
                save_audit_best_effort(&state, started_at, audit, &attempts).await;
                return Ok(json_error(status, error.to_string()));
            }
        }
    } else {
        None
    };
    let requested_model = request_model_from_body(&body);
    let candidates = db::candidates_for_client_request_model(
        &state.pool,
        Some(client.id),
        requested_model.as_deref(),
    )
    .await?;
    if candidates.is_empty() {
        let status = StatusCode::SERVICE_UNAVAILABLE;
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "no_upstream".to_string();
        audit.error_class = Some("no_healthy_upstream_keys".to_string());
        audit.error_message =
            Some("no healthy upstream keys available for requested model".to_string());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        return Ok(json_error(
            status,
            "no healthy upstream keys available for requested model".to_string(),
        ));
    }

    let mut last_failure: Option<UpstreamFailure> = None;
    for candidate in candidates {
        let upstream_url_result = if response_request.is_some() {
            build_upstream_url_for_path(&candidate.base_url, &parts.uri, "/chat/completions")
        } else {
            build_upstream_url(&candidate.base_url, &state.config.public_prefix, &parts.uri)
        };
        let upstream_url = match upstream_url_result {
            Ok(url) => url,
            Err(error) => {
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                audit.status = Some(i64::from(status.as_u16()));
                audit.outcome = "router_error".to_string();
                audit.error_class = Some("upstream_url_build_failed".to_string());
                audit.error_message = Some(error.to_string());
                save_audit_best_effort(&state, started_at, audit, &attempts).await;
                return Ok(json_error(
                    status,
                    "failed to build upstream URL".to_string(),
                ));
            }
        };
        tracing::info!(
            key_id = candidate.key_id,
            upstream = candidate.upstream_name.as_str(),
            upstream_priority = candidate.upstream_priority,
            model = candidate.resolved_model.as_deref().unwrap_or(""),
            model_priority = candidate.model_priority.unwrap_or_default(),
            key_priority = candidate.key_priority,
            "forwarding request"
        );

        let attempt_index = attempts.len() as i64 + 1;
        let attempt_started_at = Instant::now();
        match send_once(
            &state,
            SendOnceRequest {
                method: &parts.method,
                headers: &parts.headers,
                body: &body,
                candidate: &candidate,
                upstream_url,
                response_request: response_request.as_ref(),
                client_id: Some(client.id),
                usage_capture: UsageCapture {
                    enabled: should_capture_usage(&audit),
                    request_stream: audit.stream.unwrap_or(false),
                },
            },
        )
        .await
        {
            Ok(AttemptResult::Success {
                response,
                status,
                usage,
                stream_usage,
            }) => {
                db::mark_key_success(&state.pool, candidate.key_id).await?;
                audit.upstream_id = Some(candidate.upstream_id);
                audit.upstream_name = Some(candidate.upstream_name.clone());
                audit.upstream_key_id = Some(candidate.key_id);
                audit.upstream_key_name = Some(candidate.key_name.clone());
                audit.status = Some(i64::from(status.as_u16()));
                audit.outcome = "success".to_string();
                apply_token_usage(&mut audit, usage);
                attempts.push(new_attempt_audit(
                    &candidate,
                    AttemptAuditInput {
                        attempt_index,
                        status: Some(status.as_u16()),
                        outcome: "success",
                        retriable: false,
                        duration_ms: elapsed_ms(attempt_started_at),
                        retry_after_secs: None,
                        disabled_until: None,
                        error_class: None,
                        error_message: None,
                    },
                ));
                let audit_id = save_audit_best_effort(&state, started_at, audit, &attempts).await;
                if let (Some(audit_id), Some(stream_usage)) = (audit_id, stream_usage) {
                    stream_usage.set_audit_id(audit_id);
                }
                return Ok(response);
            }
            Ok(AttemptResult::NonRetriable {
                response,
                status,
                usage,
                stream_usage,
            }) => {
                audit.upstream_id = Some(candidate.upstream_id);
                audit.upstream_name = Some(candidate.upstream_name.clone());
                audit.upstream_key_id = Some(candidate.key_id);
                audit.upstream_key_name = Some(candidate.key_name.clone());
                audit.status = Some(i64::from(status.as_u16()));
                audit.outcome = outcome_for_non_retriable_status(status).to_string();
                apply_token_usage(&mut audit, usage);
                attempts.push(new_attempt_audit(
                    &candidate,
                    AttemptAuditInput {
                        attempt_index,
                        status: Some(status.as_u16()),
                        outcome: "non_retriable_response",
                        retriable: false,
                        duration_ms: elapsed_ms(attempt_started_at),
                        retry_after_secs: None,
                        disabled_until: None,
                        error_class: None,
                        error_message: None,
                    },
                ));
                let audit_id = save_audit_best_effort(&state, started_at, audit, &attempts).await;
                if let (Some(audit_id), Some(stream_usage)) = (audit_id, stream_usage) {
                    stream_usage.set_audit_id(audit_id);
                }
                return Ok(response);
            }
            Ok(AttemptResult::Retry(failure)) => {
                let disabled_until = mark_failure(&state, candidate.key_id, &failure).await?;
                attempts.push(new_attempt_audit(
                    &candidate,
                    AttemptAuditInput {
                        attempt_index,
                        status: failure.status,
                        outcome: "retry",
                        retriable: true,
                        duration_ms: elapsed_ms(attempt_started_at),
                        retry_after_secs: failure.retry_after_secs,
                        disabled_until: Some(disabled_until),
                        error_class: Some("retriable_upstream_status"),
                        error_message: Some(failure.message.as_str()),
                    },
                ));
                last_failure = Some(failure);
            }
            Err(error) => {
                let failure = UpstreamFailure {
                    status: None,
                    retry_after_secs: None,
                    message: error.to_string(),
                    body: Bytes::new(),
                };
                let disabled_until = mark_failure(&state, candidate.key_id, &failure).await?;
                attempts.push(new_attempt_audit(
                    &candidate,
                    AttemptAuditInput {
                        attempt_index,
                        status: None,
                        outcome: "transport_error",
                        retriable: true,
                        duration_ms: elapsed_ms(attempt_started_at),
                        retry_after_secs: None,
                        disabled_until: Some(disabled_until),
                        error_class: Some("upstream_transport_error"),
                        error_message: Some(failure.message.as_str()),
                    },
                ));
                last_failure = Some(failure);
            }
        }
    }

    if let Some(failure) = last_failure {
        let status = failure
            .status
            .and_then(|status| StatusCode::from_u16(status).ok())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "upstream_exhausted".to_string();
        audit.error_class = Some("all_upstream_keys_failed".to_string());
        audit.error_message = Some(failure.message.clone());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        Ok(response_from_failure(failure))
    } else {
        let status = StatusCode::SERVICE_UNAVAILABLE;
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "upstream_exhausted".to_string();
        audit.error_class = Some("all_upstream_keys_failed".to_string());
        audit.error_message = Some("all upstream keys failed".to_string());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        Ok(json_error(status, "all upstream keys failed".to_string()))
    }
}

async fn send_once(state: &AppState, input: SendOnceRequest<'_>) -> anyhow::Result<AttemptResult> {
    let mut builder = state
        .client
        .request(input.method.clone(), input.upstream_url);
    for (name, value) in input.headers {
        if should_forward_header(name) {
            builder = builder.header(name, value);
        }
    }
    let upstream_body = if let Some(response_request) = input.response_request {
        response_request.body_for_candidate(input.candidate.resolved_model.as_deref())?
    } else {
        body_for_candidate(input.body, input.candidate.resolved_model.as_deref())?
    };
    builder = builder
        .bearer_auth(&input.candidate.api_key)
        .body(upstream_body);

    let response = builder.send().await?;
    let status = response.status();
    let headers = response.headers().clone();

    if is_retriable_status(status) {
        let body = response.bytes().await.unwrap_or_default();
        let retry_after_secs = parse_retry_after(&headers);
        return Ok(AttemptResult::Retry(UpstreamFailure {
            status: Some(status.as_u16()),
            retry_after_secs,
            message: format!("upstream returned retriable status {}", status.as_u16()),
            body,
        }));
    }

    let mut response_builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if should_return_header(name) {
            response_builder = response_builder.header(name, value);
        }
    }
    let (body, usage, stream_usage) = if let Some(response_request) = input.response_request
        && status.is_success()
        && response_request.stream
        && is_event_stream_response(&headers)
    {
        let stream_usage = StreamUsageAuditHandle::new();
        let body = responses_compat::convert_streaming_response(
            state.pool.clone(),
            response.bytes_stream(),
            response_request.clone(),
            input.client_id,
            stream_usage.clone(),
        );
        (body, None, Some(stream_usage))
    } else if let Some(response_request) = input.response_request
        && status.is_success()
        && is_json_response(&headers)
    {
        let body = response.bytes().await.unwrap_or_default();
        let (body, usage) = responses_compat::convert_json_response(
            &state.pool,
            input.client_id,
            response_request,
            &body,
        )
        .await?;
        (Body::from(body), usage.map(TokenUsage::from), None)
    } else if input.usage_capture.enabled && is_json_response(&headers) {
        let body = response.bytes().await.unwrap_or_default();
        let usage = extract_token_usage(&body);
        (Body::from(body), usage, None)
    } else if input.usage_capture.enabled
        && input.usage_capture.request_stream
        && is_event_stream_response(&headers)
    {
        let stream_usage = StreamUsageAuditHandle::new();
        let body = capture_stream_usage_body(
            state.pool.clone(),
            response.bytes_stream(),
            stream_usage.clone(),
        );
        (body, None, Some(stream_usage))
    } else {
        let stream = response
            .bytes_stream()
            .map_err(|error| std::io::Error::other(error.to_string()));
        (Body::from_stream(stream), None, None)
    };
    let proxied = response_builder.body(body)?;

    if status.is_success() {
        Ok(AttemptResult::Success {
            response: proxied,
            status,
            usage,
            stream_usage,
        })
    } else {
        Ok(AttemptResult::NonRetriable {
            response: proxied,
            status,
            usage,
            stream_usage,
        })
    }
}

async fn mark_failure(
    state: &AppState,
    key_id: i64,
    failure: &UpstreamFailure,
) -> anyhow::Result<i64> {
    let ttl = failure.retry_after_secs.unwrap_or({
        if matches!(failure.status, Some(401 | 403)) {
            state.config.auth_failure_ttl_secs
        } else {
            state.config.transient_failure_ttl_secs
        }
    });
    let disabled_until = db::now_epoch() + ttl.max(1);
    db::mark_key_failure(
        &state.pool,
        key_id,
        disabled_until,
        failure.status,
        &failure.message,
    )
    .await?;
    Ok(disabled_until)
}

struct UsageCapture {
    enabled: bool,
    request_stream: bool,
}

struct SendOnceRequest<'a> {
    method: &'a axum::http::Method,
    headers: &'a HeaderMap,
    body: &'a Bytes,
    candidate: &'a Candidate,
    upstream_url: String,
    response_request: Option<&'a responses_compat::PreparedResponseRequest>,
    client_id: Option<i64>,
    usage_capture: UsageCapture,
}

enum AttemptResult {
    Success {
        response: Response<Body>,
        status: StatusCode,
        usage: Option<TokenUsage>,
        stream_usage: Option<StreamUsageAuditHandle>,
    },
    NonRetriable {
        response: Response<Body>,
        status: StatusCode,
        usage: Option<TokenUsage>,
        stream_usage: Option<StreamUsageAuditHandle>,
    },
    Retry(UpstreamFailure),
}

#[derive(Clone)]
pub(crate) struct StreamUsageAuditHandle {
    audit_id: Arc<Mutex<Option<i64>>>,
}

impl StreamUsageAuditHandle {
    pub(crate) fn new() -> Self {
        Self {
            audit_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn set_audit_id(&self, audit_id: i64) {
        if let Ok(mut current) = self.audit_id.lock() {
            *current = Some(audit_id);
        }
    }

    pub(crate) fn audit_id(&self) -> Option<i64> {
        self.audit_id.lock().ok().and_then(|current| *current)
    }
}

fn capture_stream_usage_body(
    pool: SqlitePool,
    stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    audit_handle: StreamUsageAuditHandle,
) -> Body {
    let mut stream = Box::pin(stream);
    let body_stream = async_stream::stream! {
        let mut parser = SseUsageParser::default();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    parser.push_bytes(&chunk);
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(error) => {
                    yield Err(std::io::Error::other(error.to_string()));
                    return;
                }
            }
        }
        if let (Some(audit_id), Some(usage)) = (audit_handle.audit_id(), parser.finish())
            && let Err(error) = db::update_request_audit_usage(
                &pool,
                audit_id,
                usage.input_tokens,
                usage.output_tokens,
                usage.total_tokens,
            )
            .await
            {
                tracing::warn!(error = %error, audit_id, "failed to update streaming token usage");
        }
    };
    Body::from_stream(body_stream)
}

#[derive(Default)]
struct SseUsageParser {
    buffer: String,
    usage: Option<TokenUsage>,
}

impl SseUsageParser {
    fn push_bytes(&mut self, chunk: &Bytes) {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        while let Some(index) = self.buffer.find('\n') {
            let mut line = self.buffer.drain(..=index).collect::<String>();
            if line.ends_with('\n') {
                line.pop();
            }
            if line.ends_with('\r') {
                line.pop();
            }
            self.observe_line(&line);
        }
    }

    fn finish(mut self) -> Option<TokenUsage> {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.observe_line(&line);
        }
        self.usage
    }

    fn observe_line(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(usage) = extract_token_usage_from_value(&value) {
            self.usage = Some(usage);
        }
    }
}

struct UpstreamFailure {
    status: Option<u16>,
    retry_after_secs: Option<i64>,
    message: String,
    body: Bytes,
}

fn response_from_failure(failure: UpstreamFailure) -> Response<Body> {
    let status = failure
        .status
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    if failure.body.is_empty() {
        json_error(status, failure.message)
    } else {
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .header("x-route-llm-exhausted", "true")
            .body(Body::from(failure.body))
            .expect("response builder is valid")
    }
}

fn json_error(status: StatusCode, message: String) -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "route_llm_error"
        }
    });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("response builder is valid")
}

#[derive(Debug, Serialize)]
struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelResponseItem>,
}

#[derive(Debug, Serialize)]
struct ModelResponseItem {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: &'static str,
    max_model_len: i64,
}

async fn models_list_response(state: &AppState) -> anyhow::Result<Response<Body>> {
    let public_models = db::list_public_models(&state.pool).await?;
    let body = ModelsResponse {
        object: "list",
        data: public_models
            .into_iter()
            .map(|model| ModelResponseItem {
                id: model.public_model,
                object: "model",
                created: model.created_at,
                owned_by: "route-llm",
                max_model_len: model.max_model_len,
            })
            .collect(),
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body)?))?)
}

fn request_model_from_body(body: &Bytes) -> Option<String> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return None;
    };
    value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn body_for_candidate(body: &Bytes, resolved_model: Option<&str>) -> anyhow::Result<Bytes> {
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

fn replace_model(value: &mut Value, model: String) {
    if let Some(object) = value.as_object_mut() {
        object.insert("model".to_string(), Value::String(model));
    }
}

fn request_value_streams(value: &Value) -> bool {
    value
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn ensure_stream_usage_requested(value: &mut Value) {
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

async fn save_audit_best_effort(
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

fn new_request_audit(parts: &Parts) -> db::RequestAudit {
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

fn should_capture_usage(audit: &db::RequestAudit) -> bool {
    matches!(
        audit.route_kind.as_str(),
        "chat_completions" | "responses" | "completions" | "embeddings"
    )
}

fn is_json_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("json"))
        .unwrap_or(false)
}

fn is_event_stream_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn extract_token_usage(body: &Bytes) -> Option<TokenUsage> {
    if body.len() > MAX_AUDIT_BODY_PARSE_BYTES {
        return None;
    }
    let value = serde_json::from_slice::<Value>(body).ok()?;
    extract_token_usage_from_value(&value)
}

fn extract_token_usage_from_value(value: &Value) -> Option<TokenUsage> {
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

fn usage_i64(usage: &Value, fields: &[&str]) -> Option<i64> {
    fields
        .iter()
        .find_map(|field| usage.get(*field).and_then(Value::as_i64))
}

fn apply_token_usage(audit: &mut db::RequestAudit, usage: Option<TokenUsage>) {
    if let Some(usage) = usage {
        audit.input_tokens = usage.input_tokens;
        audit.output_tokens = usage.output_tokens;
        audit.total_tokens = usage.total_tokens;
    }
}

fn enrich_request_audit_from_body(audit: &mut db::RequestAudit, body: &Bytes) {
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

struct AttemptAuditInput<'a> {
    attempt_index: i64,
    status: Option<u16>,
    outcome: &'a str,
    retriable: bool,
    duration_ms: i64,
    retry_after_secs: Option<i64>,
    disabled_until: Option<i64>,
    error_class: Option<&'a str>,
    error_message: Option<&'a str>,
}

fn new_attempt_audit(candidate: &Candidate, input: AttemptAuditInput<'_>) -> db::AttemptAudit {
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
    }
}

fn elapsed_ms(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn extract_client_ip(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    for (name, source) in [
        ("cf-connecting-ip", "cf-connecting-ip"),
        ("true-client-ip", "true-client-ip"),
        ("x-forwarded-for", "x-forwarded-for"),
        ("x-real-ip", "x-real-ip"),
    ] {
        if let Some(value) = header_string(headers, name, 200) {
            let ip = if name == "x-forwarded-for" {
                value.split(',').next().unwrap_or("").trim().to_string()
            } else {
                value
            };
            if !ip.is_empty() {
                return (
                    Some(sanitize_audit_text(&ip, 100)),
                    Some(source.to_string()),
                );
            }
        }
    }
    (None, None)
}

fn content_length(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
}

fn header_string(headers: &HeaderMap, name: &str, max_chars: usize) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| sanitize_audit_text(value, max_chars))
        .filter(|value| !value.is_empty())
}

fn sanitize_audit_text(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

fn route_kind(path: &str) -> &'static str {
    if path.ends_with("/chat/completions") || path.contains("/chat/completions/") {
        "chat_completions"
    } else if path.ends_with("/responses") || path.contains("/responses/") {
        "responses"
    } else if path.ends_with("/embeddings") || path.contains("/embeddings/") {
        "embeddings"
    } else if path.ends_with("/completions") || path.contains("/completions/") {
        "completions"
    } else if path.ends_with("/models") || path.contains("/models/") {
        "models"
    } else if path.contains("/images/") || path.ends_with("/images") {
        "images"
    } else if path.contains("/audio/") || path.ends_with("/audio") {
        "audio"
    } else {
        "other"
    }
}

fn outcome_for_non_retriable_status(status: StatusCode) -> &'static str {
    if status.is_client_error() {
        "client_error"
    } else if status.is_server_error() {
        "upstream_error"
    } else {
        "non_success"
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ")
}

fn build_upstream_url(base_url: &str, public_prefix: &str, uri: &Uri) -> anyhow::Result<String> {
    let upstream_path = path_after_public_prefix(public_prefix, uri.path());
    build_upstream_url_for_path(base_url, uri, upstream_path)
}

fn build_upstream_url_for_path(
    base_url: &str,
    uri: &Uri,
    upstream_path: &str,
) -> anyhow::Result<String> {
    let mut url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        upstream_path.trim_start_matches('/')
    );
    if let Some(query) = uri.query() {
        url.push('?');
        url.push_str(query);
    }
    Ok(url)
}

fn is_responses_create_request(parts: &Parts, public_prefix: &str) -> bool {
    parts.method == Method::POST
        && path_after_public_prefix(public_prefix, parts.uri.path()) == "/responses"
}

fn is_models_list_request(parts: &Parts, public_prefix: &str) -> bool {
    parts.method == Method::GET
        && path_after_public_prefix(public_prefix, parts.uri.path()) == "/models"
}

fn should_ignore_proxy_request(path: &str, public_prefix: &str) -> bool {
    if is_admin_or_browser_artifact_path(path) {
        return true;
    }
    !public_prefix.is_empty() && !path_matches_public_prefix(public_prefix, path)
}

fn is_admin_or_browser_artifact_path(path: &str) -> bool {
    path == "/admin"
        || path.starts_with("/admin/")
        || path == "/favicon.ico"
        || path == "/robots.txt"
        || path == "/site.webmanifest"
        || path.starts_with("/apple-touch-icon")
}

fn path_matches_public_prefix(public_prefix: &str, path: &str) -> bool {
    path.strip_prefix(public_prefix)
        .map(|stripped| stripped.is_empty() || stripped.starts_with('/'))
        .unwrap_or(false)
}

fn path_after_public_prefix<'a>(public_prefix: &str, path: &'a str) -> &'a str {
    path.strip_prefix(public_prefix)
        .filter(|stripped| stripped.is_empty() || stripped.starts_with('/'))
        .unwrap_or(path)
}

fn is_retriable_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn should_forward_header(name: &HeaderName) -> bool {
    name != AUTHORIZATION && name != HOST && name != CONTENT_LENGTH && !is_hop_by_hop_header(name)
}

fn should_return_header(name: &HeaderName) -> bool {
    name != CONTENT_LENGTH && !is_hop_by_hop_header(name)
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn parse_retry_after(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|secs| *secs > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_public_prefix_when_building_upstream_url() {
        let uri: Uri = "/v1/chat/completions?stream=true".parse().unwrap();
        let url =
            build_upstream_url("https://example.com/v1", "/v1", &uri).expect("url should build");
        assert_eq!(url, "https://example.com/v1/chat/completions?stream=true");
    }

    #[test]
    fn responses_compat_uses_chat_completions_upstream_path() {
        let uri: Uri = "/v1/responses?timeout=30".parse().unwrap();
        let url = build_upstream_url_for_path("https://example.com/v1", &uri, "/chat/completions")
            .expect("url should build");
        assert_eq!(url, "https://example.com/v1/chat/completions?timeout=30");
    }

    #[test]
    fn preserves_path_without_public_prefix() {
        let uri: Uri = "/health".parse().unwrap();
        let url = build_upstream_url("https://example.com/v1", "/v1", &uri).unwrap();
        assert_eq!(url, "https://example.com/v1/health");
    }

    #[test]
    fn detects_retriable_statuses() {
        assert!(is_retriable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retriable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retriable_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn replaces_top_level_model() {
        let mut value = serde_json::json!({
            "model": "llm-model",
            "messages": [{"role": "user", "content": "ping"}]
        });
        replace_model(&mut value, "provider-llm".to_string());
        assert_eq!(value["model"], "provider-llm");
        assert_eq!(value["messages"][0]["content"], "ping");
    }

    #[test]
    fn detects_public_models_endpoint() {
        let parts = Request::builder()
            .method(Method::GET)
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        assert!(is_models_list_request(&parts, "/v1"));
    }

    #[test]
    fn detects_responses_create_endpoint() {
        let parts = Request::builder()
            .method(Method::POST)
            .uri("/v1/responses")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        let get_parts = Request::builder()
            .method(Method::GET)
            .uri("/v1/responses")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;

        assert!(is_responses_create_request(&parts, "/v1"));
        assert!(!is_responses_create_request(&get_parts, "/v1"));
    }

    #[tokio::test]
    async fn public_models_response_includes_max_model_len() {
        let path = std::env::temp_dir().join(format!(
            "route-llm-models-response-{}.sqlite",
            std::process::id()
        ));
        let url = format!("sqlite://{}", path.display());
        let pool = db::connect(&url).await.unwrap();
        let upstream = db::upsert_upstream(&pool, "provider", "https://example.test/v1", 10, true)
            .await
            .unwrap();
        let model_id = db::upsert_upstream_model_by_id_with_max_model_len(
            &pool,
            upstream,
            "provider-llm",
            10,
            true,
            &["llm"],
            Some(262_144),
        )
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO model_alias_routes(public_model, upstream_model_id, priority, enabled)
            VALUES ('llm-model', ?, 10, 1);
            "#,
        )
        .bind(model_id)
        .execute(&pool)
        .await
        .unwrap();
        let state = AppState {
            pool: pool.clone(),
            client: reqwest::Client::new(),
            config: crate::server::ProxyConfig {
                public_prefix: "/v1".to_string(),
                transient_failure_ttl_secs: 300,
                auth_failure_ttl_secs: 3600,
                max_body_bytes: 1024 * 1024,
                admin: crate::server::AdminConfig {
                    password_hash: None,
                    session_token: None,
                    site_name: "Route LLM".to_string(),
                    site_description: "Local OpenAI-compatible routing proxy".to_string(),
                    public_base_url: None,
                },
            },
        };

        let response = models_list_response(&state).await.unwrap();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let llm_model = value["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == "llm-model")
            .unwrap();

        assert_eq!(llm_model["max_model_len"], 262_144);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
    }

    #[test]
    fn public_models_endpoint_must_be_exact_get() {
        let post_parts = Request::builder()
            .method(Method::POST)
            .uri("/v1/models")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;
        let nested_parts = Request::builder()
            .method(Method::GET)
            .uri("/v1/models/provider-llm")
            .body(Body::empty())
            .unwrap()
            .into_parts()
            .0;

        assert!(!is_models_list_request(&post_parts, "/v1"));
        assert!(!is_models_list_request(&nested_parts, "/v1"));
    }

    #[test]
    fn proxy_audit_ignores_admin_browser_and_non_public_paths() {
        assert!(should_ignore_proxy_request("/admin", "/v1"));
        assert!(should_ignore_proxy_request("/admin/missing", "/v1"));
        assert!(should_ignore_proxy_request("/favicon.ico", "/v1"));
        assert!(should_ignore_proxy_request("/apple-touch-icon.png", "/v1"));
        assert!(should_ignore_proxy_request("/robots.txt", "/v1"));
        assert!(should_ignore_proxy_request("/healthz", "/v1"));

        assert!(!should_ignore_proxy_request("/v1/models", "/v1"));
        assert!(!should_ignore_proxy_request("/v1/chat/completions", "/v1"));
        assert!(!should_ignore_proxy_request("/v10/chat/completions", ""));
    }

    #[test]
    fn body_for_candidate_rewrites_model_and_preserves_payload_shape() {
        let body = Bytes::from_static(
            br#"{"model":"llm-model","messages":[{"role":"user","content":"ping"}],"stream":true}"#,
        );

        let rewritten = body_for_candidate(&body, Some("provider-llm")).unwrap();
        let value: Value = serde_json::from_slice(&rewritten).unwrap();

        assert_eq!(value["model"], "provider-llm");
        assert_eq!(value["messages"][0]["content"], "ping");
        assert_eq!(value["stream"], true);
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn body_for_candidate_preserves_existing_stream_options() {
        let body = Bytes::from_static(
            br#"{"model":"llm-model","stream":true,"stream_options":{"foo":"bar"}}"#,
        );

        let rewritten = body_for_candidate(&body, Some("provider-llm")).unwrap();
        let value: Value = serde_json::from_slice(&rewritten).unwrap();

        assert_eq!(value["model"], "provider-llm");
        assert_eq!(value["stream_options"]["foo"], "bar");
        assert_eq!(value["stream_options"]["include_usage"], true);
    }

    #[test]
    fn sse_usage_parser_extracts_final_usage_without_response_storage() {
        let mut parser = SseUsageParser::default();

        parser.push_bytes(&Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"hi"}}]}

"#,
        ));
        parser.push_bytes(&Bytes::from_static(
            br#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}
data: [DONE]

"#,
        ));

        let usage = parser.finish().unwrap();
        assert_eq!(usage.input_tokens, Some(7));
        assert_eq!(usage.output_tokens, Some(3));
        assert_eq!(usage.total_tokens, Some(10));
    }

    #[test]
    fn body_for_candidate_leaves_body_when_rewrite_is_not_possible_or_needed() {
        let invalid = Bytes::from_static(b"not json");
        assert_eq!(
            body_for_candidate(&invalid, Some("model")).unwrap(),
            invalid
        );

        let missing_model = Bytes::from_static(br#"{"messages":[]}"#);
        assert_eq!(
            body_for_candidate(&missing_model, Some("model")).unwrap(),
            missing_model
        );

        let same_model = Bytes::from_static(br#"{"model":"same"}"#);
        assert_eq!(
            body_for_candidate(&same_model, Some("same")).unwrap(),
            same_model
        );

        let no_resolved_model = Bytes::from_static(br#"{"model":"llm-model"}"#);
        assert_eq!(
            body_for_candidate(&no_resolved_model, None).unwrap(),
            no_resolved_model
        );
    }

    #[test]
    fn request_model_from_body_only_extracts_string_model() {
        assert_eq!(
            request_model_from_body(&Bytes::from_static(br#"{"model":"llm-model"}"#)).as_deref(),
            Some("llm-model")
        );
        assert_eq!(
            request_model_from_body(&Bytes::from_static(br#"{"model":123}"#)),
            None
        );
        assert_eq!(
            request_model_from_body(&Bytes::from_static(b"bad-json")),
            None
        );
    }

    #[test]
    fn client_ip_prefers_cloudflare_then_first_forwarded_for() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.1, 198.51.100.2".parse().unwrap(),
        );
        headers.insert("cf-connecting-ip", "203.0.113.9".parse().unwrap());
        assert_eq!(
            extract_client_ip(&headers),
            (
                Some("203.0.113.9".to_string()),
                Some("cf-connecting-ip".to_string())
            )
        );

        headers.remove("cf-connecting-ip");
        assert_eq!(
            extract_client_ip(&headers),
            (
                Some("198.51.100.1".to_string()),
                Some("x-forwarded-for".to_string())
            )
        );
    }

    #[test]
    fn audit_text_is_trimmed_control_stripped_and_bounded() {
        assert_eq!(sanitize_audit_text("  a\nb\tc  ", 10), "abc");
        assert_eq!(sanitize_audit_text("abcdef", 3), "abc");
    }

    #[test]
    fn bearer_token_requires_standard_bearer_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Bearer token".parse().unwrap());
        assert_eq!(bearer_token(&headers), Some("token"));

        headers.insert(AUTHORIZATION, "bearer token".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn header_filters_drop_auth_host_content_length_and_hop_by_hop_headers() {
        assert!(!should_forward_header(&AUTHORIZATION));
        assert!(!should_forward_header(&HOST));
        assert!(!should_forward_header(&CONTENT_LENGTH));
        assert!(!should_forward_header(&HeaderName::from_static(
            "connection"
        )));
        assert!(should_forward_header(&HeaderName::from_static(
            "user-agent"
        )));

        assert!(!should_return_header(&CONTENT_LENGTH));
        assert!(!should_return_header(&HeaderName::from_static(
            "transfer-encoding"
        )));
        assert!(should_return_header(&HeaderName::from_static(
            "content-type"
        )));
    }

    #[test]
    fn retry_after_only_accepts_positive_integer_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "30".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), Some(30));

        headers.insert("retry-after", "0".parse().unwrap());
        assert_eq!(parse_retry_after(&headers), None);

        headers.insert(
            "retry-after",
            "Fri, 12 Jun 2026 00:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn public_prefix_matching_does_not_strip_partial_segments() {
        assert_eq!(path_after_public_prefix("/v1", "/v1/chat"), "/chat");
        assert_eq!(path_after_public_prefix("/v1", "/v10/chat"), "/v10/chat");
        assert_eq!(path_after_public_prefix("", "/v1/chat"), "/v1/chat");
    }

    #[test]
    fn route_kind_covers_supported_openai_style_paths() {
        assert_eq!(route_kind("/v1/chat/completions"), "chat_completions");
        assert_eq!(route_kind("/v1/responses"), "responses");
        assert_eq!(route_kind("/v1/embeddings"), "embeddings");
        assert_eq!(route_kind("/v1/completions"), "completions");
        assert_eq!(route_kind("/v1/models"), "models");
        assert_eq!(route_kind("/v1/images/generations"), "images");
        assert_eq!(route_kind("/v1/audio/speech"), "audio");
        assert_eq!(route_kind("/v1/unknown"), "other");
    }
}
