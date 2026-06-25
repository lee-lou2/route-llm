use super::*;

pub(super) fn render_login(context: AdminRenderContext<'_>, error: Option<String>) -> String {
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

pub(super) fn render_dashboard(
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

pub(super) fn render_provider_workspace(
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

pub(super) fn render_provider_side_panel(
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

pub(super) fn render_client_workspace(
    summary: &db::StateSummary,
    selected_client_id: Option<i64>,
) -> String {
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

pub(super) fn render_client_list(
    summary: &db::StateSummary,
    selected_client_id: Option<i64>,
) -> String {
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

pub(super) fn render_selected_client_routes(
    client: &db::ClientSummary,
    summary: &db::StateSummary,
) -> String {
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

pub(super) fn render_client_token_button(client: &db::ClientSummary) -> String {
    let token_count = client.tokens.len();
    let title = format!("클라이언트 토큰 {token_count}개를 확인하고 관리합니다");
    format!(
        r#"<button type="button" class="secondary compact-action" data-open-token-modal="{}" title="{}">토큰 확인</button>"#,
        client.id,
        escape_html(&title),
    )
}

pub(super) fn render_client_token_modal(client: &db::ClientSummary) -> String {
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

pub(super) fn render_client_token_rows(client: &db::ClientSummary) -> String {
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

pub(super) fn alias_has_enabled_routes(alias: &db::ModelAliasSummary) -> bool {
    alias.enabled && alias.routes.iter().any(|route| route.enabled)
}

pub(super) fn alias_visible_for_client(
    alias: &db::ModelAliasSummary,
    client: &db::ClientSummary,
) -> bool {
    alias_has_enabled_routes(alias)
        || client
            .routes
            .iter()
            .any(|route| route.public_model == alias.public_model && route.enabled)
}

pub(super) fn render_client_alias_row(
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

pub(super) fn render_model_select_options(
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

pub(super) fn render_default_route_summary(alias: &db::ModelAliasSummary) -> String {
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

pub(super) fn render_client_route_pill(
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

pub(super) fn client_route_buttons(id: i64, client_id: i64) -> String {
    format!(
        r#"<form method="post" action="/admin/routes/delete" class="inline"><input type="hidden" name="id" value="{id}"><input type="hidden" name="client_id" value="{client_id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

pub(super) fn render_provider_list(
    summary: &db::StateSummary,
    selected_provider_id: Option<i64>,
) -> String {
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

pub(super) fn render_selected_provider_detail(
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

pub(super) fn render_model_name_field(upstream: &db::UpstreamSummary) -> String {
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

pub(super) fn render_model_fetch_meta(upstream: &db::UpstreamSummary) -> String {
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

pub(super) fn render_provider_key_list(upstream: &db::UpstreamSummary) -> String {
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

pub(super) fn render_provider_model_list(
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

pub(super) fn render_alias_suggestions(aliases: &[db::ModelAliasSummary]) -> String {
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

pub(super) fn render_alias_assignment_tags(
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
