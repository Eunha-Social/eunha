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
    tokio::spawn(crate::api::ap::inbox::run_inbox_cleanup(state.clone()));
    tokio::spawn(crate::api::mastodon::media::run_media_queue(state.clone()));

    // Queue loops are sized from `[workers]` in config. Each loop claims work
    // with `FOR UPDATE SKIP LOCKED`, so adding loops within this process scales
    // the same way adding processes would.
    let workers = state.config.workers.sanitized();
    for index in 0..workers.delivery_workers {
        tokio::spawn(crate::federation::delivery::run_delivery_queue(
            state.clone(),
            index,
        ));
    }
    for index in 0..workers.inbox_workers {
        tokio::spawn(crate::api::ap::inbox::run_inbox_queue(state.clone(), index));
    }
    tracing::info!(
        delivery_workers = workers.delivery_workers,
        delivery_concurrency = workers.delivery_concurrency,
        inbox_workers = workers.inbox_workers,
        inbox_concurrency = workers.inbox_concurrency,
        "background queues started"
    );
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

/// How many times a scheduled status that wrote nothing is retried before it is
/// parked. Combined with the backoff below this spans a couple of hours, so a
/// database blip or a restart doesn't cost anyone a post.
const SCHEDULED_STATUS_MAX_ATTEMPTS: i32 = 8;

/// Why a scheduled status could not be published, which decides whether it is
/// worth trying again.
enum PublishError {
    /// These params can never produce a status (the account is gone, the row
    /// carries no params). Retrying would fail identically every minute, so the
    /// schedule is dropped.
    Permanent(anyhow::Error),
    /// Nothing was written and the cause may not recur — a database error, a
    /// lock, a restart mid-publish. Safe to run again.
    Transient(anyhow::Error),
}

impl PublishError {
    fn error(&self) -> &anyhow::Error {
        match self {
            Self::Permanent(e) | Self::Transient(e) => e,
        }
    }
}

/// A failed `fetch_one` means the account no longer exists; anything else is
/// the database being unhappy, which may well pass.
fn classify_db(e: sqlx::Error, context: &str) -> PublishError {
    let msg = format!("{context}: {e}");
    match e {
        sqlx::Error::RowNotFound => PublishError::Permanent(anyhow::anyhow!(msg)),
        _ => PublishError::Transient(anyhow::anyhow!(msg)),
    }
}

pub async fn publish_due_statuses(state: &AppState) -> anyhow::Result<()> {
    // Skip schedules that are backing off from an earlier failure, and those
    // that have exhausted their attempts (kept, but no longer retried).
    let rows = sqlx::query!(
        r#"SELECT s.id, s.account_id, s.params
           FROM scheduled_statuses s
           LEFT JOIN eunha.scheduled_status_attempts a
             ON a.scheduled_status_id = s.id
           WHERE s.scheduled_at <= now()
             AND a.failed_at IS NULL
             AND (a.run_at IS NULL OR a.run_at <= now())
           ORDER BY s.scheduled_at ASC
           LIMIT 50"#,
    )
    .fetch_all(&state.db)
    .await?;

    for row in rows {
        match publish_one(state, row.id, row.account_id, &row.params).await {
            // The status exists now, so the schedule has been consumed even if
            // some follow-up step (fan-out, notifications) logged a failure.
            Ok(()) => forget_schedule(state, row.id).await?,
            Err(PublishError::Permanent(e)) => {
                tracing::warn!(id = row.id, error = %e, "scheduled status cannot be published; dropping");
                forget_schedule(state, row.id).await?;
            }
            Err(e @ PublishError::Transient(_)) => {
                record_publish_failure(state, row.id, e.error()).await?;
            }
        }
    }
    Ok(())
}

/// Drop a schedule and its retry bookkeeping.
async fn forget_schedule(state: &AppState, scheduled_id: i64) -> anyhow::Result<()> {
    sqlx::query!(
        "DELETE FROM eunha.scheduled_status_attempts WHERE scheduled_status_id = $1",
        scheduled_id,
    )
    .execute(&state.db)
    .await?;
    sqlx::query!("DELETE FROM scheduled_statuses WHERE id = $1", scheduled_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Count an attempt that wrote nothing and schedule the next one. Once the
/// attempts run out the schedule is parked rather than deleted, so the author
/// still sees the post they scheduled.
async fn record_publish_failure(
    state: &AppState,
    scheduled_id: i64,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let err = error.to_string();
    // Exponential backoff: 1m, 2m, 4m, … capped at an hour.
    let attempts = sqlx::query_scalar!(
        r#"INSERT INTO eunha.scheduled_status_attempts
             (scheduled_status_id, attempts, run_at, last_error, created_at, updated_at)
           VALUES ($1, 1, now() + interval '1 minute', $2, now(), now())
           ON CONFLICT (scheduled_status_id) DO UPDATE
             SET attempts = eunha.scheduled_status_attempts.attempts + 1,
                 run_at = now() + LEAST(
                     interval '1 hour',
                     interval '1 minute' * pow(2, eunha.scheduled_status_attempts.attempts)
                 ),
                 last_error = $2,
                 updated_at = now()
           RETURNING attempts"#,
        scheduled_id,
        err,
    )
    .fetch_one(&state.db)
    .await?;

    if attempts >= SCHEDULED_STATUS_MAX_ATTEMPTS {
        sqlx::query!(
            r#"UPDATE eunha.scheduled_status_attempts
               SET failed_at = now(), updated_at = now()
               WHERE scheduled_status_id = $1"#,
            scheduled_id,
        )
        .execute(&state.db)
        .await?;
        tracing::error!(
            id = scheduled_id,
            attempts,
            error = %err,
            "scheduled status still unpublished after every attempt; parked (schedule kept)"
        );
    } else {
        tracing::warn!(
            id = scheduled_id,
            attempts,
            error = %err,
            "scheduled status publish failed; will retry"
        );
    }
    Ok(())
}

/// Publish one scheduled status.
///
/// The split at the `statuses` INSERT is what makes retrying safe: everything
/// before it either succeeds or leaves the database untouched, so a failure
/// there can be tried again. Once the row is inserted the post exists and the
/// schedule is spent — every later step is therefore best-effort and logged,
/// never propagated, because returning an error would re-run this function and
/// post the status a second time.
async fn publish_one(
    state: &AppState,
    scheduled_id: i64,
    account_id: i64,
    params: &Option<serde_json::Value>,
) -> Result<(), PublishError> {
    let params = params
        .as_ref()
        .ok_or_else(|| PublishError::Permanent(anyhow::anyhow!("no params")))?;

    let account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        account_id,
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| classify_db(e, "load scheduled status author"))?;

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
    .await
    .map_err(|e| classify_db(e, "insert scheduled status"))?;

    // ── Past this point the status exists; failures are logged, not returned ──

    if let Err(e) = store_statuses_tags(state, status.id, account.id, &hashtags).await {
        tracing::error!(scheduled_id, status_id = status.id, error = %e, "scheduled status published without its hashtags");
    }
    if let Err(e) = store_status_mentions(state, status.id, &resolved).await {
        tracing::error!(scheduled_id, status_id = status.id, error = %e, "scheduled status published without its mentions");
    }

    // Update last_status_at and statuses_count in account_stats
    let stats = sqlx::query!(
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
    .await;
    if let Err(e) = stats {
        tracing::error!(scheduled_id, status_id = status.id, error = %e, "failed to update account stats for scheduled status");
    }

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
                    let attached = sqlx::query!(
                        "UPDATE media_attachments SET status_id = $1 WHERE id = $2 AND account_id = $3 AND status_id IS NULL",
                        status.id, media_id, account.id,
                    )
                    .execute(&state.db)
                    .await;
                    if let Err(e) = attached {
                        tracing::error!(scheduled_id, status_id = status.id, media_id, error = %e, "failed to attach media to scheduled status");
                    }
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
                let poll_created = sqlx::query!(
                    r#"INSERT INTO polls
                         (status_id, account_id, options, multiple, hide_totals, expires_at, created_at, updated_at)
                       VALUES ($1,$2,$3,$4,$5,$6,now(),now())"#,
                    status.id, account.id, &opts as &[String], multiple, hide_totals, expires_at,
                )
                .execute(&state.db)
                .await;
                if let Err(e) = poll_created {
                    tracing::error!(scheduled_id, status_id = status.id, error = %e, "scheduled status published without its poll");
                }
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
