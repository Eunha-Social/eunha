pub mod collections;
pub mod inbox;
pub mod note;
pub mod objects;
pub mod outbox;

use crate::state::AppState;
use axum::{
    routing::{get, post},
    Router,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/{username}", get(objects::get_actor))
        .route("/users/{username}/inbox", post(inbox::shared_inbox))
        .route("/users/{username}/outbox", get(outbox::get_outbox))
        .route("/users/{username}/statuses/{id}", get(objects::get_status))
        .route(
            "/users/{username}/statuses/{id}/activity",
            get(objects::get_status_activity),
        )
        .route(
            "/users/{username}/followers",
            get(collections::get_followers),
        )
        .route(
            "/users/{username}/following",
            get(collections::get_following),
        )
        .route(
            "/users/{username}/collections/featured",
            get(collections::get_featured),
        )
        .route(
            "/users/{username}/collections",
            get(collections::get_account_collections),
        )
        .route(
            "/users/{username}/feature_authorizations/{id}",
            get(collections::get_feature_authorization),
        )
        .route(
            "/users/{username}/quote_authorizations/{id}",
            get(collections::get_quote_authorization),
        )
        // Numeric AP-ID scheme (Mastodon `numeric_ap_id`): actors served under
        // /ap/users/{id}. Sub-resources mirror the username routes above.
        .route("/ap/users/{id}", get(objects::get_actor_by_id))
        .route("/ap/users/{id}/inbox", post(inbox::shared_inbox))
        .route("/ap/users/{id}/outbox", get(outbox::get_outbox_by_id))
        .route(
            "/ap/users/{id}/statuses/{status_id}",
            get(objects::get_status_by_id),
        )
        .route(
            "/ap/users/{id}/statuses/{status_id}/activity",
            get(objects::get_status_activity_by_id),
        )
        .route(
            "/ap/users/{id}/followers",
            get(collections::get_followers_by_id),
        )
        .route(
            "/ap/users/{id}/following",
            get(collections::get_following_by_id),
        )
        // The actor advertises `featured` and `featuredCollections` beneath its
        // own URI, so under this scheme those are the URLs a peer fetches.
        // Without these two routes the request fell through to the SPA
        // fallback: Mastodon asked for a collection on every federation
        // handshake and got an HTML page, or a 503 where the frontend was not
        // built. Harmless to the handshake, which is why it went unnoticed.
        .route(
            "/ap/users/{id}/collections/featured",
            get(collections::get_featured_by_id),
        )
        .route(
            "/ap/users/{id}/collections",
            get(collections::get_account_collections_by_id),
        )
        .route("/collections/{id}", get(collections::get_collection))
        .route("/actor", get(objects::get_instance_actor))
        .route("/inbox", post(inbox::shared_inbox))
}
