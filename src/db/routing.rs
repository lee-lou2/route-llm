use super::*;
use sqlx::{Row, SqlitePool};

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
    Ok(exact_candidates)
}

pub async fn is_registered_request_model(
    pool: &SqlitePool,
    requested_model: &str,
) -> anyhow::Result<bool> {
    if resolve_model_alias(pool, requested_model).await?.is_some() {
        return Ok(true);
    }
    registered_upstream_model_exists(pool, requested_model).await
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

async fn registered_upstream_model_exists(pool: &SqlitePool, model: &str) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM upstream_models
        WHERE model = ?;
        "#,
    )
    .bind(model)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
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
