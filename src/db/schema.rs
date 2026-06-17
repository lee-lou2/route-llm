use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{path::Path, str::FromStr, time::Duration};

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    ensure_database_parent(database_url)?;

    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;
    migrate(&pool).await?;
    Ok(pool)
}

async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS clients (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            api_key_hash TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )
    .execute(pool)
    .await?;
    ensure_column(pool, "clients", "api_key", "api_key TEXT").await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS client_tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            client_id INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            api_key_hash TEXT NOT NULL UNIQUE,
            api_key TEXT,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(client_id, name)
        );
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO client_tokens(client_id, name, api_key_hash, api_key, enabled, created_at, updated_at)
        SELECT id, '기본 토큰', api_key_hash, api_key, enabled, created_at, updated_at
        FROM clients
        WHERE api_key_hash IS NOT NULL AND api_key_hash != ''
        ON CONFLICT(api_key_hash) DO NOTHING;
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstreams (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            base_url TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 100,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstream_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            upstream_id INTEGER NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            api_key TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 100,
            enabled INTEGER NOT NULL DEFAULT 1,
            disabled_until INTEGER,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            last_status INTEGER,
            last_error TEXT,
            last_used_at INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(upstream_id, name)
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS model_aliases (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            public_model TEXT NOT NULL UNIQUE,
            target_type TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstream_models (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            upstream_id INTEGER NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            max_model_len INTEGER,
            priority INTEGER NOT NULL DEFAULT 100,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(upstream_id, model)
        );
        "#,
    )
    .execute(pool)
    .await?;
    ensure_column(
        pool,
        "upstream_models",
        "max_model_len",
        "max_model_len INTEGER",
    )
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstream_model_capabilities (
            upstream_model_id INTEGER NOT NULL REFERENCES upstream_models(id) ON DELETE CASCADE,
            capability TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(upstream_model_id, capability)
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstream_discovered_models (
            upstream_id INTEGER NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
            model TEXT NOT NULL,
            max_model_len INTEGER,
            fetched_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(upstream_id, model)
        );
        "#,
    )
    .execute(pool)
    .await?;
    ensure_column(
        pool,
        "upstream_discovered_models",
        "max_model_len",
        "max_model_len INTEGER",
    )
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS client_model_routes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            client_id INTEGER NOT NULL REFERENCES clients(id) ON DELETE CASCADE,
            public_model TEXT NOT NULL REFERENCES model_aliases(public_model) ON DELETE CASCADE,
            upstream_model_id INTEGER NOT NULL REFERENCES upstream_models(id) ON DELETE CASCADE,
            priority INTEGER NOT NULL DEFAULT 100,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(client_id, public_model, upstream_model_id)
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS model_alias_routes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            public_model TEXT NOT NULL REFERENCES model_aliases(public_model) ON DELETE CASCADE,
            upstream_model_id INTEGER NOT NULL REFERENCES upstream_models(id) ON DELETE CASCADE,
            priority INTEGER NOT NULL DEFAULT 100,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(public_model, upstream_model_id)
        );
        "#,
    )
    .execute(pool)
    .await?;

    seed_default_model_aliases(pool).await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS request_audits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            completed_at INTEGER NOT NULL,
            completed_date TEXT NOT NULL,
            duration_ms INTEGER NOT NULL,
            client_id INTEGER REFERENCES clients(id) ON DELETE SET NULL,
            client_name TEXT,
            client_token_id INTEGER REFERENCES client_tokens(id) ON DELETE SET NULL,
            client_token_name TEXT,
            client_key_hash TEXT,
            client_ip TEXT,
            client_ip_source TEXT,
            cf_ray TEXT,
            cf_country TEXT,
            method TEXT NOT NULL,
            path TEXT NOT NULL,
            route_kind TEXT NOT NULL,
            has_query INTEGER NOT NULL DEFAULT 0,
            query_hash TEXT,
            model TEXT,
            stream INTEGER,
            content_type TEXT,
            request_body_bytes INTEGER,
            user_agent_hash TEXT,
            upstream_id INTEGER REFERENCES upstreams(id) ON DELETE SET NULL,
            upstream_name TEXT,
            upstream_key_id INTEGER REFERENCES upstream_keys(id) ON DELETE SET NULL,
            upstream_key_name TEXT,
            status INTEGER,
            outcome TEXT NOT NULL,
            error_class TEXT,
            error_message TEXT,
            attempts INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER,
            output_tokens INTEGER,
            total_tokens INTEGER
        );
        "#,
    )
    .execute(pool)
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "client_token_id",
        "client_token_id INTEGER REFERENCES client_tokens(id) ON DELETE SET NULL",
    )
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "client_token_name",
        "client_token_name TEXT",
    )
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "completed_date",
        "completed_date TEXT",
    )
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "input_tokens",
        "input_tokens INTEGER",
    )
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "output_tokens",
        "output_tokens INTEGER",
    )
    .await?;
    ensure_column(
        pool,
        "request_audits",
        "total_tokens",
        "total_tokens INTEGER",
    )
    .await?;
    sqlx::query(
        r#"
        UPDATE request_audits
        SET completed_date = date(completed_at, 'unixepoch', 'localtime')
        WHERE completed_date IS NULL;
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS upstream_attempt_audits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            request_audit_id INTEGER NOT NULL REFERENCES request_audits(id) ON DELETE CASCADE,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            attempt_index INTEGER NOT NULL,
            upstream_id INTEGER NOT NULL REFERENCES upstreams(id) ON DELETE CASCADE,
            upstream_name TEXT NOT NULL,
            upstream_key_id INTEGER NOT NULL REFERENCES upstream_keys(id) ON DELETE CASCADE,
            upstream_key_name TEXT NOT NULL,
            status INTEGER,
            outcome TEXT NOT NULL,
            retriable INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL,
            retry_after_secs INTEGER,
            disabled_until INTEGER,
            error_class TEXT,
            error_message TEXT
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_clients_auth
            ON clients(api_key_hash, enabled);
        CREATE INDEX IF NOT EXISTS idx_client_tokens_auth
            ON client_tokens(api_key_hash, enabled);
        CREATE INDEX IF NOT EXISTS idx_client_tokens_client
            ON client_tokens(client_id, enabled, created_at);
        CREATE INDEX IF NOT EXISTS idx_upstream_keys_routing
            ON upstream_keys(enabled, disabled_until, priority);
        CREATE INDEX IF NOT EXISTS idx_model_aliases_public_enabled
            ON model_aliases(public_model, enabled);
        CREATE INDEX IF NOT EXISTS idx_upstream_models_routing
            ON upstream_models(upstream_id, enabled, priority);
        CREATE INDEX IF NOT EXISTS idx_upstream_model_capabilities_capability
            ON upstream_model_capabilities(capability, upstream_model_id);
        CREATE INDEX IF NOT EXISTS idx_upstream_discovered_models_upstream
            ON upstream_discovered_models(upstream_id, model);
        CREATE INDEX IF NOT EXISTS idx_client_model_routes_lookup
            ON client_model_routes(client_id, public_model, enabled, priority);
        CREATE INDEX IF NOT EXISTS idx_client_model_routes_model
            ON client_model_routes(upstream_model_id);
        CREATE INDEX IF NOT EXISTS idx_model_alias_routes_lookup
            ON model_alias_routes(public_model, enabled, priority);
        CREATE INDEX IF NOT EXISTS idx_model_alias_routes_model
            ON model_alias_routes(upstream_model_id);
        CREATE INDEX IF NOT EXISTS idx_request_audits_completed
            ON request_audits(completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_date_completed
            ON request_audits(completed_date, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_route_completed
            ON request_audits(route_kind, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_client_completed
            ON request_audits(client_id, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_client_token_completed
            ON request_audits(client_token_id, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_status_completed
            ON request_audits(status, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_model_completed
            ON request_audits(model, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_request_audits_ip_completed
            ON request_audits(client_ip, completed_at DESC);
        CREATE INDEX IF NOT EXISTS idx_attempt_audits_request
            ON upstream_attempt_audits(request_audit_id, attempt_index);
        CREATE INDEX IF NOT EXISTS idx_attempt_audits_key_completed
            ON upstream_attempt_audits(upstream_key_id, created_at DESC);
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn seed_default_model_aliases(pool: &SqlitePool) -> anyhow::Result<()> {
    for (public_model, target_type) in [("llm-model", "llm"), ("multimodal-model", "multimodal")] {
        sqlx::query(
            r#"
            INSERT INTO model_aliases(public_model, target_type, enabled)
            VALUES (?, ?, 1)
            ON CONFLICT(public_model) DO NOTHING;
            "#,
        )
        .bind(public_model)
        .bind(target_type)
        .execute(pool)
        .await?;
    }
    Ok(())
}

async fn ensure_column(
    pool: &SqlitePool,
    table: &'static str,
    column: &'static str,
    definition: &'static str,
) -> anyhow::Result<()> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table});"))
        .fetch_all(pool)
        .await?;
    let exists = rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column);
    if !exists {
        sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {definition};"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

fn ensure_database_parent(database_url: &str) -> anyhow::Result<()> {
    let Some(path) = sqlite_file_path(database_url) else {
        return Ok(());
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn sqlite_file_path(database_url: &str) -> Option<&Path> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))?;
    if path == ":memory:" || path.is_empty() {
        return None;
    }
    Some(Path::new(path))
}
