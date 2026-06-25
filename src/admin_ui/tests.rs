use super::*;

use super::styles::admin_css;

fn test_admin_config() -> AdminConfig {
    AdminConfig {
        password_hash: None,
        session_token: None,
        site_name: "Route LLM".to_string(),
        site_description: "Local OpenAI-compatible routing proxy".to_string(),
        public_base_url: None,
    }
}

fn public_admin_config() -> AdminConfig {
    AdminConfig {
        public_base_url: Some("https://router.example.test".to_string()),
        ..test_admin_config()
    }
}

#[test]
fn escapes_html_controlled_values() {
    assert_eq!(
        escape_html(r#"<script a="b">'x'&</script>"#),
        "&lt;script a=&quot;b&quot;&gt;&#39;x&#39;&amp;&lt;/script&gt;"
    );
}

#[test]
fn percent_encodes_query_values() {
    assert_eq!(percent_encode("saved ok"), "saved+ok");
    assert_eq!(percent_encode("a/b?c"), "a%2Fb%3Fc");
}

#[test]
fn page_shell_includes_icons_manifest_and_open_graph_metadata() {
    let config = public_admin_config();
    let html = page_shell(&page_meta(&config), "Route LLM 관리", "<main></main>");

    assert!(html.contains(
        r#"<link rel="icon" type="image/svg+xml" href="/favicon.svg?v=20260616-simple">"#
    ));
    assert!(html.contains(r#"<link rel="alternate icon" href="/favicon.ico?v=20260616-simple">"#));
    assert!(html.contains(
        r#"<link rel="apple-touch-icon" href="/apple-touch-icon.png?v=20260616-simple">"#
    ));
    assert!(html.contains(r#"<link rel="manifest" href="/site.webmanifest?v=20260616-simple">"#));
    assert!(html.contains(r##"<meta name="theme-color" content="#172033">"##));
    assert!(
        html.contains(
            r#"<meta name="description" content="Local OpenAI-compatible routing proxy">"#
        )
    );
    assert!(html.contains(r#"<meta property="og:site_name" content="Route LLM">"#));
    assert!(html.contains(
        r#"<meta property="og:image" content="https://router.example.test/og.png?v=20260616-simple">"#
    ));
    assert!(html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#));
}

#[test]
fn login_page_uses_split_auth_layout() {
    let config = test_admin_config();
    let html = render_login(
        AdminRenderContext {
            config: &config,
            public_prefix: "/v1",
        },
        None,
    );

    assert!(html.contains("login-shell"));
    assert!(html.contains("login-brand-panel"));
    assert!(html.contains("login-card"));
    assert!(html.contains("http://127.0.0.1:8080/v1"));
    assert!(html.contains(r#"placeholder="관리자 비밀번호""#));
    assert!(!html.contains(r#"class="panel login-form""#));
}

#[test]
fn builds_models_url_from_provider_base_url() {
    assert_eq!(
        upstream_models_url("https://example.test/v1").unwrap(),
        "https://example.test/v1/models"
    );
    assert_eq!(
        upstream_models_url("https://example.test/v1/").unwrap(),
        "https://example.test/v1/models"
    );
}

#[test]
fn client_routing_hides_aliases_without_connected_models() {
    let client = db::ClientSummary {
        id: 1,
        name: "client".to_string(),
        enabled: true,
        tokens: Vec::new(),
        routes: Vec::new(),
    };
    let summary = db::StateSummary {
        clients: Vec::new(),
        upstreams: Vec::new(),
        model_aliases: vec![
            db::ModelAliasSummary {
                id: 1,
                public_model: "unused-model".to_string(),
                target_type: "llm".to_string(),
                enabled: true,
                created_at: 1,
                updated_at: 1,
                routes: Vec::new(),
            },
            db::ModelAliasSummary {
                id: 2,
                public_model: "llm-model".to_string(),
                target_type: "llm".to_string(),
                enabled: true,
                created_at: 1,
                updated_at: 1,
                routes: vec![db::ModelAliasRouteSummary {
                    id: 1,
                    upstream_model_id: 10,
                    upstream_name: "provider".to_string(),
                    upstream_model: "real-model".to_string(),
                    capabilities: vec!["llm".to_string()],
                    priority: 10,
                    enabled: true,
                }],
            },
        ],
    };

    let html = render_selected_client_routes(&client, &summary);

    assert!(!html.contains("unused-model"));
    assert!(html.contains("llm-model"));
}

#[test]
fn client_token_button_opens_modal_without_copying_masked_value() {
    let client = db::ClientSummary {
        id: 1,
        name: "client".to_string(),
        enabled: true,
        tokens: vec![db::ClientTokenSummary {
            id: 10,
            name: "production".to_string(),
            api_key_fingerprint: "sha256:abc123".to_string(),
            api_key: Some("secret-token".to_string()),
            enabled: true,
            created_at_text: "06-15 12:00:00".to_string(),
        }],
        routes: Vec::new(),
    };

    let button = render_client_token_button(&client);
    let modal = render_client_token_modal(&client);

    assert!(button.contains("토큰 확인"));
    assert!(button.contains("클라이언트 토큰 1개"));
    assert!(!button.contains("1/1개"));
    assert!(!button.contains("secret-token"));
    assert!(modal.contains("data-copy-token-value=\"secret-token\""));
    assert!(modal.contains(r#"action="/admin/client-tokens/delete""#));
    assert!(!modal.contains("복구"));
}

#[test]
fn provider_model_alias_tags_are_deletable_not_draggable() {
    let upstream = db::UpstreamSummary {
        id: 1,
        name: "provider".to_string(),
        base_url: "https://provider.example/v1".to_string(),
        priority: 10,
        enabled: true,
        models: vec![db::UpstreamModelSummary {
            id: 10,
            model: "real-model".to_string(),
            capabilities: vec!["llm".to_string()],
            max_model_len: Some(1_048_576),
            priority: 10,
            enabled: true,
        }],
        discovered_models: Vec::new(),
        keys: Vec::new(),
    };
    let aliases = vec![db::ModelAliasSummary {
        id: 1,
        public_model: "llm-model".to_string(),
        target_type: "llm".to_string(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
        routes: vec![db::ModelAliasRouteSummary {
            id: 99,
            upstream_model_id: 10,
            upstream_name: "provider".to_string(),
            upstream_model: "real-model".to_string(),
            capabilities: vec!["llm".to_string()],
            priority: 10,
            enabled: true,
        }],
    }];

    let html = render_provider_model_list(&upstream, &aliases);

    assert!(!html.contains("draggable="));
    assert!(!html.contains("data-drag-kind"));
    assert!(html.contains(r#"action="/admin/alias-routes/delete""#));
    assert!(html.contains(r#"name="id" value="99""#));
    assert!(html.contains(r#"name="provider_id" value="1""#));
}

#[test]
fn token_modal_escapes_panel_stacking_context() {
    let css = admin_css();
    let script = admin_js();

    assert!(css.contains("z-index: 10000;"));
    assert!(css.contains("body.modal-open > main"));
    assert!(css.contains("backdrop-filter: blur(2px);"));
    assert!(script.contains("document.body.appendChild(modal);"));
}

#[test]
fn token_usage_values_are_grouped() {
    let html = render_token_usage(Some(164392), Some(427), Some(164819));

    assert!(html.contains("164,819"));
    assert!(html.contains("in 164,392 / out 427"));
}

#[test]
fn key_stats_render_cumulative_duration_not_average() {
    let html = render_key_stats(&[db::KeyUsageStats {
        upstream_key_id: 1,
        upstream_name: "provider".to_string(),
        masked_api_key: "abcd...wxyz".to_string(),
        enabled: true,
        priority: 10,
        disabled_until: None,
        consecutive_failures: 0,
        last_status: Some(200),
        last_used_at: None,
        total_requests: 2,
        success_requests: 2,
        failed_requests: 0,
        total_duration_ms: 1500,
        input_tokens: 3000,
        output_tokens: 4200,
        total_tokens: 7200,
    }]);

    assert!(html.contains("누적"));
    assert!(html.contains("1.5s"));
    assert!(html.contains("7,200"));
    assert!(html.contains("3,000 / 4,200"));
    assert!(!html.contains("평균"));
}

#[test]
fn discovered_models_render_as_model_select() {
    let upstream = db::UpstreamSummary {
        id: 1,
        name: "provider".to_string(),
        base_url: "https://example.test/v1".to_string(),
        priority: 10,
        enabled: true,
        models: Vec::new(),
        discovered_models: vec![db::DiscoveredModelSummary {
            model: "remote-model".to_string(),
            max_model_len: Some(1_048_576),
            fetched_at: 1,
            fetched_at_text: "01-01 00:00:01".to_string(),
        }],
        keys: Vec::new(),
    };

    let html = render_model_name_field(&upstream);

    assert!(html.contains("data-model-name-field"));
    assert!(html.contains(r#"<select name="model" required>"#));
    assert!(html.contains("remote-model"));
}

#[test]
fn provider_detail_uses_automatic_model_refresh_without_manual_fetch_button() {
    let upstream = db::UpstreamSummary {
        id: 7,
        name: "provider".to_string(),
        base_url: "https://example.test/v1".to_string(),
        priority: 10,
        enabled: true,
        models: Vec::new(),
        discovered_models: Vec::new(),
        keys: Vec::new(),
    };

    let html = render_selected_provider_detail(&upstream, &[]);

    assert!(html.contains(r#"data-upstream-id="7""#));
    assert!(!html.contains("모델 목록 가져오기"));
    assert!(html.contains("자동으로 확인합니다"));
    assert!(html.contains(r#"name="upstream_id" value="7""#));
    assert!(html.contains("프로바이더 상세"));
}

#[test]
fn provider_workspace_uses_left_selection_list_and_selected_detail() {
    let summary = db::StateSummary {
        clients: Vec::new(),
        model_aliases: Vec::new(),
        upstreams: vec![
            db::UpstreamSummary {
                id: 1,
                name: "first-provider".to_string(),
                base_url: "https://first.example/v1".to_string(),
                priority: 10,
                enabled: true,
                models: Vec::new(),
                discovered_models: Vec::new(),
                keys: Vec::new(),
            },
            db::UpstreamSummary {
                id: 2,
                name: "second-provider".to_string(),
                base_url: "https://second.example/v1".to_string(),
                priority: 20,
                enabled: true,
                models: Vec::new(),
                discovered_models: Vec::new(),
                keys: Vec::new(),
            },
        ],
    };

    let html = render_provider_workspace(&summary, Some(2));

    assert!(html.contains("provider-select-card selected"));
    assert!(html.contains(r#"href="/admin?provider=2#settings""#));
    assert!(html.contains("second-provider"));
    assert!(html.contains("프로바이더 상세"));
    assert!(!html.contains("등록된 프로바이더"));
    assert!(!html.contains("provider-list-panel"));
}

#[test]
fn stats_panel_keeps_cache_reset_without_manual_health_check_button() {
    let html = render_stats_panel(&db::AdminStats {
        recent_requests: Vec::new(),
        client_token_stats: Vec::new(),
        key_stats: Vec::new(),
        health: db::AdminHealthSummary {
            total_keys: 0,
            enabled_keys: 0,
            ready_keys: 0,
            cached_keys: 0,
            disabled_keys: 0,
            recent_503: 0,
            recent_upstream_exhausted: 0,
            recent_5xx: 0,
            last_failure_at: None,
        },
    });

    assert!(html.contains("실패 캐시 비우기"));
    assert!(!html.contains("상태 확인"));
    assert!(!html.contains("/admin/health/check"));
}

#[test]
fn admin_script_refreshes_provider_models_in_background() {
    let script = admin_js();

    assert!(script.contains("refreshProviderModels();"));
    assert!(script.contains("/admin/upstreams/fetch-models"));
    assert!(script.contains("data-model-fetch-meta"));
}

#[test]
fn constant_time_equality_checks_full_value() {
    assert!(constant_time_eq("abc", "abc"));
    assert!(!constant_time_eq("abc", "abd"));
    assert!(!constant_time_eq("abc", "abcd"));
}
