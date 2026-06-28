pub mod api;
pub mod background;
pub mod federation;
pub mod config;
pub mod feed;
pub mod snowflake;
pub mod crypto;
pub mod db;
pub mod elk;
pub mod email;
pub mod error;
pub mod locale;
pub mod media;
pub mod middleware;
pub mod preview_card;
pub mod push;
pub mod state;
pub mod streaming;
pub mod templates;
pub mod well_known;

use axum::{extract::Request, middleware as axum_middleware, response::IntoResponse, Router};
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer};
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

pub fn build_app(state: state::AppState) -> Router {
    let compressed = Router::new()
        .merge(well_known::router())
        .merge(api::mastodon::router(state.clone()))
        .merge(api::account::router(state.clone()))
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
                elk::serve(uri).await
            }
        }))
        .layer(CompressionLayer::new());

    Router::new()
        .merge(compressed)
        // Streaming WebSocket must be outside CompressionLayer to avoid body wrapping.
        .merge(api::mastodon::streaming_router())
        .layer(axum_middleware::from_fn(middleware::log_failures))
        .layer(axum_middleware::from_fn_with_state(state.clone(), middleware::authenticate))
        .layer(axum_middleware::from_fn_with_state(state.clone(), middleware::resolve_instance))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Wrap the app with per-IP rate limiting.
///
/// Applied at the binary entrypoint rather than in [`build_app`] because it
/// relies on real peer/`X-Forwarded-For` information that the in-process test
/// harness does not provide. [`SmartIpKeyExtractor`] reads `X-Forwarded-For` /
/// `Forwarded` (set by the Cloudflare tunnel in production) and falls back to
/// the connection peer address.
///
/// Limits are deliberately generous — defense-in-depth behind Cloudflare, not a
/// primary control — so ordinary browsing and federation bursts pass freely:
/// a 300-request burst, replenishing ~20 requests/sec per client IP.
pub fn with_rate_limit(app: Router) -> Router {
    let config = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(50)
            .burst_size(300)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("invalid rate-limit configuration"),
    );

    // Evict idle per-IP buckets so the limiter's memory stays bounded.
    let limiter = config.limiter().clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            limiter.retain_recent();
        }
    });

    app.layer(GovernorLayer::new(config))
}
