use axum::{
    extract::{Extension, OriginalUri, State},
    http::StatusCode,
};
use serde_json::Value;

use crate::{
    error::{AppError, AppResult},
    middleware::ResolvedInstance,
    state::AppState,
};

mod attachment;
mod collection;
mod create;
mod fetch;
mod follow;
mod moderation;
mod quote;
mod signature;
mod status;
use collection::{handle_add, handle_remove};
use create::handle_create;
pub use fetch::{fetch_remote_status, resolve_or_fetch_remote_account};
use follow::{handle_accept_reject, handle_follow, handle_undo};
use moderation::{handle_block, handle_flag, handle_move};
use quote::{handle_feature_request, handle_quote_request};
use signature::{verify_inbound_signature, verify_object_integrity};
use status::{handle_announce, handle_delete, handle_like, handle_update};

/// Returns true if a tag's `type` field equals `type_name`, handling both
/// string (`"Mention"`) and array (`["Mention", "Link"]`) forms.
pub(super) fn tag_type_is(tag: &Value, type_name: &str) -> bool {
    match tag.get("type") {
        Some(Value::String(s)) => s == type_name,
        Some(Value::Array(arr)) => arr.iter().any(|v| v.as_str() == Some(type_name)),
        _ => false,
    }
}

/// Normalises a JSON field that may be a string, an array of strings, or absent
/// into an owned `Vec<String>`. Handles both `"x"` and `["x","y"]`.
pub(super) fn as_string_vec(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    }
}

/// Returns true when the two URI strings share the same host.
pub(super) fn same_host(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(ua), Ok(ub)) => ua.host_str() == ub.host_str(),
        _ => false,
    }
}

/// TTL of a "delete arrived first" tombstone, matching Mastodon's 6 hours.
const DELETE_UPON_ARRIVAL_TTL: i64 = 6 * 60 * 60;

fn delete_upon_arrival_key(actor: &str, uri: &str) -> String {
    format!("delete_upon_arrival:{actor}:{uri}")
}

/// Remember that an Undo/Delete for `uri` from `actor` arrived, so a later,
/// out-of-order activity carrying that id is skipped rather than resurrecting
/// the deleted object (Mastodon's `delete_later!`).
pub(super) async fn delete_later(state: &AppState, actor: &str, uri: &str) {
    if actor.is_empty() || uri.is_empty() {
        return;
    }
    let mut redis = state.redis.clone();
    let key = delete_upon_arrival_key(actor, uri);
    let _: redis::RedisResult<()> = redis::cmd("SETEX")
        .arg(&key)
        .arg(DELETE_UPON_ARRIVAL_TTL)
        .arg(1)
        .query_async(&mut redis)
        .await;
}

/// Whether an Undo/Delete for `uri` from `actor` already arrived (Mastodon's
/// `delete_arrived_first?`).
pub(super) async fn delete_arrived_first(state: &AppState, actor: &str, uri: &str) -> bool {
    if actor.is_empty() || uri.is_empty() {
        return false;
    }
    let mut redis = state.redis.clone();
    let key = delete_upon_arrival_key(actor, uri);
    let exists: i64 = redis::cmd("EXISTS")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap_or(0);
    exists == 1
}

/// Autorelease TTL for the `create:{uri}` serialization lock, matching
/// Mastodon's default `with_redis_lock` timeout.
const CREATE_LOCK_TTL_MS: usize = 15 * 60 * 1000;

/// Best-effort Redis lock guard: releases the lock (only if still the owner) on
/// drop.
pub(super) struct RedisLock {
    redis: redis::aio::ConnectionManager,
    key: String,
    token: String,
}

impl Drop for RedisLock {
    fn drop(&mut self) {
        let mut redis = self.redis.clone();
        let key = std::mem::take(&mut self.key);
        let token = std::mem::take(&mut self.token);
        tokio::spawn(async move {
            // Release only if we still hold the lock (compare-and-delete).
            let _: redis::RedisResult<()> = redis::cmd("EVAL")
                .arg("if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end")
                .arg(1)
                .arg(&key)
                .arg(&token)
                .query_async(&mut redis)
                .await;
        });
    }
}

/// Acquire the `create:{uri}` lock that serializes a status Create against a
/// concurrent Delete's `delete_later`, so the tombstone can't be set between the
/// Create's `delete_arrived_first?` check and its insert (Mastodon's
/// `with_redis_lock("create:#{object_uri}")`). Best-effort: retries briefly,
/// then proceeds without the lock rather than blocking an inbox request.
pub(super) async fn acquire_create_lock(state: &AppState, uri: &str) -> Option<RedisLock> {
    if uri.is_empty() {
        return None;
    }
    let key = format!("create:{uri}");
    let token = crate::snowflake::next_id().to_string();
    let mut redis = state.redis.clone();
    for attempt in 0..40 {
        let acquired: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(&key)
            .arg(&token)
            .arg("NX")
            .arg("PX")
            .arg(CREATE_LOCK_TTL_MS)
            .query_async(&mut redis)
            .await;
        if matches!(acquired, Ok(Some(_))) {
            return Some(RedisLock {
                redis: state.redis.clone(),
                key,
                token,
            });
        }
        if attempt < 39 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    None
}

/// Handles both `/inbox` (shared inbox) and `/users/:username/inbox`.
pub async fn shared_inbox(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<StatusCode> {
    let activity: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Ok(StatusCode::BAD_REQUEST),
    };

    let activity_type = activity.get("type").and_then(|t| t.as_str()).unwrap_or("");

    tracing::debug!(
        instance = %instance.domain,
        activity_type,
        body = %activity,
        "received ActivityPub activity"
    );

    // The actor that the activity claims to be from. Used both to enforce the
    // signature (the signing key must belong to this actor) and by handlers.
    let actor_uri = activity
        .get("actor")
        .and_then(|a| a.as_str().or_else(|| a.get("id").and_then(|i| i.as_str())))
        .unwrap_or("")
        .to_string();

    // Drop activities from domains we've defederated (admin domain block at
    // suspend severity). Mastodon silently discards these with HTTP 202 to avoid
    // backscatter, so we do the same rather than returning an error.
    if crate::federation::moderation::actor_is_suspended(&state, &actor_uri).await {
        tracing::debug!(actor = %actor_uri, activity_type, "dropping activity from suspended domain");
        return Ok(StatusCode::ACCEPTED);
    }

    // Enforce the HTTP Signature. An activity with a missing or invalid
    // signature is rejected, except `Delete` activities we cannot verify: the
    // signing actor (or its key) may already be gone, and rejecting them would
    // create backscatter, so we accept-and-ignore those (matching Mastodon).
    if let Err(reason) =
        verify_inbound_signature(&state, &headers, uri.path(), &body, &actor_uri).await
    {
        // Fall back to the FEP-8b32 Object Integrity Proof. Fedify-based servers
        // (GoToSocial, hackers.pub) sign activities with an `eddsa-jcs-2022`
        // proof, and their HTTP Signature may use a spec we don't parse or come
        // from a different host on shared-inbox/forwarded delivery. The proof
        // authenticates the activity itself, so accept when it verifies.
        if let Err(proof_reason) = verify_object_integrity(&state, &activity, &actor_uri).await {
            if activity_type == "Delete" {
                tracing::debug!(actor = %actor_uri, %reason, "unverified Delete; accepting without processing");
                return Ok(StatusCode::ACCEPTED);
            }
            tracing::warn!(
                actor = %actor_uri,
                activity_type,
                http_signature = %reason,
                integrity_proof = %proof_reason,
                "rejecting activity: neither HTTP Signature nor integrity proof verified"
            );
            return Err(AppError::Unauthorized);
        }
        tracing::info!(
            actor = %actor_uri,
            activity_type,
            "accepted via FEP-8b32 integrity proof (HTTP Signature unverified)"
        );
    }

    let outcome = match activity_type {
        "Follow" => {
            handle_follow(&state, &instance, &activity).await?;
            "handled"
        }
        "Undo" => {
            handle_undo(&state, &instance, &activity).await?;
            "handled"
        }
        "Create" => {
            handle_create(&state, &instance, &activity).await?;
            "handled"
        }
        "Delete" => {
            handle_delete(&state, &instance, &activity).await?;
            "handled"
        }
        "Announce" => {
            handle_announce(&state, &instance, &activity).await?;
            "handled"
        }
        "Like" => {
            handle_like(&state, &instance, &activity).await?;
            "handled"
        }
        "Accept" | "Reject" => {
            handle_accept_reject(&state, &instance, &activity).await?;
            "handled"
        }
        "Update" => {
            handle_update(&state, &instance, &activity).await?;
            "handled"
        }
        "Block" => {
            handle_block(&state, &activity).await?;
            "handled"
        }
        "Flag" => {
            handle_flag(&state, &activity).await?;
            "handled"
        }
        "Move" => {
            handle_move(&state, &activity).await?;
            "handled"
        }
        "Add" => {
            handle_add(&state, &activity).await?;
            "handled"
        }
        "Remove" => {
            handle_remove(&state, &activity).await?;
            "handled"
        }
        "QuoteRequest" => {
            handle_quote_request(&state, &instance, &activity).await?;
            "handled"
        }
        "FeatureRequest" => {
            handle_feature_request(&state, &instance, &activity).await?;
            "handled"
        }
        _ => "ignored",
    };
    tracing::debug!(activity_type, outcome, "ActivityPub activity processed");

    Ok(StatusCode::ACCEPTED)
}

/// Recompute a collection's `item_count` (pending + accepted items).
pub(super) async fn refresh_collection_item_count(state: &AppState, collection_id: i64) -> AppResult<()> {
    sqlx::query!(
        r#"UPDATE collections SET item_count =
             (SELECT COUNT(*) FROM collection_items WHERE collection_id = $1 AND state IN (0, 1))
           WHERE id = $1"#,
        collection_id,
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

pub(super) async fn sync_remote_poll(
    state: &AppState,
    status_id: i64,
    account_id: i64,
    object: &Value,
) -> AppResult<()> {
    let Some(items) = object
        .get("oneOf")
        .or_else(|| object.get("anyOf"))
        .and_then(|v| v.as_array())
    else {
        return Ok(());
    };

    let multiple = object.get("anyOf").is_some();
    let options: Vec<String> = items
        .iter()
        .filter_map(|item| item.get("name").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();
    if options.is_empty() {
        return Ok(());
    }

    let cached_tallies: Vec<i64> = items
        .iter()
        .map(|item| {
            item.get("replies")
                .and_then(|r| r.get("totalItems"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        })
        .collect();
    let votes_count: i64 = cached_tallies.iter().sum();
    let expires_at = object
        .get("endTime")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc());

    if let Some(poll_id) =
        sqlx::query_scalar!("SELECT id FROM polls WHERE status_id = $1", status_id,)
            .fetch_optional(&state.db)
            .await?
    {
        sqlx::query!(
            r#"UPDATE polls
               SET options = $2,
                   cached_tallies = $3,
                   votes_count = $4,
                   multiple = $5,
                   expires_at = $6,
                   updated_at = now()
               WHERE id = $1"#,
            poll_id,
            &options as &[String],
            &cached_tallies as &[i64],
            votes_count,
            multiple,
            expires_at,
        )
        .execute(&state.db)
        .await?;
    } else {
        let poll_id = crate::snowflake::next_id();
        if let Some(inserted_poll_id) = sqlx::query_scalar!(
            r#"INSERT INTO polls
                 (id, status_id, account_id, options, cached_tallies, votes_count,
                  multiple, expires_at, created_at, updated_at)
               SELECT $1,$2,$3,$4,$5,$6,$7,$8,now(),now()
               WHERE NOT EXISTS (SELECT 1 FROM polls WHERE status_id = $2)
               RETURNING id"#,
            poll_id,
            status_id,
            account_id,
            &options as &[String],
            &cached_tallies as &[i64],
            votes_count,
            multiple,
            expires_at,
        )
        .fetch_optional(&state.db)
        .await?
        {
            sqlx::query!(
                "UPDATE statuses SET poll_id = $1 WHERE id = $2",
                inserted_poll_id,
                status_id,
            )
            .execute(&state.db)
            .await?;
        }
    }

    Ok(())
}

/// Read a JSON value that may be a string IRI or an object with an `id`.
pub(super) fn json_uri(v: Option<&Value>) -> &str {
    v.and_then(|x| {
        if x.is_string() {
            x.as_str()
        } else {
            x.get("id").and_then(|i| i.as_str())
        }
    })
    .unwrap_or("")
}

/// Insert or update a mirrored remote collection (`local = false`).
pub(super) async fn upsert_remote_collection(
    state: &AppState,
    owner_id: i64,
    coll: &Value,
) -> AppResult<Option<i64>> {
    let uri = coll.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if uri.is_empty() {
        return Ok(None);
    }
    let name = coll
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Featured collection");
    let sensitive = coll
        .get("sensitive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let discoverable = coll
        .get("discoverable")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let id = sqlx::query_scalar!(
        r#"INSERT INTO collections
             (account_id, name, discoverable, local, sensitive, item_count, uri, created_at, updated_at)
           VALUES ($1, $2, $3, false, $4, 0, $5, now(), now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL
             DO UPDATE SET name = EXCLUDED.name, discoverable = EXCLUDED.discoverable,
                           sensitive = EXCLUDED.sensitive, updated_at = now()
           RETURNING id"#,
        owner_id,
        name,
        discoverable,
        sensitive,
        uri,
    )
    .fetch_optional(&state.db)
    .await?;
    Ok(id)
}

/// Mirror one `FeaturedItem` into a (remote) collection.
pub(super) async fn mirror_item_into(state: &AppState, collection_id: i64, item: &Value) -> AppResult<()> {
    let item_uri = item.get("id").and_then(|v| v.as_str());
    let account_uri = json_uri(item.get("featuredObject"));
    if account_uri.is_empty() {
        return Ok(());
    }
    let Ok(account_id) = resolve_or_fetch_remote_account(state, account_uri).await else {
        return Ok(());
    };
    sqlx::query!(
        r#"INSERT INTO collection_items
             (collection_id, account_id, state, uri, position, created_at, updated_at)
           VALUES ($1, $2, 1, $3,
                   (SELECT COALESCE(MAX(position), 0) + 1 FROM collection_items WHERE collection_id = $1),
                   now(), now())
           ON CONFLICT (account_id, collection_id)
             DO UPDATE SET state = 1, uri = EXCLUDED.uri, updated_at = now()"#,
        collection_id,
        account_id,
        item_uri,
    )
    .execute(&state.db)
    .await?;
    refresh_collection_item_count(state, collection_id).await
}
