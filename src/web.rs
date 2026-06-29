/// Serves eunha's own web frontend: a React Router (Vite) single-page app that
/// is a first-party client of the Mastodon Client-to-Server (C2S) REST API.
///
/// The built SPA lives in `frontend/dist`. Static assets (hashed JS/CSS, fonts,
/// images, manifest, etc.) are served directly; every other path falls back to
/// `index.html` so the client-side router can take over.
use axum::{
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
};

const DIST: &str = "frontend/dist";

pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Serve static assets directly. Path traversal guard: reject anything with "..".
    if !path.is_empty() && !path.contains("..") {
        let file_path = format!("{DIST}/{path}");
        if let Ok(bytes) = tokio::fs::read(&file_path).await {
            let mime = mime_guess::from_path(&file_path)
                .first_or_octet_stream()
                .to_string();
            return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
        }
    }

    serve_index().await
}

async fn serve_index() -> Response {
    let Ok(html) = tokio::fs::read_to_string(format!("{DIST}/index.html")).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The eunha web frontend is not built yet.",
        )
            .into_response();
    };

    Html(html).into_response()
}
