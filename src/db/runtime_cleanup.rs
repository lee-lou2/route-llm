use super::*;
use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeCleanupSummary {
    pub request_audits_deleted: u64,
    pub response_states_deleted: u64,
}

pub async fn cleanup_runtime_state(
    pool: &SqlitePool,
    audit_retention_days: i64,
    response_state_retention_days: i64,
) -> anyhow::Result<RuntimeCleanupSummary> {
    let now = now_epoch();
    cleanup_runtime_state_for_cutoffs(
        pool,
        retention_cutoff(now, audit_retention_days),
        retention_cutoff(now, response_state_retention_days),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn cleanup_runtime_state_before(
    pool: &SqlitePool,
    request_audit_cutoff: i64,
    response_state_cutoff: i64,
) -> anyhow::Result<RuntimeCleanupSummary> {
    cleanup_runtime_state_for_cutoffs(
        pool,
        Some(request_audit_cutoff),
        Some(response_state_cutoff),
    )
    .await
}

async fn cleanup_runtime_state_for_cutoffs(
    pool: &SqlitePool,
    request_audit_cutoff: Option<i64>,
    response_state_cutoff: Option<i64>,
) -> anyhow::Result<RuntimeCleanupSummary> {
    let request_audits_deleted = match request_audit_cutoff {
        Some(cutoff) => delete_request_audits_before(pool, cutoff).await?,
        None => 0,
    };
    let response_states_deleted = match response_state_cutoff {
        Some(cutoff) => delete_response_states_before(pool, cutoff).await?,
        None => 0,
    };
    Ok(RuntimeCleanupSummary {
        request_audits_deleted,
        response_states_deleted,
    })
}

fn retention_cutoff(now: i64, retention_days: i64) -> Option<i64> {
    if retention_days <= 0 {
        return None;
    }
    Some(now.saturating_sub(retention_days.saturating_mul(86_400)))
}

async fn delete_request_audits_before(pool: &SqlitePool, cutoff: i64) -> anyhow::Result<u64> {
    let result = sqlx::query("DELETE FROM request_audits WHERE completed_at < ?;")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

async fn delete_response_states_before(pool: &SqlitePool, cutoff: i64) -> anyhow::Result<u64> {
    let result = sqlx::query("DELETE FROM response_states WHERE created_at < ?;")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
