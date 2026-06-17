use crate::{admin_ui, assets, cli::ServeArgs, db, http_proxy};
use anyhow::Context;
use axum::{Router, routing::get};
use reqwest::Client;
use sqlx::SqlitePool;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub client: Client,
    pub config: ProxyConfig,
}

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub public_prefix: String,
    pub transient_failure_ttl_secs: i64,
    pub auth_failure_ttl_secs: i64,
    pub max_body_bytes: usize,
    pub admin: AdminConfig,
}

#[derive(Debug, Clone)]
pub struct AdminConfig {
    pub password_hash: Option<String>,
    pub session_token: Option<String>,
    pub site_name: String,
    pub site_description: String,
    pub public_base_url: Option<String>,
}

pub async fn serve(pool: SqlitePool, args: ServeArgs) -> anyhow::Result<()> {
    let addr: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("invalid bind address: {}", args.bind))?;
    let client = Client::builder()
        .timeout(Duration::from_secs(args.request_timeout_secs))
        .build()?;
    let admin = admin_config(&args);
    let state = Arc::new(AppState {
        pool,
        client,
        config: ProxyConfig {
            public_prefix: normalize_prefix(&args.public_prefix),
            transient_failure_ttl_secs: args.transient_failure_ttl_secs,
            auth_failure_ttl_secs: args.auth_failure_ttl_secs,
            max_body_bytes: args.max_body_bytes,
            admin,
        },
    });

    let app = Router::new()
        .route("/health", get(http_proxy::health))
        .merge(assets::router())
        .nest("/admin", admin_ui::router())
        .fallback(http_proxy::proxy)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("route-llm listening on http://{}", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return String::new();
    }
    format!("/{}", trimmed.trim_matches('/'))
}

fn admin_config(args: &ServeArgs) -> AdminConfig {
    let site_name = non_empty_or(&args.admin_site_name, "Route LLM");
    let site_description = non_empty_or(
        &args.admin_site_description,
        "Local OpenAI-compatible routing proxy",
    );
    let public_base_url = args
        .public_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_string());

    let password = args
        .admin_password
        .as_deref()
        .map(str::trim)
        .filter(|password| !password.is_empty());
    let Some(password) = password else {
        return AdminConfig {
            password_hash: None,
            session_token: None,
            site_name,
            site_description,
            public_base_url,
        };
    };
    let password_hash = db::hash_secret(password);
    let secret = args
        .admin_session_secret
        .as_deref()
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
        .unwrap_or(&password_hash);
    let session_token =
        db::hash_secret(&format!("route-llm-admin-session:{secret}:{password_hash}"));
    AdminConfig {
        password_hash: Some(password_hash),
        session_token: Some(session_token),
        site_name,
        site_description,
        public_base_url,
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}
