mod actions;
mod auth;
mod components;
mod forms;
mod page;
mod render;
mod scripts;
mod stats_render;
mod styles;
mod text;

use actions::*;
use auth::*;
use components::*;
use render::*;
use stats_render::*;

use crate::{
    db,
    server::{AdminConfig, AppState},
};
use anyhow::{Context, bail};
use axum::{
    Form, Json, Router,
    extract::{Query, State},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{COOKIE, LOCATION, SET_COOKIE},
    },
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;

use forms::*;
use page::{PageMeta, page_shell};
use scripts::admin_js;
use text::{constant_time_eq, escape_html, percent_encode};

const ADMIN_COOKIE: &str = "route_llm_admin";

#[derive(Debug, Clone, Copy)]
struct AdminRenderContext<'a> {
    config: &'a AdminConfig,
    public_prefix: &'a str,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login).post(login_post))
        .route("/logout", post(logout))
        .route("/clients/generate", post(generate_client))
        .route("/clients/delete", post(delete_client))
        .route("/client-tokens/generate", post(generate_client_token))
        .route("/client-tokens/delete", post(delete_client_token))
        .route("/upstreams/add", post(add_upstream))
        .route("/upstreams/delete", post(delete_upstream))
        .route("/upstreams/fetch-models", post(fetch_upstream_models))
        .route("/keys/add", post(add_key))
        .route("/keys/delete", post(delete_key))
        .route("/keys/reset", post(reset_key))
        .route("/keys/reset-all", post(reset_all_keys))
        .route("/keys/reorder", post(reorder_keys))
        .route("/models/add", post(add_model))
        .route("/models/delete", post(delete_model))
        .route("/alias-routes/delete", post(delete_alias_route))
        .route("/routes/add", post(add_client_route))
        .route("/routes/delete", post(delete_client_route))
        .route("/routes/reorder", post(reorder_client_routes))
}

#[cfg(test)]
mod tests;
