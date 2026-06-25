use super::*;
use sqlx::{Row, SqlitePool};

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
