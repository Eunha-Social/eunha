//! Mutes and blocks: mute/unmute (with optional notification muting and
//! duration), block/unblock (with follow teardown + federation), and the
//! `/blocks` and `/mutes` list endpoints.

use super::*;

// ── POST /api/v1/accounts/:id/mute ────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct MuteParams {
    /// Whether to also mute notifications from this account (default true).
    pub notifications: Option<bool>,
    /// Mute duration in seconds; 0 or absent means indefinite.
    pub duration: Option<i64>,
}

pub async fn mute_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<MuteParams>>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:mutes")?;
    // Mastodon MuteService: muting yourself is a no-op.
    if auth.account_id == target_id {
        return build_relationship(&state, auth.account_id, target_id)
            .await
            .map(Json);
    }
    let params = body.map(|Json(p)| p).unwrap_or_default();
    let hide_notifications = params.notifications.unwrap_or(true);
    let expires_at: Option<chrono::NaiveDateTime> = params
        .duration
        .filter(|&d| d > 0)
        .map(|d| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(d));

    sqlx::query!(
        r#"INSERT INTO mutes (account_id, target_account_id, hide_notifications, expires_at, created_at, updated_at)
           VALUES ($1, $2, $3, $4, now(), now())
           ON CONFLICT (account_id, target_account_id)
           DO UPDATE SET hide_notifications = EXCLUDED.hide_notifications,
                         expires_at = EXCLUDED.expires_at"#,
        auth.account_id, target_id, hide_notifications, expires_at,
    )
    .execute(&state.db)
    .await?;

    build_relationship(&state, auth.account_id, target_id)
        .await
        .map(Json)
}

// ── POST /api/v1/accounts/:id/unmute ──────────────────────────────────────

pub async fn unmute_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:mutes")?;
    sqlx::query!(
        "DELETE FROM mutes WHERE account_id = $1 AND target_account_id = $2",
        auth.account_id,
        target_id
    )
    .execute(&state.db)
    .await?;

    build_relationship(&state, auth.account_id, target_id)
        .await
        .map(Json)
}

// ── POST /api/v1/accounts/:id/block ───────────────────────────────────────

pub async fn block_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:blocks")?;
    // Mastodon BlockService: blocking yourself is a no-op.
    if auth.account_id == target_id {
        return build_relationship(&state, auth.account_id, target_id)
            .await
            .map(Json);
    }
    sqlx::query!(
        r#"INSERT INTO blocks (account_id, target_account_id, created_at, updated_at) VALUES ($1, $2, now(), now())
           ON CONFLICT (account_id, target_account_id) DO NOTHING"#,
        auth.account_id, target_id
    )
    .execute(&state.db)
    .await?;

    // Remove accepted follows in both directions and update counts. Capture the
    // direction + Follow activity uri so we can federate the termination.
    let deleted = sqlx::query!(
        "DELETE FROM follows WHERE (account_id = $1 AND target_account_id = $2) OR (account_id = $2 AND target_account_id = $1) RETURNING account_id, target_account_id, uri",
        auth.account_id, target_id
    )
    .fetch_all(&state.db)
    .await?;
    for row in &deleted {
        let _ =
            crate::counters::on_follow_removed(&state.db, row.account_id, row.target_account_id)
                .await;
    }
    // Also delete any pending follow requests in both directions, keeping uris.
    let deleted_requests = sqlx::query!(
        "DELETE FROM follow_requests WHERE (account_id = $1 AND target_account_id = $2) OR (account_id = $2 AND target_account_id = $1) RETURNING account_id, uri",
        auth.account_id, target_id
    )
    .fetch_all(&state.db)
    .await?;
    // Mirror Mastodon's FollowRequest dependent: :destroy — clear follow_request
    // notifications in both directions between blocker and blocked.
    sqlx::query!(
        "DELETE FROM notifications WHERE type = 'follow_request' AND ((account_id = $1 AND from_account_id = $2) OR (account_id = $2 AND from_account_id = $1))",
        auth.account_id, target_id,
    )
    .execute(&state.db)
    .await?;

    // Strip the blocked account's posts from the blocker's home feed
    // (Mastodon BlockWorker → FeedManager#clear_from_home).
    {
        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let blocker_id = auth.account_id;
        if feed::sync_fanout() {
            feed::unmerge_from_home(&mut redis, &db, target_id, blocker_id).await;
        } else {
            tokio::spawn(async move {
                feed::unmerge_from_home(&mut redis, &db, target_id, blocker_id).await;
            });
        }
    }

    // Federate to a remote target (Mastodon BlockService#handle_following_relationships
    // + the Block itself): Undo(Follow) for our follow, Reject(Follow) for their
    // follow / pending request.
    if let Some(target) = sqlx::query!(
        "SELECT uri, inbox_url, shared_inbox_url, domain FROM accounts WHERE id = $1",
        target_id,
    )
    .fetch_optional(&state.db)
    .await?
    {
        if target.domain.is_some() {
            if let Some(actor_row) = sqlx::query!(
                "SELECT username, private_key, id_scheme FROM accounts WHERE id = $1 AND domain IS NULL",
                auth.account_id,
            ).fetch_optional(&state.db).await? {
                if actor_row.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                    let domain = state.instance.domain.clone();
                    let actor_url = crate::federation::tag::account_uri(&domain, auth.account_id, actor_row.id_scheme, &actor_row.username);
                    let key_id = format!("{}#main-key", actor_url);
                    let inbox = if !target.shared_inbox_url.is_empty() { target.shared_inbox_url.clone() } else { target.inbox_url.clone() };

                    let activity_id = || format!("https://{}/activities/{}", domain, crate::snowflake::next_id());
                    let mut activities: Vec<serde_json::Value> = Vec::new();

                    for f in &deleted {
                        let Some(uri) = f.uri.clone().filter(|s| !s.is_empty()) else { continue };
                        if f.account_id == auth.account_id {
                            // Our follow of the remote target -> Undo(Follow).
                            if let Ok(a) = crate::federation::activity::undo_follow(
                                &activity_id(), &actor_url, &uri, &actor_url, &target.uri,
                            ) {
                                activities.push(a);
                            }
                        } else {
                            // The remote target's follow of us -> Reject(Follow).
                            if let Ok(a) = crate::federation::activity::reject_follow(
                                &activity_id(), &actor_url, &uri, &target.uri, &actor_url,
                            ) {
                                activities.push(a);
                            }
                        }
                    }
                    for r in &deleted_requests {
                        // The remote target's pending request to us -> Reject(Follow).
                        if r.account_id == target_id {
                            if let Some(uri) = r.uri.clone().filter(|s| !s.is_empty()) {
                                if let Ok(a) = crate::federation::activity::reject_follow(
                                    &activity_id(), &actor_url, &uri, &target.uri, &actor_url,
                                ) {
                                    activities.push(a);
                                }
                            }
                        }
                    }

                    // The Block activity itself.
                    let block_id = format!("https://{}/users/{}/blocks/{}", domain, actor_row.username, target_id);
                    if let Ok(b) = crate::federation::activity::block(&block_id, &actor_url, &target.uri) {
                        activities.push(b);
                    }

                    if !inbox.is_empty() {
                        for act in activities {
                            if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                                &state, act, vec![inbox.clone()], key_id.clone(),
                            ).await {
                                tracing::warn!(error = %e, "failed to enqueue block-related activity");
                            }
                        }
                    }
                }
            }
        }
    }

    build_relationship(&state, auth.account_id, target_id)
        .await
        .map(Json)
}

// ── POST /api/v1/accounts/:id/unblock ─────────────────────────────────────

pub async fn unblock_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:blocks")?;
    // Mastodon UnblockService: a no-op (and no Undo) when not actually blocking.
    let was_blocking = sqlx::query!(
        "DELETE FROM blocks WHERE account_id = $1 AND target_account_id = $2 RETURNING account_id",
        auth.account_id,
        target_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if !was_blocking {
        return build_relationship(&state, auth.account_id, target_id)
            .await
            .map(Json);
    }

    // Send Undo(Block) activity to remote target
    if let Some(target) = sqlx::query!(
        "SELECT uri, inbox_url, shared_inbox_url, domain FROM accounts WHERE id = $1",
        target_id,
    )
    .fetch_optional(&state.db)
    .await?
    {
        if target.domain.is_some() {
            if let Some(actor_row) = sqlx::query!(
                "SELECT username, private_key, id_scheme FROM accounts WHERE id = $1 AND domain IS NULL",
                auth.account_id,
            ).fetch_optional(&state.db).await? {
                if actor_row.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                    let domain = state.instance.domain.clone();
                    let actor_url = crate::federation::tag::account_uri(&domain, auth.account_id, actor_row.id_scheme, &actor_row.username);
                    let block_id = format!("https://{}/users/{}/blocks/{}", domain, actor_row.username, target_id);
                    let target_uri = target.uri.clone();
                    let undo_id = format!("{}#undo", block_id);
                    let undo = crate::federation::activity::undo_block(
                        &undo_id, &actor_url, &block_id, &target_uri,
                    )?;
                    let key_id = format!("{}#main-key", actor_url);
                    let inbox = if !target.shared_inbox_url.is_empty() { target.shared_inbox_url } else { target.inbox_url };
                    if !inbox.is_empty() {
                        if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                            &state,
                            undo,
                            vec![inbox],
                            key_id,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to enqueue Undo(Block)");
                        }
                    }
                }
            }
        }
    }

    build_relationship(&state, auth.account_id, target_id)
        .await
        .map(Json)
}

// ── GET /api/v1/blocks ────────────────────────────────────────────────────

pub async fn get_blocks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<PaginationParams>,
) -> AppResult<impl IntoResponse> {
    auth.require_scope("read:blocks")?;
    let limit = q.limit_clamped(40, 80);
    let max_id = q.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    // Paginate by block.id (matching Mastodon's Block.paginate_by_max_id)
    let rows = sqlx::query!(
        r#"SELECT b.id AS block_id, b.target_account_id FROM blocks b
           JOIN accounts a ON a.id = b.target_account_id AND a.suspended_at IS NULL
           WHERE b.account_id = $1
             AND ($2::bigint IS NULL OR b.id < $2)
             AND ($3::bigint IS NULL OR b.id > $3)
             AND ($5::bigint IS NULL OR b.id > $5)
           ORDER BY b.id DESC LIMIT $4"#,
        auth.account_id,
        max_id,
        since_id,
        limit,
        min_id,
    )
    .fetch_all(&state.db)
    .await?;

    let first_block_id = rows.first().map(|r| r.block_id.to_string());
    let last_block_id = rows.last().map(|r| r.block_id.to_string());
    let target_ids: Vec<i64> = rows.iter().map(|r| r.target_account_id).collect();

    let accounts = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &target_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let account_map: std::collections::HashMap<i64, Account> =
        accounts.into_iter().map(|a| (a.id, a)).collect();
    let accounts_ordered: Vec<Account> = target_ids
        .iter()
        .filter_map(|id| account_map.get(id).cloned())
        .collect();

    let api_accounts = batch_accounts_to_api(&state, &accounts_ordered).await;
    let bounds = first_block_id.zip(last_block_id);
    let resp_headers = crate::api::mastodon::link_headers(
        &req_headers,
        &uri,
        bounds.as_ref().map(|(n, o)| (n.as_str(), o.as_str())),
    );
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/mutes ─────────────────────────────────────────────────────

pub async fn get_mutes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<PaginationParams>,
) -> AppResult<impl IntoResponse> {
    auth.require_scope("read:mutes")?;
    let limit = q.limit_clamped(40, 80);
    let max_id = q.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    // Paginate by mute.id (matching Mastodon's Mute.paginate_by_max_id)
    let rows = sqlx::query!(
        r#"SELECT m.id AS mute_id, m.target_account_id, m.expires_at FROM mutes m
           JOIN accounts a ON a.id = m.target_account_id AND a.suspended_at IS NULL
           WHERE m.account_id = $1
             AND (m.expires_at IS NULL OR m.expires_at > now())
             AND ($2::bigint IS NULL OR m.id < $2)
             AND ($3::bigint IS NULL OR m.id > $3)
             AND ($5::bigint IS NULL OR m.id > $5)
           ORDER BY m.id DESC LIMIT $4"#,
        auth.account_id,
        max_id,
        since_id,
        limit,
        min_id,
    )
    .fetch_all(&state.db)
    .await?;

    let first_mute_id = rows.first().map(|r| r.mute_id.to_string());
    let last_mute_id = rows.last().map(|r| r.mute_id.to_string());

    let mute_expiries: std::collections::HashMap<i64, Option<chrono::NaiveDateTime>> = rows
        .iter()
        .map(|r| (r.target_account_id, r.expires_at))
        .collect();
    let target_ids: Vec<i64> = rows.iter().map(|r| r.target_account_id).collect();

    let accounts = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &target_ids,
    )
    .fetch_all(&state.db)
    .await?;
    // Restore mute-ordered sequence
    let account_map: std::collections::HashMap<i64, Account> =
        accounts.into_iter().map(|a| (a.id, a)).collect();
    let accounts_ordered: Vec<Account> = target_ids
        .iter()
        .filter_map(|id| account_map.get(id).cloned())
        .collect();

    let mute_emojis_map = batch_account_emojis(&state, &accounts_ordered).await;
    let mute_roles_map = batch_account_roles(&state, &accounts_ordered).await;
    let api_accounts: Vec<ApiAccount> = accounts_ordered
        .iter()
        .map(|a| {
            let mut api = account_from_db(a);
            api.emojis = mute_emojis_map.get(&a.id).cloned().unwrap_or_default();
            api.roles = mute_roles_map.get(&a.id).cloned().unwrap_or_default();
            if let Some(expires_at) = mute_expiries.get(&a.id).and_then(|e| *e) {
                api.mute_expires_at = Some(crate::api::mastodon::convert::mastodon_date(expires_at));
            }
            api
        })
        .collect();
    let bounds = first_mute_id.zip(last_mute_id);
    let resp_headers = crate::api::mastodon::link_headers(
        &req_headers,
        &uri,
        bounds.as_ref().map(|(n, o)| (n.as_str(), o.as_str())),
    );
    Ok((resp_headers, Json(api_accounts)))
}
