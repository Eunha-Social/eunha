use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::{
    ecdsa::SigningKey,
    pkcs8::{EncodePrivateKey, LineEnding},
};

use crate::state::AppState;

// ── VAPID key generation ───────────────────────────────────────────────────

/// Generates a P-256 VAPID keypair.
/// Returns (pkcs8_pem, public_key_base64url).
/// The PEM is stored in the DB; the base64url key is returned to clients.
pub fn generate_vapid_keypair() -> anyhow::Result<(String, String)> {
    use p256::elliptic_curve::rand_core::OsRng;
    let signing_key = SigningKey::random(&mut OsRng);
    let pem = signing_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("pkcs8 encode: {e}"))?;

    let pub_point = signing_key
        .verifying_key()
        .to_encoded_point(false); // uncompressed 65-byte point
    let pub_b64 = URL_SAFE_NO_PAD.encode(pub_point.as_bytes());

    Ok((pem.to_string(), pub_b64))
}

/// Returns the VAPID private key from instance config.
/// In single-tenant mode, keys are sourced from config rather than the DB.
pub fn get_vapid_private_key(state: &AppState) -> &str {
    &state.instance.vapid_private_key
}

pub fn get_vapid_public_key(state: &AppState) -> &str {
    &state.instance.vapid_public_key
}

// ── Push delivery ──────────────────────────────────────────────────────────

/// Payload sent to the push endpoint, matching Mastodon's format.
#[derive(serde::Serialize)]
struct PushPayload<'a> {
    notification_id: i64,
    notification_type: &'a str,
    icon: &'a str,
    title: &'a str,
    body: &'a str,
    preferred_locale: &'a str,
}

/// Deliver a push notification to all subscriptions registered for `recipient_id`
/// where the corresponding alert type is enabled.
/// Failures are logged and swallowed — push is best-effort.
pub async fn deliver(
    state: AppState,
    recipient_id: i64,
    notification_id: i64,
    notification_type: &str,
    icon: &str,
    title: &str,
    body: &str,
) {
    if let Err(e) = try_deliver(
        &state,
        recipient_id,
        notification_id,
        notification_type,
        icon,
        title,
        body,
    )
    .await
    {
        tracing::warn!(error = %e, "push delivery error");
    }
}

async fn try_deliver(
    state: &AppState,
    recipient_id: i64,
    notification_id: i64,
    notification_type: &str,
    icon: &str,
    title: &str,
    body: &str,
) -> anyhow::Result<()> {
    // Look up subscriptions for the recipient that have this alert type enabled.
    let (alert_key, alert_default) = match notification_type {
        "follow" | "follow_request" => ("follow", "true"),
        "favourite"     => ("favourite",     "true"),
        "reblog"        => ("reblog",        "true"),
        "mention"       => ("mention",       "true"),
        "poll"          => ("poll",          "false"),
        "status"        => ("status",        "false"),
        "update"        => ("update",        "false"),
        "quote"         => ("quote",         "false"),
        "quoted_update" => ("quoted_update", "false"),
        _ => return Ok(()),
    };

    let subs_query = format!(
        r#"SELECT wps.id, wps.endpoint, wps.key_p256dh, wps.key_auth
           FROM web_push_subscriptions wps
           JOIN oauth_access_tokens oat ON oat.id = wps.access_token_id
           JOIN users u ON u.id = oat.resource_owner_id
           WHERE u.account_id = $1
             AND oat.revoked_at IS NULL
             AND COALESCE((wps.data->'alerts'->>'{}')::boolean, {})"#,
        alert_key,
        alert_default,
    );

    let rows = sqlx::query_as::<_, (i64, String, String, String)>(
        &subs_query,
    )
    .bind(recipient_id)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let vapid_priv = state.instance.vapid_private_key.clone();

    let payload = serde_json::to_string(&PushPayload {
        notification_id,
        notification_type,
        icon,
        title,
        body,
        preferred_locale: "en",
    })?;

    for (_, endpoint, p256dh, auth) in &rows {
        if let Err(e) = send_one(
            state,
            endpoint,
            p256dh,
            auth,
            &vapid_priv,
            &payload,
        )
        .await
        {
            tracing::warn!(
                endpoint = %endpoint,
                error = %e,
                "push send failed"
            );
        }
    }

    Ok(())
}

async fn send_one(
    state: &AppState,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
    vapid_private_pem: &str,
    payload: &str,
) -> anyhow::Result<()> {
    use web_push::{
        ContentEncoding, SubscriptionInfo, SubscriptionKeys, VapidSignatureBuilder,
        WebPushMessageBuilder,
    };

    let sub_info = SubscriptionInfo {
        endpoint: endpoint.to_string(),
        keys: SubscriptionKeys {
            auth: auth.to_string(),
            p256dh: p256dh.to_string(),
        },
    };

    let mut builder = WebPushMessageBuilder::new(&sub_info);
    builder.set_payload(ContentEncoding::AesGcm, payload.as_bytes());
    builder.set_ttl(86400);

    let sig_builder = VapidSignatureBuilder::from_pem(
        vapid_private_pem.as_bytes(),
        &sub_info,
    )?;
    builder.set_vapid_signature(sig_builder.build()?);

    let message = builder.build()?;

    send_with_reqwest(&state.http, message).await
}

async fn send_with_reqwest(
    http: &reqwest::Client,
    message: web_push::WebPushMessage,
) -> anyhow::Result<()> {
    let endpoint = message.endpoint.to_string();
    let ttl = message.ttl;
    let mut req = http
        .post(endpoint.as_str())
        .header("TTL", ttl.to_string());

    if let Some(payload) = message.payload {
        req = req
            .header("Content-Encoding", payload.content_encoding.to_str())
            .header("Content-Type", "application/octet-stream");

        for (k, v) in &payload.crypto_headers {
            req = req.header(*k, v.as_str());
        }

        req = req.body(payload.content);
    }

    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() && status.as_u16() != 201 {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(status = %status, body = %body, endpoint = %endpoint, "push relay rejected message");
    }

    Ok(())
}

// ── Notification creation helper ───────────────────────────────────────────

/// Insert a notification record and fire push delivery in a background task.
#[allow(clippy::too_many_arguments)]
pub async fn create_and_push(
    state: &AppState,
    recipient_id: i64,
    from_account_id: i64,
    notification_type: &'static str,
    status_id: Option<i64>,
    title: String,
    body: String,
    icon: String,
) {
    let db = state.db.clone();

    // Don't notify yourself
    if recipient_id == from_account_id {
        return;
    }

    // Don't notify if there is a block in either direction
    let is_blocked = sqlx::query_scalar!(
        r#"SELECT 1 FROM blocks
           WHERE (account_id = $1 AND target_account_id = $2)
              OR (account_id = $2 AND target_account_id = $1)"#,
        recipient_id, from_account_id,
    )
    .fetch_optional(&db)
    .await
    .ok()
    .flatten()
    .is_some();
    if is_blocked {
        return;
    }

    // Don't notify if the recipient mutes the sender with hide_notifications.
    let notifications_hidden = sqlx::query_scalar!(
        r#"SELECT 1 FROM mutes
           WHERE account_id = $1 AND target_account_id = $2 AND hide_notifications = true
             AND (expires_at IS NULL OR expires_at > now())"#,
        recipient_id, from_account_id,
    )
    .fetch_optional(&db)
    .await
    .ok()
    .flatten()
    .is_some();
    if notifications_hidden {
        return;
    }

    // Don't notify if the recipient has muted the conversation
    if let Some(sid) = status_id {
        let conversation_muted = sqlx::query_scalar!(
            "SELECT 1 FROM conversation_mutes cm JOIN statuses s ON s.id = $2 WHERE cm.account_id = $1 AND cm.conversation_id = s.conversation_id LIMIT 1",
            recipient_id, sid,
        )
        .fetch_optional(&db)
        .await
        .ok()
        .flatten()
        .is_some();
        if conversation_muted {
            return;
        }
    }

    // Check notification policy: route to notification_requests if filtered
    if should_filter_notification(&db, recipient_id, from_account_id, notification_type, status_id).await {
        route_to_request(&db, recipient_id, from_account_id, status_id).await;
        return;
    }

    // Dedup: don't insert the same (account, from, type, activity) twice
    let existing = sqlx::query_scalar!(
        r#"SELECT 1 FROM notifications
           WHERE account_id = $1 AND from_account_id = $2
             AND "type" = $3
             AND (activity_id = $4 OR ($4::bigint IS NULL AND activity_id IS NULL))
           LIMIT 1"#,
        recipient_id,
        from_account_id,
        notification_type,
        status_id,
    )
    .fetch_optional(&db)
    .await;

    if matches!(existing, Ok(Some(_))) {
        return;
    }

    let activity_type_val: Option<&str> = if status_id.is_some() { Some("Status") } else { None };
    let row = sqlx::query!(
        r#"INSERT INTO notifications (account_id, from_account_id, "type", activity_type, activity_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, now(), now())
           RETURNING id"#,
        recipient_id,
        from_account_id,
        notification_type,
        activity_type_val,
        status_id,
    )
    .fetch_one(&db)
    .await;

    let notification_id = match row {
        Ok(r) => r.id,
        Err(e) => {
            tracing::warn!(error = %e, "failed to create notification");
            return;
        }
    };

    // Publish to the streaming API synchronously — it's just an in-process broadcast.
    if let Some(payload) = build_notification_payload(state, notification_id, notification_type, from_account_id, status_id).await {
        state.streaming.publish(crate::streaming::Event::Notification {
            for_account_id: recipient_id,
            payload: std::sync::Arc::new(payload),
        });
    }

    let state_clone = state.clone();
    let icon_s = icon;
    let title_s = title;
    let body_s = body;
    tokio::spawn(async move {
        deliver(
            state_clone,
            recipient_id,
            notification_id,
            notification_type,
            &icon_s,
            &title_s,
            &body_s,
        )
        .await;
    });
}

/// Send an admin.sign_up or admin.report notification to all admins/moderators
/// on the instance. Bypasses block and policy filters — admin notifications
/// are always delivered.
pub async fn notify_admins(
    state: &AppState,
    from_account_id: i64,
    notification_type: &'static str,
    report_id: Option<i64>,
) {
    let admins: Vec<i64> = match sqlx::query_scalar!(
        r#"SELECT a.id AS "id!: i64" FROM accounts a
           JOIN users u ON u.account_id = a.id
           LEFT JOIN user_roles ur ON ur.id = u.role_id
           WHERE a.domain IS NULL AND COALESCE(ur.position, 0) >= 100"#,
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch admins for notification");
            return;
        }
    };

    for admin_id in admins {
        if admin_id == from_account_id {
            continue;
        }

        let row = if let Some(rid) = report_id {
            sqlx::query_scalar!(
                r#"INSERT INTO notifications (account_id, from_account_id, "type", activity_id, activity_type, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, 'Report', now(), now())
                   RETURNING id"#,
                admin_id, from_account_id, notification_type, rid,
            )
            .fetch_one(&state.db)
            .await
        } else {
            sqlx::query_scalar!(
                r#"INSERT INTO notifications (account_id, from_account_id, "type", created_at, updated_at)
                   VALUES ($1, $2, $3, now(), now())
                   RETURNING id"#,
                admin_id, from_account_id, notification_type,
            )
            .fetch_one(&state.db)
            .await
        };

        let notification_id = match row {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(error = %e, "failed to create admin notification");
                continue;
            }
        };

        if let Some(payload) = build_admin_notification_payload(
            state, notification_id, notification_type, from_account_id, report_id,
        ).await {
            state.streaming.publish(crate::streaming::Event::Notification {
                for_account_id: admin_id,
                payload: std::sync::Arc::new(payload),
            });
        }
    }
}

async fn build_admin_notification_payload(
    state: &AppState,
    notification_id: i64,
    notification_type: &str,
    from_account_id: i64,
    report_id: Option<i64>,
) -> Option<String> {
    use crate::api::mastodon::convert::account_from_db;
    use crate::api::mastodon::accounts::fetch_account_emojis;

    let created_at = sqlx::query_scalar!(
        "SELECT created_at FROM notifications WHERE id = $1",
        notification_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let from_account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        from_account_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let report_json = if let Some(rid) = report_id {
        sqlx::query!(
            r#"SELECT r.id, r.comment, r.forwarded, r.action_taken_at, r.created_at,
                      r.status_ids, a.id AS ta_id, a.username AS ta_username,
                      CASE r.category WHEN 0 THEN 'other' WHEN 1 THEN 'spam' WHEN 2 THEN 'violation' ELSE 'other' END AS "category!"
               FROM reports r
               JOIN accounts a ON a.id = r.target_account_id
               WHERE r.id = $1"#,
            rid,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| {
            serde_json::json!({
                "id": r.id.to_string(),
                "action_taken": r.action_taken_at.is_some(),
                "action_taken_at": r.action_taken_at.map(|t| t.and_utc().to_rfc3339()),
                "category": r.category,
                "comment": r.comment,
                "forwarded": r.forwarded,
                "created_at": r.created_at.and_utc().to_rfc3339(),
                "status_ids": r.status_ids.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                "rule_ids": [],
                "target_account": {
                    "id": r.ta_id.to_string(),
                    "username": r.ta_username,
                },
            })
        })
    } else {
        None
    };

    let mut from_api = account_from_db(&from_account);
    from_api.emojis = fetch_account_emojis(state, &from_account).await;
    let payload = serde_json::json!({
        "id": notification_id.to_string(),
        "type": notification_type,
        "created_at": created_at.and_utc().to_rfc3339(),
        "group_key": format!("ungrouped-{}", notification_id),
        "account": serde_json::to_value(from_api).ok(),
        "report": report_json,
        "filtered": null,
    });

    serde_json::to_string(&payload).ok()
}

/// Returns true if the notification should be routed to notification_requests
/// instead of the main notifications feed, based on the recipient's policy.
async fn should_filter_notification(
    db: &sqlx::PgPool,
    recipient_id: i64,
    from_account_id: i64,
    notification_type: &str,
    status_id: Option<i64>,
) -> bool {
    // Only filter certain notification types (not polls or admin actions)
    if !matches!(notification_type, "follow" | "follow_request" | "mention" | "favourite" | "reblog") {
        return false;
    }

    let policy = sqlx::query!(
        r#"SELECT for_not_following, for_not_followers,
                  for_new_accounts, for_private_mentions, for_limited_accounts
           FROM notification_policies WHERE account_id = $1"#,
        recipient_id,
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(policy) = policy else {
        return false; // No policy row = no filtering
    };

    if policy.for_not_following == 0
        && policy.for_not_followers == 0
        && policy.for_new_accounts == 0
        && policy.for_private_mentions == 0
        && policy.for_limited_accounts == 0
    {
        return false; // All filters off
    }

    // filter_not_following: sender is not followed by recipient
    if policy.for_not_following != 0 {
        let follows = sqlx::query_scalar!(
            "SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2",
            recipient_id, from_account_id,
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
        if !follows {
            return true;
        }
    }

    // filter_not_followers: sender does not follow recipient
    if policy.for_not_followers != 0 {
        let is_follower = sqlx::query_scalar!(
            "SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2",
            from_account_id, recipient_id,
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
        if !is_follower {
            return true;
        }
    }

    // filter_new_accounts: sender account is less than 30 days old
    if policy.for_new_accounts != 0 {
        let is_new = sqlx::query_scalar!(
            "SELECT 1 FROM accounts WHERE id = $1 AND created_at > now() - interval '30 days'",
            from_account_id,
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
        if is_new {
            return true;
        }
    }

    // filter_private_mentions: unsolicited DM (direct mention, not a reply to own status)
    if policy.for_private_mentions != 0 && notification_type == "mention" {
        if let Some(sid) = status_id {
            let is_private_unsolicited = sqlx::query_scalar!(
                r#"SELECT 1 FROM statuses s
                   WHERE s.id = $1
                     AND s.visibility = 3 /* vis::DIRECT */
                     AND (s.in_reply_to_id IS NULL OR NOT EXISTS (
                       SELECT 1 FROM statuses parent
                       WHERE parent.id = s.in_reply_to_id
                         AND parent.account_id = $2
                     ))"#,
                sid, recipient_id,
            )
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .is_some();
            if is_private_unsolicited {
                return true;
            }
        }
    }

    // filter_limited_accounts: sender is silenced/limited on this instance
    if policy.for_limited_accounts != 0 {
        let is_limited = sqlx::query_scalar!(
            "SELECT 1 FROM accounts WHERE id = $1 AND silenced_at IS NOT NULL",
            from_account_id,
        )
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some();
        if is_limited {
            return true;
        }
    }

    false
}

/// Upsert into notification_requests for a filtered notification.
async fn route_to_request(
    db: &sqlx::PgPool,
    recipient_id: i64,
    from_account_id: i64,
    status_id: Option<i64>,
) {
    let _ = sqlx::query!(
        r#"INSERT INTO notification_requests
               (account_id, from_account_id, last_status_id, notifications_count, created_at, updated_at)
           VALUES ($1, $2, $3, 1, now(), now())
           ON CONFLICT (account_id, from_account_id) DO UPDATE
             SET notifications_count = notification_requests.notifications_count + 1,
                 last_status_id = COALESCE($3, notification_requests.last_status_id),
                 updated_at = now()"#,
        recipient_id,
        from_account_id,
        status_id,
    )
    .execute(db)
    .await;
}

async fn build_notification_payload(
    state: &AppState,
    notification_id: i64,
    notification_type: &str,
    from_account_id: i64,
    status_id: Option<i64>,
) -> Option<String> {
    use crate::api::mastodon::convert::account_from_db;
    use crate::api::mastodon::accounts::{build_status, fetch_account_emojis, fetch_reblog_data, fetch_status_media};

    let created_at = sqlx::query_scalar!(
        "SELECT created_at FROM notifications WHERE id = $1",
        notification_id
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let from_account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        from_account_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    let mut api_account = account_from_db(&from_account);
    api_account.emojis = fetch_account_emojis(state, &from_account).await;

    let status_json = if let Some(sid) = status_id {
        let s = sqlx::query_as!(
            crate::db::models::Status,
            "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
            sid,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        if let Some(s) = s {
            let saccount = sqlx::query_as!(
                crate::db::models::Account,
                "SELECT * FROM accounts WHERE id = $1",
                s.account_id,
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()?;

            let media = fetch_status_media(state, s.id).await.unwrap_or_default();
            let reblog = fetch_reblog_data(state, &s).await.unwrap_or(None);
            build_status(state, &s, &saccount, media, reblog, None)
                .await
                .ok()
                .and_then(|st| serde_json::to_value(st).ok())
        } else {
            None
        }
    } else {
        None
    };

    let payload = serde_json::json!({
        "id": notification_id.to_string(),
        "type": notification_type,
        "created_at": created_at.and_utc().to_rfc3339(),
        "group_key": format!("ungrouped-{}", notification_id),
        "account": serde_json::to_value(api_account).ok(),
        "status": status_json,
        "filtered": null,
    });

    serde_json::to_string(&payload).ok()
}
