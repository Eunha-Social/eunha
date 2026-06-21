//! ActivityPub representation of collections (Mastodon's FeaturedCollection).
//!
//! Serves a local account's collections as AP objects so remote servers can
//! discover and fetch them, and provides the activity builders used to
//! distribute collection changes to followers.
//!
//! The bidirectional feature-request / feature-authorization handshake (for
//! featuring *remote* accounts with their consent) is not yet implemented; only
//! locally-owned collections and their accepted items are federated outbound.

use axum::{
    extract::{Extension, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

use super::objects::CONTENT_TYPE;
use crate::{
    error::{AppError, AppResult},
    middleware::ResolvedInstance,
    state::AppState,
};

/// JSON-LD context for FeaturedCollection objects.
fn collection_context() -> Value {
    json!([
        "https://www.w3.org/ns/activitystreams",
        {
            "toot": "http://joinmastodon.org/ns#",
            "sensitive": "as:sensitive",
            "discoverable": "toot:discoverable",
            "Hashtag": "as:Hashtag",
            "featuredCollections": { "@id": "toot:featuredCollections", "@type": "@id" },
            "FeaturedCollection": "toot:FeaturedCollection",
            "FeaturedItem": "toot:FeaturedItem",
            "featuredObject": { "@id": "toot:featuredObject", "@type": "@id" },
        }
    ])
}

fn collection_uri(domain: &str, id: i64) -> String {
    format!("https://{domain}/collections/{id}")
}

fn item_uri(domain: &str, collection_id: i64, item_id: i64) -> String {
    format!("https://{domain}/collections/{collection_id}/items/{item_id}")
}

fn actor_uri(domain: &str, username: &str) -> String {
    format!("https://{domain}/users/{username}")
}

/// A single accepted item, ready for FeaturedItem serialization.
struct ItemRow {
    id: i64,
    account_uri: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// Fetch a collection's accepted items joined with each account's AP URI.
async fn accepted_items(state: &AppState, domain: &str, collection_id: i64) -> AppResult<Vec<ItemRow>> {
    let rows = sqlx::query!(
        r#"SELECT ci.id, ci.created_at,
                  a.uri AS "account_uri?", a.username, a.domain
           FROM collection_items ci
           JOIN accounts a ON a.id = ci.account_id
           WHERE ci.collection_id = $1 AND ci.state = 1
           ORDER BY ci.position ASC, ci.id ASC"#,
        collection_id,
    )
    .fetch_all(&state.db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            // Local accounts (domain NULL) may not have a stored uri; derive it.
            let account_uri = match (r.account_uri, r.domain) {
                (Some(uri), _) if !uri.is_empty() => uri,
                _ => actor_uri(domain, &r.username),
            };
            ItemRow {
                id: r.id,
                account_uri,
                created_at: r.created_at,
            }
        })
        .collect())
}

/// Build a FeaturedItem AP object (without `@context`, for embedding).
fn featured_item_object(domain: &str, collection_id: i64, item: &ItemRow) -> Value {
    json!({
        "id": item_uri(domain, collection_id, item.id),
        "type": "FeaturedItem",
        "featuredObject": item.account_uri,
        "published": item.created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    })
}

/// Loaded collection fields needed for AP serialization.
pub struct ApCollection {
    pub id: i64,
    pub owner_username: String,
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub sensitive: bool,
    pub discoverable: bool,
    pub url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Load a local collection (with its owner's username) for AP serialization.
pub async fn load_ap_collection(state: &AppState, id: i64) -> AppResult<Option<ApCollection>> {
    let row = sqlx::query!(
        r#"SELECT c.id, c.name, c.description, c.language, c.sensitive,
                  c.discoverable, c.url, c.created_at, c.updated_at, a.username
           FROM collections c
           JOIN accounts a ON a.id = c.account_id
           WHERE c.id = $1 AND c.local = true AND a.domain IS NULL"#,
        id,
    )
    .fetch_optional(&state.db)
    .await?;

    Ok(row.map(|r| ApCollection {
        id: r.id,
        owner_username: r.username,
        name: r.name,
        description: r.description,
        language: r.language,
        sensitive: r.sensitive,
        discoverable: r.discoverable,
        url: r.url,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }))
}

/// Build the FeaturedCollection AP object body (without `@context`).
pub async fn featured_collection_body(
    state: &AppState,
    domain: &str,
    c: &ApCollection,
) -> AppResult<Value> {
    let items = accepted_items(state, domain, c.id).await?;
    let ordered: Vec<Value> = items
        .iter()
        .map(|it| featured_item_object(domain, c.id, it))
        .collect();

    let mut obj = json!({
        "id": collection_uri(domain, c.id),
        "type": "FeaturedCollection",
        "name": c.name,
        "attributedTo": actor_uri(domain, &c.owner_username),
        "url": c.url.clone().unwrap_or_else(|| collection_uri(domain, c.id)),
        "sensitive": c.sensitive,
        "discoverable": c.discoverable,
        "published": c.created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "updated": c.updated_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "totalItems": ordered.len(),
        "orderedItems": ordered,
    });

    if let Some(desc) = &c.description {
        if let Some(lang) = &c.language {
            obj["summaryMap"] = json!({ lang: desc });
        } else {
            obj["summary"] = json!(desc);
        }
    }

    Ok(obj)
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

/// GET /collections/{id} — the FeaturedCollection AP object.
pub async fn get_collection(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    let c = load_ap_collection(&state, id).await?.ok_or(AppError::NotFound)?;
    let mut body = featured_collection_body(&state, &instance.domain, &c).await?;
    body["@context"] = collection_context();

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response())
}

/// GET /users/{username}/collections — an OrderedCollection of the account's
/// FeaturedCollection object URIs.
pub async fn get_account_collections(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    let account = sqlx::query!(
        "SELECT id FROM accounts WHERE username = $1 AND domain IS NULL",
        username,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let ids = sqlx::query_scalar!(
        "SELECT id FROM collections WHERE account_id = $1 AND discoverable = true ORDER BY created_at DESC",
        account.id,
    )
    .fetch_all(&state.db)
    .await?;

    let items: Vec<String> = ids
        .into_iter()
        .map(|id| collection_uri(&instance.domain, id))
        .collect();

    let body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}/collections", actor_uri(&instance.domain, &username)),
        "type": "OrderedCollection",
        "totalItems": items.len(),
        "orderedItems": items,
    });

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response())
}

// ── Activity builders (for outbound distribution to followers) ─────────────────

/// `Add(FeaturedCollection)` — a new collection was created.
pub fn add_collection_activity(domain: &str, owner_username: &str, collection_obj: Value) -> Value {
    let actor = actor_uri(domain, owner_username);
    json!({
        "@context": collection_context(),
        "type": "Add",
        "actor": actor,
        "target": format!("{actor}/collections"),
        "object": collection_obj,
    })
}

/// `Update(FeaturedCollection)` — a collection's metadata or items changed.
pub fn update_collection_activity(
    domain: &str,
    owner_username: &str,
    collection_id: i64,
    updated_unix: i64,
    collection_obj: Value,
) -> Value {
    let actor = actor_uri(domain, owner_username);
    json!({
        "@context": collection_context(),
        "id": format!("{}#updates/{}", collection_uri(domain, collection_id), updated_unix),
        "type": "Update",
        "actor": actor,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": collection_obj,
    })
}

/// `Remove` — a collection was deleted.
pub fn remove_collection_activity(domain: &str, owner_username: &str, collection_id: i64) -> Value {
    let actor = actor_uri(domain, owner_username);
    json!({
        "@context": collection_context(),
        "type": "Remove",
        "actor": actor,
        "target": format!("{actor}/collections"),
        "object": collection_uri(domain, collection_id),
    })
}
