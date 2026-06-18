use crate::{admin_ui, assets, cli::ServeArgs, db, http_proxy};
use anyhow::Context;
use axum::{Router, routing::get};
use reqwest::Client;
use sqlx::SqlitePool;
use std::{borrow::Cow, net::SocketAddr, sync::Arc, time::Duration};
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
    admin_config_with_legacy_env(args, |key| std::env::var(key).ok())
}

fn admin_config_with_legacy_env<F>(args: &ServeArgs, legacy_env: F) -> AdminConfig
where
    F: Fn(&str) -> Option<String>,
{
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

    let password = config_value_or_legacy_env(
        &args.admin_password,
        "API_ROUTER_ADMIN_PASSWORD",
        &legacy_env,
    );
    let Some(password) = password else {
        return AdminConfig {
            password_hash: None,
            session_token: None,
            site_name,
            site_description,
            public_base_url,
        };
    };
    let password_hash = db::hash_secret(password.as_ref());
    let secret = config_value_or_legacy_env(
        &args.admin_session_secret,
        "API_ROUTER_ADMIN_SESSION_SECRET",
        &legacy_env,
    );
    let secret = secret
        .as_ref()
        .map(|secret| secret.as_ref())
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

fn config_value_or_legacy_env<'a, F>(
    value: &'a Option<String>,
    legacy_key: &str,
    legacy_env: &F,
) -> Option<Cow<'a, str>>
where
    F: Fn(&str) -> Option<String>,
{
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Cow::Borrowed)
        .or_else(|| {
            legacy_env(legacy_key)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(Cow::Owned)
        })
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_args() -> ServeArgs {
        ServeArgs {
            bind: "127.0.0.1:8080".to_string(),
            public_prefix: "/v1".to_string(),
            request_timeout_secs: 300,
            transient_failure_ttl_secs: 300,
            auth_failure_ttl_secs: 3600,
            max_body_bytes: 32 * 1024 * 1024,
            admin_password: None,
            admin_session_secret: None,
            admin_site_name: "Route LLM".to_string(),
            admin_site_description: "Local OpenAI-compatible routing proxy".to_string(),
            public_base_url: None,
        }
    }

    #[test]
    fn admin_config_uses_legacy_password_fallback() {
        let config = admin_config_with_legacy_env(&serve_args(), |key| match key {
            "API_ROUTER_ADMIN_PASSWORD" => Some(" legacy-password ".to_string()),
            _ => None,
        });

        assert_eq!(
            config.password_hash,
            Some(db::hash_secret("legacy-password"))
        );
        assert!(config.session_token.is_some());
    }

    #[test]
    fn admin_config_prefers_canonical_password() {
        let mut args = serve_args();
        args.admin_password = Some("canonical-password".to_string());

        let config = admin_config_with_legacy_env(&args, |key| match key {
            "API_ROUTER_ADMIN_PASSWORD" => Some("legacy-password".to_string()),
            _ => None,
        });

        assert_eq!(
            config.password_hash,
            Some(db::hash_secret("canonical-password"))
        );
    }

    #[test]
    fn admin_config_uses_legacy_session_secret_fallback() {
        let mut args = serve_args();
        args.admin_password = Some("admin-password".to_string());

        let config = admin_config_with_legacy_env(&args, |key| match key {
            "API_ROUTER_ADMIN_SESSION_SECRET" => Some(" legacy-session-secret ".to_string()),
            _ => None,
        });

        let password_hash = db::hash_secret("admin-password");
        assert_eq!(
            config.session_token,
            Some(db::hash_secret(&format!(
                "route-llm-admin-session:{}:{}",
                "legacy-session-secret", password_hash
            )))
        );
    }
}
