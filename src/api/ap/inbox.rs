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
mod moderation;
mod quote;
mod signature;
use attachment::{ap_attachment_file_meta, classify_attachment_type};
use collection::{handle_add, handle_remove};
use create::handle_create;
pub use fetch::{fetch_remote_status, resolve_or_fetch_remote_account};
use moderation::{handle_block, handle_flag, handle_move};
use quote::{handle_feature_request, handle_quote_request};
use signature::{verify_inbound_signature, verify_object_integrity};

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
async fn delete_later(state: &AppState, actor: &str, uri: &str) {
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

async fn handle_follow(
    state: &AppState,
    instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity
        .get("object")
        .and_then(|o| o.as_str())
        .unwrap_or("");
    let activity_uri = activity.get("id").and_then(|i| i.as_str()).unwrap_or("");

    // Skip a Follow whose Undo already arrived out of order: the Undo(Follow)
    // recorded a tombstone for this activity id (Mastodon's delete_arrived_first?).
    if delete_arrived_first(state, actor_uri, activity_uri).await {
        return Ok(());
    }

    // The instance actor cannot be followed (Mastodon rejects these): reply with
    // a Reject signed by the instance actor.
    if object_uri == crate::federation::instance_actor::actor_url(&instance.domain) {
        if let Ok(follower_id) = resolve_or_fetch_remote_account(state, actor_uri).await {
            let inbox = sqlx::query_scalar!(
                "SELECT inbox_url FROM accounts WHERE id = $1",
                follower_id,
            )
            .fetch_optional(&state.db)
            .await?
            .filter(|s| !s.is_empty());
            if let Some(inbox) = inbox {
                let reject_id = format!(
                    "https://{}/activities/{}",
                    instance.domain,
                    crate::snowflake::next_id()
                );
                let key_id = crate::federation::instance_actor::key_id(&instance.domain);
                if let Ok(reject) = crate::federation::activity::reject_follow(
                    &reject_id,
                    object_uri,
                    activity_uri,
                    actor_uri,
                    object_uri,
                ) {
                    if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                        state,
                        reject,
                        vec![inbox],
                        key_id,
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to enqueue instance-actor Reject(Follow)");
                    }
                }
            }
        }
        return Ok(());
    }

    // Resolve the local target account. Local accounts derive their actor URL
    // from id_scheme + username/id and leave the `uri` column empty, so the
    // Follow's `object` must be matched against the derived URL (as
    // resolve_or_fetch_remote_account does) rather than the `uri` column, which
    // would miss them and silently drop the follow.
    let target_id: Option<i64> = if let Ok(parsed) = url::Url::parse(object_uri) {
        let on_our_host = parsed
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&instance.domain));
        let segments: Vec<&str> = parsed.path_segments().map(|s| s.collect()).unwrap_or_default();
        if on_our_host {
            match segments.as_slice() {
                // https://{domain}/users/{username}
                ["users", username] => sqlx::query_scalar!(
                    "SELECT id FROM accounts WHERE username = $1 AND domain IS NULL",
                    username,
                )
                .fetch_optional(&state.db)
                .await?,
                // https://{domain}/ap/users/{id}
                ["ap", "users", id] => match id.parse::<i64>() {
                    Ok(numeric) => sqlx::query_scalar!(
                        "SELECT id FROM accounts WHERE id = $1 AND domain IS NULL",
                        numeric,
                    )
                    .fetch_optional(&state.db)
                    .await?,
                    Err(_) => None,
                },
                _ => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let Some(target_id) = target_id else { return Ok(()) };
    let target = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        target_id,
    )
    .fetch_one(&state.db)
    .await?;

    let follower_id = resolve_or_fetch_remote_account(state, actor_uri).await?;

    // Fetch the follower's account for push notification details and to decide
    // whether a silenced follower must go through a request.
    let follower = sqlx::query!(
        "SELECT display_name, username, domain, avatar_remote_url, silenced_at FROM accounts WHERE id = $1",
        follower_id,
    )
    .fetch_optional(&state.db)
    .await?;
    let follower_silenced = follower.as_ref().is_some_and(|f| f.silenced_at.is_some());

    // Details for any Accept/Reject we sign as the (local) target and deliver
    // back to the follower. `object_uri` is the target's own actor URL.
    let key_id = format!("{object_uri}#main-key");
    let can_sign = target.private_key.as_deref().is_some_and(|s| !s.is_empty());
    let follower_inbox = sqlx::query_scalar!(
        "SELECT inbox_url FROM accounts WHERE id = $1",
        follower_id,
    )
    .fetch_optional(&state.db)
    .await?
    .filter(|s| !s.is_empty());

    // Reject the follow up front (Mastodon ActivityPub::Activity::Follow) when
    // the target blocks the follower — directly or by domain — or has moved.
    let follower_domain = follower.as_ref().and_then(|f| f.domain.clone());
    let should_reject = target.moved_to_account_id.is_some()
        || sqlx::query_scalar!(
            r#"SELECT EXISTS(
                 SELECT 1 FROM blocks WHERE account_id = $1 AND target_account_id = $2
                 UNION ALL
                 SELECT 1 FROM account_domain_blocks WHERE account_id = $1 AND domain = $3
               ) AS "exists!""#,
            target.id,
            follower_id,
            follower_domain,
        )
        .fetch_one(&state.db)
        .await?;
    if should_reject {
        if can_sign {
            if let Some(ref inbox) = follower_inbox {
                let reject_id = format!(
                    "https://{}/activities/{}",
                    instance.domain,
                    crate::snowflake::next_id()
                );
                if let Ok(reject) = crate::federation::activity::reject_follow(
                    &reject_id,
                    object_uri,
                    activity_uri,
                    actor_uri,
                    object_uri,
                ) {
                    if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                        state,
                        reject,
                        vec![inbox.clone()],
                        key_id.clone(),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to enqueue Reject(Follow)");
                    }
                }
            }
        }
        return Ok(());
    }

    // Fast-forward a repeat Follow: if the follower already follows the target,
    // refresh the stored uri and re-send Accept rather than opening a new
    // request (matches Mastodon's existing-follow fast path).
    let already_follows = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2
           ) AS "exists!""#,
        follower_id,
        target.id,
    )
    .fetch_one(&state.db)
    .await?;
    if already_follows {
        sqlx::query!(
            "UPDATE follows SET uri = $3, updated_at = now() WHERE account_id = $1 AND target_account_id = $2",
            follower_id,
            target.id,
            activity_uri,
        )
        .execute(&state.db)
        .await?;
        if can_sign {
            if let Some(ref inbox) = follower_inbox {
                let accept_id = format!(
                    "https://{}/activities/{}",
                    instance.domain,
                    crate::snowflake::next_id()
                );
                if let Ok(accept) = crate::federation::activity::accept_follow(
                    &accept_id,
                    object_uri,
                    activity_uri,
                    actor_uri,
                    object_uri,
                ) {
                    if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                        state,
                        accept,
                        vec![inbox.clone()],
                        key_id.clone(),
                    )
                    .await
                    {
                        tracing::warn!(error = %e, "failed to enqueue Accept(Follow)");
                    }
                }
            }
        }
        return Ok(());
    }

    // Decide what to do via feder-core's portable inbound logic; eunha executes
    // the returned Actions against Postgres + delivery.
    let (Ok(follow_id), Ok(actor_iri), Ok(object_iri)) = (
        activity_uri.parse::<feder_vocab::Iri>(),
        actor_uri.parse::<feder_vocab::Iri>(),
        object_uri.parse::<feder_vocab::Iri>(),
    ) else {
        return Ok(());
    };
    let follow = feder_vocab::Follow::new(
        follow_id,
        feder_vocab::Reference::id(actor_iri),
        feder_vocab::Reference::id(object_iri.clone()),
    );
    let accept_id = format!(
        "https://{}/activities/{}",
        instance.domain,
        crate::snowflake::next_id()
    );
    let Ok(accept_iri) = accept_id.parse::<feder_vocab::Iri>() else {
        return Ok(());
    };

    // A locked target, or a silenced follower, holds the follow as a request
    // (Mastodon: target.locked? || account.silenced?).
    let actions = feder_core::inbound::on_follow(
        follow,
        &object_iri,
        target.locked || follower_silenced,
        accept_iri,
    );

    for action in actions {
        match action {
            feder_core::inbound::Action::RecordFollowRequest => {
                sqlx::query!(
                    r#"INSERT INTO follow_requests (account_id, target_account_id, uri, created_at, updated_at)
                       VALUES ($1, $2, $3, now(), now())
                       ON CONFLICT (account_id, target_account_id) DO UPDATE SET uri = EXCLUDED.uri"#,
                    follower_id,
                    target.id,
                    activity_uri,
                )
                .execute(&state.db)
                .await?;

                if let Some(ref f) = follower {
                    let acct = match &f.domain {
                        Some(d) => format!("{}@{}", f.username, d),
                        None => f.username.clone(),
                    };
                    crate::push::create_and_push(
                        state,
                        target.id,
                        follower_id,
                        "follow_request",
                        None,
                        format!("{} wants to follow you", f.display_name),
                        acct,
                        f.avatar_remote_url.clone().unwrap_or_default(),
                    )
                    .await;
                }
            }
            feder_core::inbound::Action::RecordFollow => {
                sqlx::query!(
                    r#"INSERT INTO follows (account_id, target_account_id, uri, created_at, updated_at)
                       VALUES ($1, $2, $3, now(), now())
                       ON CONFLICT (account_id, target_account_id) DO UPDATE SET uri = EXCLUDED.uri"#,
                    follower_id,
                    target.id,
                    activity_uri,
                )
                .execute(&state.db)
                .await?;

                if let Some(ref f) = follower {
                    let acct = match &f.domain {
                        Some(d) => format!("{}@{}", f.username, d),
                        None => f.username.clone(),
                    };
                    crate::push::create_and_push(
                        state,
                        target.id,
                        follower_id,
                        "follow",
                        None,
                        format!("{} followed you", f.display_name),
                        acct,
                        f.avatar_remote_url.clone().unwrap_or_default(),
                    )
                    .await;
                }
            }
            feder_core::inbound::Action::SendAccept(accept) => {
                if target.private_key.as_deref().is_none_or(|s| s.is_empty()) {
                    tracing::warn!(username = %target.username, "local account has no private key; cannot send Accept");
                    continue;
                }
                let follower_inbox = sqlx::query_scalar!(
                    "SELECT inbox_url FROM accounts WHERE id = $1",
                    follower_id,
                )
                .fetch_optional(&state.db)
                .await?
                .filter(|s| !s.is_empty());
                let Some(inbox) = follower_inbox else {
                    tracing::warn!(
                        actor_uri,
                        "cannot send Accept: remote actor has no inbox URL"
                    );
                    continue;
                };
                let activity = serde_json::to_value(&accept)
                    .map_err(|e| crate::error::AppError::Internal(e.into()))?;
                let actor_url = crate::federation::tag::account_uri(
                    &instance.domain,
                    target.id,
                    target.id_scheme,
                    &target.username,
                );
                let key_id = format!("{actor_url}#main-key");
                tracing::debug!(inbox, actor_uri, "enqueueing Accept");
                if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                    state,
                    activity,
                    vec![inbox],
                    key_id,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to enqueue Accept");
                }
            }
        }
    }

    Ok(())
}

async fn handle_undo(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object = activity.get("object");
    let object_type = object.and_then(|o| o.get("type")).and_then(|t| t.as_str());

    match object_type {
        Some("Follow") => {
            let follow_uri = object
                .and_then(|o| o.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let undone_follow = sqlx::query!("DELETE FROM follows WHERE uri = $1", follow_uri)
                .execute(&state.db)
                .await?;
            let undone_request = sqlx::query!(
                "DELETE FROM follow_requests WHERE uri = $1 RETURNING account_id, target_account_id",
                follow_uri
            )
            .fetch_optional(&state.db)
            .await?;
            if let Some(req) = &undone_request {
                // Mirror Mastodon's FollowRequest dependent: :destroy — clear the
                // recipient's follow_request notification for the withdrawn request.
                sqlx::query!(
                    "DELETE FROM notifications WHERE account_id = $1 AND from_account_id = $2 AND type = 'follow_request'",
                    req.target_account_id,
                    req.account_id,
                )
                .execute(&state.db)
                .await?;
            }
            // The Follow may not have been processed yet (out-of-order delivery);
            // remember this Undo so a late Follow with the same id is skipped
            // rather than resurrecting the follow.
            if undone_follow.rows_affected() == 0 && undone_request.is_none() {
                delete_later(state, actor_uri, follow_uri).await;
            }
        }
        Some("Like") => {
            let like_uri = object
                .and_then(|o| o.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            // object.object is the liked status URI
            let status_uri = object
                .and_then(|o| o.get("object"))
                .and_then(|v| {
                    if v.is_string() {
                        v.as_str()
                    } else {
                        v.get("id").and_then(|i| i.as_str())
                    }
                })
                .unwrap_or("");
            let status_id =
                sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", status_uri)
                    .fetch_optional(&state.db)
                    .await?;
            let account_id =
                sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
                    .fetch_optional(&state.db)
                    .await?;
            let mut removed = false;
            if let (Some(sid), Some(aid)) = (status_id, account_id) {
                let deleted = sqlx::query!(
                    "DELETE FROM favourites WHERE account_id = $1 AND status_id = $2",
                    aid,
                    sid
                )
                .execute(&state.db)
                .await?;
                removed = deleted.rows_affected() > 0;
                sqlx::query!(
                    r#"UPDATE status_stats SET favourites_count = (SELECT COUNT(*) FROM favourites WHERE status_id = $1), updated_at = now() WHERE status_id = $1"#,
                    sid
                ).execute(&state.db).await?;
            }
            if !removed {
                delete_later(state, actor_uri, like_uri).await;
            }
        }
        Some("Announce") => {
            // Delete the remote boost status by its announce URI
            let announce_uri = object
                .and_then(|o| o.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            if !announce_uri.is_empty() {
                let deleted = sqlx::query!(
                    "DELETE FROM statuses WHERE uri = $1 RETURNING reblog_of_id",
                    announce_uri,
                )
                .fetch_optional(&state.db)
                .await?;
                match deleted {
                    Some(row) => {
                        if let Some(original_id) = row.reblog_of_id {
                            sqlx::query!(
                                r#"UPDATE status_stats SET reblogs_count = (SELECT COUNT(*) FROM statuses WHERE reblog_of_id = $1 AND deleted_at IS NULL), updated_at = now() WHERE status_id = $1"#,
                                original_id,
                            ).execute(&state.db).await?;
                        }
                    }
                    None => delete_later(state, actor_uri, announce_uri).await,
                }
            }
        }
        Some("Block") => {
            let block_uri = object
                .and_then(|o| o.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let block_object_uri = object
                .and_then(|o| o.get("object"))
                .and_then(|v| {
                    if v.is_string() {
                        v.as_str()
                    } else {
                        v.get("id").and_then(|i| i.as_str())
                    }
                })
                .unwrap_or("");
            let blocker_id =
                sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
                    .fetch_optional(&state.db)
                    .await?;
            let blockee_id = sqlx::query_scalar!(
                "SELECT id FROM accounts WHERE uri = $1 AND domain IS NULL",
                block_object_uri
            )
            .fetch_optional(&state.db)
            .await?;
            let mut removed = false;
            if let (Some(bid), Some(eid)) = (blocker_id, blockee_id) {
                let deleted = sqlx::query!(
                    "DELETE FROM blocks WHERE account_id = $1 AND target_account_id = $2",
                    bid,
                    eid
                )
                .execute(&state.db)
                .await?;
                removed = deleted.rows_affected() > 0;
            }
            if !removed {
                delete_later(state, actor_uri, block_uri).await;
            }
        }
        _ => {}
    }

    Ok(())
}

async fn handle_delete(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity.get("object").and_then(|o| {
        if o.is_string() {
            o.as_str()
        } else {
            o.get("id").and_then(|i| i.as_str())
        }
    });

    if let Some(uri) = object_uri {
        // Delete(actor) — remote account deleted itself
        if uri == actor_uri {
            sqlx::query!(
                "UPDATE accounts SET suspended_at = now() WHERE uri = $1 AND domain IS NOT NULL",
                uri,
            )
            .execute(&state.db)
            .await?;
            tracing::debug!(actor_uri, "suspended remote account on Delete(actor)");
        } else {
            // Delete(FeatureAuthorization) — a featured account revoked consent;
            // revoke the matching item (matched by the authorization URI we stored).
            let revoked = sqlx::query!(
                r#"UPDATE collection_items SET state = 3, updated_at = now()
                   WHERE approval_uri = $1 AND state = 1
                   RETURNING collection_id"#,
                uri,
            )
            .fetch_optional(&state.db)
            .await?;
            if let Some(r) = revoked {
                refresh_collection_item_count(state, r.collection_id).await?;
                return Ok(());
            }

            // Reject if the actor's domain doesn't match the object's domain —
            // prevents one server from deleting another server's content.
            if !same_host(actor_uri, uri) {
                tracing::warn!(
                    actor_uri,
                    uri,
                    "Delete: actor domain does not match object domain, ignoring"
                );
                return Ok(());
            }

            // Delete(Note/Tombstone) — soft-delete the status. Serialize against
            // a concurrent Create for this uri (same `create:{uri}` lock) so we
            // observe its committed status and it observes our tombstone.
            let _create_lock = acquire_create_lock(state, uri).await;
            let deleted = sqlx::query!("UPDATE statuses SET deleted_at = now() WHERE uri = $1", uri,)
                .execute(&state.db)
                .await?;
            // If the status isn't known yet (out-of-order delivery), remember the
            // Delete so a late Create with this URI is skipped.
            if deleted.rows_affected() == 0 {
                delete_later(state, actor_uri, uri).await;
            }

            // Create a tombstone so that a subsequent Create with the same URI is rejected.
            let actor_id =
                sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri,)
                    .fetch_optional(&state.db)
                    .await?;
            if let Some(actor_id) = actor_id {
                let tombstone_id = crate::snowflake::next_id();
                let _ = sqlx::query!(
                    r#"INSERT INTO tombstones (id, account_id, uri, created_at, updated_at)
                       SELECT $1, $2, $3::text, now(), now()
                       WHERE NOT EXISTS (SELECT 1 FROM tombstones WHERE uri = $3::text)"#,
                    tombstone_id,
                    actor_id,
                    uri,
                )
                .execute(&state.db)
                .await;
            }
        }
    }

    Ok(())
}

async fn handle_announce(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object = activity.get("object");
    let announce_uri = activity.get("id").and_then(|i| i.as_str()).unwrap_or("");

    // Skip an Announce whose Undo already arrived out of order.
    if delete_arrived_first(state, actor_uri, announce_uri).await {
        return Ok(());
    }

    // object can be a URI string or an embedded object
    let boosted_uri = object.and_then(|o| {
        if o.is_string() {
            o.as_str()
        } else {
            o.get("id").and_then(|i| i.as_str())
        }
    });

    let Some(boosted_uri) = boosted_uri else {
        return Ok(());
    };
    if actor_uri.is_empty() || announce_uri.is_empty() {
        return Ok(());
    }

    let booster_id = match resolve_or_fetch_remote_account(state, actor_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    // Find the boosted status in our database, fetching URI-only boosted
    // objects on demand like Mastodon's dereferencer path.
    let mut original_id = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
        boosted_uri,
    )
    .fetch_optional(&state.db)
    .await?;

    if original_id.is_none() {
        original_id = fetch_remote_status(state, boosted_uri).await?;
    }

    let Some(mut original_id) = original_id else {
        return Ok(());
    };
    if let Some(unwrapped_id) = sqlx::query_scalar!(
        "SELECT reblog_of_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        original_id,
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    {
        original_id = unwrapped_id;
    }

    let published = activity
        .get("published")
        .and_then(|p| p.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    // Derive the boost's visibility from the Announce's own `to`/`cc` audience,
    // mirroring Mastodon's ActivityPub::Activity::Announce#visibility_from_audience
    // (public collection in `to` → public, in `cc` → unlisted, a followers
    // collection → private, otherwise direct) instead of assuming public.
    let announce_to = as_string_vec(activity.get("to"));
    let announce_cc = as_string_vec(activity.get("cc"));
    let visibility = crate::db::models::vis::from_audience(&announce_to, &announce_cc);

    let boost_id = crate::snowflake::next_id();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO statuses
             (id, account_id, reblog_of_id, visibility, uri, url, local, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $5, false, $6, now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL AND uri != '' DO NOTHING
           RETURNING id"#,
        boost_id,
        booster_id,
        original_id,
        visibility,
        announce_uri,
        published,
    )
    .fetch_optional(&state.db)
    .await?;

    // Update the original status's reblogs_count
    let _ = sqlx::query!(
        r#"INSERT INTO status_stats (status_id, reblogs_count, created_at, updated_at)
           VALUES ($1, 1, now(), now())
           ON CONFLICT (status_id) DO UPDATE
             SET reblogs_count = (SELECT COUNT(*) FROM statuses
                                  WHERE reblog_of_id = $1 AND deleted_at IS NULL),
                 updated_at = now()"#,
        original_id,
    )
    .execute(&state.db)
    .await;

    // Notify the local author that a remote account boosted their post
    // (Mastodon notifies via LocalNotificationWorker on an incoming Announce).
    notify_status_author(
        state,
        original_id,
        booster_id,
        "reblog",
        "boosted your post",
    )
    .await;

    // Fan the boost into followers' home and list feeds so it appears
    // immediately, not only after a feed repopulate. Mirrors the local reblog
    // path (mastodon::statuses::reblog_status) and the incoming-post path
    // (handle_create). Skipped when the Announce was a duplicate (no row
    // inserted) so we never push a non-existent status id, and — like
    // Mastodon's ActivityPub::Activity::Announce#distribute, which only
    // enqueues DistributionWorker when the reblog is within_realtime_window? —
    // skipped for boosts older than the 6h real-time window so backfilled
    // announces don't resurface at the top of feeds.
    let within_realtime_window =
        chrono::Utc::now().naive_utc() - published < chrono::Duration::hours(6);
    if let (Some(boost_id), true) = (inserted, within_realtime_window) {
        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let vis_str = crate::db::models::vis::to_str(visibility);
        if crate::feed::sync_fanout() {
            crate::feed::fanout_new_status(&mut redis, &db, booster_id, boost_id, &[]).await;
            crate::feed::fanout_to_lists(&mut redis, &db, booster_id, boost_id, None, vis_str)
                .await;
        } else {
            tokio::spawn(async move {
                crate::feed::fanout_new_status(&mut redis, &db, booster_id, boost_id, &[]).await;
                crate::feed::fanout_to_lists(&mut redis, &db, booster_id, boost_id, None, vis_str)
                    .await;
            });
        }
    }

    Ok(())
}

async fn handle_like(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let activity_uri = activity.get("id").and_then(|i| i.as_str()).unwrap_or("");
    let object_uri = activity
        .get("object")
        .and_then(|o| o.as_str())
        .unwrap_or("");

    // Skip a Like whose Undo already arrived out of order.
    if delete_arrived_first(state, actor_uri, activity_uri).await {
        return Ok(());
    }

    let mut status_id = sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", object_uri)
        .fetch_optional(&state.db)
        .await?;

    if status_id.is_none() {
        status_id = fetch_remote_status(state, object_uri).await?;
    }

    let Some(status_id) = status_id else {
        return Ok(());
    };

    let account_id = match resolve_or_fetch_remote_account(state, actor_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    sqlx::query!(
        "INSERT INTO favourites (account_id, status_id, created_at, updated_at) VALUES ($1,$2, now(), now()) ON CONFLICT DO NOTHING",
        account_id,
        status_id
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        r#"INSERT INTO status_stats (status_id, favourites_count, created_at, updated_at)
           VALUES ($1, (SELECT COUNT(*) FROM favourites WHERE status_id = $1), now(), now())
           ON CONFLICT (status_id) DO UPDATE
             SET favourites_count = (SELECT COUNT(*) FROM favourites WHERE status_id = $1),
                 updated_at = now()"#,
        status_id
    )
    .execute(&state.db)
    .await?;

    // Notify the local author that a remote account favourited their post
    // (Mastodon notifies the author via LocalNotificationWorker on an incoming
    // Like). create_and_push no-ops for a remote recipient and dedups.
    notify_status_author(
        state,
        status_id,
        account_id,
        "favourite",
        "favourited your post",
    )
    .await;

    Ok(())
}

/// Notify a status's author that `actor_id` interacted with it (favourite or
/// reblog from a remote account). No-ops if the author is remote.
async fn notify_status_author(
    state: &AppState,
    status_id: i64,
    actor_id: i64,
    notification_type: &'static str,
    verb: &str,
) {
    let Ok(Some(author_id)) = sqlx::query_scalar!(
        "SELECT account_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        status_id,
    )
    .fetch_optional(&state.db)
    .await
    else {
        return;
    };
    let Ok(Some(actor)) = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        actor_id,
    )
    .fetch_optional(&state.db)
    .await
    else {
        return;
    };
    crate::push::create_and_push(
        state,
        author_id,
        actor_id,
        notification_type,
        Some(status_id),
        format!("{} {}", actor.display_name, verb),
        actor.acct(),
        crate::api::mastodon::convert::account_avatar_url_for(&actor),
    )
    .await;
}

async fn handle_accept_reject(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let activity_type = activity.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let object = activity.get("object");
    let follow_uri = object.and_then(|o| {
        if o.is_string() {
            o.as_str()
        } else {
            o.get("id").and_then(|i| i.as_str())
        }
    });

    if let Some(uri) = follow_uri {
        if activity_type == "Accept" {
            // Promote follow_request → follows when remote accepts our Follow
            let promoted = sqlx::query!(
                "DELETE FROM follow_requests WHERE uri = $1 RETURNING account_id, target_account_id",
                uri
            )
            .fetch_optional(&state.db)
            .await?;
            if let Some(row) = promoted {
                sqlx::query!(
                    r#"INSERT INTO follows (account_id, target_account_id, uri, created_at, updated_at)
                       VALUES ($1, $2, $3, now(), now()) ON CONFLICT DO NOTHING"#,
                    row.account_id,
                    row.target_account_id,
                    uri
                )
                .execute(&state.db)
                .await?;

                // Update follower/following counts
                let _ = crate::counters::on_follow_created(
                    &state.db,
                    row.account_id,
                    row.target_account_id,
                )
                .await;
            }
        } else {
            sqlx::query!("DELETE FROM follow_requests WHERE uri = $1", uri)
                .execute(&state.db)
                .await?;
        }

        // Feature-request consent: the object may be one of our outstanding
        // FeatureRequests (matched by collection_items.activity_uri).
        let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
        let item = sqlx::query!(
            r#"SELECT ci.id, ci.collection_id, a.uri AS "account_uri?"
               FROM collection_items ci
               JOIN accounts a ON a.id = ci.account_id
               WHERE ci.activity_uri = $1 AND ci.state = 0"#,
            uri,
        )
        .fetch_optional(&state.db)
        .await?;
        if let Some(item) = item {
            // Only the featured account itself may answer the request.
            if item.account_uri.as_deref() == Some(actor_uri) {
                if activity_type == "Accept" {
                    let approval_uri = activity
                        .get("result")
                        .and_then(|r| {
                            if r.is_string() {
                                r.as_str()
                            } else {
                                r.get("id").and_then(|i| i.as_str())
                            }
                        })
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    sqlx::query!(
                        r#"UPDATE collection_items
                           SET state = 1, approval_uri = $2,
                               approval_last_verified_at = now(), updated_at = now()
                           WHERE id = $1"#,
                        item.id,
                        approval_uri.as_deref(),
                    )
                    .execute(&state.db)
                    .await?;
                } else {
                    sqlx::query!(
                        "UPDATE collection_items SET state = 2, updated_at = now() WHERE id = $1",
                        item.id,
                    )
                    .execute(&state.db)
                    .await?;
                }
                refresh_collection_item_count(state, item.collection_id).await?;
            }
        }

        // Quote-request consent: the object may be one of our outstanding
        // QuoteRequests (matched by quotes.activity_uri).
        let quote = sqlx::query!(
            r#"SELECT q.id, q.status_id, a.uri AS quoted_account_uri
               FROM quotes q JOIN accounts a ON a.id = q.quoted_account_id
               WHERE q.activity_uri = $1 AND q.state = 0"#,
            uri,
        )
        .fetch_optional(&state.db)
        .await?;
        if let Some(q) = quote {
            // Only the quoted account itself may answer.
            if q.quoted_account_uri == actor_uri {
                if activity_type == "Accept" {
                    let approval_uri = activity
                        .get("result")
                        .and_then(|r| {
                            if r.is_string() {
                                r.as_str()
                            } else {
                                r.get("id").and_then(|i| i.as_str())
                            }
                        })
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string());
                    sqlx::query!(
                        "UPDATE quotes SET state = 1, approval_uri = $2, updated_at = now() WHERE id = $1",
                        q.id,
                        approval_uri.as_deref(),
                    )
                    .execute(&state.db)
                    .await?;
                    // Re-federate the now-approved quote so recipients receive
                    // the `quoteAuthorization` stamp (Mastodon sends an Update
                    // on acceptance). Best-effort; a failure here must not fail
                    // ingesting the Accept.
                    if let Ok(Some(quoting)) = sqlx::query_as!(
                        crate::db::models::Status,
                        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
                        q.status_id,
                    )
                    .fetch_optional(&state.db)
                    .await
                    {
                        if let Ok(Some(author)) = sqlx::query_as!(
                            crate::db::models::Account,
                            "SELECT * FROM accounts WHERE id = $1",
                            quoting.account_id,
                        )
                        .fetch_optional(&state.db)
                        .await
                        {
                            if let Err(e) =
                                crate::api::mastodon::statuses::federate_status_update(
                                    state, quoting.id, &author, &quoting,
                                )
                                .await
                            {
                                tracing::warn!(error = %e, "failed to federate quote acceptance Update");
                            }
                        }
                    }
                } else {
                    sqlx::query!(
                        "UPDATE quotes SET state = 2, updated_at = now() WHERE id = $1",
                        q.id,
                    )
                    .execute(&state.db)
                    .await?;
                }
            }
        }
    }

    Ok(())
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

async fn handle_update(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let fetched_object;
    let object = match activity.get("object") {
        Some(o) if o.is_object() => o,
        Some(o) if o.is_string() => {
            let Some(uri) = o.as_str() else {
                return Ok(());
            };
            fetched_object = match crate::federation::fetch::signed_get_json(state, uri).await {
                Ok(v) => v,
                Err(_) => return Ok(()),
            };
            &fetched_object
        }
        _ => return Ok(()),
    };

    let obj_type = object.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match obj_type {
        "FeaturedCollection" => {
            // Mirror an updated remote collection.
            let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
            if actor_uri.is_empty() {
                return Ok(());
            }
            if let Ok(owner_id) = resolve_or_fetch_remote_account(state, actor_uri).await {
                if let Some(cid) = upsert_remote_collection(state, owner_id, object).await? {
                    if let Some(items) = object.get("orderedItems").and_then(|v| v.as_array()) {
                        for it in items {
                            let _ = mirror_item_into(state, cid, it).await;
                        }
                    }
                }
            }
        }
        "Person" | "Service" | "Application" | "Group" | "Organization" => {
            let actor_uri = object.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if actor_uri.is_empty() {
                return Ok(());
            }

            let display_name = object
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let note = object
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let inbox_url = object
                .get("inbox")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let shared_inbox_url = object
                .get("endpoints")
                .and_then(|e| e.get("sharedInbox"))
                .and_then(|s| s.as_str())
                .map(str::to_owned);
            let public_key = object
                .get("publicKey")
                .and_then(|k| k.get("publicKeyPem"))
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string();
            let locked = object
                .get("manuallyApprovesFollowers")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let avatar_remote_url = object
                .get("icon")
                .and_then(|i| if i.is_object() { i.get("url") } else { None })
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let header_remote_url = object
                .get("image")
                .and_then(|i| if i.is_object() { i.get("url") } else { None })
                .and_then(|v| v.as_str())
                .map(str::to_owned);

            // Don't clear inbox_url or public_key if the update omits them (sparse update guard)
            sqlx::query!(
                r#"UPDATE accounts
                   SET display_name = $2,
                       note = $3,
                       inbox_url = CASE WHEN $4 != '' THEN $4 ELSE inbox_url END,
                       shared_inbox_url = COALESCE($5, shared_inbox_url),
                       public_key = CASE WHEN $6 != '' THEN $6 ELSE public_key END,
                       locked = $7,
                       avatar_remote_url = COALESCE($8, avatar_remote_url),
                       header_remote_url = COALESCE($9, header_remote_url),
                       updated_at = now()
                   WHERE uri = $1 AND domain IS NOT NULL"#,
                actor_uri,
                display_name,
                note,
                inbox_url,
                shared_inbox_url,
                public_key,
                locked,
                avatar_remote_url,
                header_remote_url,
            )
            .execute(&state.db)
            .await?;
        }
        "Note" => {
            let note_uri = object.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if note_uri.is_empty() {
                return Ok(());
            }

            let text = object
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let spoiler_text = object
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let sensitive = object
                .get("sensitive")
                .and_then(|s| s.as_bool())
                .unwrap_or(false);
            let language = object
                .get("contentMap")
                .and_then(|m| m.as_object())
                .and_then(|m| m.keys().next())
                .map(|s| s.to_string())
                .filter(|s| ["ko", "en"].contains(&s.as_str()));
            let edited_at = object
                .get("updated")
                .and_then(|p| p.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc).naive_utc());

            let updated = sqlx::query!(
                r#"UPDATE statuses
                   SET text = $2, spoiler_text = $3, sensitive = $4, language = $5,
                       edited_at = COALESCE($6, edited_at), updated_at = now()
                   WHERE uri = $1 AND deleted_at IS NULL
                   RETURNING id, account_id"#,
                note_uri,
                text,
                spoiler_text,
                sensitive,
                language,
                edited_at,
            )
            .fetch_optional(&state.db)
            .await?;

            if updated.is_none() {
                let _ = fetch_remote_status(state, note_uri).await?;
                return Ok(());
            }

            let Some(row) = updated else {
                return Ok(());
            };

            // Replace media attachments
            sqlx::query!("DELETE FROM media_attachments WHERE status_id = $1", row.id)
                .execute(&state.db)
                .await?;
            let attachments: Vec<Value> = object
                .get("attachment")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default();
            let mut media_ids: Vec<i64> = Vec::new();
            for att in &attachments {
                let att_type_str = att.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let media_type_str = att.get("mediaType").and_then(|v| v.as_str()).unwrap_or("");
                let att_type = classify_attachment_type(att_type_str, media_type_str);
                let remote_url = match att.get("url").and_then(|v| v.as_str()) {
                    Some(u) if !u.is_empty() => u,
                    _ => continue,
                };
                let description = att.get("name").and_then(|v| v.as_str()).map(str::to_owned);
                let blurhash = att
                    .get("blurhash")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                let thumbnail_remote_url = att
                    .get("icon")
                    .and_then(|i| if i.is_object() { i.get("url") } else { None })
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
                let file_content_type = if media_type_str.is_empty() {
                    None
                } else {
                    Some(media_type_str.to_owned())
                };
                let file_meta = ap_attachment_file_meta(att);
                let media_id = crate::snowflake::next_id();
                if let Ok(id) = sqlx::query_scalar!(
                    r#"INSERT INTO media_attachments (id, account_id, status_id, remote_url, description, blurhash, type, thumbnail_remote_url, file_content_type, file_meta, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, now(), now()) RETURNING id"#,
                    media_id, row.account_id, row.id, remote_url, description, blurhash, att_type, thumbnail_remote_url, file_content_type, file_meta,
                ).fetch_one(&state.db).await { media_ids.push(id); }
            }
            if !media_ids.is_empty() {
                let _ = sqlx::query!(
                    "UPDATE statuses SET ordered_media_attachment_ids = $1 WHERE id = $2",
                    &media_ids,
                    row.id
                )
                .execute(&state.db)
                .await;
            }

            // Replace hashtags
            sqlx::query!("DELETE FROM statuses_tags WHERE status_id = $1", row.id)
                .execute(&state.db)
                .await?;
            let tags_arr: Vec<Value> = match object.get("tag") {
                Some(Value::Array(arr)) => arr.clone(),
                Some(obj @ Value::Object(_)) => vec![obj.clone()],
                _ => vec![],
            };
            for tag in tags_arr.iter().filter(|t| tag_type_is(t, "Hashtag")) {
                let name = match tag
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| n.trim_start_matches('#').to_lowercase())
                    .filter(|n| !n.is_empty())
                {
                    Some(n) => n,
                    None => continue,
                };
                let tag_id = crate::snowflake::next_id();
                if let Ok(Some(tid)) = sqlx::query_scalar!(
                    r#"INSERT INTO tags (id, name, last_status_at, created_at, updated_at) VALUES ($1,$2,now(),now(),now()) ON CONFLICT (lower(name)) DO UPDATE SET last_status_at = now(), updated_at = now() RETURNING id"#,
                    tag_id, name,
                ).fetch_optional(&state.db).await {
                    let _ = sqlx::query!("INSERT INTO statuses_tags (status_id, tag_id) VALUES ($1,$2) ON CONFLICT DO NOTHING", row.id, tid)
                        .execute(&state.db).await;
                }
            }

            sync_remote_poll(state, row.id, row.account_id, object).await?;
        }
        _ => {}
    }

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
