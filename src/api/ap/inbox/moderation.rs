//! Inbound moderation-related activities: `Block` (remote actor blocks a local
//! account), `Flag` (a remote report against local accounts/statuses), and
//! `Move` (an actor migrating to a new account).

use serde_json::Value;

use crate::{error::AppResult, state::AppState};

use super::{as_string_vec, delete_arrived_first, resolve_or_fetch_remote_account};

pub(super) async fn handle_block(state: &AppState, activity: &Value) -> AppResult<()> {
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

pub(super) async fn handle_flag(state: &AppState, activity: &Value) -> AppResult<()> {
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

pub(super) async fn handle_move(state: &AppState, activity: &Value) -> AppResult<()> {
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
