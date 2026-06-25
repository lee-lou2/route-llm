use super::*;
use sqlx::SqlitePool;

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
                error_message,
                upstream_content_type,
                upstream_body_bytes,
                upstream_body_hash,
                upstream_body_kind
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);
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
        .bind(attempt.upstream_content_type.as_deref())
        .bind(attempt.upstream_body_bytes)
        .bind(attempt.upstream_body_hash.as_deref())
        .bind(attempt.upstream_body_kind.as_deref())
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

pub async fn update_request_audit_stream_error(
    pool: &SqlitePool,
    request_audit_id: i64,
    error_class: &str,
    error_message: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE request_audits
        SET
            outcome = 'response_stream_error',
            error_class = ?,
            error_message = ?
        WHERE id = ?;
        "#,
    )
    .bind(error_class)
    .bind(truncate_error(error_message))
    .bind(request_audit_id)
    .execute(pool)
    .await?;
    Ok(())
}
