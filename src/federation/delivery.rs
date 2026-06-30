//! ActivityPub activity delivery to remote inboxes.

use futures::StreamExt as _;
use serde_json::Value;
use std::time::Duration;

use crate::state::AppState;

const DELIVERY_QUEUE_BATCH: i64 = 50;
/// How many inboxes to deliver to concurrently within a batch. A few slow or
/// dead inboxes must not stall delivery to healthy servers.
const DELIVERY_QUEUE_CONCURRENCY: usize = 16;
const DELIVERY_QUEUE_IDLE: Duration = Duration::from_secs(2);
const DELIVERY_QUEUE_ERROR_IDLE: Duration = Duration::from_secs(10);
/// How often to prune finished delivery jobs.
const DELIVERY_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);

/// Deliver an activity to a single remote inbox, signed with the given key.
///
/// This is a single attempt: retries are owned by the durable delivery queue,
/// which re-schedules transient failures with exponential backoff via `run_at`.
/// Keeping this non-blocking lets the queue worker fan out concurrently instead
/// of sleeping on a failing inbox.
pub async fn deliver(
    http: &reqwest::Client,
    activity: &Value,
    inbox_url: &str,
    key_id: &str,
    private_key_pem: &str,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(activity)?;
    tracing::debug!(inbox = inbox_url, "delivering ActivityPub activity");
    feder_runtime::delivery::deliver(http, &body, inbox_url, key_id, private_key_pem).await
}

/// Classify a delivery error as transient (worth retrying). feder-runtime
/// formats HTTP failures as `HTTP <code> from …`; anything without an HTTP code
/// is a network/transport error and is retriable.
fn is_retriable(err: &anyhow::Error) -> bool {
    match http_status_of(err) {
        Some(code) => code == 408 || code == 429 || (500..=599).contains(&code),
        None => true,
    }
}

/// Extract the HTTP status code from a feder-runtime delivery error, if present.
fn http_status_of(err: &anyhow::Error) -> Option<u16> {
    let msg = err.to_string();
    let rest = msg.strip_prefix("HTTP ")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// True when the error is an HTTP 410 Gone — a definitive signal the inbox no
/// longer exists, so we should stop delivering to its domain.
fn is_gone(err: &anyhow::Error) -> bool {
    http_status_of(err) == Some(410)
}

/// Record a domain as unavailable so future fan-outs skip it.
async fn mark_domain_unavailable(state: &AppState, inbox_url: &str) {
    let Some(domain) = url::Url::parse(inbox_url).ok().and_then(|u| u.host_str().map(str::to_owned))
    else {
        return;
    };
    let _ = sqlx::query!(
        r#"INSERT INTO unavailable_domains (domain, created_at, updated_at)
           VALUES ($1, now(), now())
           ON CONFLICT (domain) DO UPDATE SET updated_at = now()"#,
        domain,
    )
    .execute(&state.db)
    .await;
    tracing::info!(domain, "marked domain unavailable after 410 Gone");
}

/// Fetch the set of domains currently marked unavailable.
async fn unavailable_domains(state: &AppState) -> std::collections::HashSet<String> {
    sqlx::query_scalar!("SELECT domain FROM unavailable_domains")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// True if `inbox_url`'s host is in `unavailable`.
fn inbox_unavailable(inbox_url: &str, unavailable: &std::collections::HashSet<String>) -> bool {
    url::Url::parse(inbox_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .map(|h| unavailable.contains(&h))
        .unwrap_or(false)
}

/// Fan out an activity to all remote follower inboxes for `actor_account_id`.
pub async fn fanout_to_followers(
    state: &AppState,
    activity: Value,
    actor_account_id: i64,
    key_id: String,
) -> anyhow::Result<u64> {
    let inboxes = sqlx::query!(
        r#"SELECT DISTINCT
             CASE WHEN a.shared_inbox_url IS NOT NULL AND a.shared_inbox_url <> ''
                  THEN a.shared_inbox_url
                  ELSE a.inbox_url
             END AS inbox
           FROM follows f
           JOIN accounts a ON a.id = f.account_id
           WHERE f.target_account_id = $1
             AND a.domain IS NOT NULL
             AND a.inbox_url <> ''
             AND a.suspended_at IS NULL"#,
        actor_account_id,
    )
    .fetch_all(&state.db)
    .await?;

    let unavailable = unavailable_domains(state).await;
    let inboxes = inboxes
        .into_iter()
        .filter_map(|row| row.inbox)
        .filter(|inbox| !inbox.is_empty())
        .filter(|inbox| {
            if inbox_unavailable(inbox, &unavailable) {
                tracing::debug!(inbox, "skipping delivery to unavailable domain");
                false
            } else {
                true
            }
        })
        .collect();

    enqueue_to_inboxes(state, activity, inboxes, key_id).await
}

/// Fan out a public/unlisted reply to a *local* account: deliver to the
/// author's followers **and** the thread (parent) author's followers, matching
/// Mastodon's `StatusReachFinder#followers_scope`
/// (`account.followers.or(thread.account.followers.not_domain_blocked_by_account(account))`).
/// Parent-author followers whose domain the author blocks are excluded; suspended
/// accounts are excluded everywhere; the inbox set is de-duplicated.
pub async fn fanout_to_reply_followers(
    state: &AppState,
    activity: Value,
    author_account_id: i64,
    thread_account_id: i64,
    key_id: String,
) -> anyhow::Result<u64> {
    let inboxes = sqlx::query!(
        r#"SELECT DISTINCT
             CASE WHEN a.shared_inbox_url IS NOT NULL AND a.shared_inbox_url <> ''
                  THEN a.shared_inbox_url
                  ELSE a.inbox_url
             END AS inbox
           FROM accounts a
           WHERE a.domain IS NOT NULL
             AND a.inbox_url <> ''
             AND a.suspended_at IS NULL
             AND (
               EXISTS (SELECT 1 FROM follows f
                       WHERE f.account_id = a.id AND f.target_account_id = $1)
               OR (
                 EXISTS (SELECT 1 FROM follows f
                         WHERE f.account_id = a.id AND f.target_account_id = $2)
                 AND a.domain NOT IN (
                   SELECT domain FROM account_domain_blocks WHERE account_id = $1
                 )
               )
             )"#,
        author_account_id,
        thread_account_id,
    )
    .fetch_all(&state.db)
    .await?;

    let unavailable = unavailable_domains(state).await;
    let inboxes = inboxes
        .into_iter()
        .filter_map(|row| row.inbox)
        .filter(|inbox| !inbox.is_empty())
        .filter(|inbox| !inbox_unavailable(inbox, &unavailable))
        .collect();

    enqueue_to_inboxes(state, activity, inboxes, key_id).await
}

/// Deliver a public status to every enabled (accepted) relay's inbox, matching
/// Mastodon's `StatusReachFinder#relay_inboxes`.
pub async fn deliver_to_relays(
    state: &AppState,
    activity: Value,
    key_id: String,
) -> anyhow::Result<u64> {
    // Mastodon's Relay state enum: accepted == 2 (the `enabled` scope).
    let rows = sqlx::query_scalar!(
        "SELECT inbox_url FROM relays WHERE state = 2 AND inbox_url <> ''",
    )
    .fetch_all(&state.db)
    .await?;

    let unavailable = unavailable_domains(state).await;
    let inboxes: Vec<String> = rows
        .into_iter()
        .filter(|inbox| !inbox.is_empty())
        .filter(|inbox| !inbox_unavailable(inbox, &unavailable))
        .collect();

    if inboxes.is_empty() {
        return Ok(0);
    }
    enqueue_to_inboxes(state, activity, inboxes, key_id).await
}

/// Deliver to a specific set of inboxes (for mentions, DMs, consent replies).
pub async fn deliver_to_inboxes(
    state: &AppState,
    activity: Value,
    inboxes: Vec<String>,
    key_id: String,
) -> anyhow::Result<u64> {
    enqueue_to_inboxes(state, activity, inboxes, key_id).await
}

/// Resolve the local signing account id from a `key_id` of the form
/// `{actor_url}#main-key`. The actor URL follows the account's id_scheme — either
/// `https://{domain}/users/{username}` or `https://{domain}/ap/users/{id}`
/// (see [`crate::federation::tag`]) — so match on whichever path shape it is
/// rather than the (empty for Mastodon imports) `accounts.uri` column.
async fn signing_account_id(state: &AppState, key_id: &str) -> anyhow::Result<i64> {
    let actor_url = key_id.split('#').next().unwrap_or(key_id);
    let url = url::Url::parse(actor_url)
        .map_err(|e| anyhow::anyhow!("invalid keyId actor URL {actor_url:?}: {e}"))?;
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();

    let id = match segments.as_slice() {
        ["ap", "users", id] => {
            let id: i64 = id
                .parse()
                .map_err(|_| anyhow::anyhow!("non-numeric ap account id in {actor_url:?}"))?;
            sqlx::query_scalar!(
                "SELECT id FROM accounts WHERE domain IS NULL AND id = $1",
                id,
            )
            .fetch_optional(&state.db)
            .await?
        }
        ["users", username] => sqlx::query_scalar!(
            "SELECT id FROM accounts WHERE domain IS NULL AND username = $1 LIMIT 1",
            username,
        )
        .fetch_optional(&state.db)
        .await?,
        _ => None,
    };
    id.ok_or_else(|| anyhow::anyhow!("no local signing account for key_id {key_id:?}"))
}

async fn enqueue_to_inboxes(
    state: &AppState,
    activity: Value,
    inboxes: Vec<String>,
    key_id: String,
) -> anyhow::Result<u64> {
    // Record the signing account, not its private key: the key is loaded from
    // `accounts` at send time so the secret lives in exactly one place.
    let actor_account_id = signing_account_id(state, &key_id).await?;

    // Never deliver to domains we've defederated (admin domain block at suspend
    // severity), even for direct sends like consent replies and mentions.
    let suspended = crate::federation::moderation::suspended_domains(state).await;
    let mut inboxes = inboxes
        .into_iter()
        .filter(|s| !s.is_empty())
        .filter(|inbox| {
            if crate::federation::moderation::inbox_suspended(inbox, &suspended) {
                tracing::debug!(inbox, "skipping delivery to suspended domain");
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    inboxes.sort();
    inboxes.dedup();

    let mut enqueued = 0;
    for inbox in inboxes {
        sqlx::query!(
            r#"INSERT INTO eunha.activity_delivery_jobs
                 (activity, inbox_url, key_id, actor_account_id, created_at, updated_at)
               VALUES ($1, $2, $3, $4, now(), now())"#,
            &activity,
            &inbox,
            &key_id,
            actor_account_id,
        )
        .execute(&state.db)
        .await?;
        enqueued += 1;
    }
    Ok(enqueued)
}

/// Run the durable ActivityPub delivery queue.
pub async fn run_delivery_queue(state: AppState) {
    let worker_id = format!("{}:{}", std::env::var("HOSTNAME").unwrap_or_else(|_| "eunha".into()), std::process::id());

    loop {
        match run_delivery_queue_batch(&state, &worker_id).await {
            Ok(0) => tokio::time::sleep(DELIVERY_QUEUE_IDLE).await,
            Ok(n) => tracing::debug!(count = n, "processed ActivityPub delivery jobs"),
            Err(e) => {
                tracing::error!(error = %e, "ActivityPub delivery queue batch failed");
                tokio::time::sleep(DELIVERY_QUEUE_ERROR_IDLE).await;
            }
        }
    }
}

async fn run_delivery_queue_batch(state: &AppState, worker_id: &str) -> anyhow::Result<usize> {
    let jobs = sqlx::query!(
        r#"WITH picked AS (
             SELECT id
             FROM eunha.activity_delivery_jobs
             WHERE delivered_at IS NULL
               AND failed_at IS NULL
               AND run_at <= now()
               AND (locked_at IS NULL OR locked_at < now() - interval '10 minutes')
             ORDER BY run_at ASC, id ASC
             LIMIT $1
             FOR UPDATE SKIP LOCKED
           )
           UPDATE eunha.activity_delivery_jobs j
           SET locked_at = now(), locked_by = $2, updated_at = now()
           FROM picked
           WHERE j.id = picked.id
           RETURNING j.id, j.activity, j.inbox_url, j.key_id, j.actor_account_id,
                     j.attempts, j.max_attempts"#,
        DELIVERY_QUEUE_BATCH,
        worker_id,
    )
    .fetch_all(&state.db)
    .await?;

    let count = jobs.len();

    // Deliver to the picked inboxes concurrently. Each job records its own
    // outcome; a single slow or dead inbox no longer blocks the rest of the
    // batch, and a failed DB update on one job is logged rather than aborting
    // the others.
    futures::stream::iter(jobs)
        .for_each_concurrent(DELIVERY_QUEUE_CONCURRENCY, |job| async move {
            process_delivery_job(
                state,
                job.id,
                &job.inbox_url,
                &job.key_id,
                &job.activity,
                job.actor_account_id,
                job.attempts,
                job.max_attempts,
            )
            .await;
        })
        .await;

    Ok(count)
}

/// Load the signing key, deliver one job, and record the outcome. A missing
/// signing account or key is permanent (the actor was deleted), so the job is
/// forced terminal rather than retried.
#[allow(clippy::too_many_arguments)]
async fn process_delivery_job(
    state: &AppState,
    id: i64,
    inbox_url: &str,
    key_id: &str,
    activity: &Value,
    actor_account_id: Option<i64>,
    attempts: i32,
    max_attempts: i32,
) {
    let private_key = match actor_account_id {
        Some(aid) => load_signing_key(state, aid).await,
        None => Err(anyhow::anyhow!("delivery job {id} has no signing account")),
    };
    let private_key = match private_key {
        Ok(pk) => pk,
        Err(e) => {
            // Force terminal by maxing out attempts so the no-key failure isn't retried.
            if let Err(db) =
                record_job_outcome(state, id, inbox_url, max_attempts, max_attempts, Err(e)).await
            {
                tracing::error!(id, error = %db, "failed to record delivery job outcome");
            }
            return;
        }
    };

    let result = deliver(&state.http, activity, inbox_url, key_id, &private_key).await;
    if let Err(e) = record_job_outcome(state, id, inbox_url, attempts, max_attempts, result).await {
        tracing::error!(id, error = %e, "failed to record delivery job outcome");
    }
}

/// Load a local account's signing private key by id. Errors if the account or
/// its key is gone.
async fn load_signing_key(state: &AppState, account_id: i64) -> anyhow::Result<String> {
    sqlx::query_scalar!("SELECT private_key FROM accounts WHERE id = $1", account_id)
        .fetch_optional(&state.db)
        .await?
        .flatten()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no signing key for account {account_id}"))
}

/// Persist the result of a single delivery attempt: mark delivered, re-schedule
/// with backoff, or mark permanently failed.
async fn record_job_outcome(
    state: &AppState,
    id: i64,
    inbox_url: &str,
    attempts: i32,
    max_attempts: i32,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    let Err(e) = result else {
        sqlx::query!(
            r#"UPDATE eunha.activity_delivery_jobs
               SET delivered_at = now(),
                   locked_at = NULL,
                   locked_by = NULL,
                   updated_at = now()
               WHERE id = $1"#,
            id,
        )
        .execute(&state.db)
        .await?;
        return Ok(());
    };

    if is_gone(&e) {
        mark_domain_unavailable(state, inbox_url).await;
    }
    let next_attempts = attempts + 1;
    let terminal = !is_retriable(&e) || next_attempts >= max_attempts;
    let error = e.to_string();
    if terminal {
        sqlx::query!(
            r#"UPDATE eunha.activity_delivery_jobs
               SET attempts = $2,
                   failed_at = now(),
                   locked_at = NULL,
                   locked_by = NULL,
                   last_error = $3,
                   updated_at = now()
               WHERE id = $1"#,
            id,
            next_attempts,
            error,
        )
        .execute(&state.db)
        .await?;
        tracing::warn!(
            id,
            inbox = inbox_url,
            attempts = next_attempts,
            error = %error,
            "ActivityPub delivery job failed permanently"
        );
    } else {
        let backoff_secs = queue_backoff_seconds(next_attempts);
        let run_at = chrono::Utc::now() + chrono::Duration::seconds(backoff_secs);
        sqlx::query!(
            r#"UPDATE eunha.activity_delivery_jobs
               SET attempts = $2,
                   run_at = $3,
                   locked_at = NULL,
                   locked_by = NULL,
                   last_error = $4,
                   updated_at = now()
               WHERE id = $1"#,
            id,
            next_attempts,
            run_at,
            error,
        )
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

/// Periodically delete finished delivery jobs. This bounds the table's growth
/// and limits how long the signing keys stored in each row persist at rest.
/// Delivered jobs are kept briefly; permanently-failed jobs are kept longer so
/// their `last_error` is available for debugging.
pub async fn run_delivery_cleanup(state: AppState) {
    loop {
        match cleanup_finished_jobs(&state).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "pruned finished delivery jobs"),
            Err(e) => tracing::error!(error = %e, "delivery job cleanup failed"),
        }
        tokio::time::sleep(DELIVERY_CLEANUP_INTERVAL).await;
    }
}

async fn cleanup_finished_jobs(state: &AppState) -> anyhow::Result<u64> {
    let result = sqlx::query!(
        r#"DELETE FROM eunha.activity_delivery_jobs
           WHERE (delivered_at IS NOT NULL AND delivered_at < now() - interval '1 day')
              OR (failed_at IS NOT NULL AND failed_at < now() - interval '7 days')"#,
    )
    .execute(&state.db)
    .await?;
    Ok(result.rows_affected())
}

fn queue_backoff_seconds(attempts: i32) -> i64 {
    let exponent = attempts.saturating_sub(1).min(10) as u32;
    (30_i64 * 2_i64.pow(exponent)).min(3600)
}
