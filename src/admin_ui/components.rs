use super::*;

pub(super) fn render_alerts(notice: Option<String>, error: Option<String>) -> String {
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

pub(super) fn status_badge(enabled: bool) -> String {
    if enabled {
        String::new()
    } else {
        r#"<span class="badge disabled">비활성</span>"#.to_string()
    }
}

pub(super) fn id_buttons(delete_action: &str, id: i64) -> String {
    format!(
        r#"<form method="post" action="{delete_action}" class="inline"><input type="hidden" name="id" value="{id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

pub(super) fn provider_scoped_id_button(delete_action: &str, id: i64, provider_id: i64) -> String {
    format!(
        r#"<form method="post" action="{delete_action}" class="inline"><input type="hidden" name="id" value="{id}"><input type="hidden" name="provider_id" value="{provider_id}"><button class="danger" type="submit">삭제</button></form>"#
    )
}

pub(super) fn provider_key_delete_button(id: i64, provider_id: i64) -> String {
    provider_scoped_id_button("/admin/keys/delete", id, provider_id)
}

pub(super) fn key_stat_buttons(id: i64) -> String {
    let delete = id_buttons("/admin/keys/delete", id);
    format!(
        r#"{delete}<form method="post" action="/admin/keys/reset" class="inline"><input type="hidden" name="id" value="{id}"><button class="secondary" type="submit">초기화</button></form>"#
    )
}
