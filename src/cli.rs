use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "route-llm")]
#[command(about = "OpenAI-compatible API key routing proxy")]
pub struct Cli {
    #[arg(
        long = "database-url",
        alias = "database",
        global = true,
        env = "ROUTE_LLM_DATABASE_URL",
        default_value = "sqlite://data/router.sqlite"
    )]
    pub database_url: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Serve(ServeArgs),
    AddClient(AddClientArgs),
    AddUpstream(AddUpstreamArgs),
    AddKey(AddKeyArgs),
    AddModelAlias(AddModelAliasArgs),
    AddUpstreamModel(AddUpstreamModelArgs),
    DisableKey(KeyIdArgs),
    EnableKey(KeyIdArgs),
    DisableModelAlias(ModelAliasNameArgs),
    EnableModelAlias(ModelAliasNameArgs),
    ResetKey(KeyIdArgs),
    List,
}

#[derive(Debug, Args, Clone)]
pub struct ServeArgs {
    #[arg(long, env = "ROUTE_LLM_BIND", default_value = "127.0.0.1:8080")]
    pub bind: String,

    #[arg(long, env = "ROUTE_LLM_PUBLIC_PREFIX", default_value = "/v1")]
    pub public_prefix: String,

    #[arg(long, env = "ROUTE_LLM_REQUEST_TIMEOUT_SECS", default_value_t = 300)]
    pub request_timeout_secs: u64,

    #[arg(
        long,
        env = "ROUTE_LLM_TRANSIENT_FAILURE_TTL_SECS",
        default_value_t = 300
    )]
    pub transient_failure_ttl_secs: i64,

    #[arg(long, env = "ROUTE_LLM_AUTH_FAILURE_TTL_SECS", default_value_t = 3600)]
    pub auth_failure_ttl_secs: i64,

    #[arg(long, env = "ROUTE_LLM_MAX_BODY_BYTES", default_value_t = 32 * 1024 * 1024)]
    pub max_body_bytes: usize,

    #[arg(long, env = "ROUTE_LLM_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    #[arg(long, env = "ROUTE_LLM_ADMIN_SESSION_SECRET")]
    pub admin_session_secret: Option<String>,

    #[arg(long, env = "ROUTE_LLM_ADMIN_SITE_NAME", default_value = "Route LLM")]
    pub admin_site_name: String,

    #[arg(
        long,
        env = "ROUTE_LLM_ADMIN_SITE_DESCRIPTION",
        default_value = "Local OpenAI-compatible routing proxy"
    )]
    pub admin_site_description: String,

    #[arg(long, env = "ROUTE_LLM_PUBLIC_BASE_URL")]
    pub public_base_url: Option<String>,
}

#[derive(Debug, Args)]
pub struct AddClientArgs {
    #[arg(long)]
    pub name: String,

    #[arg(long)]
    pub api_key: String,

    #[arg(long, default_value_t = true)]
    pub enabled: bool,
}

#[derive(Debug, Args)]
pub struct AddUpstreamArgs {
    #[arg(long)]
    pub name: String,

    #[arg(long)]
    pub base_url: String,

    #[arg(long, default_value_t = 100)]
    pub priority: i64,

    #[arg(long, default_value_t = true)]
    pub enabled: bool,
}

#[derive(Debug, Args)]
pub struct AddKeyArgs {
    #[arg(long)]
    pub upstream: String,

    #[arg(long)]
    pub name: String,

    #[arg(long)]
    pub api_key: String,

    #[arg(long, default_value_t = 100)]
    pub priority: i64,

    #[arg(long, default_value_t = true)]
    pub enabled: bool,
}

#[derive(Debug, Args)]
pub struct KeyIdArgs {
    #[arg(long)]
    pub id: i64,
}

#[derive(Debug, Args)]
pub struct AddModelAliasArgs {
    #[arg(long)]
    pub public_model: String,

    #[arg(long)]
    pub target_type: String,

    #[arg(long, default_value_t = true)]
    pub enabled: bool,
}

#[derive(Debug, Args)]
pub struct AddUpstreamModelArgs {
    #[arg(long)]
    pub upstream: String,

    #[arg(long)]
    pub model: String,

    #[arg(long = "capability", required = true)]
    pub capability: Vec<String>,

    #[arg(long, default_value_t = 100)]
    pub priority: i64,

    #[arg(long)]
    pub max_model_len: Option<i64>,

    #[arg(long, default_value_t = true)]
    pub enabled: bool,
}

#[derive(Debug, Args)]
pub struct ModelAliasNameArgs {
    #[arg(long)]
    pub public_model: String,
}
