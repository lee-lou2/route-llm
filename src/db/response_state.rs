use super::*;
use sqlx::{Row, SqlitePool};

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
