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
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
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
        .and_then(|a| {
            a.as_str()
                .or_else(|| a.get("id").and_then(|i| i.as_str()))
        })
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
        if activity_type == "Delete" {
            tracing::debug!(actor = %actor_uri, %reason, "unverified Delete; accepting without processing");
            return Ok(StatusCode::ACCEPTED);
        }
        tracing::warn!(actor = %actor_uri, activity_type, %reason, "rejecting activity: HTTP Signature not verified");
        return Err(AppError::Unauthorized);
    }

    let outcome = match activity_type {
        "Follow" => { handle_follow(&state, &instance, &activity).await?; "handled" }
        "Undo" => { handle_undo(&state, &instance, &activity).await?; "handled" }
        "Create" => { handle_create(&state, &instance, &activity).await?; "handled" }
        "Delete" => { handle_delete(&state, &instance, &activity).await?; "handled" }
        "Announce" => { handle_announce(&state, &instance, &activity).await?; "handled" }
        "Like" => { handle_like(&state, &instance, &activity).await?; "handled" }
        "Accept" | "Reject" => { handle_accept_reject(&state, &instance, &activity).await?; "handled" }
        "Update" => { handle_update(&state, &instance, &activity).await?; "handled" }
        "Block" => { handle_block(&state, &activity).await?; "handled" }
        "Flag" => { handle_flag(&state, &activity).await?; "handled" }
        "Move" => { handle_move(&state, &activity).await?; "handled" }
        "Add" => { handle_add(&state, &activity).await?; "handled" }
        "Remove" => { handle_remove(&state, &activity).await?; "handled" }
        "QuoteRequest" => { handle_quote_request(&state, &instance, &activity).await?; "handled" }
        "FeatureRequest" => { handle_feature_request(&state, &instance, &activity).await?; "handled" }
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
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(trimmed).map(|d| d.with_timezone(&chrono::Utc)));
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
            return Err(format!("signature does not cover required header {required:?}"));
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
        return Err(format!("Date header outside acceptable clock skew: {date_val:?}"));
    }

    let pem = fetch_public_key(state, key_actor)
        .await
        .map_err(|e| format!("could not fetch public key: {e}"))?;

    let hdr_vec = crate::federation::signature::headers_to_vec(headers);
    let hdr_refs: Vec<(&str, &str)> = hdr_vec.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    match crate::federation::signature::verify_request("post", path, &hdr_refs, body, &pem) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            let refreshed_pem = refresh_public_key(state, key_actor)
                .await
                .map_err(|e| format!("could not refresh public key: {e}"))?;
            crate::federation::signature::verify_request("post", path, &hdr_refs, body, &refreshed_pem)
                .map_err(|e| format!("{first_err}; after key refresh: {e}"))
        }
    }
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
    let object_uri = activity.get("object").and_then(|o| o.as_str()).unwrap_or("");
    let activity_uri = activity.get("id").and_then(|i| i.as_str()).unwrap_or("");

    let target = sqlx::query!(
        "SELECT id, locked, username, private_key FROM accounts WHERE uri = $1 AND domain IS NULL",
        object_uri,
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(target) = target else { return Ok(()) };

    let follower_id = resolve_or_fetch_remote_account(state, actor_uri).await?;

    // Fetch the follower's account for push notification details
    let follower = sqlx::query!(
        "SELECT display_name, username, domain, avatar_remote_url FROM accounts WHERE id = $1",
        follower_id,
    )
    .fetch_optional(&state.db)
    .await?;

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

    let actions = feder_core::inbound::on_follow(follow, &object_iri, target.locked, accept_iri);

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
                    tracing::warn!(actor_uri, "cannot send Accept: remote actor has no inbox URL");
                    continue;
                };
                let activity = serde_json::to_value(&accept)
                    .map_err(|e| crate::error::AppError::Internal(e.into()))?;
                let actor_url = format!("https://{}/users/{}", instance.domain, target.username);
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
            let follow_uri = object.and_then(|o| o.get("id")).and_then(|i| i.as_str()).unwrap_or("");
            sqlx::query!("DELETE FROM follows WHERE uri = $1", follow_uri).execute(&state.db).await?;
            sqlx::query!("DELETE FROM follow_requests WHERE uri = $1", follow_uri).execute(&state.db).await?;
        }
        Some("Like") => {
            // object.object is the liked status URI
            let status_uri = object
                .and_then(|o| o.get("object"))
                .and_then(|v| if v.is_string() { v.as_str() } else { v.get("id").and_then(|i| i.as_str()) })
                .unwrap_or("");
            let status_id = sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", status_uri)
                .fetch_optional(&state.db).await?;
            let account_id = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
                .fetch_optional(&state.db).await?;
            if let (Some(sid), Some(aid)) = (status_id, account_id) {
                sqlx::query!("DELETE FROM favourites WHERE account_id = $1 AND status_id = $2", aid, sid)
                    .execute(&state.db).await?;
                sqlx::query!(
                    r#"UPDATE status_stats SET favourites_count = (SELECT COUNT(*) FROM favourites WHERE status_id = $1), updated_at = now() WHERE status_id = $1"#,
                    sid
                ).execute(&state.db).await?;
            }
        }
        Some("Announce") => {
            // Delete the remote boost status by its announce URI
            let announce_uri = object.and_then(|o| o.get("id")).and_then(|i| i.as_str()).unwrap_or("");
            if !announce_uri.is_empty() {
                let deleted = sqlx::query!(
                    "DELETE FROM statuses WHERE uri = $1 RETURNING reblog_of_id",
                    announce_uri,
                ).fetch_optional(&state.db).await?;
                if let Some(row) = deleted {
                    if let Some(original_id) = row.reblog_of_id {
                        sqlx::query!(
                            r#"UPDATE status_stats SET reblogs_count = (SELECT COUNT(*) FROM statuses WHERE reblog_of_id = $1 AND deleted_at IS NULL), updated_at = now() WHERE status_id = $1"#,
                            original_id,
                        ).execute(&state.db).await?;
                    }
                }
            }
        }
        Some("Block") => {
            let block_object_uri = object
                .and_then(|o| o.get("object"))
                .and_then(|v| if v.is_string() { v.as_str() } else { v.get("id").and_then(|i| i.as_str()) })
                .unwrap_or("");
            let blocker_id = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1", actor_uri)
                .fetch_optional(&state.db).await?;
            let blockee_id = sqlx::query_scalar!("SELECT id FROM accounts WHERE uri = $1 AND domain IS NULL", block_object_uri)
                .fetch_optional(&state.db).await?;
            if let (Some(bid), Some(eid)) = (blocker_id, blockee_id) {
                sqlx::query!("DELETE FROM blocks WHERE account_id = $1 AND target_account_id = $2", bid, eid)
                    .execute(&state.db).await?;
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
        object.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()),
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
        tracing::debug!(note_uri, "Create(Note): ignoring, not related to local activity");
        return Ok(());
    }

    // Field extraction
    let text = object.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let spoiler_text = object.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let sensitive = object.get("sensitive").and_then(|s| s.as_bool()).unwrap_or(false);
    let url = object.get("url").and_then(|u| u.as_str()).map(str::to_owned);
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

    let quote_uri = object
        .get("quote")
        .and_then(|v| v.as_str())
        .or_else(|| object.get("quoteUrl").and_then(|v| v.as_str()))
        .or_else(|| object.get("quoteUri").and_then(|v| v.as_str()))
        .or_else(|| object.get("_misskey_quote").and_then(|v| v.as_str()));
    let quote_of_id: Option<i64> = if let Some(uri) = quote_uri {
        sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", uri)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let status_id = crate::snowflake::next_id();
    let created_at = published.unwrap_or_else(|| chrono::Utc::now().naive_utc());

    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO statuses
             (id, account_id, text, spoiler_text, visibility, sensitive,
              uri, url, in_reply_to_id, in_reply_to_account_id, reply,
              language, created_at, edited_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14, now())
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
        in_reply_to_id.is_some(),
        language,
        created_at,
        edited_at,
    )
    .fetch_optional(&state.db)
    .await?;

    let Some(inserted_id) = inserted else {
        return Ok(()); // duplicate
    };

    if let Some(qid) = quote_of_id {
        let _ = sqlx::query!(
            "INSERT INTO quotes (status_id, quoted_status_id, state, created_at, updated_at) VALUES ($1, $2, 1, now(), now()) ON CONFLICT DO NOTHING",
            inserted_id,
            qid,
        )
        .execute(&state.db)
        .await;
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
        let att_type_str = att.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let media_type_str = att.get("mediaType").and_then(|v| v.as_str()).unwrap_or("");
        let att_type: i32 = match att_type_str {
            "Image" => 0,
            "Video" => {
                if media_type_str.contains("gif") { 1 } else { 2 }
            }
            "Audio" => 3,
            _ => 4,
        };
        let remote_url = match att.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => continue,
        };
        let description = att.get("name").and_then(|v| v.as_str()).map(str::to_owned);
        let blurhash = att.get("blurhash").and_then(|v| v.as_str()).map(str::to_owned);
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
        let width = att.get("width").and_then(|v| v.as_i64());
        let height = att.get("height").and_then(|v| v.as_i64());
        let duration = att.get("duration").and_then(|v| v.as_f64());
        let file_meta: Option<serde_json::Value> = if width.is_some() || height.is_some() || duration.is_some() {
            let mut orig = serde_json::Map::new();
            if let Some(w) = width { orig.insert("width".into(), w.into()); }
            if let Some(h) = height { orig.insert("height".into(), h.into()); }
            if let Some(d) = duration { orig.insert("duration".into(), d.into()); }
            let mut meta = serde_json::Map::new();
            meta.insert("original".into(), serde_json::Value::Object(orig));
            Some(serde_json::Value::Object(meta))
        } else {
            None
        };

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
            let mut others: Vec<i64> = all_participant_ids.iter()
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
    for tag in tags_arr
        .iter()
        .filter(|t| tag_type_is(t, "Emoji"))
    {
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
        tokio::spawn(async move {
            tracing::debug!(uri, "fetching unknown parent status for thread resolution");
            if let Err(e) = fetch_remote_status(&state, &uri).await {
                tracing::debug!(uri, error = %e, "failed to store fetched parent status");
            }
        });
    }

    // Fanout to home and list feeds
    let vis_str = crate::db::models::vis::to_str(visibility);
    let mut redis = state.redis.clone();
    let db = state.db.clone();
    if crate::feed::sync_fanout() {
        crate::feed::fanout_new_status(&mut redis, &db, account_id, inserted_id, &tag_ids).await;
        crate::feed::fanout_to_lists(&mut redis, &db, account_id, inserted_id, in_reply_to_account_id, vis_str).await;
    } else {
        tokio::spawn(async move {
            crate::feed::fanout_new_status(&mut redis, &db, account_id, inserted_id, &tag_ids).await;
            crate::feed::fanout_to_lists(&mut redis, &db, account_id, inserted_id, in_reply_to_account_id, vis_str).await;
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
                tracing::warn!(actor_uri, uri, "Delete: actor domain does not match object domain, ignoring");
                return Ok(());
            }

            // Delete(Note/Tombstone) — soft-delete the status
            sqlx::query!(
                "UPDATE statuses SET deleted_at = now() WHERE uri = $1",
                uri,
            )
            .execute(&state.db)
            .await?;

            // Create a tombstone so that a subsequent Create with the same URI is rejected.
            let actor_id = sqlx::query_scalar!(
                "SELECT id FROM accounts WHERE uri = $1",
                actor_uri,
            )
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

    let Some(original_id) = original_id else { return Ok(()); };

    let published = activity
        .get("published")
        .and_then(|p| p.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc).naive_utc())
        .unwrap_or_else(|| chrono::Utc::now().naive_utc());

    let boost_id = crate::snowflake::next_id();
    sqlx::query!(
        r#"INSERT INTO statuses
             (id, account_id, reblog_of_id, visibility, uri, url, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $5, $6, now())
           ON CONFLICT (uri) WHERE uri IS NOT NULL AND uri != '' DO NOTHING"#,
        boost_id,
        booster_id,
        original_id,
        crate::db::models::vis::PUBLIC,
        announce_uri,
        published,
    )
    .execute(&state.db)
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

    Ok(())
}

async fn handle_like(
    state: &AppState,
    _instance: &crate::config::InstanceConfig,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity.get("object").and_then(|o| o.as_str()).unwrap_or("");

    let mut status_id = sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", object_uri)
        .fetch_optional(&state.db)
        .await?;

    if status_id.is_none() {
        status_id = fetch_remote_status(state, object_uri).await?;
    }

    let Some(status_id) = status_id else { return Ok(()); };

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

    Ok(())
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
                let _ = sqlx::query!(
                    r#"INSERT INTO account_stats (account_id, followers_count, created_at, updated_at)
                       VALUES ($1, 1, now(), now())
                       ON CONFLICT (account_id) DO UPDATE
                         SET followers_count = account_stats.followers_count + 1, updated_at = now()"#,
                    row.target_account_id,
                )
                .execute(&state.db)
                .await;
                let _ = sqlx::query!(
                    r#"INSERT INTO account_stats (account_id, following_count, created_at, updated_at)
                       VALUES ($1, 1, now(), now())
                       ON CONFLICT (account_id) DO UPDATE
                         SET following_count = account_stats.following_count + 1, updated_at = now()"#,
                    row.account_id,
                )
                .execute(&state.db)
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
            r#"SELECT q.id, a.uri AS quoted_account_uri
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
            let Some(uri) = o.as_str() else { return Ok(()); };
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

            let display_name = object.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let note = object.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let inbox_url = object.get("inbox").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let shared_inbox_url = object
                .get("endpoints").and_then(|e| e.get("sharedInbox")).and_then(|s| s.as_str())
                .map(str::to_owned);
            let public_key = object
                .get("publicKey").and_then(|k| k.get("publicKeyPem")).and_then(|p| p.as_str())
                .unwrap_or("").to_string();
            let locked = object.get("manuallyApprovesFollowers").and_then(|v| v.as_bool()).unwrap_or(false);
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
                actor_uri, display_name, note, inbox_url, shared_inbox_url, public_key, locked,
                avatar_remote_url, header_remote_url,
            )
            .execute(&state.db)
            .await?;
        }
        "Note" => {
            let note_uri = object.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if note_uri.is_empty() { return Ok(()); }

            let text = object.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let spoiler_text = object.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let sensitive = object.get("sensitive").and_then(|s| s.as_bool()).unwrap_or(false);
            let language = object
                .get("contentMap").and_then(|m| m.as_object()).and_then(|m| m.keys().next())
                .map(|s| s.to_string())
                .filter(|s| ["ko", "en"].contains(&s.as_str()));
            let edited_at = object.get("updated").and_then(|p| p.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&chrono::Utc).naive_utc());

            let updated = sqlx::query!(
                r#"UPDATE statuses
                   SET text = $2, spoiler_text = $3, sensitive = $4, language = $5,
                       edited_at = COALESCE($6, edited_at), updated_at = now()
                   WHERE uri = $1 AND deleted_at IS NULL
                   RETURNING id, account_id"#,
                note_uri, text, spoiler_text, sensitive, language, edited_at,
            )
            .fetch_optional(&state.db)
            .await?;

            if updated.is_none() {
                let _ = fetch_remote_status(state, note_uri).await?;
                return Ok(());
            }

            let Some(row) = updated else { return Ok(()); };

            // Replace media attachments
            sqlx::query!("DELETE FROM media_attachments WHERE status_id = $1", row.id)
                .execute(&state.db).await?;
            let attachments: Vec<Value> = object.get("attachment")
                .and_then(|a| a.as_array()).cloned().unwrap_or_default();
            let mut media_ids: Vec<i64> = Vec::new();
            for att in &attachments {
                let att_type_str = att.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let media_type_str = att.get("mediaType").and_then(|v| v.as_str()).unwrap_or("");
                let att_type: i32 = match att_type_str {
                    "Image" => 0,
                    "Video" => if media_type_str.contains("gif") { 1 } else { 2 },
                    "Audio" => 3,
                    _ => 4,
                };
                let remote_url = match att.get("url").and_then(|v| v.as_str()) {
                    Some(u) if !u.is_empty() => u,
                    _ => continue,
                };
                let description = att.get("name").and_then(|v| v.as_str()).map(str::to_owned);
                let blurhash = att.get("blurhash").and_then(|v| v.as_str()).map(str::to_owned);
                let thumbnail_remote_url = att.get("icon")
                    .and_then(|i| if i.is_object() { i.get("url") } else { None })
                    .and_then(|v| v.as_str()).map(str::to_owned);
                let file_content_type = if media_type_str.is_empty() { None } else { Some(media_type_str.to_owned()) };
                let media_id = crate::snowflake::next_id();
                if let Ok(id) = sqlx::query_scalar!(
                    r#"INSERT INTO media_attachments (id, account_id, status_id, remote_url, description, blurhash, type, thumbnail_remote_url, file_content_type, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9, now(), now()) RETURNING id"#,
                    media_id, row.account_id, row.id, remote_url, description, blurhash, att_type, thumbnail_remote_url, file_content_type,
                ).fetch_one(&state.db).await { media_ids.push(id); }
            }
            if !media_ids.is_empty() {
                let _ = sqlx::query!("UPDATE statuses SET ordered_media_attachment_ids = $1 WHERE id = $2", &media_ids, row.id)
                    .execute(&state.db).await;
            }

            // Replace hashtags
            sqlx::query!("DELETE FROM statuses_tags WHERE status_id = $1", row.id)
                .execute(&state.db).await?;
            let tags_arr: Vec<Value> = match object.get("tag") {
                Some(Value::Array(arr)) => arr.clone(),
                Some(obj @ Value::Object(_)) => vec![obj.clone()],
                _ => vec![],
            };
            for tag in tags_arr.iter().filter(|t| tag_type_is(t, "Hashtag")) {
                let name = match tag.get("name").and_then(|v| v.as_str())
                    .map(|n| n.trim_start_matches('#').to_lowercase())
                    .filter(|n| !n.is_empty())
                { Some(n) => n, None => continue };
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

    if let Some(poll_id) = sqlx::query_scalar!(
        "SELECT id FROM polls WHERE status_id = $1",
        status_id,
    )
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

    if poll.expires_at.map(|e| e < chrono::Utc::now().naive_utc()).unwrap_or(false) {
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

async fn handle_block(
    state: &AppState,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity.get("object").and_then(|o| {
        if o.is_string() { o.as_str() } else { o.get("id").and_then(|i| i.as_str()) }
    }).unwrap_or("");

    // Only process if the blocked account is local
    let Some(target_id) = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = $1 AND domain IS NULL", object_uri
    ).fetch_optional(&state.db).await? else { return Ok(()); };

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
        let _ = sqlx::query!("UPDATE account_stats SET following_count = GREATEST(following_count-1,0), updated_at=now() WHERE account_id=$1", row.account_id).execute(&state.db).await;
        let _ = sqlx::query!("UPDATE account_stats SET followers_count = GREATEST(followers_count-1,0), updated_at=now() WHERE account_id=$1", row.target_account_id).execute(&state.db).await;
    }
    sqlx::query!(
        "DELETE FROM follow_requests WHERE (account_id=$1 AND target_account_id=$2) OR (account_id=$2 AND target_account_id=$1)",
        blocker_id, target_id,
    ).execute(&state.db).await?;

    Ok(())
}

async fn handle_flag(
    state: &AppState,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let comment = activity.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let activity_uri = activity.get("id").and_then(|i| i.as_str()).map(str::to_owned);

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
    ).fetch_all(&state.db).await.unwrap_or_default();

    let Some(&target_account_id) = local_account_ids.first() else { return Ok(()); };

    // Find local statuses among the objects
    let status_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = ANY($1) AND deleted_at IS NULL",
        &objects as &[String],
    ).fetch_all(&state.db).await.unwrap_or_default();

    let report_id = crate::snowflake::next_id();
    sqlx::query!(
        r#"INSERT INTO reports (id, account_id, target_account_id, status_ids, comment, uri, forwarded, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,false,now(),now())
           ON CONFLICT DO NOTHING"#,
        report_id, reporter_id, target_account_id, &status_ids as &[i64], comment, activity_uri,
    ).execute(&state.db).await?;

    Ok(())
}

async fn handle_move(
    state: &AppState,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let target_uri = activity.get("object").and_then(|o| {
        if o.is_string() { o.as_str() } else { o.get("id").and_then(|i| i.as_str()) }
    }).unwrap_or("");

    if actor_uri.is_empty() || target_uri.is_empty() { return Ok(()); }

    // Fetch the new account to verify also_known_as contains the old actor URI
    let new_account_id = match resolve_or_fetch_remote_account(state, target_uri).await {
        Ok(id) => id,
        Err(_) => return Ok(()),
    };

    // Fetch the target actor to verify also_known_as
    let also_known_as: Vec<String> = sqlx::query_scalar!(
        "SELECT also_known_as FROM accounts WHERE id = $1",
        new_account_id,
    ).fetch_optional(&state.db).await?
        .flatten().unwrap_or_default();

    if !also_known_as.iter().any(|u| u == actor_uri) {
        tracing::warn!(actor_uri, target_uri, "Move rejected: target alsoKnownAs does not include actor");
        return Ok(());
    }

    // Set moved_to_account_id on the old account
    sqlx::query!(
        "UPDATE accounts SET moved_to_account_id = $1 WHERE uri = $2 AND domain IS NOT NULL",
        new_account_id, actor_uri,
    ).execute(&state.db).await?;

    tracing::debug!(actor_uri, target_uri, "processed Move: updated moved_to_account_id");
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
    let name = coll.get("name").and_then(|v| v.as_str()).unwrap_or("Featured collection");
    let sensitive = coll.get("sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
    let discoverable = coll.get("discoverable").and_then(|v| v.as_bool()).unwrap_or(true);
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

async fn handle_add(
    state: &AppState,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = json_uri(activity.get("object"));
    let target_uri = json_uri(activity.get("target"));

    if actor_uri.is_empty() || object_uri.is_empty() { return Ok(()); }

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
    ).fetch_optional(&state.db).await?.flatten().unwrap_or_default();

    if target_uri != featured_url { return Ok(()); }

    let account_id = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = $1",
        actor_uri,
    ).fetch_optional(&state.db).await?;
    let status_id = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE uri = $1 AND deleted_at IS NULL",
        object_uri,
    ).fetch_optional(&state.db).await?;

    if let (Some(aid), Some(sid)) = (account_id, status_id) {
        let pin_id = crate::snowflake::next_id();
        sqlx::query!(
            "INSERT INTO status_pins (id, account_id, status_id, created_at, updated_at) VALUES ($1,$2,$3,now(),now()) ON CONFLICT DO NOTHING",
            pin_id, aid, sid,
        ).execute(&state.db).await?;
    }

    Ok(())
}

async fn handle_remove(
    state: &AppState,
    activity: &Value,
) -> AppResult<()> {
    let actor_uri = activity.get("actor").and_then(|a| a.as_str()).unwrap_or("");
    let object_uri = activity.get("object").and_then(|o| {
        if o.is_string() { o.as_str() } else { o.get("id").and_then(|i| i.as_str()) }
    }).unwrap_or("");

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
        .fetch_optional(&state.db).await?;
    let status_id = sqlx::query_scalar!("SELECT id FROM statuses WHERE uri = $1", object_uri)
        .fetch_optional(&state.db).await?;

    if let (Some(aid), Some(sid)) = (account_id, status_id) {
        sqlx::query!("DELETE FROM status_pins WHERE account_id = $1 AND status_id = $2", aid, sid)
            .execute(&state.db).await?;
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
    let object_uri = activity.get("object").and_then(|o| o.as_str()).unwrap_or("");
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
        let reject_id = format!("{actor_url}#rejects/quote_requests/{}", crate::snowflake::next_id());
        if let Ok(r) = crate::federation::consent::reject(&reject_id, &actor_url, &quoter.uri, req_id) {
            if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                state,
                r,
                vec![inbox],
                key_id,
            )
            .await
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
        None => match fetch_remote_status(state, instrument_uri).await? {
            Some(id) => id,
            None => {
                tracing::debug!(actor_uri, instrument_uri, "QuoteRequest accepted but quoting status could not be fetched; skipping stamp");
                return Ok(());
            }
        },
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
        if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
            state,
            accept,
            vec![inbox],
            key_id,
        )
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
    let account_uri = activity.get("object").and_then(|v| v.as_str()).unwrap_or("");
    let collection_uri = activity
        .get("instrument")
        .and_then(|v| if v.is_string() { v.as_str() } else { v.get("id").and_then(|i| i.as_str()) })
        .unwrap_or("");
    if req_id.is_empty() || account_uri.is_empty() || collection_uri.is_empty() {
        return Ok(());
    }

    // The featured account must be local and active.
    let Some(local) = sqlx::query!(
        "SELECT id, username, suspended_at FROM accounts WHERE uri = $1 AND domain IS NULL",
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
        .and_then(|v| if v.is_string() { v.as_str() } else { v.get("id").and_then(|i| i.as_str()) })
        .unwrap_or("");
    if owner_uri.is_empty() {
        return Ok(());
    }
    let Ok(owner_id) = resolve_or_fetch_remote_account(state, owner_uri).await else {
        return Ok(());
    };
    let name = coll.get("name").and_then(|v| v.as_str()).unwrap_or("Featured collection");
    let sensitive = coll.get("sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
    let discoverable = coll.get("discoverable").and_then(|v| v.as_bool()).unwrap_or(true);

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
    let has_signing_key = sqlx::query_scalar!(
        "SELECT private_key FROM accounts WHERE id = $1",
        local.id,
    )
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
        let actor_url = format!("https://{domain}/users/{}", local.username);
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
            if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                state,
                accept,
                vec![inbox],
                key_id,
            )
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

async fn fetch_remote_status_depth(state: &AppState, uri: &str, depth: u8) -> AppResult<Option<i64>> {
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
                let Some(object_uri) = o.as_str() else { return Ok(None); };
                nested_fetched = match crate::federation::fetch::signed_get_json(state, object_uri).await {
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

    let text = object.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let spoiler_text = object.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let sensitive = object.get("sensitive").and_then(|s| s.as_bool()).unwrap_or(false);
    let url = object.get("url").and_then(|u| u.as_str()).map(str::to_owned);
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
    let (in_reply_to_id, in_reply_to_account_id): (Option<i64>, Option<i64>) =
        if let Some(irt) = in_reply_to_uri {
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
            found.map(|(id, aid)| (Some(id), Some(aid))).unwrap_or((None, None))
        } else {
            (None, None)
        };

    let status_id = crate::snowflake::next_id();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO statuses
             (id, account_id, text, spoiler_text, visibility, sensitive,
              uri, url, in_reply_to_id, in_reply_to_account_id, reply,
              language, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, now())
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
        in_reply_to_id.is_some(),
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
    for att in object.get("attachment").and_then(|a| a.as_array()).into_iter().flatten() {
        let media_type_str = att.get("mediaType").and_then(|v| v.as_str()).unwrap_or("");
        let att_type: i32 = match att.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "Image" => 0,
            "Video" => if media_type_str.contains("gif") { 1 } else { 2 },
            "Audio" => 3,
            _ => 4,
        };
        let Some(remote_url) = att.get("url").and_then(|v| v.as_str()).filter(|u| !u.is_empty())
        else {
            continue;
        };
        let description = att.get("name").and_then(|v| v.as_str()).map(str::to_owned);
        let blurhash = att.get("blurhash").and_then(|v| v.as_str()).map(str::to_owned);
        let file_content_type = (!media_type_str.is_empty()).then(|| media_type_str.to_owned());
        let _ = sqlx::query!(
            r#"INSERT INTO media_attachments
                 (id, account_id, status_id, remote_url, description, blurhash, type, file_content_type, created_at, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8, now(), now())"#,
            crate::snowflake::next_id(),
            account_id,
            new_id,
            remote_url,
            description,
            blurhash,
            att_type,
            file_content_type,
        )
        .execute(&state.db)
        .await;
    }

    sync_remote_poll(state, new_id, account_id, object).await?;

    Ok(Some(new_id))
}

/// Looks up a remote account by URI, fetching it from the remote server if unknown.
pub async fn resolve_or_fetch_remote_account(
    state: &AppState,
    actor_uri: &str,
) -> AppResult<i64> {
    if let Some(id) = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = $1",
        actor_uri
    )
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
}
