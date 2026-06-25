use super::*;

pub(super) fn render_stats_panel(stats: &db::AdminStats) -> String {
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

pub(super) fn render_health_summary(health: &db::AdminHealthSummary) -> String {
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

pub(super) fn render_recent_requests(requests: &[db::RecentRequestSummary]) -> String {
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

pub(super) fn render_request_client(request: &db::RecentRequestSummary) -> String {
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

pub(super) fn render_token_usage(
    input: Option<i64>,
    output: Option<i64>,
    total: Option<i64>,
) -> String {
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

pub(super) fn render_duration_ms(duration_ms: i64) -> String {
    if duration_ms >= 1000 {
        format!("{:.1}s", duration_ms as f64 / 1000.0)
    } else {
        format!("{duration_ms}ms")
    }
}

pub(super) fn format_number(value: i64) -> String {
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

pub(super) fn render_request_upstream(request: &db::RecentRequestSummary) -> String {
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

pub(super) fn render_client_token_stats(stats: &[db::ClientTokenUsageStats]) -> String {
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

pub(super) fn render_status_value(status: Option<i64>, outcome: &str) -> String {
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

pub(super) fn render_key_stats(stats: &[db::KeyUsageStats]) -> String {
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
