use super::*;
use anyhow::{Context, bail};
use sqlx::{Row, SqlitePool};

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
