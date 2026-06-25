use super::*;
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::Response,
};

pub(super) struct UpstreamFailure {
    pub(super) status: Option<u16>,
    pub(super) retry_after_secs: Option<i64>,
    pub(super) message: String,
    pub(super) body: Bytes,
    pub(super) upstream_response: Option<UpstreamResponseAudit>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct UpstreamResponseAudit {
    pub(super) content_type: Option<String>,
    pub(super) body_bytes: Option<i64>,
    pub(super) body_hash: Option<String>,
    pub(super) body_kind: Option<String>,
}

pub(super) fn response_from_failure(failure: UpstreamFailure) -> Response<Body> {
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

pub(super) fn json_error(status: StatusCode, message: String) -> Response<Body> {
    json_error_with_type(status, message, "route_llm_error")
}

pub(super) fn json_error_with_type(
    status: StatusCode,
    message: String,
    error_type: &'static str,
) -> Response<Body> {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "code": error_type
        }
    });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("response builder is valid")
}

pub(super) async fn response_conversion_error_from_response(
    response: reqwest::Response,
    headers: &HeaderMap,
    status: StatusCode,
    reason: &str,
) -> AttemptResult {
    match response.bytes().await {
        Ok(body) => {
            let upstream_response = upstream_response_audit_from_body(headers, &body);
            let message = format!(
                "{reason}; upstream_content_type={}; upstream_body_bytes={}; upstream_body_kind={}",
                upstream_response
                    .content_type
                    .as_deref()
                    .unwrap_or("unknown"),
                upstream_response.body_bytes.unwrap_or(0),
                upstream_response.body_kind.as_deref().unwrap_or("unknown")
            );
            AttemptResult::ResponseConversionError {
                response: json_error(StatusCode::BAD_GATEWAY, message.clone()),
                upstream_status: status,
                upstream_response,
                message,
            }
        }
        Err(error) => {
            let message =
                format!("{reason}; failed to read upstream response body for diagnostics: {error}");
            AttemptResult::ResponseConversionError {
                response: json_error(StatusCode::BAD_GATEWAY, message.clone()),
                upstream_status: status,
                upstream_response: upstream_response_audit_from_headers(headers),
                message,
            }
        }
    }
}

pub(super) fn upstream_response_audit_from_headers(headers: &HeaderMap) -> UpstreamResponseAudit {
    UpstreamResponseAudit {
        content_type: header_string(headers, "content-type", 200),
        ..UpstreamResponseAudit::default()
    }
}

pub(super) fn upstream_response_audit_from_body(
    headers: &HeaderMap,
    body: &Bytes,
) -> UpstreamResponseAudit {
    UpstreamResponseAudit {
        content_type: header_string(headers, "content-type", 200),
        body_bytes: Some(body.len().min(i64::MAX as usize) as i64),
        body_hash: Some(sha256_hash_bytes(body)),
        body_kind: Some(response_body_kind(body).to_string()),
    }
}

pub(super) fn sha256_hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(super) fn response_body_kind(body: &[u8]) -> &'static str {
    let Some(first_index) = body.iter().position(|byte| !byte.is_ascii_whitespace()) else {
        return "empty";
    };
    let trimmed = &body[first_index..];
    if trimmed.starts_with(b"{") || trimmed.starts_with(b"[") {
        "json_like"
    } else if trimmed.starts_with(b"data:") {
        "sse_like"
    } else if trimmed.starts_with(b"<") {
        "html_like"
    } else if std::str::from_utf8(trimmed).is_ok() {
        "text_like"
    } else {
        "binary_like"
    }
}
