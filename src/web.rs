/// Serves eunha's own web frontend: a React Router (Vite) single-page app that
/// is a first-party client of the Mastodon Client-to-Server (C2S) REST API.
///
/// The built SPA lives in `frontend/dist`. Static assets (hashed JS/CSS, fonts,
/// images, manifest, etc.) are served directly; every other path falls back to
/// `index.html` so the client-side router can take over.
use axum::{
    extract::State,
    http::{header, StatusCode, Uri},
    response::{Html, IntoResponse, Response},
};

use crate::state::AppState;

const DIST: &str = "frontend/dist";

pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
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

    serve_index(&state).await
}

async fn serve_index(state: &AppState) -> Response {
    let Ok(html) = tokio::fs::read_to_string(format!("{DIST}/index.html")).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The eunha web frontend is not built yet.",
        )
            .into_response();
    };

    let html = html.replace(
        "<title>eunha</title>",
        &format!("<title>{}</title>", escape_html_text(&state.instance.title)),
    );

    Html(html).into_response()
}

fn escape_html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
