use super::{styles::admin_css, text::escape_html};

const ASSET_VERSION: &str = "20260616-simple";

#[derive(Debug, Clone, Copy)]
pub(super) struct PageMeta<'a> {
    pub(super) site_name: &'a str,
    pub(super) site_description: &'a str,
    pub(super) public_base_url: Option<&'a str>,
}

pub(super) fn page_shell(meta: &PageMeta<'_>, title: &str, body: &str) -> String {
    let escaped_title = escape_html(title);
    let escaped_description = escape_html(meta.site_description);
    let escaped_site_name = escape_html(meta.site_name);
    let preview_meta = meta
        .public_base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|origin| {
            let origin = escape_html(origin.trim_end_matches('/'));
            format!(
                r#"
  <meta property="og:url" content="{origin}/admin">
  <meta property="og:image" content="{origin}/og.png?v={asset_version}">
  <meta property="og:image:width" content="1200">
  <meta property="og:image:height" content="630">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="{title}">
  <meta name="twitter:description" content="{description}">
  <meta name="twitter:image" content="{origin}/og.png?v={asset_version}">"#,
                title = escaped_title,
                description = escaped_description,
                asset_version = ASSET_VERSION,
            )
        })
        .unwrap_or_default();
    format!(
        r##"<!doctype html>
<html lang="ko">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="{description}">
  <meta name="theme-color" content="#172033">
  <meta property="og:type" content="website">
  <meta property="og:site_name" content="{site_name}">
  <meta property="og:title" content="{title}">
  <meta property="og:description" content="{description}">{preview_meta}
  <link rel="icon" type="image/svg+xml" href="/favicon.svg?v={asset_version}">
  <link rel="alternate icon" href="/favicon.ico?v={asset_version}">
  <link rel="apple-touch-icon" href="/apple-touch-icon.png?v={asset_version}">
  <link rel="manifest" href="/site.webmanifest?v={asset_version}">
  <title>{title}</title>
  <style>{}</style>
</head>
<body>{}</body>
</html>"##,
        admin_css(),
        body,
        title = escaped_title,
        description = escaped_description,
        site_name = escaped_site_name,
        preview_meta = preview_meta,
        asset_version = ASSET_VERSION,
    )
}
