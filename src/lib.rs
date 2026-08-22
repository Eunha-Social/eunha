pub mod api;
pub mod background;
pub mod config;
pub mod counters;
pub mod crypto;
pub mod db;
pub mod delete_account;
pub mod email;
pub mod error;
pub mod federation;
pub mod feed;
pub mod link_verification;
pub mod locale;
pub mod media;
pub mod middleware;
pub mod preview_card;
pub mod push;
pub mod rails_encryption;
pub mod schema_check;
pub mod snowflake;
pub mod software_updates;
pub mod state;
pub mod streaming;
pub mod templates;
pub mod upstream;
pub mod version;
pub mod web;
pub mod well_known;

use axum::{extract::Request, middleware as axum_middleware, response::IntoResponse, Router};
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

pub fn build_app(state: state::AppState) -> Router {
    let fallback_state = state.clone();
    let compressed = Router::new()
        .merge(well_known::router())
        .merge(api::mastodon::router(state.clone()))
        .merge(api::account::router(state.clone()))
        .merge(api::eunha::router(state.clone()))
        .merge(api::ap::router())
        .fallback(axum::routing::any(move |req: Request| async move {
            let uri = req.uri().clone();
            if uri.path().starts_with("/api/") {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    axum::Json(serde_json::json!({"error": "not found"})),
                )
                    .into_response()
            } else {
                web::serve(axum::extract::State(fallback_state.clone()), uri).await
            }
        }))
        .layer(CompressionLayer::new());

    Router::new()
        .merge(compressed)
        // Streaming WebSocket must be outside CompressionLayer to avoid body wrapping.
        .merge(api::mastodon::streaming_router())
        .layer(axum_middleware::from_fn(middleware::log_failures))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::resolve_instance,
        ))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
