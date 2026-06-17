use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(super) struct AdminQuery {
    pub(super) client: Option<i64>,
    pub(super) provider: Option<i64>,
    pub(super) notice: Option<String>,
    pub(super) error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LoginForm {
    pub(super) password: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct UpstreamForm {
    pub(super) name: String,
    pub(super) base_url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct KeyForm {
    pub(super) upstream: String,
    pub(super) upstream_id: Option<i64>,
    pub(super) api_key: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ModelForm {
    pub(super) upstream: String,
    pub(super) upstream_id: Option<i64>,
    pub(super) model: String,
    pub(super) public_model: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClientForm {
    pub(super) name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClientTokenForm {
    pub(super) client_id: i64,
    pub(super) name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClientRouteForm {
    pub(super) client_id: i64,
    pub(super) public_model: String,
    pub(super) upstream_model_id: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct ReorderForm {
    pub(super) ids: String,
    pub(super) client_id: Option<i64>,
    pub(super) provider_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct IdForm {
    pub(super) id: i64,
    pub(super) client_id: Option<i64>,
    pub(super) provider_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ModelsResponse {
    pub(super) data: Vec<ModelItem>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ModelItem {
    pub(super) id: String,
    pub(super) max_model_len: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(super) struct FetchModelsResult {
    pub(super) upstream_id: i64,
    pub(super) upstream_name: String,
    pub(super) models: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct AdminJsonError {
    pub(super) error: String,
}
