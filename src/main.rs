mod admin_ui;
mod assets;
mod cli;
mod db;
mod http_proxy;
mod server;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Command};
use db::connect;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "route_llm=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let pool = connect(&cli.database_url)
        .await
        .with_context(|| format!("failed to open database {}", cli.database_url))?;

    match cli.command {
        Command::Serve(args) => server::serve(pool, args).await,
        Command::AddClient(args) => {
            let id = db::upsert_client(&pool, &args.name, &args.api_key, args.enabled).await?;
            println!("client saved: id={id}, name={}", args.name);
            Ok(())
        }
        Command::AddUpstream(args) => {
            let id = db::upsert_upstream(
                &pool,
                &args.name,
                &args.base_url,
                args.priority,
                args.enabled,
            )
            .await?;
            println!("upstream saved: id={id}, name={}", args.name);
            Ok(())
        }
        Command::AddKey(args) => {
            let id = db::upsert_upstream_key(
                &pool,
                &args.upstream,
                &args.name,
                &args.api_key,
                args.priority,
                args.enabled,
            )
            .await?;
            println!("upstream key saved: id={id}, name={}", args.name);
            Ok(())
        }
        Command::AddModelAlias(args) => {
            let id =
                db::upsert_model_alias(&pool, &args.public_model, &args.target_type, args.enabled)
                    .await?;
            println!(
                "model alias saved: id={id}, public_model={}, target_type={}",
                args.public_model, args.target_type
            );
            Ok(())
        }
        Command::AddUpstreamModel(args) => {
            let id = if args.max_model_len.is_some() {
                db::upsert_upstream_model_with_max_model_len(
                    &pool,
                    &args.upstream,
                    &args.model,
                    args.priority,
                    args.enabled,
                    &args.capability,
                    args.max_model_len,
                )
                .await?
            } else {
                db::upsert_upstream_model(
                    &pool,
                    &args.upstream,
                    &args.model,
                    args.priority,
                    args.enabled,
                    &args.capability,
                )
                .await?
            };
            println!(
                "upstream model saved: id={id}, upstream={}, model={}, capabilities={}, max_model_len={}",
                args.upstream,
                args.model,
                args.capability.join(","),
                args.max_model_len
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "default-or-known".to_string())
            );
            Ok(())
        }
        Command::DisableKey(args) => {
            db::set_key_enabled(&pool, args.id, false).await?;
            println!("upstream key disabled: id={}", args.id);
            Ok(())
        }
        Command::EnableKey(args) => {
            db::set_key_enabled(&pool, args.id, true).await?;
            println!("upstream key enabled: id={}", args.id);
            Ok(())
        }
        Command::DisableModelAlias(args) => {
            db::set_model_alias_enabled(&pool, &args.public_model, false).await?;
            println!("model alias disabled: public_model={}", args.public_model);
            Ok(())
        }
        Command::EnableModelAlias(args) => {
            db::set_model_alias_enabled(&pool, &args.public_model, true).await?;
            println!("model alias enabled: public_model={}", args.public_model);
            Ok(())
        }
        Command::ResetKey(args) => {
            db::reset_key_health(&pool, args.id).await?;
            println!("upstream key health reset: id={}", args.id);
            Ok(())
        }
        Command::List => {
            let summary = db::list_state(&pool).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
            Ok(())
        }
    }
}
