mod audit;
mod errors;
mod models_catalog;
mod request_body;
mod stream;
mod support;

use audit::*;
use errors::*;
use models_catalog::*;
use request_body::*;
pub(crate) use stream::StreamUsageAuditHandle;
use stream::*;
use support::*;

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
        HeaderMap, Method, Request, Response, StatusCode,
        header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST},
        request::Parts,
    },
    response::IntoResponse,
};
use futures_util::TryStreamExt;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
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
        return models_list_response(&state, wants_codex_models_catalog(parts.uri.query())).await;
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
    if let Some(model) = requested_model.as_deref()
        && !db::is_registered_request_model(&state.pool, model).await?
    {
        let status = StatusCode::NOT_FOUND;
        let safe_model = sanitize_audit_text(model, 200);
        let message = format!("model `{safe_model}` was not found in the route-llm model registry");
        audit.status = Some(i64::from(status.as_u16()));
        audit.outcome = "model_not_found".to_string();
        audit.error_class = Some("model_not_found".to_string());
        audit.error_message = Some(message.clone());
        save_audit_best_effort(&state, started_at, audit, &attempts).await;
        return Ok(json_error_with_type(status, message, "model_not_found"));
    }
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
                upstream_response,
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
                        upstream_response: upstream_response.as_ref(),
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
                upstream_response,
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
                        upstream_response: upstream_response.as_ref(),
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
                        upstream_response: failure.upstream_response.as_ref(),
                    },
                ));
                last_failure = Some(failure);
            }
            Ok(AttemptResult::ResponseConversionError {
                response,
                upstream_status,
                upstream_response,
                message,
            }) => {
                let status = StatusCode::BAD_GATEWAY;
                audit.upstream_id = Some(candidate.upstream_id);
                audit.upstream_name = Some(candidate.upstream_name.clone());
                audit.upstream_key_id = Some(candidate.key_id);
                audit.upstream_key_name = Some(candidate.key_name.clone());
                audit.status = Some(i64::from(status.as_u16()));
                audit.outcome = "response_conversion_error".to_string();
                audit.error_class = Some("response_conversion_error".to_string());
                audit.error_message = Some(sanitize_audit_text(&message, 500));
                attempts.push(new_attempt_audit(
                    &candidate,
                    AttemptAuditInput {
                        attempt_index,
                        status: Some(upstream_status.as_u16()),
                        outcome: "response_conversion_error",
                        retriable: false,
                        duration_ms: elapsed_ms(attempt_started_at),
                        retry_after_secs: None,
                        disabled_until: None,
                        error_class: Some("response_conversion_error"),
                        error_message: Some(message.as_str()),
                        upstream_response: Some(&upstream_response),
                    },
                ));
                save_audit_best_effort(&state, started_at, audit, &attempts).await;
                return Ok(response);
            }
            Err(error) => {
                let failure = UpstreamFailure {
                    status: None,
                    retry_after_secs: None,
                    message: error.to_string(),
                    body: Bytes::new(),
                    upstream_response: None,
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
                        upstream_response: failure.upstream_response.as_ref(),
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
        let upstream_response = upstream_response_audit_from_body(&headers, &body);
        let retry_after_secs = parse_retry_after(&headers);
        return Ok(AttemptResult::Retry(UpstreamFailure {
            status: Some(status.as_u16()),
            retry_after_secs,
            message: format!("upstream returned retriable status {}", status.as_u16()),
            body,
            upstream_response: Some(upstream_response),
        }));
    }

    let mut response_builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        if should_return_header(name) {
            response_builder = response_builder.header(name, value);
        }
    }
    let (body, usage, stream_usage, upstream_response) = if let Some(response_request) =
        input.response_request
        && status.is_success()
    {
        if response_request.stream {
            if is_event_stream_response(&headers) {
                let stream_usage = StreamUsageAuditHandle::new();
                let body = responses_compat::convert_streaming_response(
                    state.pool.clone(),
                    response.bytes_stream(),
                    response_request.clone(),
                    input.client_id,
                    stream_usage.clone(),
                );
                (
                    body,
                    None,
                    Some(stream_usage),
                    Some(upstream_response_audit_from_headers(&headers)),
                )
            } else {
                return Ok(response_conversion_error_from_response(
                    response,
                    &headers,
                    status,
                    "upstream Responses stream adapter expected text/event-stream response",
                )
                .await);
            }
        } else if is_json_response(&headers) {
            let body = match response.bytes().await {
                Ok(body) => body,
                Err(error) => {
                    return Ok(AttemptResult::ResponseConversionError {
                        response: json_error(
                            StatusCode::BAD_GATEWAY,
                            "failed to read upstream response body for Responses conversion"
                                .to_string(),
                        ),
                        upstream_status: status,
                        upstream_response: upstream_response_audit_from_headers(&headers),
                        message: format!(
                            "failed to read upstream response body for Responses conversion: {error}"
                        ),
                    });
                }
            };
            let upstream_response = upstream_response_audit_from_body(&headers, &body);
            let (body, usage) = match responses_compat::convert_json_response(
                &state.pool,
                input.client_id,
                response_request,
                &body,
            )
            .await
            {
                Ok(converted) => converted,
                Err(error) => {
                    let message = format!(
                        "failed to convert upstream chat completion response to Responses format: {error}"
                    );
                    return Ok(AttemptResult::ResponseConversionError {
                        response: json_error(StatusCode::BAD_GATEWAY, message.clone()),
                        upstream_status: status,
                        upstream_response,
                        message,
                    });
                }
            };
            (
                Body::from(body),
                usage.map(TokenUsage::from),
                None,
                Some(upstream_response),
            )
        } else {
            return Ok(response_conversion_error_from_response(
                response,
                &headers,
                status,
                "upstream Responses adapter expected JSON response",
            )
            .await);
        }
    } else if input.usage_capture.enabled && is_json_response(&headers) {
        let body = response.bytes().await.unwrap_or_default();
        let usage = extract_token_usage(&body);
        let upstream_response = upstream_response_audit_from_body(&headers, &body);
        (Body::from(body), usage, None, Some(upstream_response))
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
        (
            body,
            None,
            Some(stream_usage),
            Some(upstream_response_audit_from_headers(&headers)),
        )
    } else {
        let stream = response
            .bytes_stream()
            .map_err(|error| std::io::Error::other(error.to_string()));
        (
            Body::from_stream(stream),
            None,
            None,
            Some(upstream_response_audit_from_headers(&headers)),
        )
    };
    let proxied = response_builder.body(body)?;

    if status.is_success() {
        Ok(AttemptResult::Success {
            response: proxied,
            status,
            usage,
            stream_usage,
            upstream_response,
        })
    } else {
        Ok(AttemptResult::NonRetriable {
            response: proxied,
            status,
            usage,
            stream_usage,
            upstream_response,
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
        upstream_response: Option<UpstreamResponseAudit>,
    },
    NonRetriable {
        response: Response<Body>,
        status: StatusCode,
        usage: Option<TokenUsage>,
        stream_usage: Option<StreamUsageAuditHandle>,
        upstream_response: Option<UpstreamResponseAudit>,
    },
    Retry(UpstreamFailure),
    ResponseConversionError {
        response: Response<Body>,
        upstream_status: StatusCode,
        upstream_response: UpstreamResponseAudit,
        message: String,
    },
}

#[cfg(test)]
mod tests;
