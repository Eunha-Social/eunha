//! ActivityPub activity delivery to remote inboxes.

use serde_json::Value;

use crate::federation::signature;
use crate::state::AppState;

/// Deliver an activity to a single remote inbox, signed with the given key.
pub async fn deliver(
    http: &reqwest::Client,
    activity: &Value,
    inbox_url: &str,
    key_id: &str,
    private_key_pem: &str,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(activity)?;
    let headers = signature::sign_request("post", inbox_url, &body, key_id, private_key_pem)?;

    tracing::debug!(inbox = inbox_url, "delivering ActivityPub activity");

    let resp = http
        .post(inbox_url)
        .header("Content-Type", "application/activity+json")
        .header("Accept", "application/activity+json")
        .header("Date", headers.date)
        .header("Digest", headers.digest)
        .header("Signature", headers.signature)
        .body(body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() && status.as_u16() != 202 {
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!(inbox = inbox_url, body = %activity, status = status.as_u16(), response = %text, "federation delivery failed with body");
        anyhow::bail!("HTTP {} from {}: {}", status.as_u16(), inbox_url, text);
    }

    tracing::debug!(inbox = inbox_url, body = %activity, status = status.as_u16(), "federation delivery succeeded");
    Ok(())
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

        for row in inboxes {
            let Some(inbox) = row.inbox.filter(|s| !s.is_empty()) else {
                continue;
            };
            let activity = activity.clone();
            let key_id = key_id.clone();
            let privkey = private_key_pem.clone();
            let http = state.http.clone();
            tokio::spawn(async move {
                if let Err(e) = deliver(&http, &activity, &inbox, &key_id, &privkey).await {
                    tracing::warn!(inbox, error = %e, "federation delivery failed");
                }
            });
        }
    });
}

/// Deliver to a specific set of inboxes (for mentions, DMs, etc.).
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
