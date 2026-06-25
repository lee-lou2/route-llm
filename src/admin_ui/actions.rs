use super::*;

pub(super) async fn index(
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

pub(super) async fn login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AdminQuery>,
) -> Response {
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

pub(super) async fn login_post(
    State(state): State<Arc<AppState>>,
    Form(form): Form<LoginForm>,
) -> Response {
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

pub(super) async fn logout() -> Response {
    let mut response = redirect("/admin/login");
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_static(
            "route_llm_admin=; Path=/admin; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

pub(super) async fn generate_client(
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

pub(super) async fn generate_client_token(
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

pub(super) async fn delete_client(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_action(state, headers, form.id).await
}

pub(super) async fn delete_client_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_token_action(state, headers, form.id, form.client_id).await
}

pub(super) async fn add_upstream(
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

pub(super) async fn delete_upstream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_upstream_action(state, headers, form.id).await
}

pub(super) async fn fetch_upstream_models(
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

pub(super) async fn add_key(
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

pub(super) async fn delete_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_key_action(state, headers, form.id, form.provider_id).await
}

pub(super) async fn reset_key(
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

pub(super) async fn reset_all_keys(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::reset_all_key_health(&state.pool).await {
        Ok(count) => redirect_notice(&format!("실패 캐시를 초기화했습니다: {count}개 토큰")),
        Err(error) => redirect_error(&format!("실패 캐시 초기화 실패: {error}")),
    }
}

pub(super) async fn add_model(
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

pub(super) async fn reorder_keys(
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

pub(super) async fn delete_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_model_action(state, headers, form.id, form.provider_id).await
}

pub(super) async fn delete_alias_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_alias_route_action(state, headers, form.id, form.provider_id).await
}

pub(super) fn infer_target_type(public_model: &str) -> &'static str {
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

pub(super) fn generated_key_name(api_key: &str) -> String {
    let hash = db::hash_secret(api_key);
    let short_hash: String = hash.chars().take(12).collect();
    format!("key-{short_hash}")
}

pub(super) async fn refresh_upstream_models(
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

pub(super) async fn fetch_model_ids(
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

pub(super) fn upstream_models_url(base_url: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(base_url).context("provider base url must be absolute")?;
    let path = format!("{}/models", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

pub(super) async fn add_client_route(
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

pub(super) async fn delete_client_route(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    delete_client_route_action(state, headers, form.id, form.client_id).await
}

pub(super) async fn reorder_client_routes(
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

pub(super) async fn delete_client_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_client(&state.pool, id).await {
        Ok(_) => redirect_notice("클라이언트를 완전히 삭제했습니다"),
        Err(error) => redirect_error(&format!("클라이언트 삭제 실패: {error}")),
    }
}

pub(super) async fn delete_client_token_action(
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

pub(super) async fn delete_upstream_action(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: i64,
) -> Response {
    if let Some(response) = require_admin(&state, &headers) {
        return response;
    }
    match db::delete_upstream(&state.pool, id).await {
        Ok(_) => redirect_notice("프로바이더를 완전히 삭제했습니다"),
        Err(error) => redirect_error(&format!("프로바이더 삭제 실패: {error}")),
    }
}

pub(super) async fn delete_key_action(
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

pub(super) async fn delete_model_action(
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

pub(super) async fn delete_alias_route_action(
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

pub(super) async fn delete_client_route_action(
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

pub(super) fn parse_ids(value: &str) -> anyhow::Result<Vec<i64>> {
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
