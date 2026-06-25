use super::*;

pub(super) fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    admin_disabled_response(state)
        .or_else(|| (!is_authenticated(state, headers)).then(|| redirect("/admin/login")))
}

pub(super) fn admin_disabled_response(state: &AppState) -> Option<Response> {
    state
        .config
        .admin
        .password_hash
        .is_none()
        .then(|| disabled_page(&state.config.admin).into_response())
}

pub(super) fn is_authenticated(state: &AppState, headers: &HeaderMap) -> bool {
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

pub(super) fn disabled_page(config: &AdminConfig) -> (StatusCode, Html<String>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Html(page_shell(
            &page_meta(config),
            "관리자 비활성화",
            r#"<main class="login"><h1>관리자 비활성화</h1><p><code>ROUTE_LLM_ADMIN_PASSWORD</code>가 필요합니다. 기존 launchd 설정과의 호환을 위해 <code>API_ROUTER_ADMIN_PASSWORD</code>도 fallback으로 허용됩니다.</p></main>"#,
        )),
    )
}

pub(super) fn redirect(path: &str) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    response
        .headers_mut()
        .insert(LOCATION, HeaderValue::from_str(path).unwrap());
    response
}

pub(super) fn redirect_notice(message: &str) -> Response {
    redirect(&format!("/admin?notice={}", percent_encode(message)))
}

pub(super) fn redirect_error(message: &str) -> Response {
    redirect(&format!("/admin?error={}", percent_encode(message)))
}

pub(super) fn redirect_notice_client(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&notice={}#client-routing",
        percent_encode(message)
    ))
}

pub(super) fn redirect_notice_client_token(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&token_client={client_id}&notice={}#clients",
        percent_encode(message)
    ))
}

pub(super) fn redirect_error_client(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&error={}#client-routing",
        percent_encode(message)
    ))
}

pub(super) fn redirect_error_client_token(message: &str, client_id: i64) -> Response {
    redirect(&format!(
        "/admin?client={client_id}&token_client={client_id}&error={}#clients",
        percent_encode(message)
    ))
}

pub(super) fn redirect_notice_provider(message: &str, provider_id: i64) -> Response {
    redirect(&format!(
        "/admin?provider={provider_id}&notice={}#settings",
        percent_encode(message)
    ))
}

pub(super) fn redirect_error_provider(message: &str, provider_id: i64) -> Response {
    redirect(&format!(
        "/admin?provider={provider_id}&error={}#settings",
        percent_encode(message)
    ))
}

pub(super) fn redirect_login_error(message: &str) -> Response {
    redirect(&format!("/admin/login?error={}", percent_encode(message)))
}

pub(super) fn page_meta(config: &AdminConfig) -> PageMeta<'_> {
    PageMeta {
        site_name: &config.site_name,
        site_description: &config.site_description,
        public_base_url: config.public_base_url.as_deref(),
    }
}

pub(super) fn display_base_url(config: &AdminConfig) -> String {
    config
        .public_base_url
        .as_deref()
        .unwrap_or("http://127.0.0.1:8080")
        .trim_end_matches('/')
        .to_string()
}

pub(super) fn display_public_endpoint(config: &AdminConfig, public_prefix: &str) -> String {
    let base_url = display_base_url(config);
    let prefix = public_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        base_url
    } else {
        format!("{base_url}{prefix}")
    }
}
