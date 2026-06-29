use axum::{
    extract::{Extension, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

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

/// Serve a local status as a bare ActivityPub `Note` object — username scheme
/// (`/users/{username}/statuses/{id}`).
pub async fn get_status(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((username, id)): Path<(String, i64)>,
) -> AppResult<Response> {
    let bundle = status_bundle(&state, &instance.domain, AccountRef::Username(&username), id).await?;
    Ok(note_response(bundle.into_note()))
}

/// Numeric-scheme status (`/ap/users/{account_id}/statuses/{id}`).
pub async fn get_status_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((account_id, id)): Path<(i64, i64)>,
) -> AppResult<Response> {
    let bundle = status_bundle(&state, &instance.domain, AccountRef::Id(account_id), id).await?;
    Ok(note_response(bundle.into_note()))
}

/// Serve the `Create(Note)` wrapper at `{status}/activity` — username scheme.
pub async fn get_status_activity(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((username, id)): Path<(String, i64)>,
) -> AppResult<Response> {
    let bundle = status_bundle(&state, &instance.domain, AccountRef::Username(&username), id).await?;
    Ok(note_response(bundle.into_create()))
}

/// Numeric-scheme `{status}/activity`.
pub async fn get_status_activity_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((account_id, id)): Path<(i64, i64)>,
) -> AppResult<Response> {
    let bundle = status_bundle(&state, &instance.domain, AccountRef::Id(account_id), id).await?;
    Ok(note_response(bundle.into_create()))
}

fn note_response(body: Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response()
}

/// How a local account is addressed in a request path: by `username`
/// (`/users/...`) or by numeric `id` (`/ap/users/...`).
#[derive(Clone, Copy)]
pub enum AccountRef<'a> {
    Username(&'a str),
    Id(i64),
}

/// Load a local account addressed by either scheme.
pub async fn load_local_account(
    state: &AppState,
    who: AccountRef<'_>,
) -> AppResult<crate::db::models::Account> {
    let account = match who {
        AccountRef::Username(username) => sqlx::query_as!(
            crate::db::models::Account,
            "SELECT * FROM accounts WHERE username = $1 AND domain IS NULL",
            username,
        )
        .fetch_optional(&state.db)
        .await?,
        AccountRef::Id(id) => sqlx::query_as!(
            crate::db::models::Account,
            "SELECT * FROM accounts WHERE id = $1 AND domain IS NULL",
            id,
        )
        .fetch_optional(&state.db)
        .await?,
    };
    account.ok_or(AppError::NotFound)
}

/// Load a status bundle, enforcing that it belongs to the addressed account and
/// is publicly dereferenceable (public or unlisted). Private/direct posts are
/// not served over unauthenticated AP GET.
async fn status_bundle(
    state: &AppState,
    domain: &str,
    who: AccountRef<'_>,
    id: i64,
) -> AppResult<super::note::NoteBundle> {
    let account = load_local_account(state, who).await?;
    let owner_ok = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM statuses s
             WHERE s.id = $1 AND s.account_id = $2
               AND s.deleted_at IS NULL AND s.visibility IN (0, 1)
           )"#,
        id,
        account.id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if !owner_ok {
        return Err(AppError::NotFound);
    }
    super::note::build_note(state, domain, id)
        .await?
        .ok_or(AppError::NotFound)
}

/// Serve the actor — username scheme (`/users/{username}`).
pub async fn get_actor(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    let account = load_local_account(&state, AccountRef::Username(&username)).await?;
    let actor = actor_json(&state, &instance.domain, &account).await?;
    Ok(note_response(actor))
}

/// Serve the actor — numeric scheme (`/ap/users/{id}`).
pub async fn get_actor_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let account = load_local_account(&state, AccountRef::Id(id)).await?;
    let actor = actor_json(&state, &instance.domain, &account).await?;
    Ok(note_response(actor))
}

pub async fn actor_json(
    state: &AppState,
    domain: &str,
    account: &crate::db::models::Account,
) -> AppResult<Value> {
    let base = format!("https://{}", domain);
    let actor_url = crate::federation::tag::account_uri_of(domain, account);

    // Account migration metadata: aliases (alsoKnownAs) + movedTo target URI.
    let also_known_as: Vec<String> = sqlx::query_scalar!(
        "SELECT uri FROM account_aliases WHERE account_id = $1 ORDER BY created_at",
        account.id,
    )
    .fetch_all(&state.db)
    .await?;
    let moved_to: Option<String> = if let Some(moved_id) = account.moved_to_account_id {
        sqlx::query_scalar!("SELECT uri FROM accounts WHERE id = $1", moved_id)
            .fetch_optional(&state.db)
            .await?
            .filter(|u| !u.is_empty())
    } else {
        None
    };

    let has_avatar = account
        .avatar_file_name
        .as_ref()
        .is_some_and(|s| !s.is_empty())
        || account
            .avatar_remote_url
            .as_ref()
            .is_some_and(|s| !s.is_empty());
    let has_header = account
        .header_file_name
        .as_ref()
        .is_some_and(|s| !s.is_empty())
        || !account.header_remote_url.is_empty();
    let avatar_url = crate::api::mastodon::convert::account_avatar_url_for(account);
    let header_url = crate::api::mastodon::convert::account_header_url_for(account);

    let actor = json!({
        "@context": [
            "https://www.w3.org/ns/activitystreams",
            "https://w3id.org/security/v1",
            {
                "manuallyApprovesFollowers": "as:manuallyApprovesFollowers",
                "alsoKnownAs": { "@id": "as:alsoKnownAs", "@type": "@id" },
                "movedTo": { "@id": "as:movedTo", "@type": "@id" },
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
        "alsoKnownAs": also_known_as,
        "movedTo": moved_to,
    });

    Ok(actor)
}
