//! ActivityPub activity delivery to remote inboxes.

use serde_json::Value;
use std::time::Duration;

use crate::state::AppState;

/// Maximum number of delivery attempts (1 initial + retries) for a transient
/// failure before giving up.
const MAX_ATTEMPTS: u32 = 4;
/// Base backoff; doubled on each retry (0.5s, 1s, 2s …).
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Deliver an activity to a single remote inbox, signed with the given key.
///
/// Transient failures (network errors, timeouts, HTTP 429/5xx) are retried with
/// exponential backoff; permanent failures (most 4xx) return immediately.
pub async fn deliver(
    http: &reqwest::Client,
    activity: &Value,
    inbox_url: &str,
    key_id: &str,
    private_key_pem: &str,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(activity)?;
    tracing::debug!(inbox = inbox_url, "delivering ActivityPub activity");

    let mut attempt = 0;
    loop {
        attempt += 1;
        match feder_runtime::delivery::deliver(http, &body, inbox_url, key_id, private_key_pem).await
        {
            Ok(()) => {
                tracing::debug!(inbox = inbox_url, attempt, "federation delivery succeeded");
                return Ok(());
            }
            Err(e) => {
                let retriable = is_retriable(&e);
                if !retriable || attempt >= MAX_ATTEMPTS {
                    tracing::warn!(
                        inbox = inbox_url, attempt, retriable, error = %e,
                        "federation delivery failed; giving up"
                    );
                    return Err(e);
                }
                let backoff = BASE_BACKOFF * 2u32.pow(attempt - 1);
                tracing::debug!(inbox = inbox_url, attempt, ?backoff, error = %e, "delivery failed; retrying");
                tokio::time::sleep(backoff).await;
            }
        }
    }
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
/// Spawns individual tokio tasks per unique inbox; returns immediately.
pub fn fanout_to_followers(
    state: &AppState,
    activity: Value,
    actor_account_id: i64,
    key_id: String,
    private_key_pem: String,
) {
    let state = state.clone();
    tokio::spawn(async move {
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
                 AND a.inbox_url <> ''"#,
            actor_account_id,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let unavailable = unavailable_domains(&state).await;

        for row in inboxes {
            let Some(inbox) = row.inbox.filter(|s| !s.is_empty()) else {
                continue;
            };
            if inbox_unavailable(&inbox, &unavailable) {
                tracing::debug!(inbox, "skipping delivery to unavailable domain");
                continue;
            }
            let activity = activity.clone();
            let key_id = key_id.clone();
            let privkey = private_key_pem.clone();
            let state = state.clone();
            tokio::spawn(async move {
                deliver_one(&state, &activity, &inbox, &key_id, &privkey).await;
            });
        }
    });
}

/// Deliver to a specific set of inboxes (for mentions, DMs, consent replies).
pub fn deliver_to_inboxes(
    http: reqwest::Client,
    activity: Value,
    inboxes: Vec<String>,
    key_id: String,
    private_key_pem: String,
) {
    for inbox in inboxes {
        let activity = activity.clone();
        let key_id = key_id.clone();
        let privkey = private_key_pem.clone();
        let http = http.clone();
        tokio::spawn(async move {
            if let Err(e) = deliver(&http, &activity, &inbox, &key_id, &privkey).await {
                tracing::warn!(inbox, error = %e, "federation delivery failed");
            }
        });
    }
}

/// Deliver once with retries, marking the domain unavailable on a 410 Gone.
async fn deliver_one(
    state: &AppState,
    activity: &Value,
    inbox: &str,
    key_id: &str,
    private_key_pem: &str,
) {
    if let Err(e) = deliver(&state.http, activity, inbox, key_id, private_key_pem).await {
        if is_gone(&e) {
            mark_domain_unavailable(state, inbox).await;
        }
        tracing::warn!(inbox, error = %e, "federation delivery failed");
    }
}
