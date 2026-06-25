use super::*;
use anyhow::{Context, bail};
use sqlx::{Row, SqlitePool};
use std::{fs::File, io::Read};

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
