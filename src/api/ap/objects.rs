use axum::{
    extract::{Extension, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::{
    error::{AppError, AppResult},
    middleware::ResolvedInstance,
    state::AppState,
};

pub const ACTIVITY_STREAMS: &str = "application/activity+json";
pub const CONTENT_TYPE: &str = "application/activity+json; charset=utf-8";

/// Serve the instance actor at `/actor`: an Application actor whose public key
/// remote servers fetch to verify our signed authorized-fetch GET requests.
pub async fn get_instance_actor(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
) -> AppResult<Response> {
    let public_key = crate::federation::instance_actor::public_key(&state)
        .await
        .map_err(AppError::Internal)?;
    let actor_url = crate::federation::instance_actor::actor_url(&instance.domain);

    let actor = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
        ],
        "id": actor_url,
        "type": "Application",
        "preferredUsername": instance.domain,
        "inbox": format!("https://{}/inbox", instance.domain),
        "url": actor_url,
        "manuallyApprovesFollowers": true,
        "publicKey": {
            "id": format!("{actor_url}#main-key"),
            "owner": actor_url,
            "publicKeyPem": public_key,
        },
    });

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(actor),
    )
        .into_response())
}

pub async fn get_actor(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    let account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE username = $1 AND domain IS NULL",
        username,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let base = format!("https://{}", instance.domain);
    let actor_url = format!("{}/users/{}", base, username);

    let has_avatar = account.avatar_file_name.as_ref().map_or(false, |s| !s.is_empty())
        || account.avatar_remote_url.as_ref().map_or(false, |s| !s.is_empty());
    let has_header = account.header_file_name.as_ref().map_or(false, |s| !s.is_empty())
        || account.header_remote_url.is_empty() == false;
    let avatar_url = crate::api::mastodon::convert::account_avatar_url_for(&account);
    let header_url = crate::api::mastodon::convert::account_header_url_for(&account);

    let actor = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
            {
                "manuallyApprovesFollowers": "as:manuallyApprovesFollowers",
                "toot": "http://joinmastodon.org/ns#",
                "featured": { "@id": "toot:featured", "@type": "@id" },
                "featuredCollections": { "@id": "toot:featuredCollections", "@type": "@id" },
                "discoverable": "toot:discoverable",
                "indexable": "toot:indexable",
                "fep": "https://w3id.org/fep/044f#",
                "quote": { "@id": "fep:quote", "@type": "@id" },
                "quoteUrl": { "@id": "fep:quote", "@type": "@id" },
            }
        ],
        "id": actor_url,
        "type": "Person",
        "following": format!("{}/following", actor_url),
        "followers": format!("{}/followers", actor_url),
        "inbox": format!("{}/inbox", actor_url),
        "outbox": format!("{}/outbox", actor_url),
        "featured": format!("{}/collections/featured", actor_url),
        "featuredCollections": format!("{}/collections", actor_url),
        "preferredUsername": account.username,
        "name": account.display_name,
        "summary": account.note,
        "url": account.url,
        "manuallyApprovesFollowers": account.locked,
        "discoverable": account.discoverable,
        "indexable": account.indexable,
        "published": account.created_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "icon": if has_avatar { Some(json!({ "type": "Image", "url": avatar_url })) } else { None },
        "image": if has_header { Some(json!({ "type": "Image", "url": header_url })) } else { None },
        "publicKey": {
            "id": format!("{}#main-key", actor_url),
            "owner": actor_url,
            "publicKeyPem": account.public_key,
        },
        "endpoints": {
            "sharedInbox": format!("{}/inbox", base),
        },
    });

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(actor),
    )
        .into_response())
}
