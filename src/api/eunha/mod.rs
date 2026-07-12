//! eunha-specific HTTP APIs that have no Mastodon C2S equivalent. Kept separate
//! from `api::mastodon` so the Mastodon-compatible surface stays clean.
use axum::{routing::get, Router};

use crate::state::AppState;

pub mod invite_tree;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/eunha/v1/invite_tree", get(invite_tree::invite_tree))
        .with_state(state)
}
