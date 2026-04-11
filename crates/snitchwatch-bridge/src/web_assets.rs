//! Compile-time embedded `web/` directory served by the axum router.
//!
//! `rust-embed` walks `web/` at build time, hashes each file, and stores it
//! in the resulting binary. At runtime we resolve a request path to an
//! embedded asset, sniff the content type from the extension, and stream
//! the bytes back. Unknown paths fall through to `index.html` so the SPA's
//! client-side routing keeps working.

use axum::{
    body::Body,
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../web/"]
pub(crate) struct WebAssets;

pub async fn serve_index() -> Response {
    serve_path("index.html")
}

pub async fn serve_asset(Path(path): Path<String>) -> Response {
    serve_path(&path)
}

pub async fn serve_fallback() -> Response {
    // SPA fallback — anything we don't recognize gets the index so client-side
    // routing in app.js handles it.
    serve_path("index.html")
}

fn serve_path(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(asset) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let header_value = HeaderValue::from_str(mime.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, header_value)
                .body(Body::from(asset.data.into_owned()))
                .expect("response builder cannot fail with static status and validated header")
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .expect("response builder cannot fail with static status and static body"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn serve_index_returns_html_with_correct_mime() {
        let response = serve_index().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/html"
        );
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("Snitchwatch"), "rebrand should be applied");
        assert!(!body_str.contains("Little Snitch"), "no leftover LS branding");
    }

    #[tokio::test]
    async fn serve_asset_returns_javascript_for_app_js() {
        let response = serve_asset(Path("js/app.js".to_string())).await;
        assert_eq!(response.status(), StatusCode::OK);
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        // mime_guess returns application/javascript or text/javascript depending on
        // the table version — accept either to keep the test resilient.
        assert!(
            mime.contains("javascript"),
            "got {mime}, expected a javascript mime"
        );
    }

    #[tokio::test]
    async fn missing_asset_falls_back_to_404() {
        let response = serve_asset(Path("does/not/exist.txt".to_string())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fallback_returns_index_for_spa_routes() {
        let response = serve_fallback().await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("<html"));
    }
}
