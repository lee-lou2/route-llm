use super::*;
use axum::http::{HeaderName, Uri};
use axum::{Router, routing::post};
use sqlx::Row;
use sqlx::SqlitePool;
use std::path::PathBuf;

#[test]
fn strips_public_prefix_when_building_upstream_url() {
    let uri: Uri = "/v1/chat/completions?stream=true".parse().unwrap();
    let url = build_upstream_url("https://example.com/v1", "/v1", &uri).expect("url should build");
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

    let response = models_list_response(&state, false).await.unwrap();
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

#[tokio::test]
async fn public_models_can_return_codex_catalog_shape() {
    let (pool, path) = proxy_test_pool("codex-models-catalog").await;
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

    let response = models_list_response(&state, true).await.unwrap();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    let llm_model = value["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["slug"] == "llm-model")
        .unwrap();

    assert_eq!(llm_model["display_name"], "llm-model");
    assert_eq!(llm_model["shell_type"], "shell_command");
    assert_eq!(llm_model["supported_reasoning_levels"][0]["effort"], "low");
    assert_eq!(llm_model["max_context_window"], 262_144);
    close_proxy_test_pool(pool, path).await;
}

#[tokio::test]
async fn responses_conversion_error_does_not_disable_upstream_key() {
    let (upstream_base_url, upstream_task) = start_invalid_json_upstream().await;
    let (pool, path) = proxy_test_pool("responses-conversion-error").await;
    let client_key = "client-secret";
    db::upsert_client(&pool, "client", client_key, true)
        .await
        .unwrap();
    let upstream_id = db::upsert_upstream(&pool, "provider", &upstream_base_url, 10, true)
        .await
        .unwrap();
    let upstream_key_id =
        db::upsert_upstream_key_by_id(&pool, upstream_id, "key", "upstream-secret", 10, true)
            .await
            .unwrap();
    let upstream_model_id = db::upsert_upstream_model_by_id_with_max_model_len(
        &pool,
        upstream_id,
        "provider-llm",
        10,
        true,
        &["llm"],
        None,
    )
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO model_alias_routes(public_model, upstream_model_id, priority, enabled)
        VALUES ('llm-model', ?, 10, 1);
        "#,
    )
    .bind(upstream_model_id)
    .execute(&pool)
    .await
    .unwrap();

    let state = Arc::new(AppState {
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
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/responses")
        .header(AUTHORIZATION, format!("Bearer {client_key}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"llm-model","input":"hello from responses"}"#,
        ))
        .unwrap();

    let response = proxy_inner(state, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("failed to convert upstream chat completion response")
    );

    let key =
        sqlx::query("SELECT disabled_until, consecutive_failures FROM upstream_keys WHERE id = ?;")
            .bind(upstream_key_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(key.get::<Option<i64>, _>("disabled_until").is_none());
    assert_eq!(key.get::<i64, _>("consecutive_failures"), 0);

    let audit = sqlx::query(
        r#"
        SELECT id, status, outcome, error_class, error_message, attempts
        FROM request_audits
        ORDER BY id DESC
        LIMIT 1;
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit.get::<i64, _>("status"), 502);
    assert_eq!(
        audit.get::<String, _>("outcome"),
        "response_conversion_error"
    );
    assert_eq!(
        audit.get::<String, _>("error_class"),
        "response_conversion_error"
    );
    assert_eq!(audit.get::<i64, _>("attempts"), 1);
    assert!(
        audit
            .get::<String, _>("error_message")
            .contains("expected value at line 1 column 1")
    );

    let attempt = sqlx::query(
        r#"
        SELECT status, outcome, retriable, disabled_until, error_class,
            upstream_content_type, upstream_body_bytes, upstream_body_hash, upstream_body_kind
        FROM upstream_attempt_audits
        WHERE request_audit_id = ?;
        "#,
    )
    .bind(audit.get::<i64, _>("id"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempt.get::<i64, _>("status"), 200);
    assert_eq!(
        attempt.get::<String, _>("outcome"),
        "response_conversion_error"
    );
    assert_eq!(attempt.get::<i64, _>("retriable"), 0);
    assert!(attempt.get::<Option<i64>, _>("disabled_until").is_none());
    assert_eq!(
        attempt.get::<String, _>("error_class"),
        "response_conversion_error"
    );
    assert_eq!(
        attempt.get::<String, _>("upstream_content_type"),
        "application/json"
    );
    assert!(attempt.get::<i64, _>("upstream_body_bytes") > 0);
    assert!(
        attempt
            .get::<String, _>("upstream_body_hash")
            .starts_with("sha256:")
    );
    assert_eq!(attempt.get::<String, _>("upstream_body_kind"), "html_like");

    upstream_task.abort();
    close_proxy_test_pool(pool, path).await;
}

#[tokio::test]
async fn responses_request_with_ignored_tools_reaches_chat_upstream() {
    let (upstream_base_url, captured_body, upstream_task) = start_json_capture_upstream().await;
    let (pool, path) = proxy_test_pool("responses-ignored-tools").await;
    let client_key = "client-secret-ignored-tools";
    db::upsert_client(&pool, "client", client_key, true)
        .await
        .unwrap();
    let upstream_id = db::upsert_upstream(&pool, "provider", &upstream_base_url, 10, true)
        .await
        .unwrap();
    db::upsert_upstream_key_by_id(&pool, upstream_id, "key", "upstream-secret", 10, true)
        .await
        .unwrap();
    let upstream_model_id = db::upsert_upstream_model_by_id_with_max_model_len(
        &pool,
        upstream_id,
        "provider-llm",
        10,
        true,
        &["llm"],
        None,
    )
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO model_alias_routes(public_model, upstream_model_id, priority, enabled)
        VALUES ('llm-model', ?, 10, 1);
        "#,
    )
    .bind(upstream_model_id)
    .execute(&pool)
    .await
    .unwrap();

    let state = Arc::new(AppState {
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
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/responses")
        .header(AUTHORIZATION, format!("Bearer {client_key}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{
                "model":"llm-model",
                "input":"hello from codex",
                "parallel_tool_calls":true,
                "tools":[
                    {"type":"namespace","name":"mcp"},
                    {"type":"web_search"},
                    {"type":"custom","name":"terminal"},
                    {"type":"image_generation"}
                ],
                "tool_choice":{"type":"namespace","name":"mcp"}
            }"#,
        ))
        .unwrap();

    let response = proxy_inner(state, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["output_text"], "pong");
    assert_eq!(value["tools"].as_array().unwrap().len(), 4);
    assert_eq!(value["tool_choice"]["type"], "namespace");

    let upstream_body = captured_body.lock().unwrap().clone().unwrap();
    assert_eq!(upstream_body["model"], "provider-llm");
    assert_eq!(upstream_body["messages"][0]["content"], "hello from codex");
    assert_eq!(upstream_body["parallel_tool_calls"], true);
    let upstream_tools = upstream_body["tools"].as_array().unwrap();
    assert_eq!(upstream_tools.len(), 1);
    assert_eq!(upstream_tools[0]["function"]["name"], "terminal");
    assert!(upstream_body.get("tool_choice").is_none());

    upstream_task.abort();
    close_proxy_test_pool(pool, path).await;
}

#[tokio::test]
async fn unknown_model_returns_not_found_without_upstream_attempt() {
    let (pool, path) = proxy_test_pool("unknown-model-not-found").await;
    let client_key = "client-secret-unknown-model";
    db::upsert_client(&pool, "client", client_key, true)
        .await
        .unwrap();
    let upstream_id = db::upsert_upstream(&pool, "provider", "http://127.0.0.1:9/v1", 10, true)
        .await
        .unwrap();
    let upstream_key_id =
        db::upsert_upstream_key_by_id(&pool, upstream_id, "key", "upstream-secret", 10, true)
            .await
            .unwrap();
    let state = Arc::new(AppState {
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
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(AUTHORIZATION, format!("Bearer {client_key}"))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"model":"unknown-model","messages":[{"role":"user","content":"ping"}]}"#,
        ))
        .unwrap();

    let response = proxy_inner(state, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["type"], "model_not_found");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown-model")
    );

    let audit = sqlx::query(
        r#"
        SELECT id, status, outcome, error_class, error_message, attempts, model
        FROM request_audits
        ORDER BY id DESC
        LIMIT 1;
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit.get::<i64, _>("status"), 404);
    assert_eq!(audit.get::<String, _>("outcome"), "model_not_found");
    assert_eq!(audit.get::<String, _>("error_class"), "model_not_found");
    assert_eq!(audit.get::<i64, _>("attempts"), 0);
    assert_eq!(audit.get::<String, _>("model"), "unknown-model");

    let attempt_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM upstream_attempt_audits;")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(attempt_count, 0);

    let key =
        sqlx::query("SELECT disabled_until, consecutive_failures FROM upstream_keys WHERE id = ?;")
            .bind(upstream_key_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(key.get::<Option<i64>, _>("disabled_until").is_none());
    assert_eq!(key.get::<i64, _>("consecutive_failures"), 0);

    close_proxy_test_pool(pool, path).await;
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
fn response_body_kind_classifies_without_storing_body_prefix() {
    assert_eq!(response_body_kind(b""), "empty");
    assert_eq!(response_body_kind(br#" {"ok":true}"#), "json_like");
    assert_eq!(response_body_kind(b"data: {}\n\n"), "sse_like");
    assert_eq!(response_body_kind(b"<html></html>"), "html_like");
    assert_eq!(response_body_kind(b"plain text"), "text_like");
    assert_eq!(response_body_kind(&[0xff, 0x00]), "binary_like");
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
    assert!(!should_forward_header(&ACCEPT_ENCODING));
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

async fn start_invalid_json_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("<html>not json</html>"))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), handle)
}

async fn start_json_capture_upstream() -> (
    String,
    Arc<Mutex<Option<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let captured_body = Arc::new(Mutex::new(None));
    let handler_captured_body = captured_body.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |body: Bytes| {
            let captured_body = handler_captured_body.clone();
            async move {
                let value: Value = serde_json::from_slice(&body).unwrap();
                {
                    let mut captured_body = captured_body.lock().unwrap();
                    *captured_body = Some(value);
                }
                Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
                    ))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), captured_body, handle)
}

async fn proxy_test_pool(name: &str) -> (SqlitePool, PathBuf) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "route-llm-http-proxy-{name}-{}-{unique}.sqlite",
        std::process::id()
    ));
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    (pool, path)
}

async fn close_proxy_test_pool(pool: SqlitePool, path: PathBuf) {
    pool.close().await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
}
