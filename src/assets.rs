use axum::{
    Router,
    body::Body,
    http::{Response, StatusCode},
    response::IntoResponse,
    routing::get,
};

const FAVICON_SVG: &[u8] = include_bytes!("../assets/favicon.svg");
const FAVICON_ICO: &[u8] = include_bytes!("../assets/favicon.ico");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../assets/apple-touch-icon.png");
const OG_IMAGE: &[u8] = include_bytes!("../assets/og.png");
const SITE_WEBMANIFEST: &[u8] = include_bytes!("../assets/site.webmanifest");
const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /admin\nAllow: /og.png\nAllow: /favicon.svg\n";

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/favicon.svg", get(favicon_svg))
        .route("/favicon.ico", get(favicon_ico))
        .route("/apple-touch-icon.png", get(apple_touch_icon))
        .route("/og.png", get(og_image))
        .route("/site.webmanifest", get(site_webmanifest))
        .route("/robots.txt", get(robots_txt))
}

pub async fn favicon_svg() -> impl IntoResponse {
    static_asset(FAVICON_SVG, "image/svg+xml; charset=utf-8")
}

pub async fn favicon_ico() -> impl IntoResponse {
    static_asset(FAVICON_ICO, "image/x-icon")
}

pub async fn apple_touch_icon() -> impl IntoResponse {
    static_asset(APPLE_TOUCH_ICON, "image/png")
}

pub async fn og_image() -> impl IntoResponse {
    static_asset(OG_IMAGE, "image/png")
}

pub async fn site_webmanifest() -> impl IntoResponse {
    static_asset(SITE_WEBMANIFEST, "application/manifest+json; charset=utf-8")
}

pub async fn robots_txt() -> impl IntoResponse {
    static_asset(ROBOTS_TXT.as_bytes(), "text/plain; charset=utf-8")
}

fn static_asset(bytes: &'static [u8], content_type: &'static str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .header("cache-control", "public, max-age=86400")
        .body(Body::from(bytes))
        .expect("static asset response should build")
}
