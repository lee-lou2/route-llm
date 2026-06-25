use super::*;
use axum::http::{HeaderMap, HeaderName, Uri};
use std::time::Instant;

pub(super) fn elapsed_ms(started_at: Instant) -> i64 {
    started_at.elapsed().as_millis().min(i64::MAX as u128) as i64
}

pub(super) fn extract_client_ip(headers: &HeaderMap) -> (Option<String>, Option<String>) {
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

pub(super) fn content_length(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= 0)
}

pub(super) fn header_string(headers: &HeaderMap, name: &str, max_chars: usize) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| sanitize_audit_text(value, max_chars))
        .filter(|value| !value.is_empty())
}

pub(super) fn sanitize_audit_text(value: &str, max_chars: usize) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect()
}

pub(super) fn route_kind(path: &str) -> &'static str {
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

pub(super) fn outcome_for_non_retriable_status(status: StatusCode) -> &'static str {
    if status.is_client_error() {
        "client_error"
    } else if status.is_server_error() {
        "upstream_error"
    } else {
        "non_success"
    }
}

pub(super) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ")
}

pub(super) fn build_upstream_url(
    base_url: &str,
    public_prefix: &str,
    uri: &Uri,
) -> anyhow::Result<String> {
    let upstream_path = path_after_public_prefix(public_prefix, uri.path());
    build_upstream_url_for_path(base_url, uri, upstream_path)
}

pub(super) fn build_upstream_url_for_path(
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

pub(super) fn is_responses_create_request(parts: &Parts, public_prefix: &str) -> bool {
    parts.method == Method::POST
        && path_after_public_prefix(public_prefix, parts.uri.path()) == "/responses"
}

pub(super) fn is_models_list_request(parts: &Parts, public_prefix: &str) -> bool {
    parts.method == Method::GET
        && path_after_public_prefix(public_prefix, parts.uri.path()) == "/models"
}

pub(super) fn should_ignore_proxy_request(path: &str, public_prefix: &str) -> bool {
    if is_admin_or_browser_artifact_path(path) {
        return true;
    }
    !public_prefix.is_empty() && !path_matches_public_prefix(public_prefix, path)
}

pub(super) fn is_admin_or_browser_artifact_path(path: &str) -> bool {
    path == "/admin"
        || path.starts_with("/admin/")
        || path == "/favicon.ico"
        || path == "/robots.txt"
        || path == "/site.webmanifest"
        || path.starts_with("/apple-touch-icon")
}

pub(super) fn path_matches_public_prefix(public_prefix: &str, path: &str) -> bool {
    path.strip_prefix(public_prefix)
        .map(|stripped| stripped.is_empty() || stripped.starts_with('/'))
        .unwrap_or(false)
}

pub(super) fn path_after_public_prefix<'a>(public_prefix: &str, path: &'a str) -> &'a str {
    path.strip_prefix(public_prefix)
        .filter(|stripped| stripped.is_empty() || stripped.starts_with('/'))
        .unwrap_or(path)
}

pub(super) fn is_retriable_status(status: StatusCode) -> bool {
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

pub(super) fn should_forward_header(name: &HeaderName) -> bool {
    name != AUTHORIZATION
        && name != HOST
        && name != CONTENT_LENGTH
        && name != ACCEPT_ENCODING
        && !is_hop_by_hop_header(name)
}

pub(super) fn should_return_header(name: &HeaderName) -> bool {
    name != CONTENT_LENGTH && !is_hop_by_hop_header(name)
}

pub(super) fn is_hop_by_hop_header(name: &HeaderName) -> bool {
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

pub(super) fn parse_retry_after(headers: &HeaderMap) -> Option<i64> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|secs| *secs > 0)
}
