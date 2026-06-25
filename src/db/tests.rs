use super::*;
use sqlx::{Row, SqlitePool};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static TEST_DB_COUNTER: AtomicUsize = AtomicUsize::new(0);

async fn test_pool(name: &str) -> (SqlitePool, PathBuf) {
    let id = TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "route-llm-{name}-{}-{id}.sqlite",
        std::process::id()
    ));
    let url = format!("sqlite://{}", path.display());
    let pool = connect(&url).await.expect("test database should open");
    (pool, path)
}

async fn close_and_remove(pool: SqlitePool, path: PathBuf) {
    pool.close().await;
    remove_sqlite_files(&path);
}

fn remove_sqlite_files(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(path.with_extension("sqlite-shm"));
    let _ = fs::remove_file(path.with_extension("sqlite-wal"));
}

async fn add_provider(
    pool: &SqlitePool,
    name: &str,
    upstream_priority: i64,
    key_name: &str,
    key_priority: i64,
) -> i64 {
    let upstream_id = upsert_upstream(
        pool,
        name,
        &format!("https://{name}.example.test/v1"),
        upstream_priority,
        true,
    )
    .await
    .unwrap();
    upsert_upstream_key(
        pool,
        name,
        key_name,
        &format!("{key_name}-secret"),
        key_priority,
        true,
    )
    .await
    .unwrap();
    upstream_id
}

async fn key_id(pool: &SqlitePool, key_name: &str) -> i64 {
    sqlx::query("SELECT id FROM upstream_keys WHERE name = ?;")
        .bind(key_name)
        .fetch_one(pool)
        .await
        .unwrap()
        .get("id")
}

async fn add_client(pool: &SqlitePool, name: &str) -> i64 {
    upsert_client(pool, name, &format!("{name}-secret"), true)
        .await
        .unwrap()
}

async fn add_alias_route(
    pool: &SqlitePool,
    public_model: &str,
    upstream_model_id: i64,
    priority: i64,
    enabled: bool,
) {
    sqlx::query(
        r#"
        INSERT INTO model_alias_routes(public_model, upstream_model_id, priority, enabled)
        VALUES (?, ?, ?, ?);
        "#,
    )
    .bind(public_model)
    .bind(upstream_model_id)
    .bind(priority)
    .bind(enabled)
    .execute(pool)
    .await
    .unwrap();
}

fn request_audit_for_path(path: &str, route_kind: &str, completed_at: i64) -> RequestAudit {
    RequestAudit {
        completed_at,
        duration_ms: 12,
        client_id: None,
        client_name: None,
        client_token_id: None,
        client_token_name: None,
        client_key_hash: Some(hash_secret("local-key")),
        client_ip: Some("203.0.113.1".to_string()),
        client_ip_source: Some("cf-connecting-ip".to_string()),
        cf_ray: None,
        cf_country: None,
        method: "GET".to_string(),
        path: path.to_string(),
        route_kind: route_kind.to_string(),
        has_query: false,
        query_hash: None,
        model: None,
        stream: None,
        content_type: None,
        request_body_bytes: None,
        user_agent_hash: Some(hash_secret("ua")),
        upstream_id: None,
        upstream_name: None,
        upstream_key_id: None,
        upstream_key_name: None,
        status: Some(200),
        outcome: "success".to_string(),
        error_class: None,
        error_message: None,
        attempts: 0,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
    }
}

#[tokio::test]
async fn seeds_default_public_model_aliases() {
    let (pool, path) = test_pool("default-aliases").await;

    let aliases = list_enabled_model_aliases(&pool).await.unwrap();
    let aliases: Vec<_> = aliases
        .into_iter()
        .map(|alias| (alias.public_model, alias.target_type))
        .collect();

    assert_eq!(
        aliases,
        vec![
            ("llm-model".to_string(), "llm".to_string()),
            ("multimodal-model".to_string(), "multimodal".to_string()),
        ]
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn capability_alias_routes_to_provider_that_supports_type() {
    let (pool, path) = test_pool("capability-routing").await;
    let multi = add_provider(&pool, "multi-only", 10, "multi-key", 10).await;
    upsert_upstream_model_by_id(&pool, multi, "provider-multi", 10, true, &["multimodal"])
        .await
        .unwrap();
    let llm = add_provider(&pool, "llm-only", 20, "llm-key", 10).await;
    upsert_upstream_model_by_id(&pool, llm, "provider-llm", 10, true, &["llm"])
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].upstream_name, "llm-only");
    assert_eq!(
        candidates[0].resolved_model.as_deref(),
        Some("provider-llm")
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn capability_routing_orders_by_upstream_model_then_key_priority() {
    let (pool, path) = test_pool("candidate-order").await;
    let upstream = add_provider(&pool, "ordered", 10, "slow-key", 20).await;
    upsert_upstream_key(&pool, "ordered", "fast-key", "fast-secret", 10, true)
        .await
        .unwrap();
    upsert_upstream_model_by_id(&pool, upstream, "slow-model", 20, true, &["llm"])
        .await
        .unwrap();
    upsert_upstream_model_by_id(&pool, upstream, "fast-model", 10, true, &["llm"])
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();
    let order: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.resolved_model.as_deref().unwrap(),
                candidate.key_name.as_str(),
            )
        })
        .collect();

    assert_eq!(
        order,
        vec![
            ("fast-model", "fast-key"),
            ("fast-model", "slow-key"),
            ("slow-model", "fast-key"),
            ("slow-model", "slow-key"),
        ]
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn alias_routes_order_models_inside_public_alias() {
    let (pool, path) = test_pool("alias-route-order").await;
    let first = add_provider(&pool, "first", 10, "first-key", 10).await;
    let first_model = upsert_upstream_model_by_id(&pool, first, "first-llm", 10, true, &["llm"])
        .await
        .unwrap();
    let second = add_provider(&pool, "second", 20, "second-key", 10).await;
    let second_model = upsert_upstream_model_by_id(&pool, second, "second-llm", 10, true, &["llm"])
        .await
        .unwrap();
    add_alias_route(&pool, "llm-model", second_model, 10, true).await;
    add_alias_route(&pool, "llm-model", first_model, 20, true).await;

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].resolved_model.as_deref(), Some("second-llm"));
    assert_eq!(candidates[1].resolved_model.as_deref(), Some("first-llm"));
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn model_registration_for_alias_preserves_capabilities_and_adds_route() {
    let (pool, path) = test_pool("model-for-alias").await;
    add_provider(&pool, "alias-provider", 10, "alias-key", 10).await;

    let first_model =
        upsert_upstream_model_for_alias(&pool, "alias-provider", "shared-model", "llm-model", true)
            .await
            .unwrap();
    let second_model = upsert_upstream_model_for_alias(
        &pool,
        "alias-provider",
        "shared-model",
        "multimodal-model",
        true,
    )
    .await
    .unwrap();

    assert_eq!(first_model, second_model);

    let state = list_state(&pool).await.unwrap();
    let provider = state
        .upstreams
        .iter()
        .find(|upstream| upstream.name == "alias-provider")
        .unwrap();
    let model = provider
        .models
        .iter()
        .find(|model| model.model == "shared-model")
        .unwrap();
    assert_eq!(model.capabilities, vec!["llm", "multimodal"]);

    let llm_alias = state
        .model_aliases
        .iter()
        .find(|alias| alias.public_model == "llm-model")
        .unwrap();
    assert!(
        llm_alias
            .routes
            .iter()
            .any(|route| route.upstream_model == "shared-model")
    );
    let multimodal_alias = state
        .model_aliases
        .iter()
        .find(|alias| alias.public_model == "multimodal-model")
        .unwrap();
    assert!(
        multimodal_alias
            .routes
            .iter()
            .any(|route| route.upstream_model == "shared-model")
    );

    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn disabled_alias_routes_block_global_fallback() {
    let (pool, path) = test_pool("alias-route-disabled").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let model = upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
        .await
        .unwrap();
    add_alias_route(&pool, "llm-model", model, 10, false).await;

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert!(candidates.is_empty());
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn exact_registered_model_routes_only_to_matching_provider() {
    let (pool, path) = test_pool("exact-model").await;
    let first = add_provider(&pool, "first", 10, "first-key", 10).await;
    upsert_upstream_model_by_id(&pool, first, "first-model", 10, true, &["llm"])
        .await
        .unwrap();
    let second = add_provider(&pool, "second", 20, "second-key", 10).await;
    upsert_upstream_model_by_id(&pool, second, "target-model", 10, true, &["llm"])
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, None, Some("target-model"))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].upstream_name, "second");
    assert_eq!(
        candidates[0].resolved_model.as_deref(),
        Some("target-model")
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn unknown_model_does_not_fall_back_to_active_keys() {
    let (pool, path) = test_pool("unknown-model").await;
    add_provider(&pool, "first", 10, "first-key", 10).await;
    add_provider(&pool, "second", 20, "second-key", 10).await;

    let candidates = candidates_for_client_request_model(&pool, None, Some("unknown-model"))
        .await
        .unwrap();

    assert!(candidates.is_empty());
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn disabled_alias_does_not_pass_through_to_active_keys() {
    let (pool, path) = test_pool("disabled-alias").await;
    add_provider(&pool, "first", 10, "first-key", 10).await;
    set_model_alias_enabled(&pool, "llm-model", false)
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert!(candidates.is_empty());
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn disabled_models_keys_and_failure_cache_are_excluded() {
    let (pool, path) = test_pool("excluded-candidates").await;
    let upstream = add_provider(&pool, "provider", 10, "disabled-key", 10).await;
    let disabled_key = key_id(&pool, "disabled-key").await;
    let cached_key =
        upsert_upstream_key(&pool, "provider", "cached-key", "cached-secret", 20, true)
            .await
            .unwrap();
    upsert_upstream_key(&pool, "provider", "healthy-key", "healthy-secret", 30, true)
        .await
        .unwrap();
    set_key_enabled(&pool, disabled_key, false).await.unwrap();
    mark_key_failure(
        &pool,
        cached_key,
        now_epoch() + 300,
        Some(429),
        "rate limited",
    )
    .await
    .unwrap();
    upsert_upstream_model_by_id(&pool, upstream, "disabled-model", 5, false, &["llm"])
        .await
        .unwrap();
    upsert_upstream_model_by_id(&pool, upstream, "healthy-model", 10, true, &["llm"])
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].key_name, "healthy-key");
    assert_eq!(
        candidates[0].resolved_model.as_deref(),
        Some("healthy-model")
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn discovered_models_are_cached_separately_from_registered_models() {
    let (pool, path) = test_pool("discovered-models").await;
    let upstream = add_provider(&pool, "discoverable", 10, "discover-key", 10).await;
    replace_upstream_discovered_models(
        &pool,
        upstream,
        &[
            "zeta-model".to_string(),
            "alpha-model".to_string(),
            "alpha-model".to_string(),
        ],
    )
    .await
    .unwrap();

    let state = list_state(&pool).await.unwrap();
    let provider = state
        .upstreams
        .iter()
        .find(|provider| provider.name == "discoverable")
        .unwrap();

    assert!(provider.models.is_empty());
    assert_eq!(
        provider
            .discovered_models
            .iter()
            .map(|model| model.model.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha-model", "zeta-model"]
    );
    assert!(
        provider
            .discovered_models
            .iter()
            .all(|model| model.max_model_len.is_none())
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn discovered_models_store_upstream_max_model_len() {
    let (pool, path) = test_pool("discovered-model-lengths").await;
    let upstream = add_provider(&pool, "discoverable", 10, "discover-key", 10).await;
    replace_upstream_discovered_model_items(
        &pool,
        upstream,
        &[
            DiscoveredModelInput {
                model: "provider-model".to_string(),
                max_model_len: None,
            },
            DiscoveredModelInput {
                model: "custom-model".to_string(),
                max_model_len: Some(65_536),
            },
        ],
    )
    .await
    .unwrap();

    let state = list_state(&pool).await.unwrap();
    let provider = state
        .upstreams
        .iter()
        .find(|provider| provider.name == "discoverable")
        .unwrap();
    let lengths = provider
        .discovered_models
        .iter()
        .map(|model| (model.model.as_str(), model.max_model_len))
        .collect::<Vec<_>>();

    assert_eq!(
        lengths,
        vec![("custom-model", Some(65_536)), ("provider-model", None),]
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn public_models_use_stored_route_length_or_default() {
    let (pool, path) = test_pool("public-model-lengths").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let large = upsert_upstream_model_by_id_with_max_model_len(
        &pool,
        upstream,
        "large-llm",
        10,
        true,
        &["llm"],
        Some(262_144),
    )
    .await
    .unwrap();
    let small = upsert_upstream_model_by_id_with_max_model_len(
        &pool,
        upstream,
        "small-llm",
        20,
        true,
        &["llm"],
        Some(131_072),
    )
    .await
    .unwrap();
    add_alias_route(&pool, "llm-model", large, 10, true).await;
    add_alias_route(&pool, "llm-model", small, 20, true).await;

    let models = list_public_models(&pool).await.unwrap();
    let lengths = models
        .into_iter()
        .map(|model| (model.public_model, model.max_model_len))
        .collect::<Vec<_>>();

    assert_eq!(
        lengths,
        vec![
            ("llm-model".to_string(), 131_072),
            ("multimodal-model".to_string(), DEFAULT_MAX_MODEL_LEN),
        ]
    );
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn reset_all_key_health_clears_failure_cache() {
    let (pool, path) = test_pool("reset-all-health").await;
    add_provider(&pool, "provider", 10, "cached-key", 10).await;
    let cached_key = key_id(&pool, "cached-key").await;
    mark_key_failure(
        &pool,
        cached_key,
        now_epoch() + 300,
        Some(500),
        "upstream failed",
    )
    .await
    .unwrap();

    let before = admin_health(&pool).await.unwrap();
    assert_eq!(before.ready_keys, 0);
    assert_eq!(before.cached_keys, 1);

    let reset_count = reset_all_key_health(&pool).await.unwrap();
    let after = admin_health(&pool).await.unwrap();

    assert_eq!(reset_count, 1);
    assert_eq!(after.ready_keys, 1);
    assert_eq!(after.cached_keys, 0);
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn client_alias_route_restricts_candidate_models_for_that_token() {
    let (pool, path) = test_pool("client-route").await;
    let first = add_provider(&pool, "first", 10, "first-key", 10).await;
    let first_model = upsert_upstream_model_by_id(&pool, first, "first-llm", 10, true, &["llm"])
        .await
        .unwrap();
    let second = add_provider(&pool, "second", 20, "second-key", 10).await;
    upsert_upstream_model_by_id(&pool, second, "second-llm", 10, true, &["llm"])
        .await
        .unwrap();
    let client_id = add_client(&pool, "restricted").await;
    upsert_client_model_route(&pool, client_id, "llm-model", first_model, 100, true)
        .await
        .unwrap();

    let restricted = candidates_for_client_request_model(&pool, Some(client_id), Some("llm-model"))
        .await
        .unwrap();
    let global = candidates_for_client_request_model(&pool, None, Some("llm-model"))
        .await
        .unwrap();

    assert_eq!(restricted.len(), 1);
    assert_eq!(restricted[0].upstream_name, "first");
    assert_eq!(restricted[0].resolved_model.as_deref(), Some("first-llm"));
    assert_eq!(global.len(), 2);
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn client_alias_route_blocks_global_fallback_when_routes_are_disabled() {
    let (pool, path) = test_pool("client-disabled-route").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let model = upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
        .await
        .unwrap();
    let client_id = add_client(&pool, "restricted").await;
    upsert_client_model_route(&pool, client_id, "llm-model", model, 100, false)
        .await
        .unwrap();

    let candidates = candidates_for_client_request_model(&pool, Some(client_id), Some("llm-model"))
        .await
        .unwrap();

    assert!(candidates.is_empty());
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn hard_delete_upstream_model_removes_alias_and_client_routes() {
    let (pool, path) = test_pool("delete-upstream-model").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let model = upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
        .await
        .unwrap();
    add_alias_route(&pool, "llm-model", model, 10, true).await;
    let client_id = add_client(&pool, "restricted").await;
    upsert_client_model_route(&pool, client_id, "llm-model", model, 10, true)
        .await
        .unwrap();

    delete_upstream_model(&pool, model).await.unwrap();

    let upstream_model_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM upstream_models WHERE id = ?;")
            .bind(model)
            .fetch_one(&pool)
            .await
            .unwrap();
    let alias_route_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_alias_routes WHERE upstream_model_id = ?;")
            .bind(model)
            .fetch_one(&pool)
            .await
            .unwrap();
    let client_route_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM client_model_routes WHERE upstream_model_id = ?;")
            .bind(model)
            .fetch_one(&pool)
            .await
            .unwrap();
    let alias_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE public_model = ?;")
            .bind("llm-model")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(upstream_model_count, 0);
    assert_eq!(alias_route_count, 0);
    assert_eq!(client_route_count, 0);
    assert_eq!(alias_count, 0);
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn deleting_alias_route_removes_matching_client_routes_and_prunes_unused_alias() {
    let (pool, path) = test_pool("delete-alias-route").await;
    upsert_model_alias(&pool, "temporary-model", "llm", true)
        .await
        .unwrap();
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let model = upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
        .await
        .unwrap();
    add_alias_route(&pool, "temporary-model", model, 10, true).await;
    let route_id: i64 = sqlx::query_scalar(
        "SELECT id FROM model_alias_routes WHERE public_model = ? AND upstream_model_id = ?;",
    )
    .bind("temporary-model")
    .bind(model)
    .fetch_one(&pool)
    .await
    .unwrap();
    let client_id = add_client(&pool, "restricted").await;
    upsert_client_model_route(&pool, client_id, "temporary-model", model, 10, true)
        .await
        .unwrap();

    let deleted_alias = delete_model_alias_route(&pool, route_id).await.unwrap();

    let alias_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_aliases WHERE public_model = ?;")
            .bind("temporary-model")
            .fetch_one(&pool)
            .await
            .unwrap();
    let alias_route_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM model_alias_routes WHERE public_model = ?;")
            .bind("temporary-model")
            .fetch_one(&pool)
            .await
            .unwrap();
    let client_route_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM client_model_routes WHERE public_model = ?;")
            .bind("temporary-model")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(deleted_alias, "temporary-model");
    assert_eq!(alias_count, 0);
    assert_eq!(alias_route_count, 0);
    assert_eq!(client_route_count, 0);
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn client_route_requires_upstream_model_to_support_alias_capability() {
    let (pool, path) = test_pool("client-route-capability").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    let model =
        upsert_upstream_model_by_id(&pool, upstream, "provider-multi", 10, true, &["multimodal"])
            .await
            .unwrap();
    let client_id = add_client(&pool, "restricted").await;

    let error = upsert_client_model_route(&pool, client_id, "llm-model", model, 100, true)
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("does not support"));
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn generated_client_key_authenticates_once_created() {
    let (pool, path) = test_pool("generated-client").await;

    let (client_id, api_key) = create_generated_client(&pool, "generated", true)
        .await
        .unwrap();
    let authenticated = authenticate_client(&pool, &api_key).await.unwrap().unwrap();

    assert_eq!(authenticated.id, client_id);
    assert_eq!(authenticated.name, "generated");
    assert_eq!(authenticated.token_name, "기본 토큰");
    assert!(!api_key.starts_with("zvzo-"));
    assert_eq!(api_key.len(), 48);
    assert!(api_key.chars().all(|ch| ch.is_ascii_hexdigit()));

    let (token_id, extra_api_key) =
        create_generated_client_token(&pool, client_id, Some("worker"), true)
            .await
            .unwrap();
    let extra_authenticated = authenticate_client(&pool, &extra_api_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(extra_authenticated.id, client_id);
    assert_eq!(extra_authenticated.token_id, token_id);
    assert_eq!(extra_authenticated.token_name, "worker");

    let state = list_state(&pool).await.unwrap();
    let client = state
        .clients
        .iter()
        .find(|client| client.id == client_id)
        .unwrap();
    assert_eq!(client.tokens.len(), 2);
    assert_eq!(client.tokens[0].api_key.as_deref(), Some(api_key.as_str()));
    assert_eq!(
        client.tokens[1].api_key.as_deref(),
        Some(extra_api_key.as_str())
    );
    let serialized = serde_json::to_string(&state).unwrap();
    assert!(!serialized.contains(&api_key));
    assert!(!serialized.contains(&extra_api_key));

    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn hard_delete_client_token_removes_auth_token() {
    let (pool, path) = test_pool("delete-client-token").await;

    let (client_id, default_key) = create_generated_client(&pool, "client", true)
        .await
        .unwrap();
    let (token_id, extra_key) =
        create_generated_client_token(&pool, client_id, Some("worker"), true)
            .await
            .unwrap();

    delete_client_token(&pool, token_id).await.unwrap();

    assert!(
        authenticate_client(&pool, &default_key)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        authenticate_client(&pool, &extra_key)
            .await
            .unwrap()
            .is_none()
    );
    let state = list_state(&pool).await.unwrap();
    let client = state
        .clients
        .iter()
        .find(|client| client.id == client_id)
        .unwrap();
    assert_eq!(client.tokens.len(), 1);
    assert_eq!(
        client.tokens[0].api_key.as_deref(),
        Some(default_key.as_str())
    );

    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn disabled_client_token_does_not_authenticate_other_tokens() {
    let (pool, path) = test_pool("disabled-client-token").await;

    let (client_id, default_key) = create_generated_client(&pool, "client", true)
        .await
        .unwrap();
    let (token_id, extra_key) =
        create_generated_client_token(&pool, client_id, Some("local"), true)
            .await
            .unwrap();

    sqlx::query("UPDATE client_tokens SET enabled = 0 WHERE id = ?;")
        .bind(token_id)
        .execute(&pool)
        .await
        .unwrap();

    assert!(
        authenticate_client(&pool, &extra_key)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        authenticate_client(&pool, &default_key)
            .await
            .unwrap()
            .is_some()
    );

    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn one_model_can_satisfy_multiple_capabilities() {
    let (pool, path) = test_pool("multi-capability").await;
    let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
    upsert_upstream_model_by_id(
        &pool,
        upstream,
        "omni-model",
        10,
        true,
        &["image", "multimodal"],
    )
    .await
    .unwrap();
    upsert_model_alias(&pool, "image-model", "image", true)
        .await
        .unwrap();

    let image = candidates_for_client_request_model(&pool, None, Some("image-model"))
        .await
        .unwrap();
    let multimodal = candidates_for_client_request_model(&pool, None, Some("multimodal-model"))
        .await
        .unwrap();

    assert_eq!(image[0].resolved_model.as_deref(), Some("omni-model"));
    assert_eq!(multimodal[0].resolved_model.as_deref(), Some("omni-model"));
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn audit_insert_sets_local_completed_date_and_attempt_rows() {
    let (pool, path) = test_pool("audit-date").await;
    let upstream_id = add_provider(&pool, "provider", 10, "key", 10).await;
    let upstream_key_id = key_id(&pool, "key").await;
    let mut request_audit =
        request_audit_for_path("/v1/chat/completions", "chat_completions", 1_781_253_600);
    request_audit.method = "POST".to_string();
    request_audit.model = Some("llm-model".to_string());
    request_audit.stream = Some(false);
    request_audit.content_type = Some("application/json".to_string());
    request_audit.request_body_bytes = Some(64);
    request_audit.upstream_id = Some(upstream_id);
    request_audit.upstream_name = Some("provider".to_string());
    request_audit.upstream_key_id = Some(upstream_key_id);
    request_audit.upstream_key_name = Some("key".to_string());
    request_audit.attempts = 1;
    request_audit.input_tokens = Some(10);
    request_audit.output_tokens = Some(20);
    request_audit.total_tokens = Some(30);
    let attempt = AttemptAudit {
        attempt_index: 1,
        upstream_id,
        upstream_name: "provider".to_string(),
        upstream_key_id,
        upstream_key_name: "key".to_string(),
        status: Some(200),
        outcome: "success".to_string(),
        retriable: false,
        duration_ms: 10,
        retry_after_secs: None,
        disabled_until: None,
        error_class: None,
        error_message: None,
        upstream_content_type: Some("application/json".to_string()),
        upstream_body_bytes: Some(128),
        upstream_body_hash: Some("sha256:test".to_string()),
        upstream_body_kind: Some("json_like".to_string()),
    };

    let id = insert_request_audit(&pool, &request_audit, &[attempt])
        .await
        .unwrap();
    let row = sqlx::query(
        r#"
        SELECT completed_date, attempts,
            (SELECT count(*) FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_rows
            ,(SELECT upstream_content_type FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_content_type
            ,(SELECT upstream_body_bytes FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_body_bytes
            ,(SELECT upstream_body_hash FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_body_hash
            ,(SELECT upstream_body_kind FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_body_kind
        FROM request_audits
        WHERE id = ?;
        "#,
    )
    .bind(id)
    .bind(id)
    .bind(id)
    .bind(id)
    .bind(id)
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(row.get::<String, _>("completed_date").len(), 10);
    assert_eq!(row.get::<i64, _>("attempts"), 1);
    assert_eq!(row.get::<i64, _>("attempt_rows"), 1);
    assert_eq!(
        row.get::<String, _>("attempt_content_type"),
        "application/json"
    );
    assert_eq!(row.get::<i64, _>("attempt_body_bytes"), 128);
    assert_eq!(row.get::<String, _>("attempt_body_hash"), "sha256:test");
    assert_eq!(row.get::<String, _>("attempt_body_kind"), "json_like");
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn admin_stats_hide_admin_and_browser_artifact_requests() {
    let (pool, path) = test_pool("admin-stats-filter").await;
    for (path, route_kind, completed_at) in [
        ("/admin", "other", 1_781_253_600),
        ("/admin/missing", "other", 1_781_253_601),
        ("/favicon.ico", "other", 1_781_253_602),
        ("/apple-touch-icon.png", "other", 1_781_253_603),
        ("/v1/chat/completions", "chat_completions", 1_781_253_604),
    ] {
        insert_request_audit(
            &pool,
            &request_audit_for_path(path, route_kind, completed_at),
            &[],
        )
        .await
        .unwrap();
    }

    let stats = list_admin_stats(&pool).await.unwrap();
    let paths = stats
        .recent_requests
        .into_iter()
        .map(|request| request.path)
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["/v1/chat/completions".to_string()]);
    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn admin_stats_group_requests_by_client_token_and_updated_usage() {
    let (pool, path) = test_pool("client-token-stats").await;
    let (client_id, api_key) = create_generated_client(&pool, "stats-client", true)
        .await
        .unwrap();
    let identity = authenticate_client(&pool, &api_key).await.unwrap().unwrap();
    let mut audit =
        request_audit_for_path("/v1/chat/completions", "chat_completions", 1_781_253_604);
    audit.client_id = Some(client_id);
    audit.client_name = Some("stats-client".to_string());
    audit.client_token_id = Some(identity.token_id);
    audit.client_token_name = Some(identity.token_name);
    audit.client_key_hash = Some(hash_secret(&api_key));
    audit.duration_ms = 45;

    let audit_id = insert_request_audit(&pool, &audit, &[]).await.unwrap();
    update_request_audit_usage(&pool, audit_id, Some(10), Some(5), Some(15))
        .await
        .unwrap();

    let stats = list_admin_stats(&pool).await.unwrap();
    let token_stats = stats
        .client_token_stats
        .iter()
        .find(|stat| stat.client_name == "stats-client")
        .unwrap();
    assert_eq!(token_stats.total_requests, 1);
    assert_eq!(token_stats.total_duration_ms, 45);
    assert_eq!(token_stats.input_tokens, 10);
    assert_eq!(token_stats.output_tokens, 5);
    assert_eq!(token_stats.total_tokens, 15);
    assert_eq!(
        stats.recent_requests[0].client_token_name.as_deref(),
        Some("기본 토큰")
    );

    close_and_remove(pool, path).await;
}

#[tokio::test]
async fn runtime_cleanup_removes_expired_audits_attempts_and_response_states() {
    let (pool, path) = test_pool("runtime-cleanup").await;
    let upstream_id = add_provider(&pool, "provider", 10, "key", 10).await;
    let upstream_key_id = key_id(&pool, "key").await;
    let old_epoch = 1_700_000_000;
    let fresh_epoch = old_epoch + 10 * 86_400;
    let mut old_audit =
        request_audit_for_path("/v1/chat/completions", "chat_completions", old_epoch);
    old_audit.upstream_id = Some(upstream_id);
    old_audit.upstream_name = Some("provider".to_string());
    old_audit.upstream_key_id = Some(upstream_key_id);
    old_audit.upstream_key_name = Some("key".to_string());
    old_audit.attempts = 1;
    let old_attempt = AttemptAudit {
        attempt_index: 1,
        upstream_id,
        upstream_name: "provider".to_string(),
        upstream_key_id,
        upstream_key_name: "key".to_string(),
        status: Some(200),
        outcome: "success".to_string(),
        retriable: false,
        duration_ms: 10,
        retry_after_secs: None,
        disabled_until: None,
        error_class: None,
        error_message: None,
        upstream_content_type: None,
        upstream_body_bytes: None,
        upstream_body_hash: None,
        upstream_body_kind: None,
    };
    let old_audit_id = insert_request_audit(&pool, &old_audit, &[old_attempt])
        .await
        .unwrap();
    let fresh_audit = request_audit_for_path("/v1/responses", "responses", fresh_epoch);
    insert_request_audit(&pool, &fresh_audit, &[])
        .await
        .unwrap();

    for (id, created_at) in [("resp_old", old_epoch), ("resp_fresh", fresh_epoch)] {
        insert_response_state(
            &pool,
            &ResponseState {
                id: id.to_string(),
                previous_response_id: None,
                client_id: None,
                model: "llm-model".to_string(),
                chat_messages_json: "[]".to_string(),
                output_json: "[]".to_string(),
                output_text: String::new(),
                input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE response_states SET created_at = ? WHERE id = ?;")
            .bind(created_at)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let summary = cleanup_runtime_state_before(&pool, fresh_epoch - 86_400, fresh_epoch - 86_400)
        .await
        .unwrap();

    assert_eq!(summary.request_audits_deleted, 1);
    assert_eq!(summary.response_states_deleted, 1);
    let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM request_audits;")
        .fetch_one(&pool)
        .await
        .unwrap();
    let attempt_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM upstream_attempt_audits WHERE request_audit_id = ?;",
    )
    .bind(old_audit_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let state_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM response_states ORDER BY id;")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(audit_count, 1);
    assert_eq!(attempt_count, 0);
    assert_eq!(state_ids, vec!["resp_fresh".to_string()]);
    close_and_remove(pool, path).await;
}
