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

/// Returns true if a tag's `type` field equals `type_name`, handling both
/// string (`"Mention"`) and array (`["Mention", "Link"]`) forms.
fn tag_type_is(tag: &Value, type_name: &str) -> bool {
    match tag.get("type") {
        Some(Value::String(s)) => s == type_name,
        Some(Value::Array(arr)) => arr.iter().any(|v| v.as_str() == Some(type_name)),
        _ => false,
    }
}

/// Normalises a JSON field that may be a string, an array of strings, or absent
/// into an owned `Vec<String>`. Handles both `"x"` and `["x","y"]`.
fn as_string_vec(v: Option<&Value>) -> Vec<String> {
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
fn same_host(a: &str, b: &str) -> bool {
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
async fn delete_arrived_first(state: &AppState, actor: &str, uri: &str) -> bool {
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
struct RedisLock {
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
async fn acquire_create_lock(state: &AppState, uri: &str) -> Option<RedisLock> {
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

/// The lower-cased list of header names a `Signature` header claims to cover
/// (its `headers="…"` parameter). Empty when the parameter is absent.
fn signed_headers_list(sig_val: &str) -> Vec<String> {
    for part in sig_val.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("headers=") {
            return rest
                .trim_matches('"')
                .split_whitespace()
                .map(|s| s.to_ascii_lowercase())
                .collect();
        }
    }
    Vec::new()
}

/// True if an HTTP-date `Date` header is within `skew` of now (in either
/// direction). Rejects malformed dates.
fn date_within_skew(date_val: &str, skew: chrono::Duration) -> bool {
    let trimmed = date_val.trim();
    let parsed = chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT")
        .map(|n| n.and_utc())
        .or_else(|_| {
            chrono::DateTime::parse_from_rfc2822(trimmed).map(|d| d.with_timezone(&chrono::Utc))
        });
    match parsed {
        Ok(signed) => (chrono::Utc::now() - signed).abs() <= skew,
        Err(_) => false,
    }
}

/// Verify the HTTP Signature on an inbound activity. Returns `Ok(())` only when
/// the request is signed by a key belonging to the same host as the claimed
/// actor and the signature (and body digest) check out.
async fn verify_inbound_signature(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    path: &str,
    body: &[u8],
    actor_uri: &str,
) -> Result<(), String> {
    let sig_val = headers
        .get("signature")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| "missing Signature header".to_string())?;

    let kid = crate::federation::signature::key_id_from_header(sig_val)
        .ok_or_else(|| "no keyId in Signature header".to_string())?;
    let key_actor = kid.split('#').next().unwrap_or(kid);

    // The signing key must belong to the same host as the activity's actor, so a
    // valid signature from one server cannot authorize an activity attributed to
    // an actor on another server.
    if !actor_uri.is_empty() && !same_host(key_actor, actor_uri) {
        return Err(format!(
            "signing key host does not match actor ({key_actor} vs {actor_uri})"
        ));
    }

    // feder's verify_request only checks the Digest if a Digest header happens to
    // be present, and never bounds the signature's age. Without the following an
    // attacker could sign only `date` and then swap the body (recomputing the
    // Digest header that the signature never covered), or replay a captured
    // request indefinitely. So, before doing any work:
    //   * require the signature to actually cover (request-target), host, date,
    //     and digest;
    //   * require a Digest header (feder then binds it to the body);
    //   * reject stale or future-dated requests (±1h skew, as Mastodon does).
    let covered = signed_headers_list(sig_val);
    for required in ["(request-target)", "host", "date", "digest"] {
        if !covered.iter().any(|h| h == required) {
            return Err(format!(
                "signature does not cover required header {required:?}"
            ));
        }
    }
    if headers.get("digest").is_none() {
        return Err("missing Digest header".to_string());
    }
    let date_val = headers
        .get("date")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| "missing Date header".to_string())?;
    if !date_within_skew(date_val, chrono::Duration::hours(1)) {
        return Err(format!(
            "Date header outside acceptable clock skew: {date_val:?}"
        ));
    }

    let pem = fetch_public_key(state, key_actor)
        .await
        .map_err(|e| format!("could not fetch public key: {e}"))?;

    let hdr_vec = crate::federation::signature::headers_to_vec(headers);
    let hdr_refs: Vec<(&str, &str)> = hdr_vec
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    match crate::federation::signature::verify_request("post", path, &hdr_refs, body, &pem) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            let refreshed_pem = refresh_public_key(state, key_actor)
                .await
                .map_err(|e| format!("could not refresh public key: {e}"))?;
            crate::federation::signature::verify_request(
                "post",
                path,
                &hdr_refs,
                body,
                &refreshed_pem,
            )
            .map_err(|e| format!("{first_err}; after key refresh: {e}"))
        }
    }
}

/// Authenticate an inbound activity via its FEP-8b32 Object Integrity Proof.
///
/// Used as a fallback when the HTTP Signature can't be verified. The proof's
/// `verificationMethod` must live on the actor's host and be declared by the
/// actor as one of its `assertionMethod` keys, which binds the signing key to
/// the claimed actor.
async fn verify_object_integrity(
    state: &AppState,
    activity: &serde_json::Value,
    actor_uri: &str,
) -> Result<(), String> {
    let (proof, verification_method) =
        feder_runtime::integrity::extract_integrity_proof(activity)
            .ok_or_else(|| "no eddsa-jcs-2022 integrity proof".to_string())?;

    // The signing key must belong to the same host as the actor.
    if actor_uri.is_empty() || !same_host(&verification_method, actor_uri) {
        return Err(format!(
            "proof key host does not match actor ({verification_method} vs {actor_uri})"
        ));
    }

    // Fetch the actor document and confirm it declares this verification method
    // as an assertionMethod, then read the Ed25519 public key.
    let actor_doc = crate::federation::fetch::signed_get_json(state, actor_uri)
        .await
        .map_err(|e| format!("could not fetch actor for proof key: {e}"))?;
    let multibase = assertion_method_key(&actor_doc, &verification_method)
        .ok_or_else(|| format!("actor does not declare assertionMethod {verification_method}"))?;
    let public_key = feder_runtime::integrity::decode_ed25519_multikey(&multibase)
        .map_err(|e| format!("invalid assertionMethod key: {e}"))?;

    feder_runtime::integrity::verify_object_integrity_proof(activity, &proof, &public_key)
        .map_err(|e| e.to_string())
}

/// Find the `publicKeyMultibase` of the `assertionMethod` whose id equals
/// `verification_method` in a fetched actor document. Handles the field being a
/// single Multikey object or an array of them.
fn assertion_method_key(actor: &serde_json::Value, verification_method: &str) -> Option<String> {
    let methods = actor.get("assertionMethod")?;
    let entries: Vec<&serde_json::Value> = match methods {
        serde_json::Value::Array(a) => a.iter().collect(),
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => return None,
    };
    entries.into_iter().find_map(|m| {
        if m.get("id").and_then(|v| v.as_str()) == Some(verification_method) {
            m.get("publicKeyMultibase")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        } else {
            None
        }
    })
}

async fn fetch_public_key(state: &AppState, actor_url: &str) -> anyhow::Result<String> {
    if let Some(pem) = sqlx::query_scalar!(
        "SELECT public_key FROM accounts WHERE uri = $1 AND public_key != ''",
        actor_url,
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(pem);
    }

    refresh_public_key(state, actor_url).await
}

async fn refresh_public_key(state: &AppState, actor_url: &str) -> anyhow::Result<String> {
    let actor = crate::federation::fetch::signed_get_json(state, actor_url).await?;
    let pem = public_key_from_actor(&actor)?;

    sqlx::query!(
        "UPDATE accounts SET public_key = $2, updated_at = now() WHERE uri = $1 AND domain IS NOT NULL",
        actor_url,
        pem,
    )
    .execute(&state.db)
    .await?;

    Ok(pem)
}

fn public_key_from_actor(actor: &Value) -> anyhow::Result<String> {
    Ok(actor
        .get("publicKey")
        .and_then(|k| k.get("publicKeyPem"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow::anyhow!("no publicKeyPem"))?
        .to_string())
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

async fn handle_create(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let object = match activity.get("object") {
        Some(o) if o.is_object() => o,
        Some(o) if o.is_string() => {
            if let Some(uri) = o.as_str() {
                let _ = fetch_remote_status(state, uri).await?;
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    let obj_type = object.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if obj_type != "Note" {
        return Ok(());
    }

    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let note_uri = object.get("id").and_then(|i| i.as_str()).unwrap_or("");
    if note_uri.is_empty() || actor_uri.is_empty() {
        return Ok(());
    }

    // Serialize against a concurrent Delete for this uri so its `delete_later`
    // can't slip in between the check below and our insert. Held for the whole
    // creation (released when this guard drops on return).
    let _create_lock = acquire_create_lock(state, note_uri).await;

    // Skip a Create whose Delete already arrived out of order (Redis tombstone),
    // in addition to the persistent tombstone check below.
    if delete_arrived_first(state, actor_uri, note_uri).await {
        return Ok(());
    }

    // Tombstone check
    let tombstoned = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM tombstones WHERE uri = $1)",
        note_uri,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if tombstoned {
        return Ok(());
    }

    let account_id = match resolve_or_fetch_remote_account(state, actor_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    // Parse tag array once: mentions, hashtags, emojis.
    // ActivityPub allows "tag" to be either a single object or an array.
    let tags_arr: Vec<Value> = match object.get("tag") {
        Some(Value::Array(arr)) => arr.clone(),
        Some(obj @ Value::Object(_)) => vec![obj.clone()],
        _ => vec![],
    };

    let mention_hrefs: Vec<String> = tags_arr
        .iter()
        .filter(|t| tag_type_is(t, "Mention"))
        .filter_map(|t| t.get("href").and_then(|v| v.as_str()).map(str::to_owned))
        .collect();

    // Collect to/cc from both the activity wrapper and the Note object (Mastodon merges both).
    // Both fields may be a string or an array.
    let audience: Vec<String> = {
        let mut a = as_string_vec(activity.get("to"));
        a.extend(as_string_vec(activity.get("cc")));
        a.extend(as_string_vec(object.get("to")));
        a.extend(as_string_vec(object.get("cc")));
        a.sort_unstable();
        a.dedup();
        a
    };

    // Look up inReplyTo status (id + account_id + whether account is local)
    let in_reply_to_uri = object.get("inReplyTo").and_then(|v| v.as_str());
    let in_reply_to_row = if let Some(uri) = in_reply_to_uri {
        sqlx::query!(
            r#"SELECT s.id, s.account_id, (a.domain IS NULL) AS "is_local!"
               FROM statuses s JOIN accounts a ON a.id = s.account_id
               WHERE s.uri = $1"#,
            uri,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    let in_reply_to_id = in_reply_to_row.as_ref().map(|r| r.id);
    let in_reply_to_account_id = in_reply_to_row.as_ref().map(|r| r.account_id);
    let in_reply_to_local = in_reply_to_row.as_ref().is_some_and(|r| r.is_local);

    // Mastodon serializes poll votes as Create(Note) where the Note's
    // `inReplyTo` is the poll status and `name` is the selected option. Store
    // these as poll_votes instead of creating a visible status.
    if let (Some(parent_id), Some(choice_name)) = (
        in_reply_to_id,
        object
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty()),
    ) {
        if handle_poll_vote_note(state, account_id, parent_id, choice_name, note_uri).await? {
            return Ok(());
        }
    }

    // Acceptance filter: only process if related to local activity (mirrors Mastodon's
    // related_to_local_activity? / addresses_local_accounts? checks).
    let is_followed_locally = sqlx::query_scalar!(
        r#"SELECT EXISTS(
            SELECT 1 FROM follows f
            JOIN accounts a ON a.id = f.account_id
            WHERE f.target_account_id = $1 AND a.domain IS NULL
        )"#,
        account_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);

    // Any URI in to/cc (from either the activity or the Note) that is a local account.
    let addresses_local = if !audience.is_empty() {
        sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE uri = ANY($1) AND domain IS NULL)",
            &audience as &[String],
        )
        .fetch_one(&state.db)
        .await?
        .unwrap_or(false)
    } else {
        false
    };

    if !is_followed_locally && !addresses_local && !in_reply_to_local {
        tracing::debug!(
            note_uri,
            "Create(Note): ignoring, not related to local activity"
        );
        return Ok(());
    }

    // Field extraction
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
    let url = object
        .get("url")
        .and_then(|u| u.as_str())
        .map(str::to_owned);
    let published = object
        .get("published")
        .and_then(|p| p.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc());
    let edited_at = object
        .get("updated")
        .and_then(|p| p.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc());

    // Visibility is determined from the Note object's own to/cc fields.
    let note_to = as_string_vec(object.get("to"));
    let note_cc = as_string_vec(object.get("cc"));
    let visibility = crate::db::models::vis::from_audience(&note_to, &note_cc);

    let language = object
        .get("contentMap")
        .and_then(|m| m.as_object())
        .and_then(|m| m.keys().next())
        .map(|s| s.to_string())
        .filter(|s| ["ko", "en"].contains(&s.as_str()));

    // FEP-044f quote linkage. Resolved after the status is inserted (below) so a
    // quoted post that quotes back can't recurse forever.
    let quote_uri = object
        .get("quote")
        .and_then(|v| v.as_str())
        .or_else(|| object.get("quoteUrl").and_then(|v| v.as_str()))
        .or_else(|| object.get("quoteUri").and_then(|v| v.as_str()))
        .or_else(|| object.get("_misskey_quote").and_then(|v| v.as_str()));

    let status_id = crate::snowflake::next_id();
    let created_at = published.unwrap_or_else(|| chrono::Utc::now().naive_utc());

    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO statuses
             (id, account_id, text, spoiler_text, visibility, sensitive,
              uri, url, in_reply_to_id, in_reply_to_account_id, reply,
              language, local, created_at, edited_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, false, $13,$14, now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL AND uri != '' DO NOTHING
           RETURNING id"#,
        status_id,
        account_id,
        text,
        spoiler_text,
        visibility,
        sensitive,
        note_uri,
        url,
        in_reply_to_id,
        in_reply_to_account_id,
        // A status with an inReplyTo is a reply even when its parent isn't known
        // locally; marking it so lets the home-feed reply filter treat an
        // unresolved-parent reply as an orphan (hidden) instead of a top-level post.
        in_reply_to_uri.is_some(),
        language,
        created_at,
        edited_at,
    )
    .fetch_optional(&state.db)
    .await?;

    let Some(inserted_id) = inserted else {
        return Ok(()); // duplicate
    };

    // Record the FEP-044f quote. Matching Mastodon, fetch the quoted post when
    // it isn't cached locally so the quote serializes instead of being silently
    // dropped; the fetch is bounded by fetch_remote_status's depth limit.
    if let Some(q) = quote_uri {
        let mut quoted: Option<(i64, i64)> = sqlx::query!(
            "SELECT id, account_id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
            q,
        )
        .fetch_optional(&state.db)
        .await?
        .map(|r| (r.id, r.account_id));
        if quoted.is_none() {
            if let Some(qid) = fetch_remote_status(state, q).await? {
                quoted = sqlx::query!("SELECT id, account_id FROM statuses WHERE id = $1", qid)
                    .fetch_optional(&state.db)
                    .await?
                    .map(|r| (r.id, r.account_id));
            }
        }
        if let Some((quoted_id, quoted_account_id)) = quoted {
            let _ = sqlx::query!(
                r#"INSERT INTO quotes
                     (id, status_id, quoted_status_id, account_id, quoted_account_id, state, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, 1, now(), now())
                   ON CONFLICT (status_id) DO NOTHING"#,
                crate::snowflake::next_id(),
                inserted_id,
                quoted_id,
                account_id,
                quoted_account_id,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Media attachments. Domains blocked with `reject_media` (or fully
    // suspended) federate text but not media, so skip storing attachments.
    let attachments: Vec<Value> =
        if crate::federation::moderation::actor_media_rejected(state, actor_uri).await {
            Vec::new()
        } else {
            object
                .get("attachment")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default()
        };
    let mut media_ids: Vec<i64> = Vec::new();
    for att in &attachments {
        // Mastodon caps a status at MEDIA_ATTACHMENTS_LIMIT (4).
        if media_ids.len() >= 4 {
            break;
        }
        let att_type_str = att.get("type").and_then(|v| v.as_str()).unwrap_or("");
        // `url` may be a string, a Link object (`{href, mediaType}`), or an
        // array of links — Mastodon resolves all of these.
        let Some((remote_url, link_media_type)) = att.get("url").and_then(attachment_url) else {
            continue;
        };
        // mediaType: explicit, else from the chosen Link, else guessed from the
        // URL's extension (matches Mastodon's `mediaType || url_to_media_type`).
        let media_type_str = att
            .get("mediaType")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or(link_media_type)
            .or_else(|| {
                let path = remote_url.split(['?', '#']).next().unwrap_or(&remote_url);
                mime_guess::from_path(path).first_raw().map(str::to_owned)
            })
            .unwrap_or_default();
        // Classify from mediaType — Mastodon serializes `type: "Document"` for
        // everything — falling back to the AP `type` hint for odd peers.
        let att_type = classify_attachment_type(att_type_str, &media_type_str);
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
            Some(media_type_str.clone())
        };
        let file_meta = ap_attachment_file_meta(att);

        let media_id = crate::snowflake::next_id();
        match sqlx::query_scalar!(
            r#"INSERT INTO media_attachments
                 (id, account_id, status_id, remote_url, description, blurhash,
                  type, thumbnail_remote_url, file_content_type, file_meta, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, now(), now())
               RETURNING id"#,
            media_id,
            account_id,
            inserted_id,
            remote_url,
            description,
            blurhash,
            att_type,
            thumbnail_remote_url,
            file_content_type,
            file_meta,
        )
        .fetch_one(&state.db)
        .await
        {
            Ok(id) => media_ids.push(id),
            Err(e) => tracing::warn!(error = %e, "failed to insert media attachment"),
        }
    }
    if !media_ids.is_empty() {
        let _ = sqlx::query!(
            "UPDATE statuses SET ordered_media_attachment_ids = $1 WHERE id = $2",
            &media_ids,
            inserted_id,
        )
        .execute(&state.db)
        .await;
    }

    // Hashtags
    let hashtag_names: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        tags_arr
            .iter()
            .filter(|t| tag_type_is(t, "Hashtag"))
            .filter_map(|t| {
                t.get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| n.trim_start_matches('#').to_lowercase())
            })
            .filter(|n| !n.is_empty() && seen.insert(n.clone()))
            .collect()
    };
    let mut tag_ids: Vec<i64> = Vec::new();
    for name in &hashtag_names {
        let tag_id = crate::snowflake::next_id();
        match sqlx::query_scalar!(
            r#"INSERT INTO tags (id, name, last_status_at, created_at, updated_at)
               VALUES ($1, $2, now(), now(), now())
               ON CONFLICT (lower(name)) DO UPDATE SET last_status_at = now(), updated_at = now()
               RETURNING id"#,
            tag_id,
            name,
        )
        .fetch_optional(&state.db)
        .await
        {
            Ok(Some(id)) => {
                tag_ids.push(id);
                let _ = sqlx::query!(
                    "INSERT INTO statuses_tags (status_id, tag_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
                    inserted_id,
                    id,
                )
                .execute(&state.db)
                .await;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(tag = name, error = %e, "failed to upsert hashtag"),
        }
    }

    // Mentions — resolve accounts and notify local ones
    let actor_info = sqlx::query!(
        "SELECT display_name, username, domain, avatar_remote_url FROM accounts WHERE id = $1",
        account_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    for href in &mention_hrefs {
        let mentioned_id = match resolve_or_fetch_remote_account(state, href).await {
            Ok(id) => id,
            Err(_) => continue,
        };
        let _ = sqlx::query!(
            "INSERT INTO mentions (status_id, account_id, created_at, updated_at) VALUES ($1,$2, now(), now()) ON CONFLICT DO NOTHING",
            inserted_id,
            mentioned_id,
        )
        .execute(&state.db)
        .await;

        let is_local = sqlx::query_scalar!(
            r#"SELECT (domain IS NULL) AS "v!" FROM accounts WHERE id = $1"#,
            mentioned_id,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
        if is_local {
            if let Some(ref info) = actor_info {
                let acct = match &info.domain {
                    Some(d) => format!("{}@{}", info.username, d),
                    None => info.username.clone(),
                };
                crate::push::create_and_push(
                    state,
                    mentioned_id,
                    account_id,
                    "mention",
                    Some(inserted_id),
                    format!("New mention from {}", info.display_name),
                    acct,
                    info.avatar_remote_url.clone().unwrap_or_default(),
                )
                .await;
            }
        }
    }

    // Conversation management for direct messages.
    // Mirrors Mastodon: participants = sender + status.active_mentions (explicit Mention tags).
    if visibility == crate::db::models::vis::DIRECT {
        // Reuse the parent status's conversation if this is a reply, otherwise create a new one.
        let conversation_id: i64 = if let Some(parent_id) = in_reply_to_id {
            let parent_conv = sqlx::query_scalar!(
                "SELECT conversation_id FROM statuses WHERE id = $1",
                parent_id,
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
            .flatten();
            if let Some(cid) = parent_conv {
                cid
            } else {
                sqlx::query_scalar!(
                    "INSERT INTO conversations (created_at, updated_at) VALUES (now(), now()) RETURNING id"
                )
                .fetch_one(&state.db)
                .await?
            }
        } else {
            sqlx::query_scalar!(
                "INSERT INTO conversations (created_at, updated_at) VALUES (now(), now()) RETURNING id"
            )
            .fetch_one(&state.db)
            .await?
        };

        let _ = sqlx::query!(
            "UPDATE statuses SET conversation_id = $1 WHERE id = $2",
            conversation_id,
            inserted_id,
        )
        .execute(&state.db)
        .await;

        // Participants = sender + explicitly mentioned accounts (mirrors Mastodon's active_mentions).
        let mentioned_local_ids: Vec<i64> = sqlx::query_scalar!(
            r#"SELECT m.account_id FROM mentions m
               JOIN accounts a ON a.id = m.account_id
               WHERE m.status_id = $1 AND a.domain IS NULL"#,
            inserted_id,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let mut all_participant_ids: Vec<i64> = std::iter::once(account_id)
            .chain(mentioned_local_ids.iter().copied())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        all_participant_ids.sort_unstable();

        // For each local recipient, upsert account_conversations.
        // participant_account_ids = everyone else in the conversation (not this recipient).
        for &local_id in &mentioned_local_ids {
            let mut others: Vec<i64> = all_participant_ids
                .iter()
                .copied()
                .filter(|&id| id != local_id)
                .collect();
            others.sort_unstable();
            let _ = sqlx::query!(
                r#"INSERT INTO account_conversations
                     (account_id, conversation_id, participant_account_ids, status_ids, last_status_id, unread)
                   VALUES ($1, $2, $3, ARRAY[$4::bigint], $4, true)
                   ON CONFLICT (account_id, conversation_id, participant_account_ids) DO UPDATE
                     SET status_ids = array_append(account_conversations.status_ids, $4),
                         last_status_id = $4,
                         unread = true"#,
                local_id,
                conversation_id,
                &others,
                inserted_id,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Custom emojis
    let actor_domain = url::Url::parse(actor_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    for tag in tags_arr.iter().filter(|t| tag_type_is(t, "Emoji")) {
        let shortcode = match tag.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.trim_matches(':').to_string(),
            None => continue,
        };
        let image_remote_url = match tag
            .get("icon")
            .and_then(|i| i.get("url"))
            .and_then(|v| v.as_str())
        {
            Some(u) => u.to_string(),
            None => continue,
        };
        let uri = tag.get("id").and_then(|v| v.as_str()).map(str::to_owned);
        let emoji_id = crate::snowflake::next_id();
        let _ = sqlx::query!(
            r#"INSERT INTO custom_emojis
                 (id, shortcode, domain, image_remote_url, uri, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,now(),now())
               ON CONFLICT (shortcode, domain)
               DO UPDATE SET image_remote_url = EXCLUDED.image_remote_url, updated_at = now()"#,
            emoji_id,
            shortcode,
            actor_domain,
            image_remote_url,
            uri,
        )
        .execute(&state.db)
        .await;
    }

    // Poll
    let poll_items = object.get("oneOf").or_else(|| object.get("anyOf"));
    if let Some(items) = poll_items.and_then(|v| v.as_array()) {
        let multiple = object.get("anyOf").is_some();
        let options: Vec<String> = items
            .iter()
            .filter_map(|item| item.get("name").and_then(|v| v.as_str()).map(str::to_owned))
            .collect();
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
        let poll_id = crate::snowflake::next_id();
        if let Ok(Some(_)) = sqlx::query_scalar!(
            r#"INSERT INTO polls
                 (id, status_id, account_id, options, cached_tallies, votes_count,
                  multiple, expires_at, created_at, updated_at)
               SELECT $1,$2,$3,$4,$5,$6,$7,$8,now(),now()
               WHERE NOT EXISTS (SELECT 1 FROM polls WHERE status_id = $2)
               RETURNING id"#,
            poll_id,
            inserted_id,
            account_id,
            &options as &[String],
            &cached_tallies as &[i64],
            votes_count,
            multiple,
            expires_at,
        )
        .fetch_optional(&state.db)
        .await
        {
            let _ = sqlx::query!(
                "UPDATE statuses SET poll_id = $1 WHERE id = $2",
                poll_id,
                inserted_id,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Thread resolution: store the unknown parent if it is dereferenceable.
    if let (Some(uri), None) = (in_reply_to_uri, in_reply_to_id) {
        let state = state.clone();
        let uri = uri.to_owned();
        let child_id = inserted_id;
        let child_author = account_id;
        tokio::spawn(async move {
            tracing::debug!(uri, "fetching unknown parent status for thread resolution");
            if let Err(e) = fetch_remote_status(&state, &uri).await {
                tracing::debug!(uri, error = %e, "failed to store fetched parent status");
                return;
            }
            // Link the now-known parent onto the child and re-run home fan-out, so
            // a reply to an account the viewer follows (whose post we only just
            // learned about) reaches the right followers instead of staying hidden
            // as an orphan reply.
            if let Ok(Some(parent)) =
                sqlx::query!("SELECT id, account_id FROM statuses WHERE uri = $1", uri,)
                    .fetch_optional(&state.db)
                    .await
            {
                let updated = sqlx::query!(
                    "UPDATE statuses SET in_reply_to_id = $2, in_reply_to_account_id = $3, updated_at = now() WHERE id = $1 AND in_reply_to_id IS NULL",
                    child_id, parent.id, parent.account_id,
                )
                .execute(&state.db)
                .await;
                if updated.map(|r| r.rows_affected() > 0).unwrap_or(false) {
                    let mut redis = state.redis.clone();
                    let db = state.db.clone();
                    crate::feed::fanout_new_status(&mut redis, &db, child_author, child_id, &[])
                        .await;
                }
            }
        });
    }

    // Fanout to home and list feeds
    let vis_str = crate::db::models::vis::to_str(visibility);
    let mut redis = state.redis.clone();
    let db = state.db.clone();
    if crate::feed::sync_fanout() {
        crate::feed::fanout_new_status(&mut redis, &db, account_id, inserted_id, &tag_ids).await;
        crate::feed::fanout_to_lists(
            &mut redis,
            &db,
            account_id,
            inserted_id,
            in_reply_to_account_id,
            vis_str,
        )
        .await;
    } else {
        tokio::spawn(async move {
            crate::feed::fanout_new_status(&mut redis, &db, account_id, inserted_id, &tag_ids)
                .await;
            crate::feed::fanout_to_lists(
                &mut redis,
                &db,
                account_id,
                inserted_id,
                in_reply_to_account_id,
                vis_str,
            )
            .await;
        });
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
async fn refresh_collection_item_count(state: &AppState, collection_id: i64) -> AppResult<()> {
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

async fn sync_remote_poll(
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

async fn handle_poll_vote_note(
    state: &AppState,
    voter_id: i64,
    status_id: i64,
    choice_name: &str,
    vote_uri: &str,
) -> AppResult<bool> {
    let Some(poll) = sqlx::query!(
        "SELECT id, options, multiple, expires_at FROM polls WHERE status_id = $1",
        status_id,
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(false);
    };

    if poll
        .expires_at
        .map(|e| e < chrono::Utc::now().naive_utc())
        .unwrap_or(false)
    {
        return Ok(true);
    }

    let Some(choice) = poll.options.iter().position(|option| option == choice_name) else {
        return Ok(true);
    };
    let choice = choice as i32;

    let already_voted = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM poll_votes WHERE poll_id = $1 AND account_id = $2)",
        poll.id,
        voter_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);

    if !poll.multiple && already_voted {
        return Ok(true);
    }

    sqlx::query!(
        r#"INSERT INTO poll_votes (account_id, poll_id, choice, uri, created_at, updated_at)
           VALUES ($1, $2, $3, $4, now(), now())
           ON CONFLICT DO NOTHING"#,
        voter_id,
        poll.id,
        choice,
        vote_uri,
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        "UPDATE polls SET votes_count = (SELECT COUNT(*) FROM poll_votes WHERE poll_id = $1), updated_at = now() WHERE id = $1",
        poll.id,
    )
    .execute(&state.db)
    .await?;

    if poll.multiple && !already_voted {
        sqlx::query!(
            "UPDATE polls SET voters_count = COALESCE(voters_count, 0) + 1, updated_at = now() WHERE id = $1",
            poll.id,
        )
        .execute(&state.db)
        .await?;
    }

    Ok(true)
}

async fn handle_block(state: &AppState, activity: &Value) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let activity_uri = activity.get("id").and_then(|i| i.as_str()).unwrap_or("");

    // Skip a Block whose Undo already arrived out of order.
    if delete_arrived_first(state, actor_uri, activity_uri).await {
        return Ok(());
    }

    let object_uri = activity
        .get("object")
        .and_then(|o| {
            if o.is_string() {
                o.as_str()
            } else {
                o.get("id").and_then(|i| i.as_str())
            }
        })
        .unwrap_or("");

    // Only process if the blocked account is local
    let Some(target_id) = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = $1 AND domain IS NULL",
        object_uri
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };

    let blocker_id = resolve_or_fetch_remote_account(state, actor_uri).await?;

    sqlx::query!(
        "INSERT INTO blocks (account_id, target_account_id, created_at, updated_at) VALUES ($1,$2, now(), now()) ON CONFLICT DO NOTHING",
        blocker_id, target_id,
    ).execute(&state.db).await?;

    // Remove mutual follows
    let deleted = sqlx::query!(
        "DELETE FROM follows WHERE (account_id=$1 AND target_account_id=$2) OR (account_id=$2 AND target_account_id=$1) RETURNING account_id, target_account_id",
        blocker_id, target_id,
    ).fetch_all(&state.db).await?;
    for row in &deleted {
        let _ =
            crate::counters::on_follow_removed(&state.db, row.account_id, row.target_account_id)
                .await;
    }
    sqlx::query!(
        "DELETE FROM follow_requests WHERE (account_id=$1 AND target_account_id=$2) OR (account_id=$2 AND target_account_id=$1)",
        blocker_id, target_id,
    ).execute(&state.db).await?;
    // Mirror Mastodon's FollowRequest dependent: :destroy — clear the local
    // target's follow_request notification from the remote blocker.
    sqlx::query!(
        "DELETE FROM notifications WHERE account_id = $1 AND from_account_id = $2 AND type = 'follow_request'",
        target_id, blocker_id,
    ).execute(&state.db).await?;

    Ok(())
}

async fn handle_flag(state: &AppState, activity: &Value) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let comment = activity
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let activity_uri = activity
        .get("id")
        .and_then(|i| i.as_str())
        .map(str::to_owned);

    // object can be a mixed array of account URIs and status URIs, or a single string
    let objects = as_string_vec(activity.get("object"));

    // Resolve the reporter (remote account)
    let reporter_id = match resolve_or_fetch_remote_account(state, actor_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    // Find local accounts among the objects
    let local_account_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = ANY($1) AND domain IS NULL",
        &objects as &[String],
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let Some(&target_account_id) = local_account_ids.first() else {
        return Ok(());
    };

    // Find local statuses among the objects
    let status_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = ANY($1) AND deleted_at IS NULL",
        &objects as &[String],
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let report_id = crate::snowflake::next_id();
    sqlx::query!(
        r#"INSERT INTO reports (id, account_id, target_account_id, status_ids, comment, uri, forwarded, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,false,now(),now())
           ON CONFLICT DO NOTHING"#,
        report_id, reporter_id, target_account_id, &status_ids as &[i64], comment, activity_uri,
    ).execute(&state.db).await?;

    Ok(())
}

async fn handle_move(state: &AppState, activity: &Value) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let target_uri = activity
        .get("object")
        .and_then(|o| {
            if o.is_string() {
                o.as_str()
            } else {
                o.get("id").and_then(|i| i.as_str())
            }
        })
        .unwrap_or("");

    if actor_uri.is_empty() || target_uri.is_empty() {
        return Ok(());
    }

    // Fetch the new account to verify also_known_as contains the old actor URI
    let new_account_id = match resolve_or_fetch_remote_account(state, target_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    // Fetch the target actor to verify also_known_as
    let also_known_as: Vec<String> = sqlx::query_scalar!(
        "SELECT also_known_as FROM accounts WHERE id = $1",
        new_account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or_default();

    if !also_known_as.iter().any(|u| u == actor_uri) {
        tracing::warn!(
            actor_uri,
            target_uri,
            "Move rejected: target alsoKnownAs does not include actor"
        );
        return Ok(());
    }

    // Set moved_to_account_id on the old account
    sqlx::query!(
        "UPDATE accounts SET moved_to_account_id = $1 WHERE uri = $2 AND domain IS NOT NULL",
        new_account_id,
        actor_uri,
    )
    .execute(&state.db)
    .await?;

    tracing::debug!(
        actor_uri,
        target_uri,
        "processed Move: updated moved_to_account_id"
    );
    Ok(())
}

/// Read a JSON value that may be a string IRI or an object with an `id`.
fn json_uri(v: Option<&Value>) -> &str {
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
async fn upsert_remote_collection(
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
async fn mirror_item_into(state: &AppState, collection_id: i64, item: &Value) -> AppResult<()> {
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

async fn handle_add(state: &AppState, activity: &Value) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = json_uri(activity.get("object"));
    let target_uri = json_uri(activity.get("target"));

    if actor_uri.is_empty() || object_uri.is_empty() {
        return Ok(());
    }

    // Collection mirroring: Add(FeaturedCollection) / Add(FeaturedItem).
    if let Some(obj) = activity.get("object").filter(|o| o.is_object()) {
        match obj.get("type").and_then(|t| t.as_str()) {
            Some("FeaturedCollection") => {
                if let Ok(owner_id) = resolve_or_fetch_remote_account(state, actor_uri).await {
                    if let Some(cid) = upsert_remote_collection(state, owner_id, obj).await? {
                        if let Some(items) = obj.get("orderedItems").and_then(|v| v.as_array()) {
                            for it in items {
                                let _ = mirror_item_into(state, cid, it).await;
                            }
                        }
                    }
                }
                return Ok(());
            }
            Some("FeaturedItem") => {
                if let Some(cid) = sqlx::query_scalar!(
                    "SELECT id FROM collections WHERE uri = $1 AND local = false",
                    target_uri,
                )
                .fetch_optional(&state.db)
                .await?
                {
                    mirror_item_into(state, cid, obj).await?;
                }
                return Ok(());
            }
            _ => {}
        }
    }

    // Check that the target is the actor's featured collection (pinned posts)
    let featured_url = sqlx::query_scalar!(
        "SELECT featured_collection_url FROM accounts WHERE uri = $1",
        actor_uri,
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or_default();

    if target_uri != featured_url {
        return Ok(());
    }

    let account_id = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri,)
        .fetch_optional(&state.db)
        .await?;
    let status_id = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
        object_uri,
    )
    .fetch_optional(&state.db)
    .await?;

    if let (Some(aid), Some(sid)) = (account_id, status_id) {
        let pin_id = crate::snowflake::next_id();
        sqlx::query!(
            "INSERT INTO status_pins (id, account_id, status_id, created_at, updated_at) VALUES ($1,$2,$3,now(),now()) ON CONFLICT DO NOTHING",
            pin_id, aid, sid,
        ).execute(&state.db).await?;
    }

    Ok(())
}

async fn handle_remove(state: &AppState, activity: &Value) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity
        .get("object")
        .and_then(|o| {
            if o.is_string() {
                o.as_str()
            } else {
                o.get("id").and_then(|i| i.as_str())
            }
        })
        .unwrap_or("");

    // Collection mirroring: Remove(FeaturedCollection) deletes the mirrored
    // collection; Remove(FeaturedItem) deletes the mirrored item. Only the
    // collection owner (the activity actor) may do so.
    let removed_collection = sqlx::query!(
        r#"DELETE FROM collections
           WHERE uri = $1 AND local = false
             AND account_id = (SELECT id FROM accounts WHERE uri = $2)
           RETURNING id"#,
        object_uri,
        actor_uri,
    )
    .fetch_optional(&state.db)
    .await?;
    if removed_collection.is_some() {
        return Ok(());
    }
    let removed_item = sqlx::query!(
        r#"DELETE FROM collection_items
           WHERE uri = $1 AND collection_id IN (
               SELECT id FROM collections
               WHERE account_id = (SELECT id FROM accounts WHERE uri = $2)
           )
           RETURNING collection_id"#,
        object_uri,
        actor_uri,
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(r) = removed_item {
        refresh_collection_item_count(state, r.collection_id).await?;
        return Ok(());
    }

    let account_id = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
        .fetch_optional(&state.db)
        .await?;
    let status_id = sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", object_uri)
        .fetch_optional(&state.db)
        .await?;

    if let (Some(aid), Some(sid)) = (account_id, status_id) {
        sqlx::query!(
            "DELETE FROM status_pins WHERE account_id = $1 AND status_id = $2",
            aid,
            sid
        )
        .execute(&state.db)
        .await?;
    }

    Ok(())
}

async fn handle_quote_request(
    state: &AppState,
    instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let req_id = activity.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity
        .get("object")
        .and_then(|o| o.as_str())
        .unwrap_or("");
    let instrument_uri = json_uri(activity.get("instrument"));

    if req_id.is_empty() || object_uri.is_empty() || actor_uri.is_empty() {
        return Ok(());
    }

    // The quoted status must be one of ours.
    let Some(status) = sqlx::query!(
        r#"SELECT s.id, s.account_id, s.quote_approval_policy,
                  a.username, a.uri AS account_uri, a.private_key
           FROM statuses s JOIN accounts a ON a.id = s.account_id
           WHERE s.uri = $1 AND s.deleted_at IS NULL AND a.domain IS NULL"#,
        object_uri,
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };

    let Ok(quoter_id) = resolve_or_fetch_remote_account(state, actor_uri).await else {
        return Ok(());
    };
    let quoter = sqlx::query!(
        "SELECT uri, inbox_url, shared_inbox_url FROM accounts WHERE id = $1",
        quoter_id,
    )
    .fetch_one(&state.db)
    .await?;
    let inbox = if !quoter.shared_inbox_url.is_empty() {
        quoter.shared_inbox_url
    } else {
        quoter.inbox_url
    };
    if status.private_key.as_deref().is_none_or(|s| s.is_empty()) {
        return Ok(());
    }
    if inbox.is_empty() {
        return Ok(());
    }

    let domain = &instance.domain;
    let actor_url = status.account_uri.clone();
    let key_id = format!("{actor_url}#main-key");

    // quote_approval_policy 0 = public (auto-accept); anything else requires the
    // owner's manual approval, which we do not auto-grant -> reject.
    if status.quote_approval_policy != 0 {
        let reject_id = format!(
            "{actor_url}#rejects/quote_requests/{}",
            crate::snowflake::next_id()
        );
        if let Ok(r) =
            crate::federation::consent::reject(&reject_id, &actor_url, &quoter.uri, req_id)
        {
            if let Err(e) =
                crate::federation::delivery::deliver_to_inboxes(state, r, vec![inbox], key_id).await
            {
                tracing::warn!(error = %e, "failed to enqueue quote Reject");
            }
        }
        return Ok(());
    }

    // Auto-accept: stamp a QuoteAuthorization. Fetch the quoting status on
    // demand if we don't already have it.
    let quoting_status_id = match sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
        instrument_uri,
    )
    .fetch_optional(&state.db)
    .await?
    {
        Some(id) => id,
        None => {
            match fetch_remote_status(state, instrument_uri).await? {
                Some(id) => id,
                None => {
                    tracing::debug!(actor_uri, instrument_uri, "QuoteRequest accepted but quoting status could not be fetched; skipping stamp");
                    return Ok(());
                }
            }
        }
    };

    // Upsert the quote and mark it accepted (one quote per quoting status).
    let quote_id = sqlx::query_scalar!(
        r#"INSERT INTO quotes
             (id, status_id, quoted_status_id, account_id, quoted_account_id, activity_uri, state, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, 1, now(), now())
           ON CONFLICT (status_id) DO UPDATE
             SET state = 1, activity_uri = EXCLUDED.activity_uri, updated_at = now()
           RETURNING id"#,
        crate::snowflake::next_id(),
        quoting_status_id,
        status.id,
        quoter_id,
        status.account_id,
        req_id,
    )
    .fetch_one(&state.db)
    .await?;

    let authorization_uri = format!(
        "https://{domain}/users/{}/quote_authorizations/{quote_id}",
        status.username
    );
    sqlx::query!(
        "UPDATE quotes SET approval_uri = $2 WHERE id = $1",
        quote_id,
        authorization_uri,
    )
    .execute(&state.db)
    .await?;

    let accept_id = format!("{actor_url}#accepts/quote_requests/{quote_id}");
    if let Ok(accept) = crate::federation::consent::accept(
        &accept_id,
        &actor_url,
        &quoter.uri,
        req_id,
        &authorization_uri,
    ) {
        if let Err(e) =
            crate::federation::delivery::deliver_to_inboxes(state, accept, vec![inbox], key_id)
                .await
        {
            tracing::warn!(error = %e, "failed to enqueue quote Accept");
        }
    }

    Ok(())
}

/// Handle an incoming `FeatureRequest`: a remote collection wants to feature one
/// of our local accounts. We fetch/store the remote collection, record an
/// accepted item, and reply with an `Accept` whose `result` points at a
/// `FeatureAuthorization` we serve. (Rejection policy is intentionally simple:
/// suspended local accounts are skipped.)
async fn handle_feature_request(
    state: &AppState,
    instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let req_id = activity.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let account_uri = activity
        .get("object")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let collection_uri = activity
        .get("instrument")
        .and_then(|v| {
            if v.is_string() {
                v.as_str()
            } else {
                v.get("id").and_then(|i| i.as_str())
            }
        })
        .unwrap_or("");
    if req_id.is_empty() || account_uri.is_empty() || collection_uri.is_empty() {
        return Ok(());
    }

    // The featured account must be local and active.
    let Some(local) = sqlx::query!(
        "SELECT id, username, suspended_at, id_scheme FROM accounts WHERE uri = $1 AND domain IS NULL",
        account_uri,
    )
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(());
    };
    if local.suspended_at.is_some() {
        return Ok(());
    }

    // Fetch the remote FeaturedCollection to learn its owner and name.
    let coll: Value = match crate::federation::fetch::signed_get_json(state, collection_uri).await {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let owner_uri = coll
        .get("attributedTo")
        .and_then(|v| {
            if v.is_string() {
                v.as_str()
            } else {
                v.get("id").and_then(|i| i.as_str())
            }
        })
        .unwrap_or("");
    if owner_uri.is_empty() {
        return Ok(());
    }
    let Ok(owner_id) = resolve_or_fetch_remote_account(state, owner_uri).await else {
        return Ok(());
    };
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

    // Upsert the remote collection (local = false).
    let collection_id = sqlx::query_scalar!(
        r#"INSERT INTO collections
             (account_id, name, discoverable, local, sensitive, item_count, uri, created_at, updated_at)
           VALUES ($1, $2, $3, false, $4, 0, $5, now(), now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL
             DO UPDATE SET name = EXCLUDED.name, updated_at = now()
           RETURNING id"#,
        owner_id,
        name,
        discoverable,
        sensitive,
        collection_uri,
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(collection_id) = collection_id else {
        return Ok(());
    };

    // Record the accepted item with our authorization URI.
    let item_id = sqlx::query_scalar!(
        r#"INSERT INTO collection_items
             (collection_id, account_id, state, activity_uri, position, created_at, updated_at)
           VALUES ($1, $2, 1, $3,
                   (SELECT COALESCE(MAX(position), 0) + 1 FROM collection_items WHERE collection_id = $1),
                   now(), now())
           ON CONFLICT (account_id, collection_id)
             DO UPDATE SET state = 1, activity_uri = EXCLUDED.activity_uri, updated_at = now()
           RETURNING id"#,
        collection_id,
        local.id,
        req_id,
    )
    .fetch_one(&state.db)
    .await?;

    let domain = &instance.domain;
    let authorization_uri = format!(
        "https://{domain}/users/{}/feature_authorizations/{item_id}",
        local.username
    );
    sqlx::query!(
        "UPDATE collection_items SET approval_uri = $2 WHERE id = $1",
        item_id,
        authorization_uri,
    )
    .execute(&state.db)
    .await?;

    // Reply with Accept(result = our FeatureAuthorization) to the collection owner.
    let owner = sqlx::query!(
        "SELECT uri, inbox_url, shared_inbox_url FROM accounts WHERE id = $1",
        owner_id,
    )
    .fetch_one(&state.db)
    .await?;
    let has_signing_key =
        sqlx::query_scalar!("SELECT private_key FROM accounts WHERE id = $1", local.id,)
            .fetch_one(&state.db)
            .await?
            .is_some_and(|s| !s.is_empty());
    if !has_signing_key {
        return Ok(());
    }

    let inbox = if !owner.shared_inbox_url.is_empty() {
        owner.shared_inbox_url
    } else {
        owner.inbox_url
    };
    if !inbox.is_empty() {
        let actor_url =
            crate::federation::tag::account_uri(domain, local.id, local.id_scheme, &local.username);
        let accept_id = format!("{actor_url}#accepts/feature_requests/{item_id}");
        let owner_uri = owner.uri;
        if let Ok(accept) = crate::federation::consent::accept(
            &accept_id,
            &actor_url,
            &owner_uri,
            req_id,
            &authorization_uri,
        ) {
            let key_id = format!("{actor_url}#main-key");
            if let Err(e) =
                crate::federation::delivery::deliver_to_inboxes(state, accept, vec![inbox], key_id)
                    .await
            {
                tracing::warn!(error = %e, "failed to enqueue feature Accept");
            }
        }
    }

    Ok(())
}

/// Resolve a status by URI, fetching and storing it from its origin server if
/// not already known locally. Returns the local status id.
///
/// This stores the core of the Note (text, audience/visibility, in-reply-to and
/// quote linkage when the referenced posts are already local, and media); it
/// does not recurse into referenced posts. Returns `Ok(None)` if the object
/// can't be fetched or isn't a storable Note.
pub async fn fetch_remote_status(state: &AppState, uri: &str) -> AppResult<Option<i64>> {
    fetch_remote_status_depth(state, uri, 0).await
}

/// Largest depth to which `fetch_remote_status` follows references (in-reply-to
/// and quoted posts), to avoid unbounded fetch chains.
const MAX_FETCH_DEPTH: u8 = 2;

async fn fetch_remote_status_depth(
    state: &AppState,
    uri: &str,
    depth: u8,
) -> AppResult<Option<i64>> {
    if uri.is_empty() {
        return Ok(None);
    }
    if let Some(id) = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
        uri,
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(Some(id));
    }

    let fetched: Value = match crate::federation::fetch::signed_get_json(state, uri).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let nested_fetched;
    let object = match fetched.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "Create" | "Update" => match fetched.get("object") {
            Some(o) if o.is_object() => o,
            Some(o) if o.is_string() => {
                let Some(object_uri) = o.as_str() else {
                    return Ok(None);
                };
                nested_fetched =
                    match crate::federation::fetch::signed_get_json(state, object_uri).await {
                        Ok(v) => v,
                        Err(_) => return Ok(None),
                    };
                &nested_fetched
            }
            _ => return Ok(None),
        },
        _ => &fetched,
    };

    // Only store Note-like objects.
    let obj_type = object.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if !matches!(obj_type, "Note" | "Article" | "Question") {
        return Ok(None);
    }
    let note_uri = object.get("id").and_then(|v| v.as_str()).unwrap_or(uri);

    let attributed_to = json_uri(object.get("attributedTo"));
    if attributed_to.is_empty() {
        return Ok(None);
    }
    let Ok(account_id) = resolve_or_fetch_remote_account(state, attributed_to).await else {
        return Ok(None);
    };

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
    let url = object
        .get("url")
        .and_then(|u| u.as_str())
        .map(str::to_owned);
    let created_at = object
        .get("published")
        .and_then(|p| p.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    let note_to = as_string_vec(object.get("to"));
    let note_cc = as_string_vec(object.get("cc"));
    let visibility = crate::db::models::vis::from_audience(&note_to, &note_cc);
    let language = object
        .get("contentMap")
        .and_then(|m| m.as_object())
        .and_then(|m| m.keys().next())
        .map(|s| s.to_string())
        .filter(|s| ["ko", "en"].contains(&s.as_str()));

    // Link in-reply-to: use the local copy if present, otherwise fetch it once.
    let in_reply_to_uri = object.get("inReplyTo").and_then(|v| v.as_str());
    let (in_reply_to_id, in_reply_to_account_id): (Option<i64>, Option<i64>) = if let Some(irt) =
        in_reply_to_uri
    {
        let mut found: Option<(i64, i64)> = sqlx::query!(
            "SELECT id, account_id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
            irt,
        )
        .fetch_optional(&state.db)
        .await?
        .map(|r| (r.id, r.account_id));
        if found.is_none() && depth < MAX_FETCH_DEPTH {
            if let Some(pid) = Box::pin(fetch_remote_status_depth(state, irt, depth + 1)).await? {
                found = sqlx::query!("SELECT id, account_id FROM statuses WHERE id = $1", pid)
                    .fetch_optional(&state.db)
                    .await?
                    .map(|r| (r.id, r.account_id));
            }
        }
        found
            .map(|(id, aid)| (Some(id), Some(aid)))
            .unwrap_or((None, None))
    } else {
        (None, None)
    };

    let status_id = crate::snowflake::next_id();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO statuses
             (id, account_id, text, spoiler_text, visibility, sensitive,
              uri, url, in_reply_to_id, in_reply_to_account_id, reply,
              language, local, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, false, $13, now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL AND uri != '' DO NOTHING
           RETURNING id"#,
        status_id,
        account_id,
        text,
        spoiler_text,
        visibility,
        sensitive,
        note_uri,
        url,
        in_reply_to_id,
        in_reply_to_account_id,
        // A status with an inReplyTo is a reply even if its parent isn't local.
        in_reply_to_uri.is_some(),
        language,
        created_at,
    )
    .fetch_optional(&state.db)
    .await?;

    // Lost an insert race — return the existing row.
    let Some(new_id) = inserted else {
        return Ok(sqlx::query_scalar!(
            "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
            note_uri,
        )
        .fetch_optional(&state.db)
        .await?);
    };

    // Quote linkage (only if the quoted post is already local).
    let quote_uri = object
        .get("quote")
        .and_then(|v| v.as_str())
        .or_else(|| object.get("quoteUrl").and_then(|v| v.as_str()))
        .or_else(|| object.get("quoteUri").and_then(|v| v.as_str()))
        .or_else(|| object.get("_misskey_quote").and_then(|v| v.as_str()));
    if let Some(q) = quote_uri {
        let mut quoted: Option<(i64, i64)> = sqlx::query!(
            "SELECT id, account_id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
            q,
        )
        .fetch_optional(&state.db)
        .await?
        .map(|r| (r.id, r.account_id));
        if quoted.is_none() && depth < MAX_FETCH_DEPTH {
            if let Some(qid) = Box::pin(fetch_remote_status_depth(state, q, depth + 1)).await? {
                quoted = sqlx::query!("SELECT id, account_id FROM statuses WHERE id = $1", qid)
                    .fetch_optional(&state.db)
                    .await?
                    .map(|r| (r.id, r.account_id));
            }
        }
        if let Some((quoted_id, quoted_account_id)) = quoted {
            let _ = sqlx::query!(
                r#"INSERT INTO quotes
                     (id, status_id, quoted_status_id, account_id, quoted_account_id, state, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, 1, now(), now())
                   ON CONFLICT (status_id) DO NOTHING"#,
                crate::snowflake::next_id(),
                new_id,
                quoted_id,
                account_id,
                quoted_account_id,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Media attachments.
    for att in object
        .get("attachment")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
    {
        let media_type_str = att.get("mediaType").and_then(|v| v.as_str()).unwrap_or("");
        let att_type = classify_attachment_type(
            att.get("type").and_then(|v| v.as_str()).unwrap_or(""),
            media_type_str,
        );
        let Some(remote_url) = att
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|u| !u.is_empty())
        else {
            continue;
        };
        let description = att.get("name").and_then(|v| v.as_str()).map(str::to_owned);
        let blurhash = att
            .get("blurhash")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let file_content_type = (!media_type_str.is_empty()).then(|| media_type_str.to_owned());
        let file_meta = ap_attachment_file_meta(att);
        let _ = sqlx::query!(
            r#"INSERT INTO media_attachments
                 (id, account_id, status_id, remote_url, description, blurhash, type, file_content_type, file_meta, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now(), now())"#,
            crate::snowflake::next_id(),
            account_id,
            new_id,
            remote_url,
            description,
            blurhash,
            att_type,
            file_content_type,
            file_meta,
        )
        .execute(&state.db)
        .await;
    }

    sync_remote_poll(state, new_id, account_id, object).await?;

    Ok(Some(new_id))
}

/// Looks up a remote account by URI, fetching it from the remote server if unknown.
pub async fn resolve_or_fetch_remote_account(state: &AppState, actor_uri: &str) -> AppResult<i64> {
    // An actor URI on our own domain is a *local* account, not a remote one.
    // Resolve it directly (local accounts store an empty `uri`, so the lookup
    // below would miss it) rather than signed-fetching our own actor endpoint,
    // which would mint a remote-looking duplicate with domain = our own domain.
    // Such duplicates break every `domain IS NULL` local check — e.g. a mention
    // resolving to the duplicate never fires the local mention notification.
    if let Ok(parsed) = url::Url::parse(actor_uri) {
        if parsed
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case(&state.instance.domain))
        {
            let segments: Vec<&str> = parsed
                .path_segments()
                .map(|s| s.collect())
                .unwrap_or_default();
            let local_id = match segments.as_slice() {
                // https://{domain}/users/{username}
                ["users", username] => {
                    sqlx::query_scalar!(
                        "SELECT id FROM accounts WHERE username = $1 AND domain IS NULL",
                        username,
                    )
                    .fetch_optional(&state.db)
                    .await?
                }
                // https://{domain}/ap/users/{id}
                ["ap", "users", id] => match id.parse::<i64>() {
                    Ok(numeric) => {
                        sqlx::query_scalar!(
                            "SELECT id FROM accounts WHERE id = $1 AND domain IS NULL",
                            numeric,
                        )
                        .fetch_optional(&state.db)
                        .await?
                    }
                    Err(_) => None,
                },
                _ => None,
            };
            // On our own domain, never fall through to a remote fetch: either we
            // found the local account or there is no such account.
            return local_id.ok_or(AppError::NotFound);
        }
    }

    if let Some(id) = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
        .fetch_optional(&state.db)
        .await?
    {
        return Ok(id);
    }

    let actor: Value = crate::federation::fetch::signed_get_json(state, actor_uri)
        .await
        .map_err(AppError::Internal)?;

    let username = actor
        .get("preferredUsername")
        .and_then(|u| u.as_str())
        .unwrap_or("unknown");
    let domain = url::Url::parse(actor_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let display_name = actor
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let note = actor
        .get("summary")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let url = actor
        .get("url")
        .and_then(|u| u.as_str())
        .unwrap_or(actor_uri)
        .to_string();
    let inbox_url = actor
        .get("inbox")
        .and_then(|i| i.as_str())
        .unwrap_or("")
        .to_string();
    let outbox_url = actor
        .get("outbox")
        .and_then(|o| o.as_str())
        .unwrap_or("")
        .to_string();
    let shared_inbox_url = actor
        .get("endpoints")
        .and_then(|e| e.get("sharedInbox"))
        .and_then(|s| s.as_str())
        .map(str::to_owned);
    let public_key = actor
        .get("publicKey")
        .and_then(|k| k.get("publicKeyPem"))
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let avatar_remote_url = actor
        .get("icon")
        .and_then(|i| if i.is_object() { i.get("url") } else { None })
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let header_remote_url = actor
        .get("image")
        .and_then(|i| if i.is_object() { i.get("url") } else { None })
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if let Some(id) = sqlx::query_scalar!(
        r#"UPDATE accounts
           SET display_name = $2,
               note = $3,
               inbox_url = $4,
               shared_inbox_url = $5,
               public_key = $6,
               avatar_remote_url = COALESCE($7, avatar_remote_url),
               header_remote_url = CASE WHEN $8 != '' THEN $8 ELSE header_remote_url END,
               updated_at = now()
           WHERE uri = $1 AND uri != ''
           RETURNING id"#,
        actor_uri,
        display_name,
        note,
        inbox_url,
        shared_inbox_url,
        public_key,
        avatar_remote_url,
        header_remote_url,
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(id);
    }

    let new_id = crate::snowflake::next_id();
    let id = sqlx::query_scalar!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri,
              inbox_url, outbox_url, shared_inbox_url, public_key,
              avatar_remote_url, header_remote_url, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, now(), now())
           RETURNING id"#,
        new_id,
        username,
        domain,
        display_name,
        note,
        url,
        actor_uri,
        inbox_url,
        outbox_url,
        shared_inbox_url,
        public_key,
        avatar_remote_url,
        header_remote_url,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_headers_list_parses_covered_set() {
        let sig = r#"keyId="https://a.test/users/x#main-key",algorithm="rsa-sha256",headers="(request-target) Host Date Digest",signature="abc""#;
        let covered = signed_headers_list(sig);
        assert!(covered.contains(&"(request-target)".to_string()));
        assert!(covered.contains(&"host".to_string()));
        assert!(covered.contains(&"date".to_string()));
        assert!(covered.contains(&"digest".to_string()));
        assert!(signed_headers_list("keyId=\"x\",signature=\"y\"").is_empty());
    }

    #[test]
    fn date_within_skew_accepts_recent_and_rejects_stale() {
        let now = chrono::Utc::now();
        let fmt = "%a, %d %b %Y %H:%M:%S GMT";
        let recent = now.format(fmt).to_string();
        let stale = (now - chrono::Duration::hours(2)).format(fmt).to_string();
        let future = (now + chrono::Duration::hours(2)).format(fmt).to_string();
        assert!(date_within_skew(&recent, chrono::Duration::hours(1)));
        assert!(!date_within_skew(&stale, chrono::Duration::hours(1)));
        assert!(!date_within_skew(&future, chrono::Duration::hours(1)));
        assert!(!date_within_skew("garbage", chrono::Duration::hours(1)));
    }

    #[test]
    fn attachment_type_prefers_media_type_for_document_attachments() {
        assert_eq!(classify_attachment_type("Document", "image/webp"), 0);
        assert_eq!(classify_attachment_type("Document", "image/jpeg"), 0);
        assert_eq!(classify_attachment_type("Document", "image/gif"), 1);
        assert_eq!(classify_attachment_type("Document", "video/mp4"), 2);
        assert_eq!(classify_attachment_type("Document", "audio/mpeg"), 3);
    }

    #[test]
    fn attachment_type_falls_back_to_activitypub_type() {
        assert_eq!(classify_attachment_type("Image", ""), 0);
        assert_eq!(classify_attachment_type("Video", ""), 2);
        assert_eq!(classify_attachment_type("Audio", ""), 3);
        assert_eq!(classify_attachment_type("Document", ""), 4);
    }
}

/// Extract a usable media href (and its declared `mediaType`, if any) from an AP
/// attachment `url`, which may be serialized as a string, a Link object
/// (`{href, mediaType}`), or an array of such links (Mastodon resolves all of
/// these via `url_to_href`). Prefers a link whose `mediaType` is image/video/audio.
fn attachment_url(value: &Value) -> Option<(String, Option<String>)> {
    match value {
        Value::String(s) if !s.is_empty() => Some((s.clone(), None)),
        Value::Object(o) => {
            let href = o
                .get("href")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let media_type = o
                .get("mediaType")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            Some((href.to_string(), media_type))
        }
        Value::Array(arr) => {
            let mut fallback: Option<(String, Option<String>)> = None;
            for el in arr {
                if let Some((href, media_type)) = attachment_url(el) {
                    let is_media = media_type.as_deref().is_some_and(|m| {
                        m.starts_with("image/")
                            || m.starts_with("video/")
                            || m.starts_with("audio/")
                    });
                    if is_media {
                        return Some((href, media_type));
                    }
                    if fallback.is_none() {
                        fallback = Some((href, media_type));
                    }
                }
            }
            fallback
        }
        _ => None,
    }
}

/// Build a Mastodon-style `file_meta` (`{"original": {...}, "focus": {...}}`)
/// from an ActivityPub attachment's `width`/`height`/`duration`/`focalPoint`.
/// Returns `None` when the attachment carries no geometry.
///
/// This must run on every media-ingestion path: the official iOS client sizes
/// its image grid by dividing the container width by the sum of the images'
/// aspect ratios, and an image with no dimensions contributes nothing — so a
/// post whose images all lack `meta.original.{width,height}` divides by zero,
/// producing a NaN layout that aborts the app (`CALayer position contains NaN`).
fn ap_attachment_file_meta(att: &serde_json::Value) -> Option<serde_json::Value> {
    let width = att.get("width").and_then(|v| v.as_i64());
    let height = att.get("height").and_then(|v| v.as_i64());
    let duration = att.get("duration").and_then(|v| v.as_f64());
    // focalPoint [x, y] -> meta.focus { x, y } (Mastodon's focus).
    let focus = att
        .get("focalPoint")
        .and_then(|v| v.as_array())
        .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
    if width.is_none() && height.is_none() && duration.is_none() && focus.is_none() {
        return None;
    }
    let mut meta = serde_json::Map::new();
    if width.is_some() || height.is_some() || duration.is_some() {
        let mut orig = serde_json::Map::new();
        if let Some(w) = width {
            orig.insert("width".into(), w.into());
        }
        if let Some(h) = height {
            orig.insert("height".into(), h.into());
        }
        if let (Some(w), Some(h)) = (width, height) {
            orig.insert("size".into(), format!("{w}x{h}").into());
            if h != 0 {
                orig.insert("aspect".into(), (w as f64 / h as f64).into());
            }
        }
        if let Some(d) = duration {
            orig.insert("duration".into(), d.into());
        }
        meta.insert("original".into(), serde_json::Value::Object(orig));
    }
    if let Some((x, y)) = focus {
        meta.insert("focus".into(), serde_json::json!({ "x": x, "y": y }));
    }
    Some(serde_json::Value::Object(meta))
}

fn classify_attachment_type(att_type_str: &str, media_type_str: &str) -> i32 {
    if media_type_str == "image/gif" {
        1
    } else if media_type_str.starts_with("image/") {
        0
    } else if media_type_str.starts_with("video/") {
        2
    } else if media_type_str.starts_with("audio/") {
        3
    } else {
        match att_type_str {
            "Image" => 0,
            "Video" => {
                if media_type_str.contains("gif") {
                    1
                } else {
                    2
                }
            }
            "Audio" => 3,
            _ => 4,
        }
    }
}
