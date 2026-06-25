use super::*;
use axum::{http::StatusCode, response::Response};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct ModelsResponse {
    object: &'static str,
    data: Vec<ModelResponseItem>,
}

#[derive(Debug, Serialize)]
pub(super) struct ModelResponseItem {
    id: String,
    object: &'static str,
    created: i64,
    owned_by: &'static str,
    max_model_len: i64,
}

pub(super) async fn models_list_response(
    state: &AppState,
    codex_catalog: bool,
) -> anyhow::Result<Response<Body>> {
    let public_models = db::list_public_models(&state.pool).await?;
    if codex_catalog {
        let body = serde_json::json!({
            "models": public_models
                .into_iter()
                .map(codex_model_catalog_item)
                .collect::<Vec<Value>>()
        });
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))?);
    }
    let body = ModelsResponse {
        object: "list",
        data: public_models
            .into_iter()
            .map(|model| ModelResponseItem {
                id: model.public_model,
                object: "model",
                created: model.created_at,
                owned_by: "route-llm",
                max_model_len: model.max_model_len,
            })
            .collect(),
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body)?))?)
}

pub(super) fn wants_codex_models_catalog(query: Option<&str>) -> bool {
    query
        .map(|query| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('=').map(|(key, _)| key).or(Some(pair)))
                .any(|key| key == "client_version")
        })
        .unwrap_or(false)
}

pub(super) fn codex_model_catalog_item(model: db::PublicModelSummary) -> Value {
    let max_model_len = model.max_model_len;
    serde_json::json!({
        "slug": model.public_model,
        "display_name": model.public_model,
        "description": "Route LLM public model alias",
        "default_reasoning_level": "low",
        "supported_reasoning_levels": [{
            "effort": "low",
            "description": "Minimal reasoning metadata for custom provider compatibility"
        }],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 100,
        "additional_speed_tiers": [],
        "service_tiers": [],
        "availability_nux": {
            "message": ""
        },
        "upgrade": Value::Null,
        "base_instructions": "",
        "model_messages": {
            "instructions_template": "",
            "instructions_variables": {}
        },
        "supports_reasoning_summaries": false,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": "low",
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text_and_image",
        "truncation_policy": {
            "mode": "tokens",
            "limit": max_model_len
        },
        "supports_parallel_tool_calls": true,
        "supports_image_detail_original": false,
        "context_window": max_model_len,
        "max_context_window": max_model_len,
        "comp_hash": "route-llm",
        "effective_context_window_percent": 100,
        "experimental_supported_tools": [],
        "input_modalities": ["text", "image"],
        "supports_search_tool": false,
        "use_responses_lite": false,
    })
}
