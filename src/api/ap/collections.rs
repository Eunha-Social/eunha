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

use super::objects::{AccountRef, CONTENT_TYPE};
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

/// Username-scheme local actor URI, for collection/featured contexts that only
/// carry the username. (Numeric-scheme accounts have no local collections to
/// serve, so the username form is sufficient here.)
fn actor_uri(domain: &str, username: &str) -> String {
    format!("https://{domain}/users/{username}")
}

/// Resolve a member account's actor URI. Local accounts use their id_scheme-aware
/// canonical URI (the stored `uri` is empty for Mastodon-imported locals); remote
/// accounts use their stored `uri`.
fn resolve_actor_uri(
    domain: &str,
    stored: Option<String>,
    is_local: bool,
    id: i64,
    id_scheme: Option<i32>,
    username: &str,
) -> String {
    if is_local {
        crate::federation::tag::account_uri(domain, id, id_scheme, username)
    } else {
        stored.unwrap_or_default()
    }
}

/// A single accepted item, ready for FeaturedItem serialization.
struct ItemRow {
    id: i64,
    account_uri: String,
    created_at: chrono::NaiveDateTime,
}

/// Fetch a collection's accepted items joined with each account's AP URI.
async fn accepted_items(
    state: &AppState,
    domain: &str,
    collection_id: i64,
) -> AppResult<Vec<ItemRow>> {
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
        "published": item.created_at.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
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
        "published": c.created_at.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "updated": c.updated_at.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
    let c = load_ap_collection(&state, id)
        .await?
        .ok_or(AppError::NotFound)?;
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
    account_collections(&state, &instance.domain, AccountRef::Username(&username)).await
}

/// Numeric-scheme collections (`/ap/users/{id}/collections`).
pub async fn get_account_collections_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    account_collections(&state, &instance.domain, AccountRef::Id(id)).await
}

async fn account_collections(
    state: &AppState,
    domain: &str,
    who: AccountRef<'_>,
) -> AppResult<Response> {
    let account = super::objects::load_local_account(state, who).await?;

    let ids = sqlx::query_scalar!(
        "SELECT id FROM collections WHERE account_id = $1 AND discoverable = true ORDER BY created_at DESC",
        account.id,
    )
    .fetch_all(&state.db)
    .await?;

    let items: Vec<String> = ids
        .into_iter()
        .map(|id| collection_uri(domain, id))
        .collect();

    let body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!(
            "{}/collections",
            crate::federation::tag::account_uri_of(domain, &account)
        ),
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

/// GET /users/{username}/feature_authorizations/{id} — the FeatureAuthorization
/// stamp proving a local account consented to being featured in a collection.
pub async fn get_feature_authorization(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((username, id)): Path<(String, i64)>,
) -> AppResult<Response> {
    let row = sqlx::query!(
        r#"SELECT c.local AS collection_local, c.id AS collection_id,
                  c.uri AS "collection_uri?", a.uri AS "account_uri?"
           FROM collection_items ci
           JOIN collections c ON c.id = ci.collection_id
           JOIN accounts a ON a.id = ci.account_id
           WHERE ci.id = $1 AND ci.state = 1
             AND a.username = $2 AND a.domain IS NULL"#,
        id,
        username,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let domain = &instance.domain;
    let auth_id = format!("https://{domain}/users/{username}/feature_authorizations/{id}");
    let collection_uri = match row.collection_uri {
        Some(uri) if !uri.is_empty() => uri,
        _ => collection_uri(domain, row.collection_id),
    };
    let account_uri = match row.account_uri {
        Some(uri) if !uri.is_empty() => uri,
        _ => actor_uri(domain, &username),
    };

    let mut body =
        crate::federation::consent::feature_authorization(&auth_id, &collection_uri, &account_uri)
            .map_err(AppError::Internal)?;
    body["@context"] = json!([
        "https://www.w3.org/ns/activitystreams",
        {
            "toot": "http://joinmastodon.org/ns#",
            "FeatureAuthorization": "toot:FeatureAuthorization",
            "interactingObject": { "@id": "toot:interactingObject", "@type": "@id" },
            "interactionTarget": { "@id": "toot:interactionTarget", "@type": "@id" },
        }
    ]);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response())
}

/// GET /users/{username}/quote_authorizations/{id} — the QuoteAuthorization
/// stamp proving a local account authorized a quote of one of its posts.
pub async fn get_quote_authorization(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path((username, id)): Path<(String, i64)>,
) -> AppResult<Response> {
    let row = sqlx::query!(
        r#"SELECT qs.uri AS "quoted_status_uri?", ss.uri AS "quoting_status_uri?",
                  qa.id AS quoted_account_id, qa.id_scheme AS quoted_account_id_scheme
           FROM quotes q
           JOIN statuses qs ON qs.id = q.quoted_status_id
           JOIN statuses ss ON ss.id = q.status_id
           JOIN accounts qa ON qa.id = q.quoted_account_id
           WHERE q.id = $1 AND q.state = 1
             AND qa.username = $2 AND qa.domain IS NULL"#,
        id,
        username,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let (Some(quoted_status_uri), Some(quoting_status_uri)) =
        (row.quoted_status_uri, row.quoting_status_uri)
    else {
        return Err(AppError::NotFound);
    };

    let domain = &instance.domain;
    let auth_id = format!("https://{domain}/users/{username}/quote_authorizations/{id}");
    // The quoted account is local, so its actor id follows from its id scheme.
    let quoted_account_uri = crate::federation::tag::account_uri(
        domain,
        row.quoted_account_id,
        row.quoted_account_id_scheme,
        &username,
    );
    let mut body = crate::federation::consent::quote_authorization(
        &auth_id,
        &quoted_account_uri,
        &quoting_status_uri,
        &quoted_status_uri,
    )
    .map_err(AppError::Internal)?;
    body["@context"] = json!([
        "https://www.w3.org/ns/activitystreams",
        {
            "toot": "http://joinmastodon.org/ns#",
            "QuoteAuthorization": "toot:QuoteAuthorization",
            "interactingObject": { "@id": "toot:interactingObject", "@type": "@id" },
            "interactionTarget": { "@id": "toot:interactionTarget", "@type": "@id" },
        }
    ]);

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response())
}

// ── followers / following / featured collections ──────────────────────────────

#[derive(serde::Deserialize)]
pub struct PageQuery {
    pub page: Option<bool>,
    pub max_id: Option<i64>,
}

/// Whether a relation collection (followers/following) is paged.
enum Relation {
    Followers,
    Following,
}

/// GET /users/{username}/followers — an OrderedCollection of follower actor URIs.
pub async fn get_followers(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PageQuery>,
) -> AppResult<Response> {
    relation_collection(
        &state,
        &instance.domain,
        AccountRef::Username(&username),
        Relation::Followers,
        q,
    )
    .await
}

/// GET /users/{username}/following — an OrderedCollection of followed actor URIs.
pub async fn get_following(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PageQuery>,
) -> AppResult<Response> {
    relation_collection(
        &state,
        &instance.domain,
        AccountRef::Username(&username),
        Relation::Following,
        q,
    )
    .await
}

/// Numeric-scheme followers (`/ap/users/{id}/followers`).
pub async fn get_followers_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
    axum::extract::Query(q): axum::extract::Query<PageQuery>,
) -> AppResult<Response> {
    relation_collection(
        &state,
        &instance.domain,
        AccountRef::Id(id),
        Relation::Followers,
        q,
    )
    .await
}

/// Numeric-scheme following (`/ap/users/{id}/following`).
pub async fn get_following_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
    axum::extract::Query(q): axum::extract::Query<PageQuery>,
) -> AppResult<Response> {
    relation_collection(
        &state,
        &instance.domain,
        AccountRef::Id(id),
        Relation::Following,
        q,
    )
    .await
}

async fn relation_collection(
    state: &AppState,
    domain: &str,
    who: AccountRef<'_>,
    rel: Relation,
    q: PageQuery,
) -> AppResult<Response> {
    let account = super::objects::load_local_account(state, who).await?;

    let (rel_name, total): (&str, i64) = match rel {
        Relation::Followers => (
            "followers",
            sqlx::query_scalar!(
                "SELECT COUNT(*) FROM follows WHERE target_account_id = $1",
                account.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(0),
        ),
        Relation::Following => (
            "following",
            sqlx::query_scalar!(
                "SELECT COUNT(*) FROM follows WHERE account_id = $1",
                account.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(0),
        ),
    };

    let base = format!(
        "{}/{rel_name}",
        crate::federation::tag::account_uri_of(domain, &account)
    );
    let hidden = account.hide_collections.unwrap_or(false);

    // Summary view: advertise the count, and a first page only when not hidden.
    if q.page != Some(true) {
        let mut body = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": base,
            "type": "OrderedCollection",
            "totalItems": total,
        });
        if !hidden {
            body["first"] = json!(format!("{base}?page=true"));
        }
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, CONTENT_TYPE)],
            Json(body),
        )
            .into_response());
    }

    // Hidden collections expose only the count, never the membership.
    if hidden {
        let body = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("{base}?page=true"),
            "type": "OrderedCollectionPage",
            "partOf": base,
            "totalItems": total,
            "orderedItems": [],
        });
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, CONTENT_TYPE)],
            Json(body),
        )
            .into_response());
    }

    const PAGE_SIZE: i64 = 40;
    // (follow_id, resolved actor uri) pairs, newest follow first.
    let rows: Vec<(i64, String)> = match rel {
        Relation::Followers => sqlx::query!(
            r#"SELECT f.id, a.id AS account_id, a.id_scheme, a.uri AS account_uri, a.username, (a.domain IS NULL) AS "is_local!"
               FROM follows f JOIN accounts a ON a.id = f.account_id
               WHERE f.target_account_id = $1 AND ($2::bigint IS NULL OR f.id < $2)
               ORDER BY f.id DESC LIMIT $3"#,
            account.id,
            q.max_id,
            PAGE_SIZE,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.id, resolve_actor_uri(domain, r.account_uri, r.is_local, r.account_id, r.id_scheme, &r.username)))
        .collect(),
        Relation::Following => sqlx::query!(
            r#"SELECT f.id, a.id AS account_id, a.id_scheme, a.uri AS account_uri, a.username, (a.domain IS NULL) AS "is_local!"
               FROM follows f JOIN accounts a ON a.id = f.target_account_id
               WHERE f.account_id = $1 AND ($2::bigint IS NULL OR f.id < $2)
               ORDER BY f.id DESC LIMIT $3"#,
            account.id,
            q.max_id,
            PAGE_SIZE,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.id, resolve_actor_uri(domain, r.account_uri, r.is_local, r.account_id, r.id_scheme, &r.username)))
        .collect(),
    };

    let items: Vec<String> = rows.iter().map(|(_, uri)| uri.clone()).collect();
    let next = (rows.len() as i64 == PAGE_SIZE)
        .then(|| {
            rows.last()
                .map(|(id, _)| format!("{base}?page=true&max_id={id}"))
        })
        .flatten();

    let mut body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{base}?page=true{}", q.max_id.map(|m| format!("&max_id={m}")).unwrap_or_default()),
        "type": "OrderedCollectionPage",
        "partOf": base,
        "totalItems": total,
        "orderedItems": items,
    });
    if let Some(next) = next {
        body["next"] = json!(next);
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(body),
    )
        .into_response())
}

/// GET /users/{username}/collections/featured — an OrderedCollection of the
/// account's pinned status URIs (Mastodon's `featured` collection).
pub async fn get_featured(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
) -> AppResult<Response> {
    featured_collection(&state, &instance.domain, AccountRef::Username(&username)).await
}

/// Numeric-scheme featured collection (`/ap/users/{id}/collections/featured`).
pub async fn get_featured_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    featured_collection(&state, &instance.domain, AccountRef::Id(id)).await
}

async fn featured_collection(
    state: &AppState,
    domain: &str,
    who: AccountRef<'_>,
) -> AppResult<Response> {
    let account = super::objects::load_local_account(state, who).await?;

    // Pinned, publicly-visible statuses, newest pin first (mirrors Mastodon).
    let rows = sqlx::query!(
        r#"SELECT s.id, s.uri AS "uri?"
           FROM status_pins p JOIN statuses s ON s.id = p.status_id
           WHERE p.account_id = $1 AND s.deleted_at IS NULL AND s.visibility IN (0, 1)
           ORDER BY p.id DESC"#,
        account.id,
    )
    .fetch_all(&state.db)
    .await?;

    // The account's own scheme, not the one it was asked under: an actor has a
    // single canonical URI, and the collection has to be a path beneath it.
    let actor = crate::federation::tag::account_uri_of(domain, &account);
    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            r.uri
                .clone()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| format!("{actor}/statuses/{}", r.id))
        })
        .collect();

    let body = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{actor}/collections/featured"),
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
