use serde::Serialize;

pub const DEFAULT_MAX_MODEL_LEN: i64 = 1_048_576;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub upstream_id: i64,
    pub key_id: i64,
    pub key_name: String,
    pub upstream_name: String,
    pub base_url: String,
    pub api_key: String,
    pub resolved_model: Option<String>,
    pub upstream_priority: i64,
    pub model_priority: Option<i64>,
    pub key_priority: i64,
}

#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub id: i64,
    pub name: String,
    pub token_id: i64,
    pub token_name: String,
}

#[derive(Debug, Clone)]
pub struct RequestAudit {
    pub completed_at: i64,
    pub duration_ms: i64,
    pub client_id: Option<i64>,
    pub client_name: Option<String>,
    pub client_token_id: Option<i64>,
    pub client_token_name: Option<String>,
    pub client_key_hash: Option<String>,
    pub client_ip: Option<String>,
    pub client_ip_source: Option<String>,
    pub cf_ray: Option<String>,
    pub cf_country: Option<String>,
    pub method: String,
    pub path: String,
    pub route_kind: String,
    pub has_query: bool,
    pub query_hash: Option<String>,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub content_type: Option<String>,
    pub request_body_bytes: Option<i64>,
    pub user_agent_hash: Option<String>,
    pub upstream_id: Option<i64>,
    pub upstream_name: Option<String>,
    pub upstream_key_id: Option<i64>,
    pub upstream_key_name: Option<String>,
    pub status: Option<i64>,
    pub outcome: String,
    pub error_class: Option<String>,
    pub error_message: Option<String>,
    pub attempts: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ResponseState {
    pub id: String,
    pub previous_response_id: Option<String>,
    pub client_id: Option<i64>,
    pub model: String,
    pub chat_messages_json: String,
    pub output_json: String,
    pub output_text: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct AttemptAudit {
    pub attempt_index: i64,
    pub upstream_id: i64,
    pub upstream_name: String,
    pub upstream_key_id: i64,
    pub upstream_key_name: String,
    pub status: Option<i64>,
    pub outcome: String,
    pub retriable: bool,
    pub duration_ms: i64,
    pub retry_after_secs: Option<i64>,
    pub disabled_until: Option<i64>,
    pub error_class: Option<String>,
    pub error_message: Option<String>,
    pub upstream_content_type: Option<String>,
    pub upstream_body_bytes: Option<i64>,
    pub upstream_body_hash: Option<String>,
    pub upstream_body_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StateSummary {
    pub clients: Vec<ClientSummary>,
    pub upstreams: Vec<UpstreamSummary>,
    pub model_aliases: Vec<ModelAliasSummary>,
}

#[derive(Debug, Serialize)]
pub struct ClientSummary {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub tokens: Vec<ClientTokenSummary>,
    pub routes: Vec<ClientModelRouteSummary>,
}

#[derive(Debug, Serialize)]
pub struct ClientTokenSummary {
    pub id: i64,
    pub name: String,
    pub api_key_fingerprint: String,
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    pub enabled: bool,
    pub created_at_text: String,
}

#[derive(Debug, Serialize)]
pub struct ClientModelRouteSummary {
    pub id: i64,
    pub public_model: String,
    pub upstream_model_id: i64,
    pub upstream_name: String,
    pub upstream_model: String,
    pub capabilities: Vec<String>,
    pub priority: i64,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct UpstreamSummary {
    pub id: i64,
    pub name: String,
    pub base_url: String,
    pub priority: i64,
    pub enabled: bool,
    pub models: Vec<UpstreamModelSummary>,
    pub discovered_models: Vec<DiscoveredModelSummary>,
    pub keys: Vec<KeySummary>,
}

#[derive(Debug, Serialize)]
pub struct UpstreamModelSummary {
    pub id: i64,
    pub model: String,
    pub capabilities: Vec<String>,
    pub max_model_len: Option<i64>,
    pub priority: i64,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredModelSummary {
    pub model: String,
    pub max_model_len: Option<i64>,
    pub fetched_at: i64,
    pub fetched_at_text: String,
}

#[derive(Debug, Serialize)]
pub struct KeySummary {
    pub id: i64,
    pub name: String,
    pub masked_api_key: String,
    pub priority: i64,
    pub enabled: bool,
    pub disabled_until: Option<i64>,
    pub consecutive_failures: i64,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AdminStats {
    pub recent_requests: Vec<RecentRequestSummary>,
    pub client_token_stats: Vec<ClientTokenUsageStats>,
    pub key_stats: Vec<KeyUsageStats>,
    pub health: AdminHealthSummary,
}

#[derive(Debug, Serialize)]
pub struct AdminHealthSummary {
    pub total_keys: i64,
    pub enabled_keys: i64,
    pub ready_keys: i64,
    pub cached_keys: i64,
    pub disabled_keys: i64,
    pub recent_503: i64,
    pub recent_upstream_exhausted: i64,
    pub recent_5xx: i64,
    pub last_failure_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecentRequestSummary {
    pub id: i64,
    pub completed_at: String,
    pub duration_ms: i64,
    pub client_name: Option<String>,
    pub client_token_name: Option<String>,
    pub client_token_fingerprint: Option<String>,
    pub client_ip: Option<String>,
    pub method: String,
    pub path: String,
    pub route_kind: String,
    pub model: Option<String>,
    pub upstream_name: Option<String>,
    pub upstream_key_id: Option<i64>,
    pub status: Option<i64>,
    pub outcome: String,
    pub attempts: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct KeyUsageStats {
    pub upstream_key_id: i64,
    pub upstream_name: String,
    pub masked_api_key: String,
    pub enabled: bool,
    pub priority: i64,
    pub disabled_until: Option<String>,
    pub consecutive_failures: i64,
    pub last_status: Option<i64>,
    pub last_used_at: Option<String>,
    pub total_requests: i64,
    pub success_requests: i64,
    pub failed_requests: i64,
    pub total_duration_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct ClientTokenUsageStats {
    pub client_token_id: i64,
    pub client_name: String,
    pub token_name: String,
    pub api_key_fingerprint: String,
    pub enabled: bool,
    pub last_used_at: Option<String>,
    pub total_requests: i64,
    pub success_requests: i64,
    pub failed_requests: i64,
    pub total_duration_ms: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelAliasSummary {
    pub id: i64,
    pub public_model: String,
    pub target_type: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub routes: Vec<ModelAliasRouteSummary>,
}

#[derive(Debug, Clone)]
pub struct PublicModelSummary {
    pub public_model: String,
    pub created_at: i64,
    pub max_model_len: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelAliasRouteSummary {
    pub id: i64,
    pub upstream_model_id: i64,
    pub upstream_name: String,
    pub upstream_model: String,
    pub capabilities: Vec<String>,
    pub priority: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct DiscoveredModelInput {
    pub model: String,
    pub max_model_len: Option<i64>,
}

#[derive(Debug)]
pub struct UpstreamModelFetchContext {
    pub upstream_id: i64,
    pub upstream_name: String,
    pub base_url: String,
    pub api_key: String,
}
