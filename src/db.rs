mod models;
mod schema;

pub use models::*;
pub use schema::connect;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{fs::File, io::Read, time::SystemTime};

pub async fn upsert_client(
    pool: &SqlitePool,
    name: &str,
    api_key: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    if api_key.trim().is_empty() {
        bail!("client api key must not be empty");
    }
    let hash = hash_secret(api_key);
    let row = sqlx::query(
        r#"
        INSERT INTO clients(name, api_key_hash, api_key, enabled)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(name) DO UPDATE SET
            api_key_hash = excluded.api_key_hash,
            api_key = excluded.api_key,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(name)
    .bind(hash)
    .bind(api_key)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    let id = row.get("id");
    upsert_client_token(pool, id, "기본 토큰", api_key, enabled).await?;
    Ok(id)
}

pub async fn create_generated_client(
    pool: &SqlitePool,
    name: &str,
    enabled: bool,
) -> anyhow::Result<(i64, String)> {
    let api_key = generate_client_api_key()?;
    let id = upsert_client(pool, name, &api_key, enabled).await?;
    Ok((id, api_key))
}

pub async fn create_generated_client_token(
    pool: &SqlitePool,
    client_id: i64,
    name: Option<&str>,
    enabled: bool,
) -> anyhow::Result<(i64, String)> {
    let api_key = generate_client_api_key()?;
    let token_name = match name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(name) => name.to_string(),
        None => next_client_token_name(pool, client_id).await?,
    };
    let id = upsert_client_token(pool, client_id, &token_name, &api_key, enabled).await?;
    Ok((id, api_key))
}

async fn next_client_token_name(pool: &SqlitePool, client_id: i64) -> anyhow::Result<String> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM client_tokens
        WHERE client_id = ?;
        "#,
    )
    .bind(client_id)
    .fetch_one(pool)
    .await?;
    Ok(format!("토큰 {}", count + 1))
}

async fn upsert_client_token(
    pool: &SqlitePool,
    client_id: i64,
    name: &str,
    api_key: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    if name.trim().is_empty() {
        bail!("client token name must not be empty");
    }
    if api_key.trim().is_empty() {
        bail!("client api key must not be empty");
    }
    let hash = hash_secret(api_key);
    let row = sqlx::query(
        r#"
        INSERT INTO client_tokens(client_id, name, api_key_hash, api_key, enabled)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(client_id, name) DO UPDATE SET
            api_key_hash = excluded.api_key_hash,
            api_key = excluded.api_key,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(client_id)
    .bind(name.trim())
    .bind(hash)
    .bind(api_key)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub fn generate_client_api_key() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 24];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom")?
        .read_exact(&mut bytes)
        .context("failed to read random bytes")?;
    Ok(hex::encode(bytes))
}

pub async fn upsert_upstream(
    pool: &SqlitePool,
    name: &str,
    base_url: &str,
    priority: i64,
    enabled: bool,
) -> anyhow::Result<i64> {
    validate_base_url(base_url)?;
    let row = sqlx::query(
        r#"
        INSERT INTO upstreams(name, base_url, priority, enabled)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(name) DO UPDATE SET
            base_url = excluded.base_url,
            priority = excluded.priority,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(name)
    .bind(base_url.trim_end_matches('/'))
    .bind(priority)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub async fn upsert_upstream_key(
    pool: &SqlitePool,
    upstream_name: &str,
    name: &str,
    api_key: &str,
    priority: i64,
    enabled: bool,
) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT id FROM upstreams WHERE name = ?")
        .bind(upstream_name)
        .fetch_optional(pool)
        .await?;
    let upstream_id = row
        .map(|row| row.get("id"))
        .with_context(|| format!("upstream not found: {upstream_name}"))?;
    upsert_upstream_key_by_id(pool, upstream_id, name, api_key, priority, enabled).await
}

pub async fn next_upstream_key_priority(
    pool: &SqlitePool,
    upstream_name: &str,
) -> anyhow::Result<i64> {
    let priority = sqlx::query_scalar(
        r#"
        SELECT COALESCE(MAX(k.priority), 0) + 10
        FROM upstream_keys k
        JOIN upstreams u ON u.id = k.upstream_id
        WHERE u.name = ?;
        "#,
    )
    .bind(upstream_name)
    .fetch_one(pool)
    .await?;
    Ok(priority)
}

pub async fn upsert_upstream_key_by_id(
    pool: &SqlitePool,
    upstream_id: i64,
    name: &str,
    api_key: &str,
    priority: i64,
    enabled: bool,
) -> anyhow::Result<i64> {
    if api_key.trim().is_empty() {
        bail!("upstream api key must not be empty");
    }
    let row = sqlx::query(
        r#"
        INSERT INTO upstream_keys(upstream_id, name, api_key, priority, enabled)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(upstream_id, name) DO UPDATE SET
            api_key = excluded.api_key,
            priority = excluded.priority,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(upstream_id)
    .bind(name)
    .bind(api_key)
    .bind(priority)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub async fn upsert_upstream_model(
    pool: &SqlitePool,
    upstream_name: &str,
    model: &str,
    priority: i64,
    enabled: bool,
    capabilities: &[String],
) -> anyhow::Result<i64> {
    upsert_upstream_model_with_max_model_len(
        pool,
        upstream_name,
        model,
        priority,
        enabled,
        capabilities,
        None,
    )
    .await
}

pub async fn upsert_upstream_model_with_max_model_len(
    pool: &SqlitePool,
    upstream_name: &str,
    model: &str,
    priority: i64,
    enabled: bool,
    capabilities: &[String],
    max_model_len: Option<i64>,
) -> anyhow::Result<i64> {
    let row = sqlx::query("SELECT id FROM upstreams WHERE name = ?")
        .bind(upstream_name)
        .fetch_optional(pool)
        .await?;
    let upstream_id = row
        .map(|row| row.get("id"))
        .with_context(|| format!("upstream not found: {upstream_name}"))?;
    let capability_refs: Vec<&str> = capabilities.iter().map(String::as_str).collect();
    upsert_upstream_model_by_id_with_max_model_len(
        pool,
        upstream_id,
        model,
        priority,
        enabled,
        &capability_refs,
        max_model_len,
    )
    .await
}

#[cfg(test)]
pub async fn upsert_upstream_model_by_id(
    pool: &SqlitePool,
    upstream_id: i64,
    model: &str,
    priority: i64,
    enabled: bool,
    capabilities: &[&str],
) -> anyhow::Result<i64> {
    upsert_upstream_model_by_id_with_max_model_len(
        pool,
        upstream_id,
        model,
        priority,
        enabled,
        capabilities,
        None,
    )
    .await
}

pub async fn upsert_upstream_model_by_id_with_max_model_len(
    pool: &SqlitePool,
    upstream_id: i64,
    model: &str,
    priority: i64,
    enabled: bool,
    capabilities: &[&str],
    max_model_len: Option<i64>,
) -> anyhow::Result<i64> {
    let model = model.trim();
    if model.is_empty() {
        bail!("upstream model must not be empty");
    }
    if capabilities.is_empty() {
        bail!("upstream model must have at least one capability");
    }

    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        r#"
        INSERT INTO upstream_models(upstream_id, model, max_model_len, priority, enabled)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(upstream_id, model) DO UPDATE SET
            priority = excluded.priority,
            max_model_len = COALESCE(excluded.max_model_len, upstream_models.max_model_len),
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(upstream_id)
    .bind(model)
    .bind(max_model_len)
    .bind(priority)
    .bind(enabled)
    .fetch_one(&mut *tx)
    .await?;
    let upstream_model_id = row.get("id");

    sqlx::query("DELETE FROM upstream_model_capabilities WHERE upstream_model_id = ?;")
        .bind(upstream_model_id)
        .execute(&mut *tx)
        .await?;
    for capability in capabilities {
        let capability = capability.trim();
        if capability.is_empty() {
            bail!("model capability must not be empty");
        }
        sqlx::query(
            r#"
            INSERT INTO upstream_model_capabilities(upstream_model_id, capability)
            VALUES (?, ?)
            ON CONFLICT(upstream_model_id, capability) DO NOTHING;
            "#,
        )
        .bind(upstream_model_id)
        .bind(capability)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(upstream_model_id)
}

pub async fn upsert_upstream_model_for_alias(
    pool: &SqlitePool,
    upstream_name: &str,
    model: &str,
    public_model: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    let upstream_name = upstream_name.trim();
    let model = model.trim();
    let public_model = public_model.trim();
    if upstream_name.is_empty() {
        bail!("upstream name must not be empty");
    }
    if model.is_empty() {
        bail!("upstream model must not be empty");
    }
    if public_model.is_empty() {
        bail!("public model alias must not be empty");
    }

    let mut tx = pool.begin().await?;
    let upstream_row = sqlx::query("SELECT id FROM upstreams WHERE name = ?")
        .bind(upstream_name)
        .fetch_optional(&mut *tx)
        .await?;
    let upstream_id: i64 = upstream_row
        .map(|row| row.get("id"))
        .with_context(|| format!("upstream not found: {upstream_name}"))?;

    let alias_row = sqlx::query("SELECT target_type FROM model_aliases WHERE public_model = ?")
        .bind(public_model)
        .fetch_optional(&mut *tx)
        .await?;
    let target_type: String = alias_row
        .map(|row| row.get("target_type"))
        .with_context(|| format!("model alias not found: {public_model}"))?;

    let model_priority: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), 0) + 10 FROM upstream_models WHERE upstream_id = ?;",
    )
    .bind(upstream_id)
    .fetch_one(&mut *tx)
    .await?;
    let discovered_max_model_len: Option<i64> = sqlx::query_scalar(
        "SELECT max_model_len FROM upstream_discovered_models WHERE upstream_id = ? AND model = ?;",
    )
    .bind(upstream_id)
    .bind(model)
    .fetch_optional(&mut *tx)
    .await?;
    let max_model_len = discovered_max_model_len;

    let model_row = sqlx::query(
        r#"
        INSERT INTO upstream_models(upstream_id, model, max_model_len, priority, enabled)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(upstream_id, model) DO UPDATE SET
            max_model_len = COALESCE(excluded.max_model_len, upstream_models.max_model_len),
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(upstream_id)
    .bind(model)
    .bind(max_model_len)
    .bind(model_priority)
    .bind(enabled)
    .fetch_one(&mut *tx)
    .await?;
    let upstream_model_id = model_row.get("id");

    sqlx::query(
        r#"
        INSERT INTO upstream_model_capabilities(upstream_model_id, capability)
        VALUES (?, ?)
        ON CONFLICT(upstream_model_id, capability) DO NOTHING;
        "#,
    )
    .bind(upstream_model_id)
    .bind(&target_type)
    .execute(&mut *tx)
    .await?;

    let route_priority: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(priority), 0) + 10 FROM model_alias_routes WHERE public_model = ?;",
    )
    .bind(public_model)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO model_alias_routes(public_model, upstream_model_id, priority, enabled)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(public_model, upstream_model_id) DO UPDATE SET
            enabled = excluded.enabled,
            updated_at = unixepoch();
        "#,
    )
    .bind(public_model)
    .bind(upstream_model_id)
    .bind(route_priority)
    .bind(enabled)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(upstream_model_id)
}

pub async fn upsert_model_alias(
    pool: &SqlitePool,
    public_model: &str,
    target_type: &str,
    enabled: bool,
) -> anyhow::Result<i64> {
    let public_model = public_model.trim();
    let target_type = target_type.trim();
    if public_model.is_empty() {
        bail!("public model alias must not be empty");
    }
    if target_type.is_empty() {
        bail!("target model type must not be empty");
    }
    let row = sqlx::query(
        r#"
        INSERT INTO model_aliases(public_model, target_type, enabled)
        VALUES (?, ?, ?)
        ON CONFLICT(public_model) DO UPDATE SET
            target_type = excluded.target_type,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(public_model)
    .bind(target_type)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub async fn ensure_model_alias(
    pool: &SqlitePool,
    public_model: &str,
    target_type: &str,
) -> anyhow::Result<i64> {
    let public_model = public_model.trim();
    let target_type = target_type.trim();
    if public_model.is_empty() {
        bail!("public model alias must not be empty");
    }
    if target_type.is_empty() {
        bail!("target model type must not be empty");
    }
    let row = sqlx::query(
        r#"
        INSERT INTO model_aliases(public_model, target_type, enabled)
        VALUES (?, ?, 1)
        ON CONFLICT(public_model) DO UPDATE SET
            enabled = 1,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(public_model)
    .bind(target_type)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub async fn upsert_client_model_route(
    pool: &SqlitePool,
    client_id: i64,
    public_model: &str,
    upstream_model_id: i64,
    priority: i64,
    enabled: bool,
) -> anyhow::Result<i64> {
    let public_model = public_model.trim();
    if public_model.is_empty() {
        bail!("public model alias must not be empty");
    }

    let supported = sqlx::query(
        r#"
        SELECT 1
        FROM model_aliases a
        JOIN upstream_model_capabilities c ON c.capability = a.target_type
        WHERE a.public_model = ? AND c.upstream_model_id = ?
        LIMIT 1;
        "#,
    )
    .bind(public_model)
    .bind(upstream_model_id)
    .fetch_optional(pool)
    .await?;
    if supported.is_none() {
        bail!("selected upstream model does not support this alias target type");
    }

    let row = sqlx::query(
        r#"
        INSERT INTO client_model_routes(client_id, public_model, upstream_model_id, priority, enabled)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(client_id, public_model, upstream_model_id) DO UPDATE SET
            priority = excluded.priority,
            enabled = excluded.enabled,
            updated_at = unixepoch()
        RETURNING id;
        "#,
    )
    .bind(client_id)
    .bind(public_model)
    .bind(upstream_model_id)
    .bind(priority)
    .bind(enabled)
    .fetch_one(pool)
    .await?;
    Ok(row.get("id"))
}

pub async fn next_client_model_route_priority(
    pool: &SqlitePool,
    client_id: i64,
    public_model: &str,
) -> anyhow::Result<i64> {
    let priority = sqlx::query_scalar(
        r#"
        SELECT COALESCE(MAX(priority), 0) + 10
        FROM client_model_routes
        WHERE client_id = ? AND public_model = ?;
        "#,
    )
    .bind(client_id)
    .bind(public_model)
    .fetch_one(pool)
    .await?;
    Ok(priority)
}

pub async fn authenticate_client(
    pool: &SqlitePool,
    api_key: &str,
) -> anyhow::Result<Option<ClientIdentity>> {
    let hash = hash_secret(api_key);
    let row = sqlx::query(
        r#"
        SELECT
            c.id AS client_id,
            c.name AS client_name,
            t.id AS token_id,
            t.name AS token_name
        FROM client_tokens t
        JOIN clients c ON c.id = t.client_id
        WHERE t.api_key_hash = ?
          AND t.enabled = 1
          AND c.enabled = 1
        LIMIT 1;
        "#,
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| ClientIdentity {
        id: row.get("client_id"),
        name: row.get("client_name"),
        token_id: row.get("token_id"),
        token_name: row.get("token_name"),
    }))
}

#[cfg(test)]
pub async fn list_enabled_model_aliases(
    pool: &SqlitePool,
) -> anyhow::Result<Vec<ModelAliasSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT id, public_model, target_type, enabled, created_at, updated_at
        FROM model_aliases
        WHERE enabled = 1
        ORDER BY id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(model_alias_summary_from_row).collect())
}

pub async fn list_public_models(pool: &SqlitePool) -> anyhow::Result<Vec<PublicModelSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT id, public_model, target_type, created_at
        FROM model_aliases
        WHERE enabled = 1
        ORDER BY id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut models = Vec::with_capacity(rows.len());
    for row in rows {
        let public_model = row.get::<String, _>("public_model");
        let target_type = row.get::<String, _>("target_type");
        let route_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM model_alias_routes WHERE public_model = ?;")
                .bind(&public_model)
                .fetch_one(pool)
                .await?;
        let stored_max_model_len: Option<i64> = if route_count > 0 {
            sqlx::query_scalar(
                r#"
                SELECT MIN(m.max_model_len)
                FROM model_alias_routes r
                JOIN upstream_models m ON m.id = r.upstream_model_id
                JOIN upstreams u ON u.id = m.upstream_id
                WHERE r.public_model = ?
                  AND r.enabled = 1
                  AND m.enabled = 1
                  AND u.enabled = 1
                  AND m.max_model_len IS NOT NULL;
                "#,
            )
            .bind(&public_model)
            .fetch_one(pool)
            .await?
        } else {
            sqlx::query_scalar(
                r#"
                SELECT MIN(m.max_model_len)
                FROM upstream_model_capabilities c
                JOIN upstream_models m ON m.id = c.upstream_model_id
                JOIN upstreams u ON u.id = m.upstream_id
                WHERE c.capability = ?
                  AND m.enabled = 1
                  AND u.enabled = 1
                  AND m.max_model_len IS NOT NULL;
                "#,
            )
            .bind(&target_type)
            .fetch_one(pool)
            .await?
        };
        models.push(PublicModelSummary {
            public_model,
            created_at: row.get("created_at"),
            max_model_len: stored_max_model_len.unwrap_or(DEFAULT_MAX_MODEL_LEN),
        });
    }

    Ok(models)
}

async fn active_candidates(pool: &SqlitePool) -> anyhow::Result<Vec<Candidate>> {
    let now = now_epoch();
    let rows = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            k.id AS key_id,
            k.name AS key_name,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key,
            NULL AS resolved_model,
            u.priority AS upstream_priority,
            NULL AS model_priority,
            k.priority AS key_priority
        FROM upstream_keys k
        JOIN upstreams u ON u.id = k.upstream_id
        WHERE
            u.enabled = 1
            AND k.enabled = 1
            AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
        ORDER BY u.priority ASC, k.priority ASC, k.id ASC;
        "#,
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Candidate {
            upstream_id: row.get("upstream_id"),
            key_id: row.get("key_id"),
            key_name: row.get("key_name"),
            upstream_name: row.get("upstream_name"),
            base_url: row.get("base_url"),
            api_key: row.get("api_key"),
            resolved_model: row.get("resolved_model"),
            upstream_priority: row.get("upstream_priority"),
            model_priority: row.get("model_priority"),
            key_priority: row.get("key_priority"),
        })
        .collect())
}

pub async fn candidates_for_client_request_model(
    pool: &SqlitePool,
    client_id: Option<i64>,
    requested_model: Option<&str>,
) -> anyhow::Result<Vec<Candidate>> {
    let Some(requested_model) = requested_model else {
        return active_candidates(pool).await;
    };
    if let Some(alias) = resolve_model_alias(pool, requested_model).await? {
        if !alias.enabled {
            return Ok(Vec::new());
        }
        if let Some(client_id) = client_id {
            let client_routes = candidates_for_client_alias_routes(
                pool,
                client_id,
                requested_model,
                &alias.target_type,
            )
            .await?;
            if client_routes.has_routes {
                return Ok(client_routes.candidates);
            }
        }
        let alias_routes =
            candidates_for_model_alias_routes(pool, requested_model, &alias.target_type).await?;
        if alias_routes.has_routes {
            return Ok(alias_routes.candidates);
        }
        return candidates_for_model_capability(pool, &alias.target_type).await;
    }

    let exact_candidates = candidates_for_exact_model(pool, requested_model).await?;
    if exact_candidates.is_empty() {
        active_candidates(pool).await
    } else {
        Ok(exact_candidates)
    }
}

pub async fn get_response_state(
    pool: &SqlitePool,
    id: &str,
    client_id: Option<i64>,
) -> anyhow::Result<Option<ResponseState>> {
    let row = sqlx::query(
        r#"
        SELECT
            id,
            previous_response_id,
            client_id,
            model,
            chat_messages_json,
            output_json,
            output_text,
            input_tokens,
            output_tokens,
            total_tokens
        FROM response_states
        WHERE id = ? AND ((client_id IS NULL AND ? IS NULL) OR client_id = ?)
        LIMIT 1;
        "#,
    )
    .bind(id)
    .bind(client_id)
    .bind(client_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| ResponseState {
        id: row.get("id"),
        previous_response_id: row.get("previous_response_id"),
        client_id: row.get("client_id"),
        model: row.get("model"),
        chat_messages_json: row.get("chat_messages_json"),
        output_json: row.get("output_json"),
        output_text: row.get("output_text"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        total_tokens: row.get("total_tokens"),
    }))
}

pub async fn insert_response_state(pool: &SqlitePool, state: &ResponseState) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO response_states(
            id,
            previous_response_id,
            client_id,
            model,
            chat_messages_json,
            output_json,
            output_text,
            input_tokens,
            output_tokens,
            total_tokens,
            created_at,
            updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch(), unixepoch())
        ON CONFLICT(id) DO UPDATE SET
            previous_response_id = excluded.previous_response_id,
            client_id = excluded.client_id,
            model = excluded.model,
            chat_messages_json = excluded.chat_messages_json,
            output_json = excluded.output_json,
            output_text = excluded.output_text,
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            total_tokens = excluded.total_tokens,
            updated_at = unixepoch();
        "#,
    )
    .bind(&state.id)
    .bind(&state.previous_response_id)
    .bind(state.client_id)
    .bind(&state.model)
    .bind(&state.chat_messages_json)
    .bind(&state.output_json)
    .bind(&state.output_text)
    .bind(state.input_tokens)
    .bind(state.output_tokens)
    .bind(state.total_tokens)
    .execute(pool)
    .await?;
    Ok(())
}

struct ClientRouteCandidates {
    has_routes: bool,
    candidates: Vec<Candidate>,
}

struct AliasRouteCandidates {
    has_routes: bool,
    candidates: Vec<Candidate>,
}

struct ModelAliasRoute {
    target_type: String,
    enabled: bool,
}

async fn resolve_model_alias(
    pool: &SqlitePool,
    public_model: &str,
) -> anyhow::Result<Option<ModelAliasRoute>> {
    let row = sqlx::query(
        r#"
        SELECT target_type, enabled
        FROM model_aliases
        WHERE public_model = ?
        LIMIT 1;
        "#,
    )
    .bind(public_model)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| ModelAliasRoute {
        target_type: row.get("target_type"),
        enabled: row.get::<i64, _>("enabled") == 1,
    }))
}

async fn candidates_for_client_alias_routes(
    pool: &SqlitePool,
    client_id: i64,
    public_model: &str,
    capability: &str,
) -> anyhow::Result<ClientRouteCandidates> {
    let route_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM client_model_routes
        WHERE client_id = ? AND public_model = ?;
        "#,
    )
    .bind(client_id)
    .bind(public_model)
    .fetch_one(pool)
    .await?;
    if route_count == 0 {
        return Ok(ClientRouteCandidates {
            has_routes: false,
            candidates: Vec::new(),
        });
    }

    let now = now_epoch();
    let rows = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            k.id AS key_id,
            k.name AS key_name,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key,
            m.model AS resolved_model,
            u.priority AS upstream_priority,
            m.priority AS model_priority,
            k.priority AS key_priority
        FROM client_model_routes r
        JOIN upstream_models m ON m.id = r.upstream_model_id
        JOIN upstream_model_capabilities c ON c.upstream_model_id = m.id
        JOIN upstreams u ON u.id = m.upstream_id
        JOIN upstream_keys k ON k.upstream_id = u.id
        WHERE
            r.client_id = ?
            AND r.public_model = ?
            AND r.enabled = 1
            AND c.capability = ?
            AND m.enabled = 1
            AND u.enabled = 1
            AND k.enabled = 1
            AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
        ORDER BY
            r.priority ASC,
            k.priority ASC,
            r.id ASC,
            k.id ASC;
        "#,
    )
    .bind(client_id)
    .bind(public_model)
    .bind(capability)
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(ClientRouteCandidates {
        has_routes: true,
        candidates: rows.into_iter().map(candidate_from_row).collect(),
    })
}

async fn candidates_for_model_alias_routes(
    pool: &SqlitePool,
    public_model: &str,
    capability: &str,
) -> anyhow::Result<AliasRouteCandidates> {
    let route_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM model_alias_routes
        WHERE public_model = ?;
        "#,
    )
    .bind(public_model)
    .fetch_one(pool)
    .await?;
    if route_count == 0 {
        return Ok(AliasRouteCandidates {
            has_routes: false,
            candidates: Vec::new(),
        });
    }

    let now = now_epoch();
    let rows = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            k.id AS key_id,
            k.name AS key_name,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key,
            m.model AS resolved_model,
            u.priority AS upstream_priority,
            r.priority AS model_priority,
            k.priority AS key_priority
        FROM model_alias_routes r
        JOIN upstream_models m ON m.id = r.upstream_model_id
        JOIN upstream_model_capabilities c ON c.upstream_model_id = m.id
        JOIN upstreams u ON u.id = m.upstream_id
        JOIN upstream_keys k ON k.upstream_id = u.id
        WHERE
            r.public_model = ?
            AND r.enabled = 1
            AND c.capability = ?
            AND m.enabled = 1
            AND u.enabled = 1
            AND k.enabled = 1
            AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
        ORDER BY
            r.priority ASC,
            k.priority ASC,
            r.id ASC,
            k.id ASC;
        "#,
    )
    .bind(public_model)
    .bind(capability)
    .bind(now)
    .fetch_all(pool)
    .await?;

    Ok(AliasRouteCandidates {
        has_routes: true,
        candidates: rows.into_iter().map(candidate_from_row).collect(),
    })
}

async fn candidates_for_model_capability(
    pool: &SqlitePool,
    capability: &str,
) -> anyhow::Result<Vec<Candidate>> {
    let now = now_epoch();
    let rows = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            k.id AS key_id,
            k.name AS key_name,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key,
            m.model AS resolved_model,
            u.priority AS upstream_priority,
            m.priority AS model_priority,
            k.priority AS key_priority
        FROM upstream_model_capabilities c
        JOIN upstream_models m ON m.id = c.upstream_model_id
        JOIN upstreams u ON u.id = m.upstream_id
        JOIN upstream_keys k ON k.upstream_id = u.id
        WHERE
            c.capability = ?
            AND m.enabled = 1
            AND u.enabled = 1
            AND k.enabled = 1
            AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
        ORDER BY u.priority ASC, m.priority ASC, k.priority ASC, m.id ASC, k.id ASC;
        "#,
    )
    .bind(capability)
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(candidate_from_row).collect())
}

async fn candidates_for_exact_model(
    pool: &SqlitePool,
    model: &str,
) -> anyhow::Result<Vec<Candidate>> {
    let now = now_epoch();
    let rows = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            k.id AS key_id,
            k.name AS key_name,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key,
            m.model AS resolved_model,
            u.priority AS upstream_priority,
            m.priority AS model_priority,
            k.priority AS key_priority
        FROM upstream_models m
        JOIN upstreams u ON u.id = m.upstream_id
        JOIN upstream_keys k ON k.upstream_id = u.id
        WHERE
            m.model = ?
            AND m.enabled = 1
            AND u.enabled = 1
            AND k.enabled = 1
            AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
        ORDER BY u.priority ASC, m.priority ASC, k.priority ASC, m.id ASC, k.id ASC;
        "#,
    )
    .bind(model)
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(candidate_from_row).collect())
}

fn candidate_from_row(row: sqlx::sqlite::SqliteRow) -> Candidate {
    Candidate {
        upstream_id: row.get("upstream_id"),
        key_id: row.get("key_id"),
        key_name: row.get("key_name"),
        upstream_name: row.get("upstream_name"),
        base_url: row.get("base_url"),
        api_key: row.get("api_key"),
        resolved_model: row.get("resolved_model"),
        upstream_priority: row.get("upstream_priority"),
        model_priority: row.get("model_priority"),
        key_priority: row.get("key_priority"),
    }
}

pub async fn insert_request_audit(
    pool: &SqlitePool,
    audit: &RequestAudit,
    attempts: &[AttemptAudit],
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        r#"
        INSERT INTO request_audits (
            completed_at,
            completed_date,
            duration_ms,
            client_id,
            client_name,
            client_token_id,
            client_token_name,
            client_key_hash,
            client_ip,
            client_ip_source,
            cf_ray,
            cf_country,
            method,
            path,
            route_kind,
            has_query,
            query_hash,
            model,
            stream,
            content_type,
            request_body_bytes,
            user_agent_hash,
            upstream_id,
            upstream_name,
            upstream_key_id,
            upstream_key_name,
            status,
            outcome,
            error_class,
            error_message,
            attempts,
            input_tokens,
            output_tokens,
            total_tokens
        )
        VALUES (?, date(?, 'unixepoch', 'localtime'), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
        "#,
    )
    .bind(audit.completed_at)
    .bind(audit.completed_at)
    .bind(audit.duration_ms)
    .bind(audit.client_id)
    .bind(audit.client_name.as_deref())
    .bind(audit.client_token_id)
    .bind(audit.client_token_name.as_deref())
    .bind(audit.client_key_hash.as_deref())
    .bind(audit.client_ip.as_deref())
    .bind(audit.client_ip_source.as_deref())
    .bind(audit.cf_ray.as_deref())
    .bind(audit.cf_country.as_deref())
    .bind(audit.method.as_str())
    .bind(audit.path.as_str())
    .bind(audit.route_kind.as_str())
    .bind(audit.has_query)
    .bind(audit.query_hash.as_deref())
    .bind(audit.model.as_deref())
    .bind(audit.stream)
    .bind(audit.content_type.as_deref())
    .bind(audit.request_body_bytes)
    .bind(audit.user_agent_hash.as_deref())
    .bind(audit.upstream_id)
    .bind(audit.upstream_name.as_deref())
    .bind(audit.upstream_key_id)
    .bind(audit.upstream_key_name.as_deref())
    .bind(audit.status)
    .bind(audit.outcome.as_str())
    .bind(audit.error_class.as_deref())
    .bind(audit.error_message.as_deref().map(truncate_error))
    .bind(audit.attempts)
    .bind(audit.input_tokens)
    .bind(audit.output_tokens)
    .bind(audit.total_tokens)
    .execute(pool)
    .await?;

    let request_audit_id = result.last_insert_rowid();
    for attempt in attempts {
        sqlx::query(
            r#"
            INSERT INTO upstream_attempt_audits (
                request_audit_id,
                attempt_index,
                upstream_id,
                upstream_name,
                upstream_key_id,
                upstream_key_name,
                status,
                outcome,
                retriable,
                duration_ms,
                retry_after_secs,
                disabled_until,
                error_class,
                error_message
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
            "#,
        )
        .bind(request_audit_id)
        .bind(attempt.attempt_index)
        .bind(attempt.upstream_id)
        .bind(attempt.upstream_name.as_str())
        .bind(attempt.upstream_key_id)
        .bind(attempt.upstream_key_name.as_str())
        .bind(attempt.status)
        .bind(attempt.outcome.as_str())
        .bind(attempt.retriable)
        .bind(attempt.duration_ms)
        .bind(attempt.retry_after_secs)
        .bind(attempt.disabled_until)
        .bind(attempt.error_class.as_deref())
        .bind(attempt.error_message.as_deref().map(truncate_error))
        .execute(pool)
        .await?;
    }

    Ok(request_audit_id)
}

pub async fn update_request_audit_usage(
    pool: &SqlitePool,
    request_audit_id: i64,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE request_audits
        SET
            input_tokens = COALESCE(?, input_tokens),
            output_tokens = COALESCE(?, output_tokens),
            total_tokens = COALESCE(?, total_tokens)
        WHERE id = ?;
        "#,
    )
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(total_tokens)
    .bind(request_audit_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_key_success(pool: &SqlitePool, key_id: i64) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE upstream_keys
        SET
            disabled_until = NULL,
            consecutive_failures = 0,
            last_status = NULL,
            last_error = NULL,
            last_used_at = unixepoch(),
            updated_at = unixepoch()
        WHERE id = ?;
        "#,
    )
    .bind(key_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_key_failure(
    pool: &SqlitePool,
    key_id: i64,
    disabled_until: i64,
    status: Option<u16>,
    error: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE upstream_keys
        SET
            disabled_until = ?,
            consecutive_failures = consecutive_failures + 1,
            last_status = ?,
            last_error = ?,
            last_used_at = unixepoch(),
            updated_at = unixepoch()
        WHERE id = ?;
        "#,
    )
    .bind(disabled_until)
    .bind(status.map(i64::from))
    .bind(truncate_error(error))
    .bind(key_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_key_enabled(pool: &SqlitePool, key_id: i64, enabled: bool) -> anyhow::Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE upstream_keys
        SET enabled = ?, updated_at = unixepoch()
        WHERE id = ?;
        "#,
    )
    .bind(enabled)
    .bind(key_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("upstream key not found: {key_id}");
    }
    Ok(())
}

pub async fn reorder_upstream_keys(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<()> {
    reorder_priorities(pool, "upstream_keys", ids).await
}

pub async fn reorder_client_model_routes(pool: &SqlitePool, ids: &[i64]) -> anyhow::Result<()> {
    reorder_priorities(pool, "client_model_routes", ids).await
}

pub async fn set_model_alias_enabled(
    pool: &SqlitePool,
    public_model: &str,
    enabled: bool,
) -> anyhow::Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE model_aliases
        SET enabled = ?, updated_at = unixepoch()
        WHERE public_model = ?;
        "#,
    )
    .bind(enabled)
    .bind(public_model)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("model alias not found: {public_model}");
    }
    Ok(())
}

pub async fn delete_client(pool: &SqlitePool, client_id: i64) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM client_model_routes WHERE client_id = ?;")
        .bind(client_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM client_tokens WHERE client_id = ?;")
        .bind(client_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM clients WHERE id = ?;")
        .bind(client_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        bail!("client not found: {client_id}");
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete_client_token(pool: &SqlitePool, token_id: i64) -> anyhow::Result<()> {
    let result = sqlx::query("DELETE FROM client_tokens WHERE id = ?;")
        .bind(token_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("client token not found: {token_id}");
    }
    Ok(())
}

pub async fn delete_upstream(pool: &SqlitePool, upstream_id: i64) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        UPDATE request_audits
        SET upstream_key_id = NULL
        WHERE upstream_key_id IN (
            SELECT id FROM upstream_keys WHERE upstream_id = ?
        );
        "#,
    )
    .bind(upstream_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE request_audits SET upstream_id = NULL WHERE upstream_id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        DELETE FROM upstream_attempt_audits
        WHERE upstream_id = ?
           OR upstream_key_id IN (
               SELECT id FROM upstream_keys WHERE upstream_id = ?
           );
        "#,
    )
    .bind(upstream_id)
    .bind(upstream_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM client_model_routes
        WHERE upstream_model_id IN (
            SELECT id FROM upstream_models WHERE upstream_id = ?
        );
        "#,
    )
    .bind(upstream_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM model_alias_routes
        WHERE upstream_model_id IN (
            SELECT id FROM upstream_models WHERE upstream_id = ?
        );
        "#,
    )
    .bind(upstream_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        r#"
        DELETE FROM upstream_model_capabilities
        WHERE upstream_model_id IN (
            SELECT id FROM upstream_models WHERE upstream_id = ?
        );
        "#,
    )
    .bind(upstream_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM upstream_discovered_models WHERE upstream_id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM upstream_keys WHERE upstream_id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM upstream_models WHERE upstream_id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM upstreams WHERE id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        bail!("upstream not found: {upstream_id}");
    }
    tx.commit().await?;
    prune_unused_model_aliases(pool).await?;
    Ok(())
}

pub async fn delete_upstream_key(pool: &SqlitePool, key_id: i64) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE request_audits SET upstream_key_id = NULL WHERE upstream_key_id = ?;")
        .bind(key_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM upstream_attempt_audits WHERE upstream_key_id = ?;")
        .bind(key_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM upstream_keys WHERE id = ?;")
        .bind(key_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        bail!("upstream key not found: {key_id}");
    }
    tx.commit().await?;
    Ok(())
}

pub async fn delete_upstream_model(
    pool: &SqlitePool,
    upstream_model_id: i64,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM client_model_routes WHERE upstream_model_id = ?;")
        .bind(upstream_model_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM model_alias_routes WHERE upstream_model_id = ?;")
        .bind(upstream_model_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM upstream_model_capabilities WHERE upstream_model_id = ?;")
        .bind(upstream_model_id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM upstream_models WHERE id = ?;")
        .bind(upstream_model_id)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() == 0 {
        bail!("upstream model not found: {upstream_model_id}");
    }
    tx.commit().await?;
    prune_unused_model_aliases(pool).await?;
    Ok(())
}

pub async fn delete_model_alias_route(pool: &SqlitePool, route_id: i64) -> anyhow::Result<String> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        r#"
        SELECT public_model, upstream_model_id
        FROM model_alias_routes
        WHERE id = ?;
        "#,
    )
    .bind(route_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        bail!("model alias route not found: {route_id}");
    };
    let public_model: String = row.get("public_model");
    let upstream_model_id: i64 = row.get("upstream_model_id");

    sqlx::query(
        "DELETE FROM client_model_routes WHERE public_model = ? AND upstream_model_id = ?;",
    )
    .bind(&public_model)
    .bind(upstream_model_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM model_alias_routes WHERE id = ?;")
        .bind(route_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    prune_unused_model_aliases(pool).await?;
    Ok(public_model)
}

pub async fn delete_client_model_route(pool: &SqlitePool, route_id: i64) -> anyhow::Result<()> {
    let result = sqlx::query("DELETE FROM client_model_routes WHERE id = ?;")
        .bind(route_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        bail!("client model route not found: {route_id}");
    }
    prune_unused_model_aliases(pool).await?;
    Ok(())
}

pub async fn prune_unused_model_aliases(pool: &SqlitePool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        DELETE FROM model_aliases
        WHERE NOT EXISTS (
            SELECT 1
            FROM model_alias_routes r
            WHERE r.public_model = model_aliases.public_model
        )
          AND NOT EXISTS (
            SELECT 1
            FROM client_model_routes r
            WHERE r.public_model = model_aliases.public_model
        );
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn reorder_priorities(pool: &SqlitePool, table: &str, ids: &[i64]) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    match table {
        "upstream_keys" | "client_model_routes" => {}
        _ => bail!("unsupported reorder table"),
    }

    let mut tx = pool.begin().await?;
    for (index, id) in ids.iter().enumerate() {
        let priority = (index as i64 + 1) * 10;
        let sql =
            format!("UPDATE {table} SET priority = ?, updated_at = unixepoch() WHERE id = ?;");
        let result = sqlx::query(&sql)
            .bind(priority)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            bail!("row not found for reorder: {id}");
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn reset_key_health(pool: &SqlitePool, key_id: i64) -> anyhow::Result<()> {
    let result = sqlx::query(
        r#"
        UPDATE upstream_keys
        SET
            disabled_until = NULL,
            consecutive_failures = 0,
            last_status = NULL,
            last_error = NULL,
            updated_at = unixepoch()
        WHERE id = ?;
        "#,
    )
    .bind(key_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        bail!("upstream key not found: {key_id}");
    }
    Ok(())
}

pub async fn reset_all_key_health(pool: &SqlitePool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE upstream_keys
        SET
            disabled_until = NULL,
            consecutive_failures = 0,
            last_status = NULL,
            last_error = NULL,
            updated_at = unixepoch();
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn upstream_model_fetch_context(
    pool: &SqlitePool,
    upstream_id: i64,
) -> anyhow::Result<UpstreamModelFetchContext> {
    let row = sqlx::query(
        r#"
        SELECT
            u.id AS upstream_id,
            u.name AS upstream_name,
            u.base_url AS base_url,
            k.api_key AS api_key
        FROM upstreams u
        JOIN upstream_keys k ON k.upstream_id = u.id
        WHERE u.id = ?
          AND u.enabled = 1
          AND k.enabled = 1
          AND (k.disabled_until IS NULL OR k.disabled_until <= unixepoch())
        ORDER BY k.priority ASC, k.id ASC
        LIMIT 1;
        "#,
    )
    .bind(upstream_id)
    .fetch_optional(pool)
    .await?;
    let row = row.with_context(|| {
        format!("enabled upstream and enabled api key not found for upstream {upstream_id}")
    })?;
    Ok(UpstreamModelFetchContext {
        upstream_id: row.get("upstream_id"),
        upstream_name: row.get("upstream_name"),
        base_url: row.get("base_url"),
        api_key: row.get("api_key"),
    })
}

#[cfg(test)]
pub async fn replace_upstream_discovered_models(
    pool: &SqlitePool,
    upstream_id: i64,
    models: &[String],
) -> anyhow::Result<()> {
    let models = models
        .iter()
        .map(|model| DiscoveredModelInput {
            model: model.clone(),
            max_model_len: None,
        })
        .collect::<Vec<_>>();
    replace_upstream_discovered_model_items(pool, upstream_id, &models).await
}

pub async fn replace_upstream_discovered_model_items(
    pool: &SqlitePool,
    upstream_id: i64,
    models: &[DiscoveredModelInput],
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM upstream_discovered_models WHERE upstream_id = ?;")
        .bind(upstream_id)
        .execute(&mut *tx)
        .await?;
    for model in models {
        let model_name = model.model.trim();
        if model_name.is_empty() {
            continue;
        }
        let max_model_len = model.max_model_len;
        sqlx::query(
            r#"
            INSERT INTO upstream_discovered_models(upstream_id, model, max_model_len, fetched_at)
            VALUES (?, ?, ?, unixepoch())
            ON CONFLICT(upstream_id, model) DO UPDATE SET
                max_model_len = excluded.max_model_len,
                fetched_at = excluded.fetched_at;
            "#,
        )
        .bind(upstream_id)
        .bind(model_name)
        .bind(max_model_len)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn list_state(pool: &SqlitePool) -> anyhow::Result<StateSummary> {
    let client_rows = sqlx::query(
        r#"
        SELECT id, name, enabled
        FROM clients
        ORDER BY id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;
    let mut clients = Vec::with_capacity(client_rows.len());
    for row in client_rows {
        let id = row.get("id");
        clients.push(ClientSummary {
            id,
            name: row.get("name"),
            enabled: row.get::<i64, _>("enabled") == 1,
            tokens: client_tokens_for_client(pool, id).await?,
            routes: client_model_routes_for_client(pool, id).await?,
        });
    }

    let upstream_rows = sqlx::query(
        r#"
        SELECT id, name, base_url, priority, enabled
        FROM upstreams
        ORDER BY priority ASC, id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut upstreams = Vec::with_capacity(upstream_rows.len());
    for row in upstream_rows {
        let id = row.get("id");
        let model_rows = sqlx::query(
            r#"
            SELECT id, model, max_model_len, priority, enabled
            FROM upstream_models
            WHERE upstream_id = ?
            ORDER BY priority ASC, id ASC;
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await?;
        let mut models = Vec::with_capacity(model_rows.len());
        for model_row in model_rows {
            let model_id = model_row.get("id");
            let capability_rows = sqlx::query(
                r#"
                SELECT capability
                FROM upstream_model_capabilities
                WHERE upstream_model_id = ?
                ORDER BY capability ASC;
                "#,
            )
            .bind(model_id)
            .fetch_all(pool)
            .await?;
            models.push(UpstreamModelSummary {
                id: model_id,
                model: model_row.get("model"),
                capabilities: capability_rows
                    .into_iter()
                    .map(|capability| capability.get("capability"))
                    .collect(),
                max_model_len: model_row.get("max_model_len"),
                priority: model_row.get("priority"),
                enabled: model_row.get::<i64, _>("enabled") == 1,
            });
        }

        let key_rows = sqlx::query(
            r#"
            SELECT
                id,
                name,
                api_key,
                priority,
                enabled,
                disabled_until,
                consecutive_failures,
                last_status,
                last_error,
                last_used_at
            FROM upstream_keys
            WHERE upstream_id = ?
            ORDER BY priority ASC, id ASC;
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await?;

        let discovered_model_rows = sqlx::query(
            r#"
            SELECT
                model,
                max_model_len,
                fetched_at,
                strftime('%m-%d %H:%M:%S', fetched_at, 'unixepoch', 'localtime') AS fetched_at_text
            FROM upstream_discovered_models
            WHERE upstream_id = ?
            ORDER BY model ASC;
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await?;

        upstreams.push(UpstreamSummary {
            id,
            name: row.get("name"),
            base_url: row.get("base_url"),
            priority: row.get("priority"),
            enabled: row.get::<i64, _>("enabled") == 1,
            models,
            discovered_models: discovered_model_rows
                .into_iter()
                .map(|model| DiscoveredModelSummary {
                    model: model.get("model"),
                    max_model_len: model.get("max_model_len"),
                    fetched_at: model.get("fetched_at"),
                    fetched_at_text: model.get("fetched_at_text"),
                })
                .collect(),
            keys: key_rows
                .into_iter()
                .map(|key| KeySummary {
                    id: key.get("id"),
                    name: key.get("name"),
                    masked_api_key: mask_secret(key.get::<String, _>("api_key").as_str()),
                    priority: key.get("priority"),
                    enabled: key.get::<i64, _>("enabled") == 1,
                    disabled_until: key.get("disabled_until"),
                    consecutive_failures: key.get("consecutive_failures"),
                    last_status: key.get("last_status"),
                    last_error: key.get("last_error"),
                    last_used_at: key.get("last_used_at"),
                })
                .collect(),
        });
    }

    let model_alias_rows = sqlx::query(
        r#"
        SELECT id, public_model, target_type, enabled, created_at, updated_at
        FROM model_aliases
        ORDER BY id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;
    let mut model_aliases = Vec::with_capacity(model_alias_rows.len());
    for row in model_alias_rows {
        let public_model = row.get::<String, _>("public_model");
        model_aliases.push(ModelAliasSummary {
            id: row.get("id"),
            target_type: row.get("target_type"),
            enabled: row.get::<i64, _>("enabled") == 1,
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            routes: model_alias_routes_for_alias(pool, &public_model).await?,
            public_model,
        });
    }

    Ok(StateSummary {
        clients,
        upstreams,
        model_aliases,
    })
}

pub async fn list_admin_stats(pool: &SqlitePool) -> anyhow::Result<AdminStats> {
    let recent_rows = sqlx::query(
        r#"
        SELECT
            id,
            strftime('%m-%d %H:%M:%S', completed_at, 'unixepoch', 'localtime') AS completed_at_text,
            duration_ms,
            client_name,
            client_token_name,
            client_key_hash,
            client_ip,
            method,
            path,
            route_kind,
            model,
            upstream_name,
            upstream_key_id,
            status,
            outcome,
            attempts,
            input_tokens,
            output_tokens,
            total_tokens
        FROM request_audits
        WHERE path NOT LIKE '/admin%'
          AND path NOT IN ('/favicon.ico', '/robots.txt', '/site.webmanifest')
          AND path NOT LIKE '/apple-touch-icon%'
        ORDER BY completed_at DESC, id DESC
        LIMIT 12;
        "#,
    )
    .fetch_all(pool)
    .await?;
    let recent_requests = recent_rows
        .into_iter()
        .map(|row| RecentRequestSummary {
            id: row.get("id"),
            completed_at: row.get("completed_at_text"),
            duration_ms: row.get("duration_ms"),
            client_name: row.get("client_name"),
            client_token_name: row.get("client_token_name"),
            client_token_fingerprint: row
                .get::<Option<String>, _>("client_key_hash")
                .as_deref()
                .map(api_key_fingerprint),
            client_ip: row.get("client_ip"),
            method: row.get("method"),
            path: row.get("path"),
            route_kind: row.get("route_kind"),
            model: row.get("model"),
            upstream_name: row.get("upstream_name"),
            upstream_key_id: row.get("upstream_key_id"),
            status: row.get("status"),
            outcome: row.get("outcome"),
            attempts: row.get("attempts"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
        })
        .collect();

    let client_token_rows = sqlx::query(
        r#"
        SELECT
            t.id AS client_token_id,
            c.name AS client_name,
            t.name AS token_name,
            t.api_key_hash AS api_key_hash,
            t.enabled AS enabled,
            CASE
                WHEN MAX(a.completed_at) IS NULL THEN NULL
                ELSE strftime('%m-%d %H:%M:%S', MAX(a.completed_at), 'unixepoch', 'localtime')
            END AS last_used_at_text,
            COUNT(a.id) AS total_requests,
            COALESCE(SUM(CASE WHEN a.outcome = 'success' THEN 1 ELSE 0 END), 0) AS success_requests,
            COALESCE(SUM(CASE WHEN a.id IS NOT NULL AND a.outcome != 'success' THEN 1 ELSE 0 END), 0) AS failed_requests,
            COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms,
            COALESCE(SUM(a.input_tokens), 0) AS input_tokens,
            COALESCE(SUM(a.output_tokens), 0) AS output_tokens,
            COALESCE(SUM(a.total_tokens), 0) AS total_tokens
        FROM client_tokens t
        JOIN clients c ON c.id = t.client_id
        LEFT JOIN request_audits a ON a.client_token_id = t.id
        GROUP BY t.id
        ORDER BY c.id ASC, t.created_at ASC, t.id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;
    let client_token_stats = client_token_rows
        .into_iter()
        .map(|row| ClientTokenUsageStats {
            client_token_id: row.get("client_token_id"),
            client_name: row.get("client_name"),
            token_name: row.get("token_name"),
            api_key_fingerprint: api_key_fingerprint(&row.get::<String, _>("api_key_hash")),
            enabled: row.get::<i64, _>("enabled") == 1,
            last_used_at: row.get("last_used_at_text"),
            total_requests: row.get("total_requests"),
            success_requests: row.get("success_requests"),
            failed_requests: row.get("failed_requests"),
            total_duration_ms: row.get("total_duration_ms"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
        })
        .collect();

    let key_rows = sqlx::query(
        r#"
        SELECT
            k.id AS upstream_key_id,
            u.name AS upstream_name,
            k.api_key AS api_key,
            k.enabled AS enabled,
            k.priority AS priority,
            CASE
                WHEN k.disabled_until IS NULL THEN NULL
                ELSE strftime('%m-%d %H:%M:%S', k.disabled_until, 'unixepoch', 'localtime')
            END AS disabled_until_text,
            k.consecutive_failures AS consecutive_failures,
            k.last_status AS last_status,
            CASE
                WHEN k.last_used_at IS NULL THEN NULL
                ELSE strftime('%m-%d %H:%M:%S', k.last_used_at, 'unixepoch', 'localtime')
            END AS last_used_at_text,
            COUNT(a.id) AS total_requests,
            COALESCE(SUM(CASE WHEN a.outcome = 'success' THEN 1 ELSE 0 END), 0) AS success_requests,
            COALESCE(SUM(CASE WHEN a.id IS NOT NULL AND a.outcome != 'success' THEN 1 ELSE 0 END), 0) AS failed_requests,
            COALESCE(SUM(a.duration_ms), 0) AS total_duration_ms,
            COALESCE(SUM(a.input_tokens), 0) AS input_tokens,
            COALESCE(SUM(a.output_tokens), 0) AS output_tokens,
            COALESCE(SUM(a.total_tokens), 0) AS total_tokens
        FROM upstream_keys k
        JOIN upstreams u ON u.id = k.upstream_id
        LEFT JOIN request_audits a ON a.upstream_key_id = k.id
        GROUP BY k.id
        ORDER BY u.priority ASC, u.id ASC, k.priority ASC, k.id ASC;
        "#,
    )
    .fetch_all(pool)
    .await?;
    let key_stats = key_rows
        .into_iter()
        .map(|row| KeyUsageStats {
            upstream_key_id: row.get("upstream_key_id"),
            upstream_name: row.get("upstream_name"),
            masked_api_key: mask_secret(row.get::<String, _>("api_key").as_str()),
            enabled: row.get::<i64, _>("enabled") == 1,
            priority: row.get("priority"),
            disabled_until: row.get("disabled_until_text"),
            consecutive_failures: row.get("consecutive_failures"),
            last_status: row.get("last_status"),
            last_used_at: row.get("last_used_at_text"),
            total_requests: row.get("total_requests"),
            success_requests: row.get("success_requests"),
            failed_requests: row.get("failed_requests"),
            total_duration_ms: row.get("total_duration_ms"),
            input_tokens: row.get("input_tokens"),
            output_tokens: row.get("output_tokens"),
            total_tokens: row.get("total_tokens"),
        })
        .collect();

    let health = admin_health(pool).await?;

    Ok(AdminStats {
        recent_requests,
        client_token_stats,
        key_stats,
        health,
    })
}

pub async fn admin_health(pool: &SqlitePool) -> anyhow::Result<AdminHealthSummary> {
    let now = now_epoch();
    let key_row = sqlx::query(
        r#"
        SELECT
            COUNT(k.id) AS total_keys,
            COALESCE(SUM(CASE WHEN u.enabled = 1 AND k.enabled = 1 THEN 1 ELSE 0 END), 0) AS enabled_keys,
            COALESCE(SUM(CASE
                WHEN u.enabled = 1
                 AND k.enabled = 1
                 AND (k.disabled_until IS NULL OR k.disabled_until <= ?)
                THEN 1 ELSE 0 END), 0) AS ready_keys,
            COALESCE(SUM(CASE
                WHEN u.enabled = 1
                 AND k.enabled = 1
                 AND k.disabled_until IS NOT NULL
                 AND k.disabled_until > ?
                THEN 1 ELSE 0 END), 0) AS cached_keys,
            COALESCE(SUM(CASE WHEN u.enabled = 0 OR k.enabled = 0 THEN 1 ELSE 0 END), 0) AS disabled_keys
        FROM upstream_keys k
        JOIN upstreams u ON u.id = k.upstream_id;
        "#,
    )
    .bind(now)
    .bind(now)
    .fetch_one(pool)
    .await?;

    let recent_since = now - 600;
    let request_row = sqlx::query(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN status = 503 THEN 1 ELSE 0 END), 0) AS recent_503,
            COALESCE(SUM(CASE WHEN outcome = 'upstream_exhausted' THEN 1 ELSE 0 END), 0) AS recent_upstream_exhausted,
            COALESCE(SUM(CASE WHEN status >= 500 AND status < 600 THEN 1 ELSE 0 END), 0) AS recent_5xx,
            MAX(CASE WHEN outcome != 'success' OR status >= 500 THEN completed_at ELSE NULL END) AS last_failure_at
        FROM request_audits
        WHERE completed_at >= ?
          AND path LIKE '/v1/%';
        "#,
    )
    .bind(recent_since)
    .fetch_one(pool)
    .await?;

    let last_failure_epoch: Option<i64> = request_row.get("last_failure_at");
    let last_failure_at: Option<String> = if let Some(epoch) = last_failure_epoch {
        sqlx::query_scalar("SELECT strftime('%m-%d %H:%M:%S', ?, 'unixepoch', 'localtime');")
            .bind(epoch)
            .fetch_one(pool)
            .await?
    } else {
        None
    };

    Ok(AdminHealthSummary {
        total_keys: key_row.get("total_keys"),
        enabled_keys: key_row.get("enabled_keys"),
        ready_keys: key_row.get("ready_keys"),
        cached_keys: key_row.get("cached_keys"),
        disabled_keys: key_row.get("disabled_keys"),
        recent_503: request_row.get("recent_503"),
        recent_upstream_exhausted: request_row.get("recent_upstream_exhausted"),
        recent_5xx: request_row.get("recent_5xx"),
        last_failure_at,
    })
}

fn api_key_fingerprint(api_key_hash: &str) -> String {
    let short_hash: String = api_key_hash.chars().take(12).collect();
    format!("sha256:{short_hash}")
}

#[cfg(test)]
fn model_alias_summary_from_row(row: sqlx::sqlite::SqliteRow) -> ModelAliasSummary {
    ModelAliasSummary {
        id: row.get("id"),
        public_model: row.get("public_model"),
        target_type: row.get("target_type"),
        enabled: row.get::<i64, _>("enabled") == 1,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        routes: Vec::new(),
    }
}

async fn model_alias_routes_for_alias(
    pool: &SqlitePool,
    public_model: &str,
) -> anyhow::Result<Vec<ModelAliasRouteSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT
            r.id AS id,
            r.upstream_model_id AS upstream_model_id,
            u.name AS upstream_name,
            m.model AS upstream_model,
            r.priority AS priority,
            r.enabled AS enabled,
            COALESCE((
                SELECT group_concat(capability, ',')
                FROM (
                    SELECT capability
                    FROM upstream_model_capabilities
                    WHERE upstream_model_id = m.id
                    ORDER BY capability ASC
                )
            ), '') AS capabilities
        FROM model_alias_routes r
        JOIN upstream_models m ON m.id = r.upstream_model_id
        JOIN upstreams u ON u.id = m.upstream_id
        WHERE r.public_model = ?
        ORDER BY r.priority ASC, r.id ASC;
        "#,
    )
    .bind(public_model)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let capabilities = row
                .get::<String, _>("capabilities")
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect();
            ModelAliasRouteSummary {
                id: row.get("id"),
                upstream_model_id: row.get("upstream_model_id"),
                upstream_name: row.get("upstream_name"),
                upstream_model: row.get("upstream_model"),
                capabilities,
                priority: row.get("priority"),
                enabled: row.get::<i64, _>("enabled") == 1,
            }
        })
        .collect())
}

async fn client_tokens_for_client(
    pool: &SqlitePool,
    client_id: i64,
) -> anyhow::Result<Vec<ClientTokenSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            name,
            api_key_hash,
            api_key,
            enabled,
            strftime('%m-%d %H:%M:%S', created_at, 'unixepoch', 'localtime') AS created_at_text
        FROM client_tokens
        WHERE client_id = ?
        ORDER BY created_at ASC, id ASC;
        "#,
    )
    .bind(client_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| ClientTokenSummary {
            id: row.get("id"),
            name: row.get("name"),
            api_key_fingerprint: api_key_fingerprint(&row.get::<String, _>("api_key_hash")),
            api_key: row.get("api_key"),
            enabled: row.get::<i64, _>("enabled") == 1,
            created_at_text: row.get("created_at_text"),
        })
        .collect())
}

async fn client_model_routes_for_client(
    pool: &SqlitePool,
    client_id: i64,
) -> anyhow::Result<Vec<ClientModelRouteSummary>> {
    let rows = sqlx::query(
        r#"
        SELECT
            r.id AS id,
            r.public_model AS public_model,
            r.upstream_model_id AS upstream_model_id,
            u.name AS upstream_name,
            m.model AS upstream_model,
            r.priority AS priority,
            r.enabled AS enabled,
            COALESCE((
                SELECT group_concat(capability, ',')
                FROM (
                    SELECT capability
                    FROM upstream_model_capabilities
                    WHERE upstream_model_id = m.id
                    ORDER BY capability ASC
                )
            ), '') AS capabilities
        FROM client_model_routes r
        JOIN upstream_models m ON m.id = r.upstream_model_id
        JOIN upstreams u ON u.id = m.upstream_id
        WHERE r.client_id = ?
        ORDER BY r.public_model ASC, r.priority ASC, r.id ASC;
        "#,
    )
    .bind(client_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let capabilities = row
                .get::<String, _>("capabilities")
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect();
            ClientModelRouteSummary {
                id: row.get("id"),
                public_model: row.get("public_model"),
                upstream_model_id: row.get("upstream_model_id"),
                upstream_name: row.get("upstream_name"),
                upstream_model: row.get("upstream_model"),
                capabilities,
                priority: row.get("priority"),
                enabled: row.get::<i64, _>("enabled") == 1,
            }
        })
        .collect())
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs() as i64
}

pub fn mask_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 8 {
        return "****".to_string();
    }
    let start: String = chars.iter().take(4).collect();
    let end: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{start}...{end}")
}

pub fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

fn validate_base_url(base_url: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(base_url).context("base_url must be an absolute URL")?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => bail!("base_url scheme must be http or https, got {scheme}"),
    }
}

fn truncate_error(error: &str) -> String {
    const MAX_LEN: usize = 500;
    if error.len() <= MAX_LEN {
        return error.to_string();
    }
    error.chars().take(MAX_LEN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let first_model =
            upsert_upstream_model_by_id(&pool, first, "first-llm", 10, true, &["llm"])
                .await
                .unwrap();
        let second = add_provider(&pool, "second", 20, "second-key", 10).await;
        let second_model =
            upsert_upstream_model_by_id(&pool, second, "second-llm", 10, true, &["llm"])
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

        let first_model = upsert_upstream_model_for_alias(
            &pool,
            "alias-provider",
            "shared-model",
            "llm-model",
            true,
        )
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
        let model =
            upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
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
    async fn unknown_model_falls_back_to_active_key_order_without_rewrite() {
        let (pool, path) = test_pool("unknown-model").await;
        add_provider(&pool, "first", 10, "first-key", 10).await;
        add_provider(&pool, "second", 20, "second-key", 10).await;

        let candidates = candidates_for_client_request_model(&pool, None, Some("unknown-model"))
            .await
            .unwrap();

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].upstream_name, "first");
        assert_eq!(candidates[0].resolved_model, None);
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
        let first_model =
            upsert_upstream_model_by_id(&pool, first, "first-llm", 10, true, &["llm"])
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

        let restricted =
            candidates_for_client_request_model(&pool, Some(client_id), Some("llm-model"))
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
        let model =
            upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
                .await
                .unwrap();
        let client_id = add_client(&pool, "restricted").await;
        upsert_client_model_route(&pool, client_id, "llm-model", model, 100, false)
            .await
            .unwrap();

        let candidates =
            candidates_for_client_request_model(&pool, Some(client_id), Some("llm-model"))
                .await
                .unwrap();

        assert!(candidates.is_empty());
        close_and_remove(pool, path).await;
    }

    #[tokio::test]
    async fn hard_delete_upstream_model_removes_alias_and_client_routes() {
        let (pool, path) = test_pool("delete-upstream-model").await;
        let upstream = add_provider(&pool, "provider", 10, "key", 10).await;
        let model =
            upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
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
        let alias_route_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM model_alias_routes WHERE upstream_model_id = ?;",
        )
        .bind(model)
        .fetch_one(&pool)
        .await
        .unwrap();
        let client_route_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM client_model_routes WHERE upstream_model_id = ?;",
        )
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
        let model =
            upsert_upstream_model_by_id(&pool, upstream, "provider-llm", 10, true, &["llm"])
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
        let model = upsert_upstream_model_by_id(
            &pool,
            upstream,
            "provider-multi",
            10,
            true,
            &["multimodal"],
        )
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
        };

        let id = insert_request_audit(&pool, &request_audit, &[attempt])
            .await
            .unwrap();
        let row = sqlx::query(
            r#"
            SELECT completed_date, attempts,
                (SELECT count(*) FROM upstream_attempt_audits WHERE request_audit_id = ?) AS attempt_rows
            FROM request_audits
            WHERE id = ?;
            "#,
        )
        .bind(id)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(row.get::<String, _>("completed_date").len(), 10);
        assert_eq!(row.get::<i64, _>("attempts"), 1);
        assert_eq!(row.get::<i64, _>("attempt_rows"), 1);
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
}
