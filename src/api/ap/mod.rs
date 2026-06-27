pub mod collections;
pub mod inbox;
pub mod note;
pub mod objects;
pub mod outbox;

use axum::{
    routing::{get, post},
    Router,
};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/{username}", get(objects::get_actor))
        .route("/users/{username}/inbox", post(inbox::shared_inbox))
        .route("/users/{username}/outbox", get(outbox::get_outbox))
        .route("/users/{username}/statuses/{id}", get(objects::get_status))
        .route("/users/{username}/statuses/{id}/activity", get(objects::get_status_activity))
        .route("/users/{username}/followers", get(collections::get_followers))
        .route("/users/{username}/following", get(collections::get_following))
        .route("/users/{username}/collections/featured", get(collections::get_featured))
        .route("/users/{username}/collections", get(collections::get_account_collections))
        .route(
            "/users/{username}/feature_authorizations/{id}",
            get(collections::get_feature_authorization),
        )
        .route(
            "/users/{username}/quote_authorizations/{id}",
            get(collections::get_quote_authorization),
        )
        .route("/collections/{id}", get(collections::get_collection))
        .route("/actor", get(objects::get_instance_actor))
        .route("/inbox", post(inbox::shared_inbox))
}
