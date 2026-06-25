use super::*;
use sqlx::{Row, SqlitePool};

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
