mod forms;
mod page;
mod scripts;
mod styles;
mod text;

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

async fn index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<AdminQuery>,
) -> Response {
    if let Some(response) = admin_disabled_response(&state) {
        return response;
    }
    if !is_authenticated(&state, &headers) {
        return redirect("/admin/login");
    }

    match (
        db::list_state(&state.pool).await,
        db::list_admin_stats(&state.pool).await,
    ) {
        (Ok(summary), Ok(stats)) => Html(render_dashboard(
            AdminRenderContext {
                config: &state.config.admin,
                public_prefix: &state.config.public_prefix,
            },
            &summary,
            &stats,
            query,
        ))
        .into_response(),
        (Err(error), _) => redirect_error(&format!("상태를 불러오지 못했습니다: {error}")),
        (_, Err(error)) => redirect_error(&format!("통계를 불러오지 못했습니다: {error}")),
    }
}

async fn login(State(state): State<Arc<AppState>>, Query(query): Query<AdminQuery>) -> Response {
    if let Some(response) = admin_disabled_response(&state) {
        return response;
    }
    Html(render_login(
        AdminRenderContext {
            config: &state.config.admin,
            public_prefix: &state.config.public_prefix,
        },
        query.error,
    ))
    .into_response()
}

async fn login_post(State(state): State<Arc<AppState>>, Form(form): Form<LoginForm>) -> Response {
    if let Some(response) = admin_disabled_response(&state) {
        return response;
    }

    let Some(password_hash) = state.config.admin.password_hash.as_deref() else {
        return disabled_page(&state.config.admin).into_response();
    };
    if !constant_time_eq(&db::hash_secret(&form.password), password_hash) {
        return redirect_login_error("비밀번호가 올바르지 않습니다");
    }

    let Some(session_token) = state.config.admin.session_token.as_deref() else {
        return disabled_page(&state.config.admin).into_response();
    };
    let cookie = format!(
        "{ADMIN_COOKIE}={session_token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=86400"
    );
    let mut response = redirect("/admin");
    response
        .headers_mut()
        .insert(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    response
}

async fn logout() -> Response {
    let mut response = redirect("/admin/login");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static(
            "route_llm_admin=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

async fn generate_client(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ClientForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::create_generated_client(&state.pool, &form.name, true).await {
        Ok((id, _api_key)) => {
            redirect_notice_client_token("클라이언트를 생성하고 기본 토큰을 발급했습니다", id)
        }
        Err(error) => redirect_error(&format!("토큰 생성 실패: {error}")),
    }
}

async fn generate_client_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ClientTokenForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::create_generated_client_token(&state.pool, form.client_id, form.name.as_deref(), true)
        .await
    {
        Ok((_id, _api_key)) => {
            redirect_notice_client_token("클라이언트 토큰을 발급했습니다", form.client_id)
        }
        Err(error) => redirect_error(&format!("클라이언트 토큰 발급 실패: {error}")),
    }
}

async fn delete_client(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_action(state, headers, form.id).await
}

async fn delete_client_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_token_action(state, headers, form.id, form.client_id).await
}

async fn add_upstream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<UpstreamForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::upsert_upstream(&state.pool, &form.name, &form.base_url, 100, true).await {
        Ok(id) => redirect_notice_provider("프로바이더를 저장했습니다", id),
        Err(error) => redirect_error(&format!("프로바이더 저장 실패: {error}")),
    }
}

async fn delete_upstream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_upstream_action(state, headers, form.id).await
}

async fn fetch_upstream_models(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }

    match refresh_upstream_models(&state, form.id).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(AdminJsonError {
                error: format!("모델 목록 조회 실패: {error}"),
            }),
        )
            .into_response(),
    }
}

async fn add_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<KeyForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let key_name = generated_key_name(&form.api_key);
    let priority = match db::next_upstream_key_priority(&state.pool, &form.upstream).await {
        Ok(priority) => priority,
        Err(error) => {
            return match form.upstream_id {
                Some(provider_id) => redirect_error_provider(
                    &format!("토큰 순서를 계산하지 못했습니다: {error}"),
                    provider_id,
                ),
                None => redirect_error(&format!("토큰 순서를 계산하지 못했습니다: {error}")),
            };
        }
    };
    match db::upsert_upstream_key(
        &state.pool,
        &form.upstream,
        &key_name,
        &form.api_key,
        priority,
        true,
    )
    .await
    {
        Ok(_) => match form.upstream_id {
            Some(provider_id) => {
                redirect_notice_provider("업스트림 키를 저장했습니다", provider_id)
            }
            None => redirect_notice("업스트림 키를 저장했습니다"),
        },
        Err(error) => match form.upstream_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("업스트림 키 저장 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("업스트림 키 저장 실패: {error}")),
        },
    }
}

async fn delete_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_key_action(state, headers, form.id, form.provider_id).await
}

async fn reset_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::reset_key_health(&state.pool, form.id).await {
        Ok(_) => match form.provider_id {
            Some(provider_id) => redirect_notice_provider("키 상태를 초기화했습니다", provider_id),
            None => redirect_notice("키 상태를 초기화했습니다"),
        },
        Err(error) => redirect_error(&format!("키 상태 초기화 실패: {error}")),
    }
}

async fn reset_all_keys(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::reset_all_key_health(&state.pool).await {
        Ok(count) => redirect_notice(&format!("실패 캐시를 초기화했습니다: {count}개 토큰")),
        Err(error) => redirect_error(&format!("실패 캐시 초기화 실패: {error}")),
    }
}

async fn add_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ModelForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let target_type = infer_target_type(&form.public_model);
    if let Err(error) = db::ensure_model_alias(&state.pool, &form.public_model, target_type).await {
        return match form.upstream_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("모델명 저장 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("모델명 저장 실패: {error}")),
        };
    }
    match db::upsert_upstream_model_for_alias(
        &state.pool,
        &form.upstream,
        &form.model,
        &form.public_model,
        true,
    )
    .await
    {
        Ok(_) => match form.upstream_id {
            Some(provider_id) => {
                redirect_notice_provider("모델을 alias에 연결했습니다", provider_id)
            }
            None => redirect_notice("모델을 alias에 연결했습니다"),
        },
        Err(error) => match form.upstream_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("모델 저장 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("모델 저장 실패: {error}")),
        },
    }
}

async fn reorder_keys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ReorderForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let ids = match parse_ids(&form.ids) {
        Ok(ids) => ids,
        Err(error) => {
            return match form.provider_id {
                Some(provider_id) => {
                    redirect_error_provider(&format!("토큰 순서 저장 실패: {error}"), provider_id)
                }
                None => redirect_error(&format!("토큰 순서 저장 실패: {error}")),
            };
        }
    };
    match db::reorder_upstream_keys(&state.pool, &ids).await {
        Ok(_) => match form.provider_id {
            Some(provider_id) => redirect_notice_provider("토큰 순서를 저장했습니다", provider_id),
            None => redirect_notice("토큰 순서를 저장했습니다"),
        },
        Err(error) => match form.provider_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("토큰 순서 저장 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("토큰 순서 저장 실패: {error}")),
        },
    }
}

async fn delete_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_model_action(state, headers, form.id, form.provider_id).await
}

async fn delete_alias_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_alias_route_action(state, headers, form.id, form.provider_id).await
}

fn infer_target_type(public_model: &str) -> &'static str {
    let model = public_model.to_ascii_lowercase();
    if model.contains("multimodal") || model.contains("vision") || model.contains("omni") {
        "multimodal"
    } else if model.contains("image") {
        "image"
    } else if model.contains("tts") {
        "tts"
    } else if model.contains("stt")
        || model.contains("transcription")
        || model.contains("transcribe")
    {
        "stt"
    } else if model.contains("audio") {
        "audio"
    } else if model.contains("video") {
        "video"
    } else if model.contains("embedding") || model.contains("embed") {
        "embedding"
    } else {
        "llm"
    }
}

fn generated_key_name(api_key: &str) -> String {
    let hash = db::hash_secret(api_key);
    let short_hash: String = hash.chars().take(12).collect();
    format!("key-{short_hash}")
}

async fn refresh_upstream_models(
    state: &AppState,
    upstream_id: i64,
) -> anyhow::Result<FetchModelsResult> {
    let context = db::upstream_model_fetch_context(&state.pool, upstream_id).await?;
    let models = fetch_model_ids(state, &context).await?;
    db::replace_upstream_discovered_model_items(&state.pool, context.upstream_id, &models).await?;
    let model_ids = models
        .into_iter()
        .map(|model| model.model)
        .collect::<Vec<_>>();
    Ok(FetchModelsResult {
        upstream_id: context.upstream_id,
        upstream_name: context.upstream_name,
        models: model_ids,
    })
}

async fn fetch_model_ids(
    state: &AppState,
    context: &db::UpstreamModelFetchContext,
) -> anyhow::Result<Vec<db::DiscoveredModelInput>> {
    let url = upstream_models_url(&context.base_url)?;
    let response = state
        .client
        .get(url)
        .bearer_auth(&context.api_key)
        .send()
        .await
        .context("upstream /models request failed")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("upstream /models response read failed")?;
    if !status.is_success() {
        bail!("upstream returned status {}", status.as_u16());
    }

    let parsed: ModelsResponse =
        serde_json::from_slice(&body).context("upstream response is not /v1/models JSON")?;
    let mut models = parsed
        .data
        .into_iter()
        .filter_map(|model| {
            let id = model.id.trim().to_string();
            if id.is_empty() {
                return None;
            }
            Some(db::DiscoveredModelInput {
                model: id,
                max_model_len: model.max_model_len,
            })
        })
        .collect::<Vec<_>>();
    models.sort_by(|left, right| left.model.cmp(&right.model));
    models.dedup_by(|left, right| left.model == right.model);
    models.truncate(500);
    if models.is_empty() {
        bail!("upstream returned no model ids");
    }
    Ok(models)
}

fn upstream_models_url(base_url: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base_url).context("provider base url must be absolute")?;
    let path = format!("{}/models", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn add_client_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ClientRouteForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let priority =
        match db::next_client_model_route_priority(&state.pool, form.client_id, &form.public_model)
            .await
        {
            Ok(priority) => priority,
            Err(error) => {
                return redirect_error_client(
                    &format!("클라이언트 라우팅 저장 실패: {error}"),
                    form.client_id,
                );
            }
        };
    match db::upsert_client_model_route(
        &state.pool,
        form.client_id,
        &form.public_model,
        form.upstream_model_id,
        priority,
        true,
    )
    .await
    {
        Ok(_) => redirect_notice_client("클라이언트 라우팅을 저장했습니다", form.client_id),
        Err(error) => redirect_error_client(
            &format!("클라이언트 라우팅 저장 실패: {error}"),
            form.client_id,
        ),
    }
}

async fn delete_client_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_route_action(state, headers, form.id, form.client_id).await
}

async fn reorder_client_routes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<ReorderForm>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let ids = match parse_ids(&form.ids) {
        Ok(ids) => ids,
        Err(error) => {
            return match form.client_id {
                Some(client_id) => redirect_error_client(
                    &format!("클라이언트 라우팅 순서 저장 실패: {error}"),
                    client_id,
                ),
                None => redirect_error(&format!("클라이언트 라우팅 순서 저장 실패: {error}")),
            };
        }
    };
    match db::reorder_client_model_routes(&state.pool, &ids).await {
        Ok(_) => match form.client_id {
            Some(client_id) => {
                redirect_notice_client("클라이언트 라우팅 순서를 저장했습니다", client_id)
            }
            None => redirect_notice("클라이언트 라우팅 순서를 저장했습니다"),
        },
        Err(error) => match form.client_id {
            Some(client_id) => redirect_error_client(
                &format!("클라이언트 라우팅 순서 저장 실패: {error}"),
                client_id,
            ),
            None => redirect_error(&format!("클라이언트 라우팅 순서 저장 실패: {error}")),
        },
    }
}

async fn delete_client_action(state: Arc<AppState>, headers: HeaderMap, id: i64) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_client(&state.pool, id).await {
        Ok(_) => redirect_notice("클라이언트를 완전히 삭제했습니다"),
        Err(error) => redirect_error(&format!("클라이언트 삭제 실패: {error}")),
    }
}

async fn delete_client_token_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
    client_id: Option<i64>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    let target_client_id = client_id.unwrap_or_default();
    match db::delete_client_token(&state.pool, id).await {
        Ok(_) if target_client_id > 0 => {
            redirect_notice_client_token("클라이언트 토큰을 완전히 삭제했습니다", target_client_id)
        }
        Ok(_) => redirect_notice("클라이언트 토큰을 완전히 삭제했습니다"),
        Err(error) if target_client_id > 0 => redirect_error_client_token(
            &format!("클라이언트 토큰 삭제 실패: {error}"),
            target_client_id,
        ),
        Err(error) => redirect_error(&format!("클라이언트 토큰 삭제 실패: {error}")),
    }
}

async fn delete_upstream_action(state: Arc<AppState>, headers: HeaderMap, id: i64) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_upstream(&state.pool, id).await {
        Ok(_) => redirect_notice("프로바이더를 완전히 삭제했습니다"),
        Err(error) => redirect_error(&format!("프로바이더 삭제 실패: {error}")),
    }
}

async fn delete_key_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
    provider_id: Option<i64>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_upstream_key(&state.pool, id).await {
        Ok(_) => match provider_id {
            Some(provider_id) => redirect_notice_provider("키를 완전히 삭제했습니다", provider_id),
            None => redirect_notice("키를 완전히 삭제했습니다"),
        },
        Err(error) => match provider_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("키 삭제 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("키 삭제 실패: {error}")),
        },
    }
}

async fn delete_model_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
    provider_id: Option<i64>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_upstream_model(&state.pool, id).await {
        Ok(_) => match provider_id {
            Some(provider_id) => {
                redirect_notice_provider("모델과 연결된 라우팅을 완전히 삭제했습니다", provider_id)
            }
            None => redirect_notice("모델과 연결된 라우팅을 완전히 삭제했습니다"),
        },
        Err(error) => match provider_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("모델 삭제 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("모델 삭제 실패: {error}")),
        },
    }
}

async fn delete_alias_route_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
    provider_id: Option<i64>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_model_alias_route(&state.pool, id).await {
        Ok(public_model) => match provider_id {
            Some(provider_id) => redirect_notice_provider(
                &format!("{public_model} alias 연결을 완전히 삭제했습니다"),
                provider_id,
            ),
            None => redirect_notice(&format!("{public_model} alias 연결을 완전히 삭제했습니다")),
        },
        Err(error) => match provider_id {
            Some(provider_id) => {
                redirect_error_provider(&format!("alias 연결 삭제 실패: {error}"), provider_id)
            }
            None => redirect_error(&format!("alias 연결 삭제 실패: {error}")),
        },
    }
}

async fn delete_client_route_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
    client_id: Option<i64>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_client_model_route(&state.pool, id).await {
        Ok(_) => match client_id {
            Some(client_id) => {
                redirect_notice_client("클라이언트 라우팅을 완전히 삭제했습니다", client_id)
            }
            None => redirect_notice("클라이언트 라우팅을 완전히 삭제했습니다"),
        },
        Err(error) => match client_id {
            Some(client_id) => {
                redirect_error_client(&format!("클라이언트 라우팅 삭제 실패: {error}"), client_id)
            }
            None => redirect_error(&format!("클라이언트 라우팅 삭제 실패: {error}")),
        },
    }
}

fn parse_ids(value: &str) -> anyhow::Result<Vec<i64>> {
    let ids: Result<Vec<_>, _> = value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::parse::<i64>)
        .collect();
    let ids = ids.context("ids must be comma-separated integers")?;
    if ids.is_empty() {
        bail!("ids must not be empty");
    }
    Ok(ids)
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    admin_disabled_response(state)
        .or_else(|| (!is_authenticated(state, headers)).then(|| redirect("/admin/login")))
}

fn admin_disabled_response(state: &AppState) -> Option<Response> {
    state
        .config
        .admin
        .password_hash
        .is_none()
        .then(|| disabled_page(&state.config.admin).into_response())
}

fn is_authenticated(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = state.config.admin.session_token.as_deref() else {
        return false;
    };
    let Some(cookie_header) = headers.get(COOKIE).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    cookie_header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .any(|(name, value)| name == ADMIN_COOKIE && constant_time_eq(value, expected))
}

fn disabled_page(config: &AdminConfig) -> (StatusCode, Html<String>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(page_shell(
            &page_meta(config),
            "관리자 비활성화",
            r#"<main class="login"><h1>관리자 비활성화</h1><p><code>ROUTE_LLM_ADMIN_PASSWORD</code>가 필요합니다.</p></main>"#,
        )),
    )
}

fn redirect(path: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_str(path).unwrap());
    response
}

fn redirect_notice(message: &str) -> Response {
    redirect(&format!("/admin?notice={}", percent_encode(message)))
}

fn redirect_error(message: &str) -> Response {
    redirect(&format!("/admin?error={}", percent_encode(message)))
}

fn redirect_notice_client(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&notice={}#client-routing",
        percent_encode(message)
    ))
}

fn redirect_notice_client_token(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&token_client={client_id}&notice={}#clients",
        percent_encode(message)
    ))
}

fn redirect_error_client(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&error={}#client-routing",
        percent_encode(message)
    ))
}

fn redirect_error_client_token(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&token_client={client_id}&error={}#clients",
        percent_encode(message)
    ))
}

fn redirect_notice_provider(message: &str, provider_id: i64) -> Response {
    redirect(&format!(
        "/admin?provider={provider_id}&notice={}#settings",
        percent_encode(message)
    ))
}

fn redirect_error_provider(message: &str, provider_id: i64) -> Response {
    redirect(&format!(
        "/admin?provider={provider_id}&error={}#settings",
        percent_encode(message)
    ))
}

fn redirect_login_error(message: &str) -> Response {
    redirect(&format!("/admin/login?error={}", percent_encode(message)))
}

fn page_meta(config: &AdminConfig) -> PageMeta<'_> {
    PageMeta {
        site_name: &config.site_name,
        site_description: &config.site_description,
        public_base_url: config.public_base_url.as_deref(),
    }
}

fn display_base_url(config: &AdminConfig) -> String {
    config
        .public_base_url
        .as_deref()
        .unwrap_or("http://127.0.0.1:8080")
        .trim_end_matches('/')
        .to_string()
}

fn display_public_endpoint(config: &AdminConfig, public_prefix: &str) -> String {
    let base_url = display_base_url(config);
    let prefix = public_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        base_url
    } else {
        format!("{base_url}{prefix}")
    }
}

fn render_login(context: AdminRenderContext<'_>, error: Option<String>) -> String {
    let alert = error
        .map(|message| {
            format!(
                r#"<div class="alert error">{}</div>"#,
                escape_html(&message)
            )
        })
        .unwrap_or_default();
    let config = context.config;
    let site_name = escape_html(&config.site_name);
    let display_base_url = escape_html(&display_base_url(config));
    let public_endpoint = escape_html(&display_public_endpoint(config, context.public_prefix));
    page_shell(
        &page_meta(config),
        &format!("{} 로그인", config.site_name),
        &format!(
            r#"
<main class="login">
  <section class="login-shell" aria-labelledby="login-title">
    <div class="login-brand-panel">
      <div>
        <div class="login-mark">API</div>
        <span class="login-domain">{display_base_url}</span>
        <h1>{site_name}</h1>
      </div>
      <div class="login-endpoint">
        <span>API</span>
        <code>{public_endpoint}</code>
      </div>
    </div>
    <section class="login-card">
      <div class="login-card-head">
        <span class="section-kicker">관리자</span>
        <h2 id="login-title">로그인</h2>
      </div>
      {alert}
      <form method="post" action="/admin/login" class="login-form">
        <label>비밀번호
          <input type="password" name="password" autocomplete="current-password" placeholder="관리자 비밀번호" required autofocus>
        </label>
        <button type="submit">로그인</button>
      </form>
    </section>
  </section>
</main>
"#
        ),
    )
}

fn render_dashboard(
    context: AdminRenderContext<'_>,
    summary: &db::StateSummary,
    stats: &db::AdminStats,
    query: AdminQuery,
) -> String {
    let notices = render_alerts(query.notice, query.error);
    let selected_client_id = query
        .client
        .or_else(|| summary.clients.first().map(|c| c.id));
    let selected_provider_id = query
        .provider
        .or_else(|| summary.upstreams.first().map(|upstream| upstream.id));
    let workspace = render_client_workspace(summary, selected_client_id);
    let provider_workspace = render_provider_workspace(summary, selected_provider_id);
    let stats_panel = render_stats_panel(stats);
    let config = context.config;
    let site_name = escape_html(&config.site_name);
    let public_endpoint = escape_html(&display_public_endpoint(config, context.public_prefix));

    page_shell(
        &page_meta(config),
        &format!("{} 관리", config.site_name),
        &format!(
            r##"
<header class="topbar">
  <div class="brand">
    <div class="brand-mark">API</div>
    <div>
      <h1>{site_name}</h1>
      <span>{public_endpoint}</span>
    </div>
  </div>
  <form method="post" action="/admin/logout"><button class="secondary" type="submit">로그아웃</button></form>
</header>
<main>
  {notices}
  {workspace}
  {provider_workspace}
  {stats_panel}
</main>
{}
"##,
            admin_js()
        ),
    )
}

fn render_provider_workspace(
    summary: &db::StateSummary,
    selected_provider_id: Option<i64>,
) -> String {
    let provider_side_panel = render_provider_side_panel(summary, selected_provider_id);
    let selected_panel = match selected_provider_id
        .and_then(|id| summary.upstreams.iter().find(|upstream| upstream.id == id))
    {
        Some(upstream) => render_selected_provider_detail(upstream, &summary.model_aliases),
        None => r#"
<section class="provider-detail-panel panel">
  <div class="empty-state">
    <h2>프로바이더 없음</h2>
    <p>새 프로바이더를 추가하면 API 토큰과 모델을 연결할 수 있습니다.</p>
  </div>
</section>
"#
        .to_string(),
    };

    format!(
        r#"
<section id="settings" class="settings-section">
  <div class="settings-layout">
    {provider_side_panel}
    {selected_panel}
  </div>
</section>
"#
    )
}

fn render_provider_side_panel(
    summary: &db::StateSummary,
    selected_provider_id: Option<i64>,
) -> String {
    let provider_list = render_provider_list(summary, selected_provider_id);
    format!(
        r#"
<aside class="settings-side-panel panel">
    <div class="panel-head">
      <div>
        <h2>프로바이더</h2>
      </div>
    </div>
    <form method="post" action="/admin/upstreams/add" class="provider-create-form">
      <label>이름 <input name="name" required placeholder="openai-compatible"></label>
      <label>Base URL <input name="base_url" required placeholder="https://example.com/v1"></label>
      <button type="submit">추가</button>
    </form>
    {provider_list}
</aside>
"#
    )
}

fn render_client_workspace(summary: &db::StateSummary, selected_client_id: Option<i64>) -> String {
    let client_list = render_client_list(summary, selected_client_id);
    let selected_panel = match selected_client_id
        .and_then(|id| summary.clients.iter().find(|client| client.id == id))
    {
        Some(client) => render_selected_client_routes(client, summary),
        None => r#"
<section id="client-routing" class="client-routing-panel panel">
  <div class="empty-state">
    <h2>클라이언트 없음</h2>
    <p>새 클라이언트 토큰을 만들면 alias 라우팅을 지정할 수 있습니다.</p>
  </div>
</section>
"#
        .to_string(),
    };

    format!(
        r#"
<section id="clients" class="routing-shell">
  <aside class="client-list-panel panel">
    <div class="panel-head">
      <div>
        <h2>클라이언트</h2>
      </div>
    </div>
    <form method="post" action="/admin/clients/generate" class="client-create-form">
      <label>이름 <input name="name" required placeholder="web-app-production"></label>
      <button type="submit">생성</button>
    </form>
    {client_list}
  </aside>
  {selected_panel}
</section>
"#
    )
}

fn render_client_list(summary: &db::StateSummary, selected_client_id: Option<i64>) -> String {
    if summary.clients.is_empty() {
        return r#"<p class="empty">등록된 클라이언트가 없습니다.</p>"#.to_string();
    }

    summary
        .clients
        .iter()
        .map(|client| {
            let route_count = client.routes.iter().filter(|route| route.enabled).count();
            let token_count = client.tokens.iter().filter(|token| token.enabled).count();
            let selected = selected_client_id == Some(client.id);
            let selected_class = if selected { " selected" } else { "" };
            format!(
                r#"
<article class="client-select-card{selected_class}">
  <a class="client-select-link" href="/admin?client={}#client-routing">
    <span class="client-name">{}</span>
    <span class="client-meta">{route_count}개 라우팅 · {token_count}개 토큰</span>
  </a>
  <div class="client-card-actions">
    {}
    {}
    {}
  </div>
  {}
</article>
"#,
                client.id,
                escape_html(&client.name),
                status_badge(client.enabled),
                render_client_token_button(client),
                id_buttons("/admin/clients/delete", client.id),
                render_client_token_modal(client),
            )
        })
        .collect::<String>()
}

fn render_selected_client_routes(client: &db::ClientSummary, summary: &db::StateSummary) -> String {
    let visible_aliases = summary
        .model_aliases
        .iter()
        .filter(|alias| alias_visible_for_client(alias, client))
        .collect::<Vec<_>>();
    let alias_editors = if visible_aliases.is_empty() {
        r#"<p class="empty">연결된 alias 모델이 없습니다.</p>"#.to_string()
    } else {
        visible_aliases
            .into_iter()
            .map(|alias| render_client_alias_row(client, alias, summary))
            .collect::<String>()
    };
    format!(
        r#"
<section id="client-routing" class="client-routing-panel panel">
  <header class="client-routing-head">
    <div>
      <span class="section-kicker">라우팅</span>
      <h2>{}</h2>
    </div>
    <div class="selected-client-meta">
      {}
    </div>
  </header>
  <div class="client-route-editors">{alias_editors}</div>
</section>
"#,
        escape_html(&client.name),
        status_badge(client.enabled),
    )
}

fn render_client_token_button(client: &db::ClientSummary) -> String {
    let token_count = client.tokens.len();
    let title = format!("클라이언트 토큰 {token_count}개를 확인하고 관리합니다");
    format!(
        r#"<button type="button" class="secondary compact-action" data-open-token-modal="{}" title="{}">토큰 확인</button>"#,
        client.id,
        escape_html(&title),
    )
}

fn render_client_token_modal(client: &db::ClientSummary) -> String {
    let tokens = render_client_token_rows(client);
    format!(
        r#"
<div class="modal-backdrop" data-token-modal="{}" hidden>
  <section class="token-modal panel" role="dialog" aria-modal="true" aria-labelledby="token-modal-title-{}">
    <header class="modal-head">
      <div>
        <span class="section-kicker">클라이언트 토큰</span>
        <h2 id="token-modal-title-{}">{}</h2>
      </div>
      <button type="button" class="secondary icon-button" data-close-token-modal aria-label="닫기">닫기</button>
    </header>
    <form method="post" action="/admin/client-tokens/generate" class="token-create-form">
      <input type="hidden" name="client_id" value="{}">
      <label>토큰 이름 <input name="name" placeholder="production, local, worker"></label>
      <button type="submit">새 토큰 발급</button>
    </form>
    <div class="client-token-list">
      {tokens}
    </div>
  </section>
</div>
"#,
        client.id,
        client.id,
        client.id,
        escape_html(&client.name),
        client.id,
    )
}

fn render_client_token_rows(client: &db::ClientSummary) -> String {
    if client.tokens.is_empty() {
        return r#"<p class="empty">발급된 토큰이 없습니다.</p>"#.to_string();
    }
    client
        .tokens
        .iter()
        .map(|token| {
            let copy_control = match token.api_key.as_deref() {
                Some(api_key) if !api_key.is_empty() => format!(
                    r#"<button type="button" class="secondary" data-copy-token-value="{}"><span>복사</span></button>"#,
                    escape_html(api_key)
                ),
                _ => r#"<button type="button" class="secondary" disabled>원문 없음</button>"#
                    .to_string(),
            };
            let disabled_class = if token.enabled { "" } else { " disabled" };
            format!(
                r#"
<div class="client-token-row{disabled_class}">
  <div class="client-token-main">
    <strong>{}</strong>
    <code>{}</code>
    <span>{}</span>
  </div>
  <div class="client-token-actions">
    {}
    <form method="post" action="/admin/client-tokens/delete" class="inline">
      <input type="hidden" name="id" value="{}">
      <input type="hidden" name="client_id" value="{}">
      <button class="danger" type="submit">삭제</button>
    </form>
  </div>
</div>
"#,
                escape_html(&token.name),
                escape_html(&token.api_key_fingerprint),
                escape_html(&token.created_at_text),
                copy_control,
                token.id,
                client.id,
            )
        })
        .collect::<String>()
}

fn alias_has_enabled_routes(alias: &db::ModelAliasSummary) -> bool {
    alias.enabled && alias.routes.iter().any(|route| route.enabled)
}

fn alias_visible_for_client(alias: &db::ModelAliasSummary, client: &db::ClientSummary) -> bool {
    alias_has_enabled_routes(alias)
        || client
            .routes
            .iter()
            .any(|route| route.public_model == alias.public_model && route.enabled)
}

fn render_client_alias_row(
    client: &db::ClientSummary,
    alias: &db::ModelAliasSummary,
    summary: &db::StateSummary,
) -> String {
    let route_rows = client
        .routes
        .iter()
        .filter(|route| route.public_model == alias.public_model)
        .enumerate()
        .map(|(index, route)| {
            render_client_route_pill(route, client.id, &alias.public_model, index + 1)
        })
        .collect::<String>();
    let route_rows = if route_rows.is_empty() {
        r#"<span class="route-placeholder muted">기본 라우팅</span>"#.to_string()
    } else {
        route_rows
    };
    let (model_options, has_models) = render_model_select_options(alias, summary);
    let disabled = if has_models { "" } else { " disabled" };
    let default_routes = render_default_route_summary(alias);

    format!(
        r#"
<article class="client-alias-row">
  <div class="alias-row-title">
    <div>
      <code>{}</code>
    </div>
    <div class="default-route-line">{default_routes}</div>
  </div>
  <div class="client-route-lane">
    <div class="route-pills" data-sortable data-sort-scope="client:{}:{}" data-reorder-action="/admin/routes/reorder">{route_rows}</div>
  </div>
  <form method="post" action="/admin/routes/add" class="route-add-form">
    <input type="hidden" name="client_id" value="{}">
    <input type="hidden" name="public_model" value="{}">
    <select name="upstream_model_id" required{disabled}>{model_options}</select>
    <button type="submit"{disabled}>추가</button>
  </form>
</article>
"#,
        escape_html(&alias.public_model),
        client.id,
        escape_html(&alias.public_model),
        client.id,
        escape_html(&alias.public_model),
    )
}

fn render_model_select_options(
    alias: &db::ModelAliasSummary,
    summary: &db::StateSummary,
) -> (String, bool) {
    let options = summary
        .upstreams
        .iter()
        .filter(|upstream| upstream.enabled)
        .flat_map(|upstream| {
            upstream
                .models
                .iter()
                .filter(move |model| {
                    model.enabled && model.capabilities.contains(&alias.target_type)
                })
                .map(move |model| {
                    format!(
                        r#"<option value="{}">{} / {}</option>"#,
                        model.id,
                        escape_html(&upstream.name),
                        escape_html(&model.model),
                    )
                })
        })
        .collect::<String>();
    if options.is_empty() {
        (
            r#"<option value="">연결 가능한 모델 없음</option>"#.to_string(),
            false,
        )
    } else {
        (
            format!(r#"<option value="">모델 선택</option>{options}"#),
            true,
        )
    }
}

fn render_default_route_summary(alias: &db::ModelAliasSummary) -> String {
    let routes = alias
        .routes
        .iter()
        .filter(|route| route.enabled)
        .map(|route| {
            format!(
                r#"<span>{} / <code>{}</code></span>"#,
                escape_html(&route.upstream_name),
                escape_html(&route.upstream_model)
            )
        })
        .collect::<String>();
    if routes.is_empty() {
        r#"<span class="muted">기본 모델 없음</span>"#.to_string()
    } else {
        routes
    }
}

fn render_client_route_pill(
    route: &db::ClientModelRouteSummary,
    client_id: i64,
    public_model: &str,
    rank: usize,
) -> String {
    format!(
        r#"
<div class="route-pill{}" draggable="true" data-sort-item data-sort-id="{}" data-sort-scope="client:{}:{}">
  <span class="rank">{rank}</span>
  <span class="route-name"><strong>{}</strong><code>{}</code></span>
  <span class="route-actions">{}</span>
</div>
"#,
        if route.enabled { "" } else { " disabled" },
        route.id,
        client_id,
        escape_html(public_model),
        escape_html(&route.upstream_name),
        escape_html(&route.upstream_model),
        client_route_buttons(route.id, client_id),
    )
}

fn client_route_buttons(id: i64, client_id: i64) -> String {
    format!(
        r#"<form method="post" action="/admin/routes/delete" class="inline"><input type="hidden" name="id" value="{id}"><input type="hidden" name="client_id" value="{client_id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

fn render_provider_list(summary: &db::StateSummary, selected_provider_id: Option<i64>) -> String {
    if summary.upstreams.is_empty() {
        return r#"<p class="empty">프로바이더가 없습니다.</p>"#.to_string();
    }
    summary
        .upstreams
        .iter()
        .map(|upstream| {
            let selected = selected_provider_id == Some(upstream.id);
            let selected_class = if selected { " selected" } else { "" };
            let key_count = upstream.keys.iter().filter(|key| key.enabled).count();
            let model_count = upstream.models.iter().filter(|model| model.enabled).count();
            format!(
                r#"
<article class="provider-select-card{selected_class}">
  <a class="provider-select-link" href="/admin?provider={}#settings">
    <span class="provider-name">{}</span>
    <span class="provider-meta">{key_count}개 토큰 · {model_count}개 모델</span>
  </a>
  <div class="provider-select-actions">
    {}
    {}
  </div>
</article>
"#,
                upstream.id,
                escape_html(&upstream.name),
                status_badge(upstream.enabled),
                id_buttons("/admin/upstreams/delete", upstream.id),
            )
        })
        .collect::<String>()
}

fn render_selected_provider_detail(
    upstream: &db::UpstreamSummary,
    aliases: &[db::ModelAliasSummary],
) -> String {
    let key_list = render_provider_key_list(upstream);
    let model_list = render_provider_model_list(upstream, aliases);
    let model_name_field = render_model_name_field(upstream);
    let model_fetch_meta = render_model_fetch_meta(upstream);
    let model_form = format!(
        r#"
<form method="post" action="/admin/models/add" class="inline-create model-create">
  <input type="hidden" name="upstream" value="{}">
  <input type="hidden" name="upstream_id" value="{}">
  {model_name_field}
  <label class="autocomplete-field">모델명 Alias
    <input name="public_model" required placeholder="llm-model" autocomplete="off" data-alias-autocomplete>
    <div class="autocomplete-menu" data-alias-options>{}</div>
  </label>
  <button type="submit">모델 연결</button>
</form>
"#,
        escape_html(&upstream.name),
        upstream.id,
        render_alias_suggestions(aliases),
    );

    format!(
        r#"
<section class="provider-detail-panel panel" data-upstream-id="{}">
  <header class="provider-detail-head">
    <div>
      <span class="section-kicker">프로바이더 상세</span>
      <h2>{}</h2>
      <p><code>{}</code></p>
    </div>
    <div class="selected-provider-meta">
      {}
    </div>
  </header>
  <div class="provider-grid">
    <section class="provider-subpanel">
      <div class="subhead"><h4>API 토큰</h4></div>
      <form method="post" action="/admin/keys/add" class="inline-create key-create">
        <input type="hidden" name="upstream" value="{}">
        <input type="hidden" name="upstream_id" value="{}">
        <label>API Key <input name="api_key" type="password" required autocomplete="off"></label>
        <button type="submit">추가</button>
      </form>
      {key_list}
    </section>
    <section class="provider-subpanel">
      <div class="subhead"><h4>모델</h4></div>
      {model_form}
      {model_fetch_meta}
      {model_list}
    </section>
  </div>
</section>
"#,
        upstream.id,
        escape_html(&upstream.name),
        escape_html(&upstream.base_url),
        status_badge(upstream.enabled),
        escape_html(&upstream.name),
        upstream.id,
    )
}

fn render_model_name_field(upstream: &db::UpstreamSummary) -> String {
    if upstream.discovered_models.is_empty() {
        return r#"<label data-model-name-field>실제 모델명 <input name="model" required placeholder="provider-real-model"></label>"#
            .to_string();
    }

    let options = upstream
        .discovered_models
        .iter()
        .map(|model| {
            let registered = upstream
                .models
                .iter()
                .any(|registered| registered.model == model.model);
            let label = if registered {
                format!("{} (등록됨)", model.model)
            } else {
                model.model.clone()
            };
            format!(
                r#"<option value="{}">{}</option>"#,
                escape_html(&model.model),
                escape_html(&label)
            )
        })
        .collect::<String>();
    format!(
        r#"<label data-model-name-field>실제 모델명 <select name="model" required><option value="">실제 모델 선택</option>{options}</select></label>"#
    )
}

fn render_model_fetch_meta(upstream: &db::UpstreamSummary) -> String {
    let Some(last_model) = upstream.discovered_models.last() else {
        return r#"<p class="form-hint" data-model-fetch-meta>페이지를 열면 실제 모델 목록을 자동으로 확인합니다.</p>"#
            .to_string();
    };
    format!(
        r#"<p class="form-hint" data-model-fetch-meta>가져온 모델 {}개 · 마지막 조회 {}</p>"#,
        upstream.discovered_models.len(),
        escape_html(&last_model.fetched_at_text)
    )
}

fn render_provider_key_list(upstream: &db::UpstreamSummary) -> String {
    if upstream.keys.is_empty() {
        return r#"<p class="empty">등록된 키가 없습니다.</p>"#.to_string();
    }
    let rows = upstream
        .keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let disabled_class = if key.enabled { "" } else { " disabled" };
            format!(
                r#"
<div class="provider-key-card{disabled_class}" draggable="true" data-sort-item data-sort-id="{}" data-sort-scope="keys:{}">
  <span class="rank">{}</span>
  <code>{}</code>
  <div class="provider-key-actions">{}</div>
</div>
"#,
                key.id,
                upstream.id,
                index + 1,
                escape_html(&key.masked_api_key),
                provider_key_delete_button(key.id, upstream.id),
            )
        })
        .collect::<String>();
    format!(
        r#"<div class="provider-key-list" data-sortable data-sort-scope="keys:{}" data-reorder-action="/admin/keys/reorder">{rows}</div>"#,
        upstream.id
    )
}

fn render_provider_model_list(
    upstream: &db::UpstreamSummary,
    aliases: &[db::ModelAliasSummary],
) -> String {
    if upstream.models.is_empty() {
        return r#"<p class="empty">등록된 모델이 없습니다.</p>"#.to_string();
    }
    let rows = upstream
        .models
        .iter()
        .map(|model| {
            let disabled = !upstream.enabled || !model.enabled;
            let disabled_class = if disabled { " disabled" } else { "" };
            format!(
                r#"
<div class="provider-model-card{disabled_class}" data-model-id="{}" data-registered-model="{}">
  <div class="provider-model-main">
    <code>{}</code>
    <div class="alias-tags">{}</div>
  </div>
  <div class="provider-model-actions">
    {}
    {}
  </div>
</div>
"#,
                model.id,
                escape_html(&model.model),
                escape_html(&model.model),
                render_alias_assignment_tags(model.id, upstream.id, aliases),
                status_badge(model.enabled),
                provider_scoped_id_button("/admin/models/delete", model.id, upstream.id),
            )
        })
        .collect::<String>();
    format!(r#"<div class="provider-model-list">{rows}</div>"#)
}

fn render_alias_suggestions(aliases: &[db::ModelAliasSummary]) -> String {
    let suggestions = aliases
        .iter()
        .filter(|alias| alias_has_enabled_routes(alias))
        .map(|alias| {
            format!(
                r#"<button type="button" class="autocomplete-option" data-autocomplete-value="{}">{}</button>"#,
                escape_html(&alias.public_model),
                escape_html(&alias.public_model),
            )
        })
        .collect::<String>();
    if suggestions.is_empty() {
        r#"<span class="autocomplete-empty">연결된 alias 없음</span>"#.to_string()
    } else {
        suggestions
    }
}

fn render_alias_assignment_tags(
    model_id: i64,
    provider_id: i64,
    aliases: &[db::ModelAliasSummary],
) -> String {
    let tags = aliases
        .iter()
        .flat_map(|alias| {
            alias.routes.iter().filter_map(move |route| {
                if alias.enabled && route.enabled && route.upstream_model_id == model_id {
                    Some(format!(
                        r#"<span class="alias-tag alias-tag-control"><span>{}</span><form method="post" action="/admin/alias-routes/delete" class="inline"><input type="hidden" name="id" value="{}"><input type="hidden" name="provider_id" value="{}"><button class="alias-tag-remove" type="submit" aria-label="{} alias 연결 삭제">삭제</button></form></span>"#,
                        escape_html(&alias.public_model),
                        route.id,
                        provider_id,
                        escape_html(&alias.public_model),
                    ))
                } else {
                    None
                }
            })
        })
        .collect::<String>();
    if tags.is_empty() {
        r#"<span class="alias-tag muted">alias 없음</span>"#.to_string()
    } else {
        tags
    }
}

fn render_stats_panel(stats: &db::AdminStats) -> String {
    let recent = render_recent_requests(&stats.recent_requests);
    let client_token_stats = render_client_token_stats(&stats.client_token_stats);
    let key_stats = render_key_stats(&stats.key_stats);
    let health = render_health_summary(&stats.health);
    format!(
        r#"
<section class="stats-section panel">
  <div class="stats-head">
    <div>
      <span class="section-kicker">통계</span>
      <h2>호출 내역</h2>
    </div>
    <div class="stats-actions">
      <form method="post" action="/admin/keys/reset-all" class="inline"><button class="danger" type="submit">실패 캐시 비우기</button></form>
    </div>
  </div>
  {health}
  <div class="stats-layout">
    <section class="stats-block">
      <h3>최근 호출</h3>
      {recent}
    </section>
    <section class="stats-block">
      <h3>클라이언트 토큰</h3>
      {client_token_stats}
      <h3 class="stats-subtitle">프로바이더 토큰</h3>
      {key_stats}
    </section>
  </div>
</section>
"#
    )
}

fn render_health_summary(health: &db::AdminHealthSummary) -> String {
    let last_failure = health.last_failure_at.as_deref().unwrap_or("-");
    let health_class = if health.cached_keys == 0 && health.recent_503 == 0 {
        "good"
    } else {
        "warn"
    };
    format!(
        r#"
<div class="health-strip {health_class}">
  <span><strong>{}/{}</strong><small>사용 가능 토큰</small></span>
  <span><strong>{}</strong><small>실패 캐시</small></span>
  <span><strong>{}</strong><small>최근 10분 503</small></span>
  <span><strong>{}</strong><small>최근 10분 5xx</small></span>
  <span><strong>{}</strong><small>마지막 실패</small></span>
</div>
"#,
        health.ready_keys,
        health.enabled_keys,
        health.cached_keys,
        health.recent_503,
        health.recent_5xx,
        escape_html(last_failure),
    )
}

fn render_recent_requests(requests: &[db::RecentRequestSummary]) -> String {
    if requests.is_empty() {
        return r#"<p class="empty">아직 호출 내역이 없습니다.</p>"#.to_string();
    }
    let rows = requests
        .iter()
        .map(|request| {
            format!(
                r#"
<tr>
  <td>{}</td>
  <td><code>{}</code><span class="table-subtext">{}</span></td>
  <td>{}</td>
  <td>{}</td>
  <td>{}</td>
  <td>{}</td>
  <td>{}ms</td>
</tr>
"#,
                escape_html(&request.completed_at),
                escape_html(request.model.as_deref().unwrap_or("-")),
                escape_html(&request.route_kind),
                render_request_client(request),
                render_request_upstream(request),
                render_status_value(request.status, &request.outcome),
                render_token_usage(
                    request.input_tokens,
                    request.output_tokens,
                    request.total_tokens
                ),
                request.duration_ms,
            )
        })
        .collect::<String>();
    format!(
        r#"<table class="compact-table recent-table"><thead><tr><th>시간</th><th>모델</th><th>클라이언트</th><th>라우팅</th><th>결과</th><th>토큰</th><th>소요</th></tr></thead><tbody>{rows}</tbody></table>"#
    )
}

fn render_request_client(request: &db::RecentRequestSummary) -> String {
    let client = request.client_name.as_deref().unwrap_or("unknown");
    let token = request
        .client_token_name
        .as_deref()
        .or(request.client_token_fingerprint.as_deref())
        .unwrap_or("토큰 없음");
    format!(
        r#"<span>{}</span><span class="table-subtext">{}</span>"#,
        escape_html(client),
        escape_html(token)
    )
}

fn render_token_usage(input: Option<i64>, output: Option<i64>, total: Option<i64>) -> String {
    let total = total.map(format_number).unwrap_or_else(|| "-".to_string());
    let input = input.map(format_number).unwrap_or_else(|| "-".to_string());
    let output = output.map(format_number).unwrap_or_else(|| "-".to_string());
    format!(
        r#"<span>{}</span><span class="table-subtext">in {} / out {}</span>"#,
        escape_html(&total),
        escape_html(&input),
        escape_html(&output)
    )
}

fn render_duration_ms(duration_ms: i64) -> String {
    if duration_ms >= 1000 {
        format!("{:.1}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

fn format_number(value: i64) -> String {
    let digits = (value as i128).abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let mut output = grouped.chars().rev().collect::<String>();
    if value < 0 {
        output.insert(0, '-');
    }
    output
}

fn render_request_upstream(request: &db::RecentRequestSummary) -> String {
    let upstream = request.upstream_name.as_deref().unwrap_or("-");
    let token = request
        .upstream_key_id
        .map(|id| format!("토큰 #{id}"))
        .unwrap_or_else(|| "토큰 없음".to_string());
    format!(
        r#"<span>{}</span><span class="table-subtext">{}</span>"#,
        escape_html(upstream),
        escape_html(&token)
    )
}

fn render_client_token_stats(stats: &[db::ClientTokenUsageStats]) -> String {
    if stats.is_empty() {
        return r#"<p class="empty">발급된 클라이언트 토큰이 없습니다.</p>"#.to_string();
    }
    let cards = stats
        .iter()
        .map(|stat| {
            let last_used = stat.last_used_at.as_deref().unwrap_or("-");
            let duration = render_duration_ms(stat.total_duration_ms);
            format!(
                r#"
<article class="key-stat-card{}">
  <header>
    <div>
      <span class="section-kicker">{}</span>
      <strong>{}</strong>
      <code>{}</code>
    </div>
    {}
  </header>
  <div class="stat-metrics">
    <span><strong>{}</strong><small>전체</small></span>
    <span><strong>{}</strong><small>성공</small></span>
    <span><strong>{}</strong><small>실패</small></span>
    <span><strong>{}</strong><small>누적</small></span>
    <span><strong>{}</strong><small>토큰</small></span>
  </div>
  <dl class="stat-details">
    <div><dt>입력/출력 토큰</dt><dd>{} / {}</dd></div>
    <div><dt>마지막 사용</dt><dd>{}</dd></div>
  </dl>
</article>
"#,
                if stat.enabled { "" } else { " disabled" },
                escape_html(&stat.client_name),
                escape_html(&stat.token_name),
                escape_html(&stat.api_key_fingerprint),
                status_badge(stat.enabled),
                stat.total_requests,
                stat.success_requests,
                stat.failed_requests,
                escape_html(&duration),
                format_number(stat.total_tokens),
                format_number(stat.input_tokens),
                format_number(stat.output_tokens),
                escape_html(last_used),
            )
        })
        .collect::<String>();
    format!(r#"<div class="key-stat-list">{cards}</div>"#)
}

fn render_status_value(status: Option<i64>, outcome: &str) -> String {
    let status = status
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let class = if outcome == "success" {
        "status-ok"
    } else {
        "status-warn"
    };
    format!(
        r#"<span class="{class}">{}</span><span class="table-subtext">{}</span>"#,
        escape_html(&status),
        escape_html(outcome)
    )
}

fn render_key_stats(stats: &[db::KeyUsageStats]) -> String {
    if stats.is_empty() {
        return r#"<p class="empty">등록된 토큰이 없습니다.</p>"#.to_string();
    }
    let cards = stats
        .iter()
        .map(|stat| {
            let total_duration = render_duration_ms(stat.total_duration_ms);
            let last_used = stat.last_used_at.as_deref().unwrap_or("-");
            let last_status = stat
                .last_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let disabled_until = stat.disabled_until.as_deref().unwrap_or("-");
            format!(
                r#"
<article class="key-stat-card{}">
  <header>
    <div>
      <span class="section-kicker">{}</span>
      <code>{}</code>
    </div>
    <div class="key-stat-actions">
      {}
      {}
    </div>
  </header>
  <div class="stat-metrics">
    <span><strong>{}</strong><small>전체</small></span>
    <span><strong>{}</strong><small>성공</small></span>
    <span><strong>{}</strong><small>실패</small></span>
    <span><strong>{}</strong><small>누적</small></span>
    <span><strong>{}</strong><small>토큰</small></span>
  </div>
  <dl class="stat-details">
    <div><dt>입력/출력 토큰</dt><dd>{} / {}</dd></div>
    <div><dt>마지막 사용</dt><dd>{}</dd></div>
    <div><dt>최근 상태</dt><dd>{}</dd></div>
    <div><dt>연속 실패</dt><dd>{}</dd></div>
    <div><dt>캐시 만료</dt><dd>{}</dd></div>
  </dl>
</article>
"#,
                if stat.enabled { "" } else { " disabled" },
                escape_html(&stat.upstream_name),
                escape_html(&stat.masked_api_key),
                status_badge(stat.enabled),
                key_stat_buttons(stat.upstream_key_id),
                stat.total_requests,
                stat.success_requests,
                stat.failed_requests,
                escape_html(&total_duration),
                format_number(stat.total_tokens),
                format_number(stat.input_tokens),
                format_number(stat.output_tokens),
                escape_html(last_used),
                escape_html(&last_status),
                stat.consecutive_failures,
                escape_html(disabled_until),
            )
        })
        .collect::<String>();
    format!(r#"<div class="key-stat-list">{cards}</div>"#)
}

fn render_alerts(notice: Option<String>, error: Option<String>) -> String {
    let mut output = String::new();
    if let Some(message) = notice {
        output.push_str(&format!(
            r#"<div class="alert ok">{}</div>"#,
            escape_html(&message)
        ));
    }
    if let Some(message) = error {
        output.push_str(&format!(
            r#"<div class="alert error">{}</div>"#,
            escape_html(&message)
        ));
    }
    output
}

fn status_badge(enabled: bool) -> String {
    if enabled {
        String::new()
    } else {
        r#"<span class="badge disabled">비활성</span>"#.to_string()
    }
}

fn id_buttons(delete_action: &str, id: i64) -> String {
    format!(
        r#"<form method="post" action="{delete_action}" class="inline"><input type="hidden" name="id" value="{id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

fn provider_scoped_id_button(delete_action: &str, id: i64, provider_id: i64) -> String {
    format!(
        r#"<form method="post" action="{delete_action}" class="inline"><input type="hidden" name="id" value="{id}"><input type="hidden" name="provider_id" value="{provider_id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

fn provider_key_delete_button(id: i64, provider_id: i64) -> String {
    provider_scoped_id_button("/admin/keys/delete", id, provider_id)
}

fn key_stat_buttons(id: i64) -> String {
    let delete = id_buttons("/admin/keys/delete", id);
    format!(
        r#"{delete}<form method="post" action="/admin/keys/reset" class="inline"><input type="hidden" name="id" value="{id}"><button class="secondary" type="submit">초기화</button></form>"#
    )
}

#[cfg(test)]
mod tests {
    use super::styles::admin_css;
    use super::*;

    fn test_admin_config() -> AdminConfig {
        AdminConfig {
            password_hash: None,
            session_token: None,
            site_name: "Route LLM".to_string(),
            site_description: "Local OpenAI-compatible routing proxy".to_string(),
            public_base_url: None,
        }
    }

    fn public_admin_config() -> AdminConfig {
        AdminConfig {
            public_base_url: Some("https://router.example.test".to_string()),
            ..test_admin_config()
        }
    }

    #[test]
    fn escapes_html_controlled_values() {
        assert_eq!(
            escape_html(r#"<script a="b">'x'&</script>"#),
            "&lt;script a=&quot;b&quot;&gt;&#39;x&#39;&amp;&lt;/script&gt;"
        );
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent_encode("saved ok"), "saved+ok");
        assert_eq!(percent_encode("a/b?c"), "a%2Fb%3Fc");
    }

    #[test]
    fn page_shell_includes_icons_manifest_and_open_graph_metadata() {
        let config = public_admin_config();
        let html = page_shell(&page_meta(&config), "Route LLM 관리", "<main></main>");

        assert!(html.contains(
            r#"<link rel="icon" type="image/svg+xml" href="/favicon.svg?v=20260616-simple">"#
        ));
        assert!(
            html.contains(r#"<link rel="alternate icon" href="/favicon.ico?v=20260616-simple">"#)
        );
        assert!(html.contains(
            r#"<link rel="apple-touch-icon" href="/apple-touch-icon.png?v=20260616-simple">"#
        ));
        assert!(
            html.contains(r#"<link rel="manifest" href="/site.webmanifest?v=20260616-simple">"#)
        );
        assert!(html.contains(r##"<meta name="theme-color" content="#172033">"##));
        assert!(html.contains(
            r#"<meta name="description" content="Local OpenAI-compatible routing proxy">"#
        ));
        assert!(html.contains(r#"<meta property="og:site_name" content="Route LLM">"#));
        assert!(html.contains(
            r#"<meta property="og:image" content="https://router.example.test/og.png?v=20260616-simple">"#
        ));
        assert!(html.contains(r#"<meta name="twitter:card" content="summary_large_image">"#));
    }

    #[test]
    fn login_page_uses_split_auth_layout() {
        let config = test_admin_config();
        let html = render_login(
            AdminRenderContext {
                config: &config,
                public_prefix: "/v1",
            },
            None,
        );

        assert!(html.contains("login-shell"));
        assert!(html.contains("login-brand-panel"));
        assert!(html.contains("login-card"));
        assert!(html.contains("http://127.0.0.1:8080/v1"));
        assert!(html.contains(r#"placeholder="관리자 비밀번호""#));
        assert!(!html.contains(r#"class="panel login-form""#));
    }

    #[test]
    fn builds_models_url_from_provider_base_url() {
        assert_eq!(
            upstream_models_url("https://example.test/v1").unwrap(),
            "https://example.test/v1/models"
        );
        assert_eq!(
            upstream_models_url("https://example.test/v1/").unwrap(),
            "https://example.test/v1/models"
        );
    }

    #[test]
    fn client_routing_hides_aliases_without_connected_models() {
        let client = db::ClientSummary {
            id: 1,
            name: "client".to_string(),
            enabled: true,
            tokens: Vec::new(),
            routes: Vec::new(),
        };
        let summary = db::StateSummary {
            clients: Vec::new(),
            upstreams: Vec::new(),
            model_aliases: vec![
                db::ModelAliasSummary {
                    id: 1,
                    public_model: "unused-model".to_string(),
                    target_type: "llm".to_string(),
                    enabled: true,
                    created_at: 1,
                    updated_at: 1,
                    routes: Vec::new(),
                },
                db::ModelAliasSummary {
                    id: 2,
                    public_model: "llm-model".to_string(),
                    target_type: "llm".to_string(),
                    enabled: true,
                    created_at: 1,
                    updated_at: 1,
                    routes: vec![db::ModelAliasRouteSummary {
                        id: 1,
                        upstream_model_id: 10,
                        upstream_name: "provider".to_string(),
                        upstream_model: "real-model".to_string(),
                        capabilities: vec!["llm".to_string()],
                        priority: 10,
                        enabled: true,
                    }],
                },
            ],
        };

        let html = render_selected_client_routes(&client, &summary);

        assert!(!html.contains("unused-model"));
        assert!(html.contains("llm-model"));
    }

    #[test]
    fn client_token_button_opens_modal_without_copying_masked_value() {
        let client = db::ClientSummary {
            id: 1,
            name: "client".to_string(),
            enabled: true,
            tokens: vec![db::ClientTokenSummary {
                id: 10,
                name: "production".to_string(),
                api_key_fingerprint: "sha256:abc123".to_string(),
                api_key: Some("secret-token".to_string()),
                enabled: true,
                created_at_text: "06-15 12:00:00".to_string(),
            }],
            routes: Vec::new(),
        };

        let button = render_client_token_button(&client);
        let modal = render_client_token_modal(&client);

        assert!(button.contains("토큰 확인"));
        assert!(button.contains("클라이언트 토큰 1개"));
        assert!(!button.contains("1/1개"));
        assert!(!button.contains("secret-token"));
        assert!(modal.contains("data-copy-token-value=\"secret-token\""));
        assert!(modal.contains(r#"action="/admin/client-tokens/delete""#));
        assert!(!modal.contains("복구"));
    }

    #[test]
    fn provider_model_alias_tags_are_deletable_not_draggable() {
        let upstream = db::UpstreamSummary {
            id: 1,
            name: "provider".to_string(),
            base_url: "https://provider.example/v1".to_string(),
            priority: 10,
            enabled: true,
            models: vec![db::UpstreamModelSummary {
                id: 10,
                model: "real-model".to_string(),
                capabilities: vec!["llm".to_string()],
                max_model_len: Some(1_048_576),
                priority: 10,
                enabled: true,
            }],
            discovered_models: Vec::new(),
            keys: Vec::new(),
        };
        let aliases = vec![db::ModelAliasSummary {
            id: 1,
            public_model: "llm-model".to_string(),
            target_type: "llm".to_string(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            routes: vec![db::ModelAliasRouteSummary {
                id: 99,
                upstream_model_id: 10,
                upstream_name: "provider".to_string(),
                upstream_model: "real-model".to_string(),
                capabilities: vec!["llm".to_string()],
                priority: 10,
                enabled: true,
            }],
        }];

        let html = render_provider_model_list(&upstream, &aliases);

        assert!(!html.contains("draggable="));
        assert!(!html.contains("data-drag-kind"));
        assert!(html.contains(r#"action="/admin/alias-routes/delete""#));
        assert!(html.contains(r#"name="id" value="99""#));
        assert!(html.contains(r#"name="provider_id" value="1""#));
    }

    #[test]
    fn token_modal_escapes_panel_stacking_context() {
        let css = admin_css();
        let script = admin_js();

        assert!(css.contains("z-index: 10000;"));
        assert!(css.contains("body.modal-open > main"));
        assert!(css.contains("backdrop-filter: blur(2px);"));
        assert!(script.contains("document.body.appendChild(modal);"));
    }

    #[test]
    fn token_usage_values_are_grouped() {
        let html = render_token_usage(Some(164392), Some(427), Some(164819));

        assert!(html.contains("164,819"));
        assert!(html.contains("in 164,392 / out 427"));
    }

    #[test]
    fn key_stats_render_cumulative_duration_not_average() {
        let html = render_key_stats(&[db::KeyUsageStats {
            upstream_key_id: 1,
            upstream_name: "provider".to_string(),
            masked_api_key: "abcd...wxyz".to_string(),
            enabled: true,
            priority: 10,
            disabled_until: None,
            consecutive_failures: 0,
            last_status: Some(200),
            last_used_at: None,
            total_requests: 2,
            success_requests: 2,
            failed_requests: 0,
            total_duration_ms: 1500,
            input_tokens: 3000,
            output_tokens: 4200,
            total_tokens: 7200,
        }]);

        assert!(html.contains("누적"));
        assert!(html.contains("1.5s"));
        assert!(html.contains("7,200"));
        assert!(html.contains("3,000 / 4,200"));
        assert!(!html.contains("평균"));
    }

    #[test]
    fn discovered_models_render_as_model_select() {
        let upstream = db::UpstreamSummary {
            id: 1,
            name: "provider".to_string(),
            base_url: "https://example.test/v1".to_string(),
            priority: 10,
            enabled: true,
            models: Vec::new(),
            discovered_models: vec![db::DiscoveredModelSummary {
                model: "remote-model".to_string(),
                max_model_len: Some(1_048_576),
                fetched_at: 1,
                fetched_at_text: "01-01 00:00:01".to_string(),
            }],
            keys: Vec::new(),
        };

        let html = render_model_name_field(&upstream);

        assert!(html.contains("data-model-name-field"));
        assert!(html.contains(r#"<select name="model" required>"#));
        assert!(html.contains("remote-model"));
    }

    #[test]
    fn provider_detail_uses_automatic_model_refresh_without_manual_fetch_button() {
        let upstream = db::UpstreamSummary {
            id: 7,
            name: "provider".to_string(),
            base_url: "https://example.test/v1".to_string(),
            priority: 10,
            enabled: true,
            models: Vec::new(),
            discovered_models: Vec::new(),
            keys: Vec::new(),
        };

        let html = render_selected_provider_detail(&upstream, &[]);

        assert!(html.contains(r#"data-upstream-id="7""#));
        assert!(!html.contains("모델 목록 가져오기"));
        assert!(html.contains("자동으로 확인합니다"));
        assert!(html.contains(r#"name="upstream_id" value="7""#));
        assert!(html.contains("프로바이더 상세"));
    }

    #[test]
    fn provider_workspace_uses_left_selection_list_and_selected_detail() {
        let summary = db::StateSummary {
            clients: Vec::new(),
            model_aliases: Vec::new(),
            upstreams: vec![
                db::UpstreamSummary {
                    id: 1,
                    name: "first-provider".to_string(),
                    base_url: "https://first.example/v1".to_string(),
                    priority: 10,
                    enabled: true,
                    models: Vec::new(),
                    discovered_models: Vec::new(),
                    keys: Vec::new(),
                },
                db::UpstreamSummary {
                    id: 2,
                    name: "second-provider".to_string(),
                    base_url: "https://second.example/v1".to_string(),
                    priority: 20,
                    enabled: true,
                    models: Vec::new(),
                    discovered_models: Vec::new(),
                    keys: Vec::new(),
                },
            ],
        };

        let html = render_provider_workspace(&summary, Some(2));

        assert!(html.contains("provider-select-card selected"));
        assert!(html.contains(r#"href="/admin?provider=2#settings""#));
        assert!(html.contains("second-provider"));
        assert!(html.contains("프로바이더 상세"));
        assert!(!html.contains("등록된 프로바이더"));
        assert!(!html.contains("provider-list-panel"));
    }

    #[test]
    fn stats_panel_keeps_cache_reset_without_manual_health_check_button() {
        let html = render_stats_panel(&db::AdminStats {
            recent_requests: Vec::new(),
            client_token_stats: Vec::new(),
            key_stats: Vec::new(),
            health: db::AdminHealthSummary {
                total_keys: 0,
                enabled_keys: 0,
                ready_keys: 0,
                cached_keys: 0,
                disabled_keys: 0,
                recent_503: 0,
                recent_upstream_exhausted: 0,
                recent_5xx: 0,
                last_failure_at: None,
            },
        });

        assert!(html.contains("실패 캐시 비우기"));
        assert!(!html.contains("상태 확인"));
        assert!(!html.contains("/admin/health/check"));
    }

    #[test]
    fn admin_script_refreshes_provider_models_in_background() {
        let script = admin_js();

        assert!(script.contains("refreshProviderModels();"));
        assert!(script.contains("/admin/upstreams/fetch-models"));
        assert!(script.contains("data-model-fetch-meta"));
    }

    #[test]
    fn constant_time_equality_checks_full_value() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
