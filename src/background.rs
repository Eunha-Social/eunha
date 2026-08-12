use std::time::Duration;

use crate::state::AppState;

/// Spawns all background tasks. Called once at startup.
pub fn spawn(state: AppState) {
    tokio::spawn(run_scheduled_statuses(state.clone()));
    tokio::spawn(run_poll_expiry(state.clone()));
    tokio::spawn(run_suspended_account_cleanup(state.clone()));
    tokio::spawn(crate::federation::delivery::run_delivery_cleanup(
        state.clone(),
    ));
    tokio::spawn(crate::api::mastodon::media::run_media_queue(state.clone()));
    tokio::spawn(crate::federation::delivery::run_delivery_queue(state));
}

// ── Scheduled status publisher ────────────────────────────────────────────

async fn run_scheduled_statuses(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = publish_due_statuses(&state).await {
            tracing::error!(error = %e, "scheduled status publish failed");
        }
    }
}

pub async fn publish_due_statuses(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query!(
        r#"SELECT id, account_id, params
           FROM scheduled_statuses
           WHERE scheduled_at <= now()
           ORDER BY scheduled_at ASC
           LIMIT 50"#,
    )
    .fetch_all(&state.db)
    .await?;

    for row in rows {
        match publish_one(state, row.id, row.account_id, &row.params).await {
            Ok(()) => {
                sqlx::query!("DELETE FROM scheduled_statuses WHERE id = $1", row.id)
                    .execute(&state.db)
                    .await?;
            }
            Err(e) => {
                tracing::warn!(id = row.id, error = %e, "failed to publish scheduled status");
                // Delete anyway to avoid retrying indefinitely on bad params
                sqlx::query!("DELETE FROM scheduled_statuses WHERE id = $1", row.id)
                    .execute(&state.db)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn publish_one(
    state: &AppState,
    _scheduled_id: i64,
    account_id: i64,
    params: &Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let params = params
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no params"))?;

    let account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        account_id,
    )
    .fetch_one(&state.db)
    .await?;

    let text = params["text"].as_str().unwrap_or("").to_string();
    let visibility = params["visibility"]
        .as_str()
        .unwrap_or("public")
        .to_string();
    let spoiler_text = params["spoiler_text"].as_str().unwrap_or("").to_string();
    let sensitive = params["sensitive"].as_bool().unwrap_or(false);
    let language = params["language"].as_str().map(str::to_string);
    let in_reply_to_id: Option<i64> = params["in_reply_to_id"]
        .as_str()
        .and_then(|s| s.parse::<i64>().ok());

    // Resolve the parent's account for in_reply_to_account_id and replies_count
    let in_reply_to_account_id: Option<i64> = if let Some(parent_id) = in_reply_to_id {
        sqlx::query_scalar!(
            "SELECT account_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
            parent_id,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
    } else {
        None
    };
    let is_reply = in_reply_to_id.is_some();

    use crate::api::mastodon::formatting::render_content;
    use crate::api::mastodon::statuses::{
        build_mention_map, extract_hashtags, extract_mention_handles, resolve_mention_accounts,
        store_status_mentions, store_statuses_tags,
    };

    let domain = &state.instance.domain;

    let hashtags = extract_hashtags(&text);
    let mention_handles = extract_mention_handles(&text);
    let resolved = resolve_mention_accounts(state, &mention_handles, domain).await;
    let mention_map = build_mention_map(&resolved, domain);
    let content = render_content(&text, domain, &mention_map);

    let status_id = crate::snowflake::next_id();
    let uri = format!(
        "https://{}/users/{}/statuses/{}",
        domain, account.username, status_id
    );

    let visibility_int = crate::db::models::vis::from_str(&visibility);
    let status = sqlx::query_as!(
        crate::db::models::Status,
        r#"INSERT INTO statuses
             (id, account_id, text, spoiler_text, visibility,
              language, sensitive, in_reply_to_id, in_reply_to_account_id, reply, uri, url, local, created_at, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11, true, now(), now())
           RETURNING *"#,
        status_id, account.id, text, spoiler_text, visibility_int,
        language, sensitive, in_reply_to_id, in_reply_to_account_id, is_reply, uri,
    )
    .fetch_one(&state.db)
    .await?;

    store_statuses_tags(state, status.id, account.id, &hashtags).await?;
    store_status_mentions(state, status.id, &resolved).await?;

    // Update last_status_at and statuses_count in account_stats
    sqlx::query!(
        r#"INSERT INTO account_stats (account_id, statuses_count, last_status_at, created_at, updated_at)
           VALUES ($1, 1, $2, now(), now())
           ON CONFLICT (account_id) DO UPDATE
             SET statuses_count = account_stats.statuses_count + 1,
                 last_status_at = GREATEST(account_stats.last_status_at, $2),
                 updated_at = now()"#,
        account.id,
        status.created_at,
    )
    .execute(&state.db)
    .await?;

    // Increment parent's replies_count
    if let Some(parent_id) = in_reply_to_id {
        let _ = sqlx::query!(
            r#"INSERT INTO status_stats (status_id, replies_count, created_at, updated_at)
               VALUES ($1, 1, now(), now())
               ON CONFLICT (status_id) DO UPDATE
                 SET replies_count = status_stats.replies_count + 1,
                     updated_at = now()"#,
            parent_id,
        )
        .execute(&state.db)
        .await;
    }

    // Attach media ids if any
    if let Some(ids) = params["media_ids"].as_array() {
        for id_val in ids {
            if let Some(id_str) = id_val.as_str() {
                if let Ok(media_id) = id_str.parse::<i64>() {
                    sqlx::query!(
                        "UPDATE media_attachments SET status_id = $1 WHERE id = $2 AND account_id = $3 AND status_id IS NULL",
                        status.id, media_id, account.id,
                    )
                    .execute(&state.db)
                    .await?;
                }
            }
        }
    }

    // Create poll if present
    if let Some(poll) = params["poll"].as_object() {
        if let Some(options) = poll.get("options").and_then(|o| o.as_array()) {
            if options.len() >= 2 {
                let expires_in = poll.get("expires_in").and_then(|v| v.as_i64());
                let multiple = poll
                    .get("multiple")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let hide_totals = poll
                    .get("hide_totals")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let expires_at = expires_in
                    .map(|s| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(s));
                let opts: Vec<String> = options
                    .iter()
                    .filter_map(|o| o.as_str())
                    .map(|o| o.to_string())
                    .collect();
                sqlx::query!(
                    r#"INSERT INTO polls
                         (status_id, account_id, options, multiple, hide_totals, expires_at, created_at, updated_at)
                       VALUES ($1,$2,$3,$4,$5,$6,now(),now())"#,
                    status.id, account.id, &opts as &[String], multiple, hide_totals, expires_at,
                )
                .execute(&state.db)
                .await?;
            }
        }
    }

    // Publish to streaming and fan-out to feeds
    use crate::api::mastodon::status_serialize::{
        build_status, fetch_status_media, spawn_card_fetch,
    };
    let mut status_with_uri = status.clone();
    status_with_uri.uri = Some(uri);
    spawn_card_fetch(state, status_with_uri.id, content);
    if let Ok(media) = fetch_status_media(state, status_with_uri.id).await {
        if let Ok(api_status) =
            build_status(state, &status_with_uri, &account, media, None, None).await
        {
            if matches!(visibility.as_str(), "public" | "unlisted" | "private") {
                if let Ok(payload) = serde_json::to_string(&api_status) {
                    let hashtags: Vec<String> =
                        api_status.tags.iter().map(|t| t.name.clone()).collect();
                    state.streaming.publish(crate::streaming::Event::NewStatus {
                        author_id: account.id,
                        is_public: visibility == "public",
                        is_direct: visibility == "direct",
                        status_id: status_with_uri.id,
                        hashtags,
                        has_media: !api_status.media_attachments.is_empty(),
                        payload: std::sync::Arc::new(payload),
                    });
                }
            }
        }
    }

    // Fan-out to follower home feeds and list feeds
    let tag_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT tag_id FROM statuses_tags WHERE status_id = $1",
        status.id,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut redis = state.redis.clone();
    let db = state.db.clone();
    let author_id = account.id;
    let sid = status.id;
    let vis = visibility.clone();
    crate::feed::fanout_new_status(&mut redis, &db, author_id, sid, &tag_ids).await;
    crate::feed::fanout_to_lists(
        &mut redis,
        &db,
        author_id,
        sid,
        in_reply_to_account_id,
        &vis,
    )
    .await;

    // Send mention notifications (mirrors post_status)
    let mut notified = std::collections::HashSet::new();
    if let Some(parent_account_id) = in_reply_to_account_id {
        crate::push::create_and_push(
            state,
            parent_account_id,
            account.id,
            "mention",
            Some(status.id),
            format!("{} mentioned you", account.display_name),
            account.acct().clone(),
            crate::api::mastodon::convert::account_avatar_url_for(&account),
        )
        .await;
        notified.insert(parent_account_id);
    }
    for (_, mentioned) in &resolved {
        if mentioned.id == account.id || notified.contains(&mentioned.id) {
            continue;
        }
        crate::push::create_and_push(
            state,
            mentioned.id,
            account.id,
            "mention",
            Some(status.id),
            format!("{} mentioned you", account.display_name),
            account.acct().clone(),
            crate::api::mastodon::convert::account_avatar_url_for(&account),
        )
        .await;
        notified.insert(mentioned.id);
    }

    Ok(())
}

// ── Suspended account cleanup ─────────────────────────────────────────────

/// Mastodon's `Scheduler::SuspendedUserCleanupScheduler`: once a suspension has
/// stood for `DELAY_TO_DELETION`, the account's data is purged for good. Since
/// account deletion is expensive, only a few are processed per pass.
async fn run_suspended_account_cleanup(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(120));
    loop {
        interval.tick().await;
        if let Err(e) = process_deletion_requests(&state).await {
            tracing::error!(error = %e, "suspended account cleanup failed");
        }
    }
}

/// `MAX_DELETIONS_PER_JOB`
const MAX_DELETIONS_PER_PASS: i64 = 10;

pub async fn process_deletion_requests(state: &AppState) -> anyhow::Result<()> {
    let cutoff = chrono::Utc::now().naive_utc() - crate::delete_account::DELAY_TO_DELETION;
    let due: Vec<i64> = sqlx::query_scalar!(
        r#"SELECT account_id FROM account_deletion_requests
           WHERE created_at < $1
           ORDER BY id ASC
           LIMIT $2"#,
        cutoff,
        MAX_DELETIONS_PER_PASS,
    )
    .fetch_all(&state.db)
    .await?;

    for account_id in due {
        // `Admin::AccountDeletionWorker`: both records are kept, only the data goes.
        if let Err(e) = crate::delete_account::call(
            state,
            account_id,
            crate::delete_account::Options::default(),
        )
        .await
        {
            tracing::error!(account_id, error = %e, "scheduled account deletion failed");
        }
    }
    Ok(())
}

// ── Poll expiry notifier ──────────────────────────────────────────────────

async fn run_poll_expiry(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = notify_expired_polls(&state).await {
            tracing::error!(error = %e, "poll expiry task failed");
        }
    }
}

pub async fn notify_expired_polls(state: &AppState) -> anyhow::Result<()> {
    // Find polls that just expired and haven't had expiry notifications sent yet.
    // We track this with a simple approach: notify all unique voters + the poll author
    // for polls that expired in the last 2 minutes (our tick interval + buffer).
    let expired = sqlx::query!(
        r#"SELECT p.id, p.status_id, p.account_id
           FROM polls p
           WHERE p.expires_at IS NOT NULL
             AND p.expires_at <= now()
             AND p.expires_at > now() - interval '2 minutes'
           LIMIT 100"#,
    )
    .fetch_all(&state.db)
    .await?;

    for poll in expired {
        if let Err(e) =
            crate::api::mastodon::polls::federate_poll_update(state, poll.status_id).await
        {
            tracing::warn!(poll_id = poll.id, error = %e, "failed to enqueue expired poll ActivityPub update");
        }

        // Collect recipients: poll author + all voters
        let mut recipients: Vec<i64> = vec![poll.account_id];
        let voters = sqlx::query_scalar!(
            "SELECT DISTINCT account_id FROM poll_votes WHERE poll_id = $1",
            poll.id,
        )
        .fetch_all(&state.db)
        .await?;
        recipients.extend(voters);
        recipients.dedup();

        for recipient_id in recipients {
            crate::push::create_and_push(
                state,
                recipient_id,
                poll.account_id,
                "poll",
                Some(poll.status_id),
                "A poll you voted in has ended".into(),
                "".into(),
                "".into(),
            )
            .await;
        }
    }
    Ok(())
}
