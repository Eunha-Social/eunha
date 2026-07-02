use axum::{
    extract::{Extension, Multipart, Path, Query, RawQuery, State},
    http::{header, HeaderMap, Uri},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    db::models::Account,
    error::{AppError, AppResult},
    feed,
    middleware::{AuthenticatedUser, ResolvedInstance},
    push,
    state::AppState,
};
use super::{
    convert::{account_from_db, status_from_db},
    types::{Account as ApiAccount, PaginationParams, Preferences, Relationship, SuggestionV2},
};

// ── GET /api/v1/accounts/verify_credentials ────────────────────────────────

pub async fn verify_credentials(
    State(state): State<AppState>,
    Extension(ResolvedInstance(_instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<ApiAccount>> {
    auth.require_scope("read:accounts")?;
    let account = fetch_account(&state, auth.account_id).await?;
    let mut api_account = account_from_db(&account);
    api_account.emojis = fetch_account_emojis(&state, &account).await;
    apply_account_stats(&state, &mut api_account, account.id).await;

    let d = user_defaults(&state, account.id).await;
    let (default_privacy, default_sensitive, default_language, default_quote_policy) =
        (d.privacy, d.sensitive, d.language, d.quote_policy);

    let follow_requests: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM follow_requests WHERE target_account_id = $1 LIMIT 40) sub",
        account.id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    api_account.source = Some(super::types::AccountSource {
        privacy: default_privacy,
        sensitive: default_sensitive,
        language: default_language,
        note: account.note.clone(),
        fields: super::convert::fields_from_db(account.fields.as_ref().unwrap_or(&serde_json::json!([]))),
        follow_requests_count: follow_requests,
        discoverable: account.discoverable,
        indexable: account.indexable,
        hide_collections: account.hide_collections,
        attribution_domains: account.attribution_domains.clone().unwrap_or_default(),
        quote_policy: default_quote_policy,
    });

    api_account.roles = fetch_account_roles(&state, account.id).await;
    api_account.role = fetch_account_role(&state, account.id).await;

    Ok(Json(api_account))
}

/// Fetch highlighted roles for a local account.  Returns an empty vec for
/// remote accounts (they have no row in `users`).
async fn fetch_account_roles(state: &AppState, account_id: i64) -> Vec<super::types::AccountRole> {
    let row = sqlx::query!(
        r#"SELECT ur.id, ur.name, ur.color
           FROM users u
           JOIN user_roles ur ON ur.id = u.role_id
           WHERE u.account_id = $1"#,
        account_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    match row {
        Some(r) => vec![super::types::AccountRole {
            id: r.id.to_string(),
            name: r.name,
            color: r.color,
        }],
        None => vec![],
    }
}

/// Fetch the single current role for a local account (used in CredentialAccount).
pub async fn fetch_account_role(state: &AppState, account_id: i64) -> Option<super::types::Role> {
    let row = sqlx::query!(
        r#"SELECT ur.id, ur.name, ur.color, ur.permissions, ur.highlighted
           FROM users u
           JOIN user_roles ur ON ur.id = u.role_id
           WHERE u.account_id = $1"#,
        account_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    Some(super::types::Role {
        id: row.id.to_string(),
        name: row.name,
        color: row.color,
        permissions: row.permissions.to_string(),
        highlighted: row.highlighted,
    })
}

// ── GET /api/v1/accounts/lookup ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LookupQuery {
    pub acct: String,
    pub resolve: Option<bool>,
}

pub async fn lookup_account(
    State(state): State<AppState>,
    Query(q): Query<LookupQuery>,
) -> AppResult<Json<ApiAccount>> {
    // acct can be "username" (local) or "username@domain" (remote)
    let (username, domain) = match q.acct.split_once('@') {
        Some((user, domain)) => (user.to_lowercase(), Some(domain.to_lowercase())),
        None => (q.acct.to_lowercase(), None),
    };

    let found = match domain {
        None => sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE lower(username) = $1 AND domain IS NULL",
            username,
        )
        .fetch_optional(&state.db)
        .await?,

        Some(ref d) => sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE lower(username) = $1 AND lower(domain) = $2",
            username,
            d,
        )
        .fetch_optional(&state.db)
        .await?,
    };

    if let Some(account) = found {
        let mut api = account_from_db(&account);
        api.emojis = fetch_account_emojis(&state, &account).await;
        api.roles = fetch_account_roles(&state, account.id).await;
        apply_account_stats(&state, &mut api, account.id).await;
        return Ok(Json(api));
    }

    // Not found locally — attempt WebFinger resolution if requested and domain is known
    if q.resolve.unwrap_or(false) {
        if let Some(ref d) = domain {
            let acct_uri = format!("acct:{}@{}", username, d);
            let wf_url = format!("https://{}/.well-known/webfinger?resource={}", d, acct_uri);
            if let Ok(resp) = state.fetch.get(&wf_url)
                .header("Accept", "application/jrd+json, application/json")
                .send()
                .await
            {
                if let Ok(jrd) = resp.json::<serde_json::Value>().await {
                    let actor_uri = jrd
                        .get("links")
                        .and_then(|l| l.as_array())
                        .and_then(|links| {
                            links.iter().find(|l| {
                                l.get("rel").and_then(|r| r.as_str()) == Some("self")
                                    && l.get("type").and_then(|t| t.as_str())
                                        .map(|t| t.contains("activity+json") || t.contains("ld+json"))
                                        .unwrap_or(false)
                            })
                        })
                        .and_then(|l| l.get("href"))
                        .and_then(|h| h.as_str())
                        .map(str::to_owned);

                    if let Some(uri) = actor_uri {
                        let account_id = crate::api::ap::inbox::resolve_or_fetch_remote_account(
                            &state, &uri,
                        ).await?;
                        let account = sqlx::query_as!(
                            Account,
                            "SELECT * FROM accounts WHERE id = $1",
                            account_id,
                        )
                        .fetch_one(&state.db)
                        .await?;
                        let mut api = account_from_db(&account);
                        api.emojis = fetch_account_emojis(&state, &account).await;
                        api.roles = fetch_account_roles(&state, account.id).await;
                        return Ok(Json(api));
                    }
                }
            }
        }
    }

    Err(AppError::NotFound)
}

// ── GET /api/v1/accounts/:id ───────────────────────────────────────────────

pub async fn get_account(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ApiAccount>> {
    let account = fetch_account(&state, id).await?;
    // Local accounts that are unconfirmed or pending approval are invisible (404).
    if account.domain.is_none() {
        let approval_required = state.instance.approval_required;
        let ok = sqlx::query_scalar!(
            r#"SELECT u.confirmed_at IS NOT NULL
                 AND (u.approved OR NOT $2) AS "ok!"
               FROM users u
               WHERE u.account_id = $1"#,
            account.id,
            approval_required,
        )
        .fetch_optional(&state.db)
        .await?;
        match ok {
            None | Some(false) => return Err(AppError::NotFound),
            _ => {}
        }
    }
    let mut api_account = account_from_db(&account);
    api_account.emojis = fetch_account_emojis(&state, &account).await;
    api_account.roles = fetch_account_roles(&state, account.id).await;
    apply_account_stats(&state, &mut api_account, account.id).await;
    if let Some(moved_account_id) = account.moved_to_account_id {
        if let Ok(Some(moved)) = sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE id = $1 LIMIT 1",
            moved_account_id,
        )
        .fetch_optional(&state.db)
        .await {
            let mut moved_api = account_from_db(&moved);
            moved_api.emojis = fetch_account_emojis(&state, &moved).await;
            moved_api.roles = fetch_account_roles(&state, moved.id).await;
            apply_account_stats(&state, &mut moved_api, moved.id).await;
            api_account.moved = Some(Box::new(moved_api));
        }
    }
    Ok(Json(api_account))
}

// ── GET /api/v1/accounts/:id/statuses ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct StatusesQuery {
    #[serde(flatten)]
    pub pagination: PaginationParams,
    pub only_media: Option<bool>,
    pub exclude_replies: Option<bool>,
    pub exclude_reblogs: Option<bool>,
    pub exclude_direct: Option<bool>,
    pub pinned: Option<bool>,
    pub tagged: Option<String>,
}

pub async fn get_account_statuses(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<StatusesQuery>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<impl IntoResponse> {
    let account = fetch_account(&state, id).await?;
    if account.suspended_at.is_some() {
        return Ok((HeaderMap::new(), Json(Vec::<super::types::Status>::new())));
    }
    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);

    // If the target has blocked the viewer, deny access (Mastodon returns 403).
    if let Some(vid) = viewer_id {
        if vid != account.id {
            let blocked = sqlx::query_scalar!(
                "SELECT 1 FROM blocks WHERE account_id = $1 AND target_account_id = $2",
                account.id, vid,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if blocked {
                return Err(AppError::Forbidden);
            }
        }
    }

    let is_self = viewer_id == Some(account.id);
    let is_follower = if !is_self {
        if let Some(vid) = viewer_id {
            sqlx::query_scalar!(
                "SELECT EXISTS(SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2)",
                vid, account.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };

    if q.pinned == Some(true) {
        let pinned_statuses = sqlx::query_as!(
            crate::db::models::Status,
            r#"SELECT s.* FROM statuses s
               JOIN status_pins sp ON sp.status_id = s.id
               WHERE sp.account_id = $1 AND s.deleted_at IS NULL
                 AND (
                   s.visibility IN (0, 1)
                   OR ($2::boolean = true)
                   OR ($3::boolean = true AND s.visibility = 2)
                 )
               ORDER BY sp.id DESC"#,
            account.id,
            is_self,
            is_follower,
        )
        .fetch_all(&state.db)
        .await?;
        let pin_ids: Vec<i64> = pinned_statuses.iter()
            .map(|s| s.reblog_of_id.unwrap_or(s.id))
            .collect();
        let pin_ctxs = if let Some(vid) = viewer_id {
            super::statuses::batch_viewer_contexts(&state, vid, &pin_ids).await?
        } else {
            std::collections::HashMap::new()
        };
        let pin_status_ids: Vec<i64> = pinned_statuses.iter().map(|s| s.id).collect();
        let pin_media_map = batch_status_media(&state, &pin_status_ids).await?;
        let pin_reblog_map = batch_reblog_data(&state, &pinned_statuses).await?;
        let pin_quote_map = batch_quote_data(&state, &pinned_statuses, viewer_id).await?;
        let pin_reblog_ids: Vec<i64> = pin_reblog_map.values().map(|(rs, _, _)| rs.id).collect();
        let mut pin_enrich_ids = pin_status_ids.clone();
        pin_enrich_ids.extend_from_slice(&pin_reblog_ids);
        let pin_tags_map = batch_statuses_tags(&state, &pin_enrich_ids).await?;
        let pin_mentions_map = batch_status_mentions(&state, &pin_enrich_ids).await?;
        let all_pin_statuses: Vec<crate::db::models::Status> = pinned_statuses.iter().cloned()
            .chain(pin_reblog_map.values().map(|(rs, _, _)| rs.clone()))
            .collect();
        let pin_emojis_map = batch_status_emojis(&state, &all_pin_statuses).await?;
        let pin_polls_map = batch_status_polls(&state, &pin_enrich_ids, viewer_id).await?;
        let pin_cards_map = batch_status_cards(&state, &pin_enrich_ids).await?;
        let pin_all_accounts_for_emoji: Vec<crate::db::models::Account> = {
            let mut seen = std::collections::HashSet::new();
            std::iter::once(&account)
                .chain(pin_reblog_map.values().map(|(_, ra, _)| ra))
                .filter(|a| seen.insert(a.id))
                .cloned()
                .collect()
        };
        let pin_account_emojis_map = batch_account_emojis(&state, &pin_all_accounts_for_emoji).await;
        let pin_account_roles_map = batch_account_roles(&state, &pin_all_accounts_for_emoji).await;
        let mut result = Vec::with_capacity(pinned_statuses.len());
        for s in &pinned_statuses {
            let media = pin_media_map.get(&s.id).cloned().unwrap_or_default();
            let reblog = pin_reblog_map.get(&s.id).cloned();
            let effective_id = s.reblog_of_id.unwrap_or(s.id);
            let ctx = pin_ctxs.get(&effective_id).cloned();
            let mentions = pin_mentions_map.get(&s.id).cloned().unwrap_or_default();
            let rb_mentions = reblog.as_ref()
                .and_then(|(rs, _, _)| pin_mentions_map.get(&rs.id))
                .cloned()
                .unwrap_or_default();
            let mut api_status = status_from_db(s, &account, media, reblog, ctx, &mentions, &rb_mentions);
            api_status.account.emojis = pin_account_emojis_map.get(&account.id).cloned().unwrap_or_default();
            api_status.account.roles = pin_account_roles_map.get(&account.id).cloned().unwrap_or_default();
            api_status.tags = pin_tags_map.get(&s.id).cloned().unwrap_or_default();
            api_status.mentions = mentions;
            api_status.emojis = pin_emojis_map.get(&s.id).cloned().unwrap_or_default();
            api_status.poll = pin_polls_map.get(&s.id).cloned();
            api_status.card = pin_cards_map.get(&s.id).cloned();
            api_status.quote = pin_quote_map.get(&s.id).cloned();
            if let Some(ref mut rb) = api_status.reblog {
                let rid: i64 = rb.id.parse().unwrap_or(0);
                let rb_id: i64 = rb.account.id.parse().unwrap_or(0);
                rb.account.emojis = pin_account_emojis_map.get(&rb_id).cloned().unwrap_or_default();
                rb.account.roles = pin_account_roles_map.get(&rb_id).cloned().unwrap_or_default();
                rb.tags = pin_tags_map.get(&rid).cloned().unwrap_or_default();
                rb.mentions = rb_mentions;
                rb.emojis = pin_emojis_map.get(&rid).cloned().unwrap_or_default();
                rb.poll = pin_polls_map.get(&rid).cloned();
                rb.card = pin_cards_map.get(&rid).cloned();
            }
            api_status.pinned = Some(true);
            result.push(api_status);
        }
        return Ok((HeaderMap::new(), Json(result)));
    }

    let limit = q.pagination.limit_clamped(20, 40);
    let max_id = q.pagination.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.pagination.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.pagination.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    let tagged_lower = q.tagged.as_deref().map(|t| t.to_lowercase());
    let exclude_direct = q.exclude_direct.unwrap_or(false);
    let statuses = if min_id.is_some() {
        sqlx::query_as!(
            crate::db::models::Status,
            r#"SELECT statuses.* FROM statuses
               WHERE account_id = $1
                 AND deleted_at IS NULL
                 AND ($2::bigint IS NULL OR id > $2)
                 AND ($3::boolean IS NOT TRUE OR reblog_of_id IS NULL)
                 AND ($4::boolean IS NOT TRUE OR in_reply_to_id IS NULL OR in_reply_to_account_id = $1)
                 AND (
                   visibility IN (0, 1)
                   OR ($5::boolean = true)
                   OR ($6::boolean = true AND visibility = 2)
                   OR (
                     NOT $10::boolean
                     AND $11::bigint IS NOT NULL
                     AND visibility = 3
                     AND EXISTS (SELECT 1 FROM mentions WHERE status_id = statuses.id AND account_id = $11)
                   )
                 )
                 AND (
                   text != ''
                   OR reblog_of_id IS NOT NULL
                   OR poll_id IS NOT NULL
                   OR EXISTS (SELECT 1 FROM media_attachments WHERE status_id = statuses.id)
                 )
                 AND ($8::boolean IS NOT TRUE OR
                   EXISTS (SELECT 1 FROM media_attachments WHERE status_id = statuses.id)
                 )
                 AND ($9::text IS NULL OR EXISTS (
                   SELECT 1 FROM statuses_tags st
                   JOIN tags t ON t.id = st.tag_id
                   WHERE st.status_id = statuses.id AND t.name = $9
                 ))
                 AND ($11::bigint IS NULL OR reblog_of_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM blocks b
                   JOIN statuses orig ON orig.id = statuses.reblog_of_id
                   WHERE b.account_id = $11 AND b.target_account_id = orig.account_id
                 ))
                 AND ($11::bigint IS NULL OR reblog_of_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM account_domain_blocks adb
                   JOIN statuses orig ON orig.id = statuses.reblog_of_id
                   JOIN accounts orig_a ON orig_a.id = orig.account_id
                   WHERE adb.account_id = $11 AND adb.domain = orig_a.domain
                 ))
               ORDER BY id ASC
               LIMIT $7"#,
            account.id,
            min_id,
            q.exclude_reblogs.unwrap_or(false),
            q.exclude_replies.unwrap_or(false),
            is_self,
            is_follower,
            limit,
            q.only_media.unwrap_or(false),
            tagged_lower,
            exclude_direct,
            viewer_id,
        )
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as!(
            crate::db::models::Status,
            r#"SELECT statuses.* FROM statuses
               WHERE account_id = $1
                 AND deleted_at IS NULL
                 AND ($2::bigint IS NULL OR id < $2)
                 AND ($3::bigint IS NULL OR id > $3)
                 AND ($4::boolean IS NOT TRUE OR reblog_of_id IS NULL)
                 AND ($5::boolean IS NOT TRUE OR in_reply_to_id IS NULL OR in_reply_to_account_id = $1)
                 AND (
                   visibility IN (0, 1)
                   OR ($6::boolean = true)
                   OR ($7::boolean = true AND visibility = 2)
                   OR (
                     NOT $11::boolean
                     AND $12::bigint IS NOT NULL
                     AND visibility = 3
                     AND EXISTS (SELECT 1 FROM mentions WHERE status_id = statuses.id AND account_id = $12)
                   )
                 )
                 AND (
                   text != ''
                   OR reblog_of_id IS NOT NULL
                   OR poll_id IS NOT NULL
                   OR EXISTS (SELECT 1 FROM media_attachments WHERE status_id = statuses.id)
                 )
                 AND ($9::boolean IS NOT TRUE OR
                   EXISTS (SELECT 1 FROM media_attachments WHERE status_id = statuses.id)
                 )
                 AND ($10::text IS NULL OR EXISTS (
                   SELECT 1 FROM statuses_tags st
                   JOIN tags t ON t.id = st.tag_id
                   WHERE st.status_id = statuses.id AND t.name = $10
                 ))
                 AND ($12::bigint IS NULL OR reblog_of_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM blocks b
                   JOIN statuses orig ON orig.id = statuses.reblog_of_id
                   WHERE b.account_id = $12 AND b.target_account_id = orig.account_id
                 ))
                 AND ($12::bigint IS NULL OR reblog_of_id IS NULL OR NOT EXISTS (
                   SELECT 1 FROM account_domain_blocks adb
                   JOIN statuses orig ON orig.id = statuses.reblog_of_id
                   JOIN accounts orig_a ON orig_a.id = orig.account_id
                   WHERE adb.account_id = $12 AND adb.domain = orig_a.domain
                 ))
               ORDER BY id DESC
               LIMIT $8"#,
            account.id,
            max_id,
            since_id,
            q.exclude_reblogs.unwrap_or(false),
            q.exclude_replies.unwrap_or(false),
            is_self,
            is_follower,
            limit,
            q.only_media.unwrap_or(false),
            tagged_lower,
            exclude_direct,
            viewer_id,
        )
        .fetch_all(&state.db)
        .await?
    };

    let filter_map = if let Some(vid) = viewer_id {
        super::timelines::compute_filter_results(&state, vid, &statuses, "account").await
    } else {
        std::collections::HashMap::new()
    };
    let statuses: Vec<crate::db::models::Status> = statuses.into_iter()
        .filter(|s| !filter_map.get(&s.id).is_some_and(|(hide, _)| *hide))
        .collect();

    let effective_ids: Vec<i64> = statuses.iter()
        .map(|s| s.reblog_of_id.unwrap_or(s.id))
        .collect();
    let ctxs = if let Some(vid) = viewer_id {
        super::statuses::batch_viewer_contexts(&state, vid, &effective_ids).await?
    } else {
        std::collections::HashMap::new()
    };

    let all_status_ids: Vec<i64> = statuses.iter().map(|s| s.id).collect();
    let media_map = batch_status_media(&state, &all_status_ids).await?;
    let reblog_map = batch_reblog_data(&state, &statuses).await?;
    let quote_map = batch_quote_data(&state, &statuses, viewer_id).await?;
    let reblog_ids: Vec<i64> = reblog_map.values().map(|(rs, _, _)| rs.id).collect();
    let mut enrich_ids = all_status_ids.clone();
    enrich_ids.extend_from_slice(&reblog_ids);
    let tags_map = batch_statuses_tags(&state, &enrich_ids).await?;
    let mentions_map = batch_status_mentions(&state, &enrich_ids).await?;
    let all_statuses_for_emoji: Vec<crate::db::models::Status> = statuses.iter().cloned()
        .chain(reblog_map.values().map(|(rs, _, _)| rs.clone()))
        .collect();
    let emojis_map = batch_status_emojis(&state, &all_statuses_for_emoji).await?;
    let polls_map = batch_status_polls(&state, &enrich_ids, viewer_id).await?;
    let cards_map = batch_status_cards(&state, &enrich_ids).await?;

    let all_accounts_for_emoji: Vec<crate::db::models::Account> = {
        let mut seen = std::collections::HashSet::new();
        std::iter::once(&account)
            .chain(reblog_map.values().map(|(_, ra, _)| ra))
            .filter(|a| seen.insert(a.id))
            .cloned()
            .collect()
    };
    let account_emojis_map = batch_account_emojis(&state, &all_accounts_for_emoji).await;
    let statuses_roles_map = batch_account_roles(&state, &all_accounts_for_emoji).await;

    let mut result = Vec::with_capacity(statuses.len());
    for s in &statuses {
        let media = media_map.get(&s.id).cloned().unwrap_or_default();
        let reblog = reblog_map.get(&s.id).cloned();
        let effective_id = s.reblog_of_id.unwrap_or(s.id);
        let ctx = ctxs.get(&effective_id).cloned();
        let mentions = mentions_map.get(&s.id).cloned().unwrap_or_default();
        let rb_mentions = reblog.as_ref()
            .and_then(|(rs, _, _)| mentions_map.get(&rs.id))
            .cloned()
            .unwrap_or_default();
        let mut api = status_from_db(s, &account, media, reblog, ctx, &mentions, &rb_mentions);
        api.account.emojis = account_emojis_map.get(&account.id).cloned().unwrap_or_default();
        api.account.roles = statuses_roles_map.get(&account.id).cloned().unwrap_or_default();
        api.tags = tags_map.get(&s.id).cloned().unwrap_or_default();
        api.mentions = mentions;
        api.emojis = emojis_map.get(&s.id).cloned().unwrap_or_default();
        api.poll = polls_map.get(&s.id).cloned();
        api.card = cards_map.get(&s.id).cloned();
        api.quote = quote_map.get(&s.id).cloned();
        if let Some(ref mut rb) = api.reblog {
            let rid: i64 = rb.id.parse().unwrap_or(0);
            let rb_id: i64 = rb.account.id.parse().unwrap_or(0);
            rb.account.emojis = account_emojis_map.get(&rb_id).cloned().unwrap_or_default();
            rb.account.roles = statuses_roles_map.get(&rb_id).cloned().unwrap_or_default();
            rb.tags = tags_map.get(&rid).cloned().unwrap_or_default();
            rb.mentions = rb_mentions;
            rb.emojis = emojis_map.get(&rid).cloned().unwrap_or_default();
            rb.poll = polls_map.get(&rid).cloned();
            rb.card = cards_map.get(&rid).cloned();
        }
        if let Some((_, ref filter_json)) = filter_map.get(&s.id) {
            if let Some(arr) = filter_json.as_array() {
                if !arr.is_empty() {
                    api.filtered = Some(arr.clone());
                }
            }
        }
        result.push(api);
    }

    let link = result.first().zip(result.last()).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest.id, &oldest.id)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(result)))
}

// ── GET /api/v1/accounts/relationships ────────────────────────────────────

pub async fn get_relationships(
    State(state): State<AppState>,
    RawQuery(qs): RawQuery,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Vec<Relationship>>> {
    auth.require_scope("read:follows")?;
    // serde_urlencoded treats id[]=v1&id[]=v2 as a duplicate field → 400.
    // Parse with form_urlencoded which correctly returns each pair separately.
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(
            qs.as_deref().unwrap_or("").as_bytes()
        )
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    let with_suspended = pairs.iter().any(|(k, v)| k == "with_suspended" && (v == "true" || v == "1"));

    let mut ids: Vec<i64> = pairs.iter()
        .filter(|(k, _)| k == "id[]" || k == "id")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    // Without with_suspended, filter out suspended accounts (matches Mastodon default)
    if !with_suspended {
        let non_suspended: Vec<i64> = sqlx::query_scalar!(
            "SELECT id FROM accounts WHERE id = ANY($1::bigint[]) AND suspended_at IS NULL",
            &ids,
        )
        .fetch_all(&state.db)
        .await?;
        let allowed: std::collections::HashSet<i64> = non_suspended.into_iter().collect();
        ids.retain(|id| allowed.contains(id));
    }

    if ids.is_empty() {
        return Ok(Json(vec![]));
    }
    let results = batch_build_relationships(&state, auth.account_id, &ids).await?;
    Ok(Json(results))
}

// ── POST /api/v1/accounts/:id/follow ──────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct FollowParams {
    pub reblogs: Option<bool>,
    pub notify: Option<bool>,
    pub languages: Option<Vec<String>>,
}

pub async fn follow_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<FollowParams>>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:follows")?;
    if auth.account_id == target_id {
        return Err(AppError::Forbidden);
    }
    let params = body.map(|Json(p)| p).unwrap_or_default();
    let show_reblogs = params.reblogs.unwrap_or(true);
    let notify = params.notify.unwrap_or(false);
    let languages: Vec<String> = params.languages.unwrap_or_default();

    let target = fetch_account(&state, target_id).await?;

    // Mastodon FollowService gating (#following_not_possible? / #following_not_allowed?):
    // an unavailable target is 404; blocked/blocking, domain-blocked, and moved
    // targets are not allowed (403).
    if target.suspended_at.is_some() {
        return Err(AppError::NotFound);
    }
    if target.moved_to_account_id.is_some() {
        return Err(AppError::Forbidden);
    }
    let blocked_either = sqlx::query_scalar!(
        r#"SELECT 1 FROM blocks
           WHERE (account_id = $1 AND target_account_id = $2)
              OR (account_id = $2 AND target_account_id = $1)
           LIMIT 1"#,
        auth.account_id, target_id,
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if blocked_either {
        return Err(AppError::Forbidden);
    }
    if let Some(ref dom) = target.domain {
        // Instance-level domain block, or the requester's own account-level block.
        let domain_blocked = sqlx::query_scalar!(
            r#"SELECT 1 FROM domain_blocks WHERE domain = $1
               UNION ALL
               SELECT 1 FROM account_domain_blocks WHERE account_id = $2 AND domain = $1
               LIMIT 1"#,
            dom,
            auth.account_id,
        )
        .fetch_optional(&state.db)
        .await?
        .is_some();
        if domain_blocked {
            return Err(AppError::Forbidden);
        }
    }

    // Check if accepted follow already exists — update settings only
    let existing = sqlx::query!(
        "SELECT 1 as exists FROM follows WHERE account_id = $1 AND target_account_id = $2",
        auth.account_id, target_id,
    )
    .fetch_optional(&state.db)
    .await?;

    if existing.is_some() {
        sqlx::query!(
            "UPDATE follows SET show_reblogs = $3, notify = $4, languages = $5
             WHERE account_id = $1 AND target_account_id = $2",
            auth.account_id, target_id, show_reblogs, notify, &languages,
        )
        .execute(&state.db)
        .await?;
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }

    // Check if a pending follow request already exists
    let pending = sqlx::query!(
        "SELECT 1 as exists FROM follow_requests WHERE account_id = $1 AND target_account_id = $2",
        auth.account_id, target_id,
    )
    .fetch_optional(&state.db)
    .await?;

    if pending.is_some() {
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }

    let requester = fetch_account(&state, auth.account_id).await?;

    // Mastodon FollowLimitValidator: cap new follows/requests. Free up to LIMIT,
    // then max(round(followers * RATIO), LIMIT).
    {
        const FOLLOW_LIMIT: i64 = 7_500;
        const FOLLOW_RATIO: f64 = 1.1;
        let stats = sqlx::query!(
            "SELECT following_count, followers_count FROM account_stats WHERE account_id = $1",
            auth.account_id,
        )
        .fetch_optional(&state.db)
        .await?;
        let following = stats.as_ref().map(|s| s.following_count).unwrap_or(0);
        let followers = stats.as_ref().map(|s| s.followers_count).unwrap_or(0);
        let limit = if following < FOLLOW_LIMIT {
            FOLLOW_LIMIT
        } else {
            ((followers as f64 * FOLLOW_RATIO).round() as i64).max(FOLLOW_LIMIT)
        };
        if following >= limit {
            return Err(AppError::Unprocessable(format!(
                "Validation failed: You are trying to follow too many people (limit: {limit})"
            )));
        }
    }

    // Remote account: always use follow_requests and send a Follow activity.
    if target.domain.is_some() {
        let follow_uri = format!(
            "https://{}/users/{}/follows/{}",
            state.instance.domain,
            requester.username,
            crate::snowflake::next_id()
        );
        sqlx::query!(
            r#"INSERT INTO follow_requests (account_id, target_account_id, show_reblogs, notify, languages, uri, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, now(), now())
               ON CONFLICT (account_id, target_account_id) DO UPDATE SET uri = EXCLUDED.uri"#,
            auth.account_id,
            target_id,
            show_reblogs,
            notify,
            &languages,
            follow_uri,
        )
        .execute(&state.db)
        .await?;

        let has_signing_key = requester.private_key.as_deref().is_some_and(|s| !s.is_empty());
        if !has_signing_key {
            tracing::warn!(username = %requester.username, "local account has no private key; cannot deliver Follow");
        }
        if has_signing_key {
            let actor_url = crate::federation::tag::account_uri_of(&state.instance.domain, &requester);
            let key_id = format!("{}#main-key", actor_url);
            let follow_activity =
                crate::federation::activity::follow(&follow_uri, &actor_url, &target.uri)?;
            let inbox = if !target.shared_inbox_url.is_empty() {
                target.shared_inbox_url.clone()
            } else {
                target.inbox_url.clone()
            };
            let target_uri = target.uri.clone();
            let inbox = if inbox.is_empty() {
                tracing::warn!(target_uri, "inbox URL missing; re-fetching actor profile");
                match crate::api::ap::inbox::resolve_or_fetch_remote_account(&state, &target_uri).await {
                    Err(e) => {
                        tracing::warn!(target_uri, error = %e, "failed to re-fetch actor; dropping Follow");
                        None
                    }
                    Ok(_) => {
                        sqlx::query!(
                            r#"SELECT CASE WHEN shared_inbox_url <> '' THEN shared_inbox_url ELSE inbox_url END AS inbox
                               FROM accounts WHERE uri = $1"#,
                            target_uri,
                        )
                        .fetch_optional(&state.db)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|r| r.inbox)
                        .filter(|s| !s.is_empty())
                    }
                }
            } else {
                Some(inbox)
            };
            if let Some(inbox) = inbox {
                if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                    &state,
                    follow_activity,
                    vec![inbox],
                    key_id,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to enqueue Follow");
                }
            } else {
                tracing::warn!(target_uri, "still no inbox URL after re-fetch; dropping Follow");
            }
        }

        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }

    // Locked target, or a silenced requester, goes through a follow request
    // (Mastodon FollowService: target.locked? || source.silenced?).
    if target.locked || requester.silenced_at.is_some() {
        sqlx::query!(
            r#"INSERT INTO follow_requests (account_id, target_account_id, show_reblogs, notify, languages, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, now(), now())"#,
            auth.account_id, target_id, show_reblogs, notify, &languages,
        )
        .execute(&state.db)
        .await?;
        push::create_and_push(
            &state, target_id, auth.account_id, "follow_request", None,
            format!("{} wants to follow you", requester.display_name),
            requester.acct().clone(), super::convert::account_avatar_url_for(&requester),
        ).await;
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }

    sqlx::query!(
        r#"INSERT INTO follows (account_id, target_account_id, show_reblogs, notify, languages, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, now(), now())"#,
        auth.account_id, target_id, show_reblogs, notify, &languages,
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        "INSERT INTO account_stats (account_id, followers_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET followers_count = account_stats.followers_count + 1, updated_at = now()",
        target_id
    )
    .execute(&state.db)
    .await?;
    sqlx::query!(
        "INSERT INTO account_stats (account_id, following_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET following_count = account_stats.following_count + 1, updated_at = now()",
        auth.account_id
    )
    .execute(&state.db)
    .await?;

    push::create_and_push(
        &state,
        target_id,
        auth.account_id,
        "follow",
        None,
        format!("{} followed you", requester.display_name),
        requester.acct().clone(),
        super::convert::account_avatar_url_for(&requester),
    ).await;

    let mut redis = state.redis.clone();
    let db = state.db.clone();
    let follower_id = auth.account_id;
    if feed::sync_fanout() {
        feed::backfill_follow(&mut redis, &db, follower_id, target_id).await;
    } else {
        tokio::spawn(async move {
            feed::backfill_follow(&mut redis, &db, follower_id, target_id).await;
        });
    }

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
}

// ── POST /api/v1/accounts/:id/unfollow ────────────────────────────────────

pub async fn unfollow_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:follows")?;

    let deleted = sqlx::query!(
        "DELETE FROM follows WHERE account_id = $1 AND target_account_id = $2 RETURNING uri",
        auth.account_id,
        target_id,
    )
    .fetch_optional(&state.db)
    .await?;

    let follow_uri_opt: Option<String> = if let Some(ref d) = deleted {
        sqlx::query!(
            "UPDATE account_stats SET followers_count = GREATEST(followers_count - 1, 0), updated_at = now() WHERE account_id = $1",
            target_id
        )
        .execute(&state.db)
        .await?;
        sqlx::query!(
            "UPDATE account_stats SET following_count = GREATEST(following_count - 1, 0), updated_at = now() WHERE account_id = $1",
            auth.account_id
        )
        .execute(&state.db)
        .await?;
        d.uri.clone()
    } else {
        // Canceling a pending request: keep its uri so the Undo(Follow)
        // references the original Follow activity (matches Mastodon).
        sqlx::query!(
            "DELETE FROM follow_requests WHERE account_id = $1 AND target_account_id = $2 RETURNING uri",
            auth.account_id,
            target_id,
        )
        .fetch_optional(&state.db)
        .await?
        .and_then(|r| r.uri)
    };

    // Strip the ex-followee's posts from the home feed (Mastodon UnfollowService
    // → FeedManager#unmerge_from_home). Only when an accepted follow was removed;
    // a cancelled request never fanned anything out.
    if deleted.is_some() {
        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let follower_id = auth.account_id;
        if feed::sync_fanout() {
            feed::unmerge_from_home(&mut redis, &db, target_id, follower_id).await;
        } else {
            tokio::spawn(async move {
                feed::unmerge_from_home(&mut redis, &db, target_id, follower_id).await;
            });
        }
    }

    // Send Undo(Follow) to remote target
    let target = fetch_account(&state, target_id).await?;
    if target.domain.is_some() {
        let requester = fetch_account(&state, auth.account_id).await?;
        if requester.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
            let actor_url = crate::federation::tag::account_uri_of(&state.instance.domain, &requester);
            let key_id = format!("{}#main-key", actor_url);
            let follow_uri = follow_uri_opt
                .clone()
                .unwrap_or_else(|| actor_url.clone());
            let undo_id = format!(
                "https://{}/activities/{}",
                state.instance.domain,
                crate::snowflake::next_id()
            );
            let undo = crate::federation::activity::undo_follow(
                &undo_id, &actor_url, &follow_uri, &actor_url, &target.uri,
            )?;
            let inbox = if !target.shared_inbox_url.is_empty() {
                target.shared_inbox_url.clone()
            } else {
                target.inbox_url.clone()
            };
            if !inbox.is_empty() {
                if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                    &state,
                    undo,
                    vec![inbox],
                    key_id,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to enqueue Undo(Follow)");
                }
            }
        }
    }

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
}

// ── GET /api/v1/accounts/:id/followers ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct FollowersQuery {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

// ── GET /api/v1/accounts/:id/pins ─────────────────────────────────────────

pub async fn get_account_pins(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<super::types::Status>>> {
    let account = fetch_account(&state, id).await?;
    if account.suspended_at.is_some() {
        return Ok(Json(vec![]));
    }
    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);

    // Block check
    if let Some(vid) = viewer_id {
        if vid != account.id {
            let blocked = sqlx::query_scalar!(
                "SELECT 1 FROM blocks WHERE account_id = $1 AND target_account_id = $2",
                account.id, vid,
            ).fetch_optional(&state.db).await?.is_some();
            if blocked {
                return Err(AppError::Forbidden);
            }
        }
    }

    let is_self = viewer_id == Some(account.id);
    let is_follower = if !is_self {
        if let Some(vid) = viewer_id {
            sqlx::query_scalar!(
                "SELECT EXISTS(SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2)",
                vid, account.id,
            ).fetch_one(&state.db).await?.unwrap_or(false)
        } else { false }
    } else { false };

    let pinned_statuses = sqlx::query_as!(
        crate::db::models::Status,
        r#"SELECT s.* FROM statuses s
           JOIN status_pins sp ON sp.status_id = s.id
           WHERE sp.account_id = $1 AND s.deleted_at IS NULL
             AND (
               s.visibility IN (0, 1)
               OR ($2::boolean = true)
               OR ($3::boolean = true AND s.visibility = 2)
             )
           ORDER BY sp.id DESC"#,
        account.id, is_self, is_follower,
    ).fetch_all(&state.db).await?;

    let pin_filter_map = if let Some(vid) = viewer_id {
        super::timelines::compute_filter_results(&state, vid, &pinned_statuses, "account").await
    } else {
        std::collections::HashMap::new()
    };
    let pinned_statuses: Vec<crate::db::models::Status> = pinned_statuses.into_iter()
        .filter(|s| !pin_filter_map.get(&s.id).is_some_and(|(hide, _)| *hide))
        .collect();

    let pin_status_ids: Vec<i64> = pinned_statuses.iter().map(|s| s.id).collect();
    let pin_ids: Vec<i64> = pinned_statuses.iter()
        .map(|s| s.reblog_of_id.unwrap_or(s.id))
        .collect();
    let pin_ctxs = if let Some(vid) = viewer_id {
        super::statuses::batch_viewer_contexts(&state, vid, &pin_ids).await?
    } else {
        std::collections::HashMap::new()
    };
    let pin_media_map = batch_status_media(&state, &pin_status_ids).await?;
    let pin_reblog_map = batch_reblog_data(&state, &pinned_statuses).await?;
    let pin_quote_map = batch_quote_data(&state, &pinned_statuses, viewer_id).await?;
    let pin_reblog_ids: Vec<i64> = pin_reblog_map.values().map(|(rs, _, _)| rs.id).collect();
    let mut pin_enrich_ids = pin_status_ids.clone();
    pin_enrich_ids.extend_from_slice(&pin_reblog_ids);
    let pin_tags_map = batch_statuses_tags(&state, &pin_enrich_ids).await?;
    let pin_mentions_map = batch_status_mentions(&state, &pin_enrich_ids).await?;
    let all_pin_statuses: Vec<crate::db::models::Status> = pinned_statuses.iter().cloned()
        .chain(pin_reblog_map.values().map(|(rs, _, _)| rs.clone()))
        .collect();
    let pin_emojis_map = batch_status_emojis(&state, &all_pin_statuses).await?;
    let pin_polls_map = batch_status_polls(&state, &pin_enrich_ids, viewer_id).await?;
    let pin_cards_map = batch_status_cards(&state, &pin_enrich_ids).await?;
    let pin_all_accounts_for_emoji: Vec<crate::db::models::Account> = {
        let mut seen = std::collections::HashSet::new();
        std::iter::once(&account)
            .chain(pin_reblog_map.values().map(|(_, ra, _)| ra))
            .filter(|a| seen.insert(a.id))
            .cloned()
            .collect()
    };
    let pin_account_emojis_map = batch_account_emojis(&state, &pin_all_accounts_for_emoji).await;
    let pin_account_roles_map = batch_account_roles(&state, &pin_all_accounts_for_emoji).await;

    let mut result = Vec::with_capacity(pinned_statuses.len());
    for s in &pinned_statuses {
        let media = pin_media_map.get(&s.id).cloned().unwrap_or_default();
        let reblog = pin_reblog_map.get(&s.id).cloned();
        let effective_id = s.reblog_of_id.unwrap_or(s.id);
        let ctx = pin_ctxs.get(&effective_id).cloned();
        let mentions = pin_mentions_map.get(&s.id).cloned().unwrap_or_default();
        let rb_mentions = reblog.as_ref()
            .and_then(|(rs, _, _)| pin_mentions_map.get(&rs.id))
            .cloned()
            .unwrap_or_default();
        let mut api_status = status_from_db(s, &account, media, reblog, ctx, &mentions, &rb_mentions);
        api_status.account.emojis = pin_account_emojis_map.get(&account.id).cloned().unwrap_or_default();
        api_status.account.roles = pin_account_roles_map.get(&account.id).cloned().unwrap_or_default();
        api_status.tags = pin_tags_map.get(&s.id).cloned().unwrap_or_default();
        api_status.mentions = mentions;
        api_status.emojis = pin_emojis_map.get(&s.id).cloned().unwrap_or_default();
        api_status.poll = pin_polls_map.get(&s.id).cloned();
        api_status.card = pin_cards_map.get(&s.id).cloned();
        api_status.quote = pin_quote_map.get(&s.id).cloned();
        if let Some(ref mut rb) = api_status.reblog {
            let rid: i64 = rb.id.parse().unwrap_or(0);
            let rb_id: i64 = rb.account.id.parse().unwrap_or(0);
            rb.account.emojis = pin_account_emojis_map.get(&rb_id).cloned().unwrap_or_default();
            rb.account.roles = pin_account_roles_map.get(&rb_id).cloned().unwrap_or_default();
            rb.tags = pin_tags_map.get(&rid).cloned().unwrap_or_default();
            rb.mentions = rb_mentions;
            rb.emojis = pin_emojis_map.get(&rid).cloned().unwrap_or_default();
            rb.poll = pin_polls_map.get(&rid).cloned();
            rb.card = pin_cards_map.get(&rid).cloned();
        }
        if let Some((_, ref filter_json)) = pin_filter_map.get(&s.id) {
            if let Some(arr) = filter_json.as_array() {
                if !arr.is_empty() {
                    api_status.filtered = Some(arr.clone());
                }
            }
        }
        api_status.pinned = Some(true);
        result.push(api_status);
    }
    Ok(Json(result))
}

pub async fn get_account_followers(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<FollowersQuery>,
    viewer: Option<Extension<AuthenticatedUser>>,
) -> AppResult<impl IntoResponse> {
    let target = fetch_account(&state, id).await?;
    if target.suspended_at.is_some() {
        return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
    }
    let viewer_id = viewer.map(|Extension(a)| a.account_id);
    // Respect hide_collections unless the viewer is the account owner
    if target.hide_collections.unwrap_or(false) && viewer_id != Some(id) {
        return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
    }
    // If target has blocked the viewer, return empty list
    if let Some(vid) = viewer_id {
        if vid != id {
            let blocked = sqlx::query_scalar!(
                "SELECT 1 FROM blocks WHERE account_id = $1 AND target_account_id = $2",
                id, vid,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if blocked {
                return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
            }
        }
    }

    let limit = q.pagination.limit_clamped(40, 80);
    let max_id = q.pagination.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.pagination.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.pagination.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    // Paginate by follow.id (matching Mastodon's Follow.paginate_by_max_id)
    let follow_rows = sqlx::query!(
        r#"SELECT f.id as follow_id, f.account_id FROM follows f
           JOIN accounts a ON a.id = f.account_id
           WHERE f.target_account_id = $1
             AND ($2::bigint IS NULL OR f.id < $2)
             AND ($3::bigint IS NULL OR f.id > $3)
             AND ($6::bigint IS NULL OR f.id > $6)
             AND a.suspended_at IS NULL
             AND ($4::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM blocks b
                 WHERE (b.account_id = $4 AND b.target_account_id = a.id)
                    OR (b.account_id = a.id AND b.target_account_id = $4)
             ))
             AND ($4::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mutes WHERE account_id = $4 AND target_account_id = a.id
             ))
           ORDER BY f.id DESC LIMIT $5"#,
        id, max_id, since_id, viewer_id, limit, min_id
    )
    .fetch_all(&state.db)
    .await?;

    let first_follow_id = follow_rows.first().map(|r| r.follow_id.to_string());
    let last_follow_id = follow_rows.last().map(|r| r.follow_id.to_string());
    let account_ids: Vec<i64> = follow_rows.iter().map(|r| r.account_id).collect();
    let account_map: std::collections::HashMap<i64, Account> = if account_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        sqlx::query_as!(Account, "SELECT * FROM accounts WHERE id = ANY($1::bigint[])", &account_ids)
            .fetch_all(&state.db).await?
            .into_iter().map(|a| (a.id, a)).collect()
    };
    // Preserve follow-id ordering
    let accounts: Vec<Account> = follow_rows.iter()
        .filter_map(|r| account_map.get(&r.account_id).cloned())
        .collect();

    let api_accounts = batch_accounts_to_api(&state, &accounts).await;
    let link = first_follow_id.zip(last_follow_id).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest, &oldest)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/accounts/:id/following ────────────────────────────────────

pub async fn get_account_following(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<FollowersQuery>,
    viewer: Option<Extension<AuthenticatedUser>>,
) -> AppResult<impl IntoResponse> {
    let target = fetch_account(&state, id).await?;
    if target.suspended_at.is_some() {
        return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
    }
    let viewer_id = viewer.map(|Extension(a)| a.account_id);
    // Respect hide_collections unless the viewer is the account owner
    if target.hide_collections.unwrap_or(false) && viewer_id != Some(id) {
        return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
    }
    // If target has blocked the viewer, return empty list
    if let Some(vid) = viewer_id {
        if vid != id {
            let blocked = sqlx::query_scalar!(
                "SELECT 1 FROM blocks WHERE account_id = $1 AND target_account_id = $2",
                id, vid,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if blocked {
                return Ok((HeaderMap::new(), Json(Vec::<ApiAccount>::new())));
            }
        }
    }

    let limit = q.pagination.limit_clamped(40, 80);
    let max_id = q.pagination.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.pagination.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.pagination.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    // Paginate by follow.id (matching Mastodon's Follow.paginate_by_max_id)
    let follow_rows = sqlx::query!(
        r#"SELECT f.id as follow_id, f.target_account_id FROM follows f
           JOIN accounts a ON a.id = f.target_account_id
           WHERE f.account_id = $1
             AND ($2::bigint IS NULL OR f.id < $2)
             AND ($3::bigint IS NULL OR f.id > $3)
             AND ($6::bigint IS NULL OR f.id > $6)
             AND a.suspended_at IS NULL
             AND ($4::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM blocks b
                 WHERE (b.account_id = $4 AND b.target_account_id = a.id)
                    OR (b.account_id = a.id AND b.target_account_id = $4)
             ))
             AND ($4::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mutes WHERE account_id = $4 AND target_account_id = a.id
             ))
           ORDER BY f.id DESC LIMIT $5"#,
        id, max_id, since_id, viewer_id, limit, min_id
    )
    .fetch_all(&state.db)
    .await?;

    let first_follow_id = follow_rows.first().map(|r| r.follow_id.to_string());
    let last_follow_id = follow_rows.last().map(|r| r.follow_id.to_string());
    let account_ids: Vec<i64> = follow_rows.iter().map(|r| r.target_account_id).collect();
    let account_map: std::collections::HashMap<i64, Account> = if account_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        sqlx::query_as!(Account, "SELECT * FROM accounts WHERE id = ANY($1::bigint[])", &account_ids)
            .fetch_all(&state.db).await?
            .into_iter().map(|a| (a.id, a)).collect()
    };
    // Preserve follow-id ordering
    let accounts: Vec<Account> = follow_rows.iter()
        .filter_map(|r| account_map.get(&r.target_account_id).cloned())
        .collect();

    let api_accounts = batch_accounts_to_api(&state, &accounts).await;
    let link = first_follow_id.zip(last_follow_id).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest, &oldest)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/accounts/search ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountSearchQuery {
    pub q: String,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub resolve: Option<bool>,
    pub following: Option<bool>,
}

pub async fn search_accounts(
    State(state): State<AppState>,
    Query(q): Query<AccountSearchQuery>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<ApiAccount>>> {
    let limit = q.limit.unwrap_or(40).clamp(1, 80);
    let offset = q.offset.unwrap_or(0).max(0);
    let pattern = format!("%{}%", q.q.to_lowercase());

    let mut accounts = if q.following.unwrap_or(false) {
        if let Some(Extension(ref auth)) = auth {
            sqlx::query_as!(
                Account,
                r#"SELECT a.* FROM accounts a
                   JOIN follows f ON f.target_account_id = a.id
                   WHERE f.account_id = $1
                     AND a.suspended_at IS NULL
                     AND (lower(a.username) LIKE $2 OR lower(a.display_name) LIKE $2)
                   ORDER BY a.username LIMIT $3 OFFSET $4"#,
                auth.account_id, pattern, limit, offset
            )
            .fetch_all(&state.db)
            .await?
        } else {
            vec![]
        }
    } else {
        sqlx::query_as!(
            Account,
            r#"SELECT * FROM accounts
               WHERE suspended_at IS NULL
                 AND (lower(username) LIKE $1 OR lower(display_name) LIKE $1)
               ORDER BY username LIMIT $2 OFFSET $3"#,
            pattern, limit, offset
        )
        .fetch_all(&state.db)
        .await?
    };

    // If resolve=true and the query looks like user@domain, try WebFinger for any
    // remote account not already in the local results.
    if q.resolve.unwrap_or(false) && accounts.is_empty() {
        if let Some((username, domain)) = q.q.split_once('@') {
            let username = username.to_lowercase();
            let domain = domain.to_lowercase();
            // Only attempt fetch if not already present locally
            let already_known = sqlx::query_scalar!(
                "SELECT id FROM accounts WHERE lower(username) = $1 AND lower(domain) = $2",
                username, domain,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();

            if !already_known {
                let acct_uri = format!("acct:{}@{}", username, domain);
                let wf_url = format!("https://{}/.well-known/webfinger?resource={}", domain, acct_uri);
                if let Ok(resp) = state.fetch.get(&wf_url)
                    .header("Accept", "application/jrd+json, application/json")
                    .send()
                    .await
                {
                    if let Ok(jrd) = resp.json::<serde_json::Value>().await {
                        let actor_uri = jrd
                            .get("links").and_then(|l| l.as_array())
                            .and_then(|links| links.iter().find(|l| {
                                l.get("rel").and_then(|r| r.as_str()) == Some("self")
                                    && l.get("type").and_then(|t| t.as_str())
                                        .map(|t| t.contains("activity+json") || t.contains("ld+json"))
                                        .unwrap_or(false)
                            }))
                            .and_then(|l| l.get("href"))
                            .and_then(|h| h.as_str())
                            .map(str::to_owned);

                        if let Some(uri) = actor_uri {
                            if let Ok(account_id) =
                                crate::api::ap::inbox::resolve_or_fetch_remote_account(&state, &uri).await
                            {
                                if let Ok(account) = sqlx::query_as!(
                                    Account,
                                    "SELECT * FROM accounts WHERE id = $1",
                                    account_id,
                                )
                                .fetch_one(&state.db)
                                .await
                                {
                                    accounts.push(account);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(Json(batch_accounts_to_api(&state, &accounts).await))
}

// ── PATCH /api/v1/accounts/update_credentials ─────────────────────────────

async fn do_update_credentials(
    state: &AppState,
    auth: &AuthenticatedUser,
    instance_domain: &str,
    mut multipart: Multipart,
) -> AppResult<Account> {
    let mut display_name: Option<String> = None;
    let mut note: Option<String> = None;
    let mut locked: Option<bool> = None;
    let mut bot: Option<bool> = None;
    let mut discoverable: Option<bool> = None;
    let mut avatar_url: Option<String> = None;
    let mut avatar_content_type: Option<String> = None;
    let mut header_url: Option<String> = None;
    let mut header_content_type: Option<String> = None;
    let mut source_privacy: Option<String> = None;
    let mut source_sensitive: Option<bool> = None;
    let mut source_language: Option<Option<String>> = None;
    let mut source_hide_collections: Option<bool> = None;
    let mut source_quote_policy: Option<String> = None;
    let mut indexable: Option<bool> = None;
    // fields_attributes[N][name] / fields_attributes[N][value]
    let mut fields_map: std::collections::BTreeMap<u32, (String, String)> = std::collections::BTreeMap::new();
    let mut fields_submitted = false;
    let mut attribution_domains: Option<Vec<String>> = None;

    while let Some(field) = multipart.next_field().await.map_err(|e| AppError::Unprocessable(e.to_string()))? {
        let name = field.name().unwrap_or("").to_string();
        // Parse attribution_domains[] array fields
        if name == "attribution_domains[]" {
            let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
            attribution_domains.get_or_insert_with(Vec::new).push(v);
            continue;
        }
        // Parse fields_attributes[N][name] and fields_attributes[N][value]
        if let Some(rest) = name.strip_prefix("fields_attributes[") {
            if let Some((idx_str, key)) = rest.split_once(']') {
                if let Ok(idx) = idx_str.parse::<u32>() {
                    let text = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                    fields_submitted = true;
                    let entry = fields_map.entry(idx).or_default();
                    match key {
                        "[name]" => entry.0 = text,
                        "[value]" => entry.1 = text,
                        _ => {}
                    }
                }
            }
            continue;
        }
        match name.as_str() {
            "display_name" => {
                display_name = Some(field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?);
            }
            "note" => {
                note = Some(field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?);
            }
            "locked" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                locked = Some(v == "true" || v == "1");
            }
            "bot" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                bot = Some(v == "true" || v == "1");
            }
            "discoverable" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                discoverable = Some(v == "true" || v == "1");
            }
            "source[privacy]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                if matches!(v.as_str(), "public" | "unlisted" | "private" | "direct") {
                    source_privacy = Some(v);
                }
            }
            "source[sensitive]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                source_sensitive = Some(v == "true" || v == "1");
            }
            "source[language]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                source_language = Some(if v.is_empty() { None } else { Some(v) });
            }
            "hide_collections" | "source[hide_collections]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                source_hide_collections = Some(v == "true" || v == "1");
            }
            "source[quote_policy]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                if matches!(v.as_str(), "public" | "followers" | "nobody") {
                    source_quote_policy = Some(v);
                }
            }
            "indexable" | "source[indexable]" => {
                let v = field.text().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                indexable = Some(v == "true" || v == "1");
            }
            "avatar" => {
                let ct = field.content_type().unwrap_or("application/octet-stream").to_string();
                let data = field.bytes().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                if !data.is_empty() {
                    let key = crate::media::account_avatar_key(auth.account_id, &ct);
                    state.storage.store(&data, &key, &ct).await?;
                    avatar_url = key.rsplit('/').next().map(str::to_string);
                    avatar_content_type = Some(ct);
                }
            }
            "header" => {
                let ct = field.content_type().unwrap_or("application/octet-stream").to_string();
                let data = field.bytes().await.map_err(|e| AppError::Unprocessable(e.to_string()))?;
                if !data.is_empty() {
                    let key = crate::media::account_header_key(auth.account_id, &ct);
                    state.storage.store(&data, &key, &ct).await?;
                    header_url = key.rsplit('/').next().map(str::to_string);
                    header_content_type = Some(ct);
                }
            }
            _ => {}
        }
    }

    // Persist posting preferences into users.settings (JSON).
    if source_privacy.is_some()
        || source_sensitive.is_some()
        || source_language.is_some()
        || source_quote_policy.is_some()
    {
        let mut settings = user_settings_json(state, auth.account_id).await;
        let obj = settings.as_object_mut().expect("settings json object");
        if let Some(p) = &source_privacy {
            obj.insert("privacy".into(), serde_json::json!(p));
        }
        if let Some(s) = source_sensitive {
            obj.insert("sensitive".into(), serde_json::json!(s));
        }
        if let Some(l) = &source_language {
            obj.insert("language".into(), serde_json::to_value(l).unwrap_or(serde_json::Value::Null));
        }
        if let Some(q) = &source_quote_policy {
            obj.insert("quote_policy".into(), serde_json::json!(q));
        }
        let s = settings.to_string();
        sqlx::query!(
            "UPDATE users SET settings = $1, updated_at = now() WHERE account_id = $2",
            s,
            auth.account_id,
        )
        .execute(&state.db)
        .await?;
    }

    if let Some(ref dn) = display_name {
        sqlx::query!("UPDATE accounts SET display_name = $1 WHERE id = $2", dn, auth.account_id)
            .execute(&state.db).await?;
    }
    if let Some(ref n) = note {
        let domain = instance_domain;
        let note_html = super::formatting::render_content(n, domain, &std::collections::HashMap::new());
        let note_html = if note_html.is_empty() { String::new() } else { note_html };
        sqlx::query!("UPDATE accounts SET note = $1 WHERE id = $2", note_html, auth.account_id)
            .execute(&state.db).await?;
    }
    if let Some(l) = locked {
        sqlx::query!("UPDATE accounts SET locked = $1 WHERE id = $2", l, auth.account_id)
            .execute(&state.db).await?;
        // Auto-approve pending follow requests when account becomes unlocked
        if !l {
            // Promote all pending follow requests to accepted follows
            let pending = sqlx::query!(
                "DELETE FROM follow_requests WHERE target_account_id = $1 RETURNING account_id",
                auth.account_id,
            )
            .fetch_all(&state.db)
            .await?;
            for row in &pending {
                let _ = sqlx::query!(
                    r#"INSERT INTO follows (account_id, target_account_id, created_at, updated_at)
                       VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING"#,
                    row.account_id, auth.account_id
                )
                .execute(&state.db)
                .await;
                let _ = sqlx::query!(
                    "INSERT INTO account_stats (account_id, followers_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET followers_count = account_stats.followers_count + 1, updated_at = now()",
                    auth.account_id
                )
                .execute(&state.db)
                .await;
                let _ = sqlx::query!(
                    "INSERT INTO account_stats (account_id, following_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET following_count = account_stats.following_count + 1, updated_at = now()",
                    row.account_id
                )
                .execute(&state.db)
                .await;
                crate::push::create_and_push(
                    state,
                    auth.account_id,
                    row.account_id,
                    "follow",
                    None,
                    "New follower".into(),
                    "".into(),
                    "".into(),
                )
                .await;
            }
        }
    }
    if let Some(b) = bot {
        let actor_type = if b { "Service" } else { "Person" };
        sqlx::query!("UPDATE accounts SET actor_type = $1 WHERE id = $2", actor_type, auth.account_id)
            .execute(&state.db).await?;
    }
    if let Some(d) = discoverable {
        sqlx::query!("UPDATE accounts SET discoverable = $1 WHERE id = $2", d, auth.account_id)
            .execute(&state.db).await?;
    }
    if let Some(ix) = indexable {
        sqlx::query!("UPDATE accounts SET indexable = $1 WHERE id = $2", ix, auth.account_id)
            .execute(&state.db).await?;
    }
    if let Some(ref filename) = avatar_url {
        sqlx::query!(
            "UPDATE accounts SET avatar_file_name = $1, avatar_content_type = $2, avatar_updated_at = now() WHERE id = $3",
            filename, avatar_content_type, auth.account_id
        )
        .execute(&state.db).await?;
    }
    if let Some(ref filename) = header_url {
        sqlx::query!(
            "UPDATE accounts SET header_file_name = $1, header_content_type = $2, header_updated_at = now() WHERE id = $3",
            filename, header_content_type, auth.account_id
        )
        .execute(&state.db).await?;
    }

    // Collect non-empty fields and save as JSONB
    if fields_submitted {
        let fields_json: serde_json::Value = fields_map
            .into_values()
            .filter(|(n, _)| !n.is_empty())
            .map(|(n, v)| serde_json::json!({"name": n, "value": v, "verified_at": null}))
            .collect();
        sqlx::query!(
            "UPDATE accounts SET fields = $1 WHERE id = $2",
            fields_json, auth.account_id
        )
        .execute(&state.db).await?;
    }

    // default_privacy, default_sensitive, default_language are stored in users.settings (YAML)
    // in Mastodon's schema; we don't persist them here.
    let _ = (&source_privacy, source_sensitive, &source_language);
    if let Some(hc) = source_hide_collections {
        sqlx::query!(
            "UPDATE accounts SET hide_collections = $1 WHERE id = $2",
            hc, auth.account_id
        )
        .execute(&state.db).await?;
    }
    if let Some(ref domains) = attribution_domains {
        sqlx::query!(
            "UPDATE accounts SET attribution_domains = $1 WHERE id = $2",
            domains, auth.account_id
        )
        .execute(&state.db).await?;
    }
    // default_quote_policy is in users.settings (YAML) in Mastodon's schema; not persisted here.
    let _ = &source_quote_policy;

    sqlx::query!("UPDATE accounts SET updated_at = now() WHERE id = $1", auth.account_id)
        .execute(&state.db)
        .await?;

    fetch_account(state, auth.account_id).await
}

async fn distribute_account_update(state: &AppState, domain: &str, account: &Account) {
    if account.private_key.as_deref().is_none_or(|s| s.is_empty()) {
        return;
    }
    if account.domain.is_some() {
        return;
    }
    let actor_url = crate::federation::tag::account_uri_of(domain, account);
    let Ok(actor) = crate::api::ap::objects::actor_json(state, domain, account).await else {
        return;
    };
    let update_id = format!("{}#updates/{}", actor_url, account.updated_at.and_utc().timestamp());
    let Ok(activity) = crate::federation::activity::update_actor(&update_id, &actor_url, actor) else {
        return;
    };
    let key_id = format!("{}#main-key", actor_url);
    if let Err(e) =
        crate::federation::delivery::fanout_to_followers(state, activity, account.id, key_id).await
    {
        tracing::warn!(error = %e, "failed to enqueue account Update fanout");
    }
}

pub async fn update_credentials(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(crate::middleware::ResolvedInstance(instance)): Extension<crate::middleware::ResolvedInstance>,
    multipart: Multipart,
) -> AppResult<Json<ApiAccount>> {
    auth.require_scope("write:accounts")?;
    let account = do_update_credentials(&state, &auth, &instance.domain, multipart).await?;
    distribute_account_update(&state, &instance.domain, &account).await;
    build_credential_account_response(&state, &auth, account).await
}

// ── PATCH /api/v1/profile (profile-specific update) ──────────────────────

pub async fn patch_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(crate::middleware::ResolvedInstance(instance)): Extension<crate::middleware::ResolvedInstance>,
    multipart: Multipart,
) -> AppResult<Json<super::types::Profile>> {
    auth.require_scope("write:accounts")?;
    let account = do_update_credentials(&state, &auth, &instance.domain, multipart).await?;
    distribute_account_update(&state, &instance.domain, &account).await;

    let domain = &instance.domain;
    let featured_tag_rows = sqlx::query!(
        r#"SELECT ft.id, t.name, ft.statuses_count, ft.last_status_at
           FROM featured_tags ft
           JOIN tags t ON t.id = ft.tag_id
           WHERE ft.account_id = $1
           ORDER BY ft.id"#,
        account.id,
    )
    .fetch_all(&state.db)
    .await?;

    let featured_tags = featured_tag_rows
        .into_iter()
        .map(|r| super::types::FeaturedTag {
            id: r.id.to_string(),
            name: r.name.clone(),
            url: format!("https://{}/@{}/tagged/{}", domain, account.username, r.name),
            statuses_count: r.statuses_count.to_string(),
            last_status_at: r.last_status_at.map(|t| t.format("%Y-%m-%d").to_string()),
        })
        .collect();

    let a = &account;
    let fields = super::convert::fields_from_db(a.fields.as_ref().unwrap_or(&serde_json::json!([])));
    let formatted_fields = fields.iter().map(|f| super::types::Field {
        name: f.name.clone(),
        value: super::formatting::format_field_value(&f.value),
        verified_at: f.verified_at.clone(),
    }).collect();
    Ok(Json(super::types::Profile {
        id: a.id.to_string(),
        username: a.username.clone(),
        display_name: a.display_name.clone(),
        note: a.note.clone(),
        fields,
        formatted_note: a.note.clone(),
        formatted_fields,
        avatar: Some(super::convert::account_avatar_url_for(a)),
        avatar_static: Some(super::convert::account_avatar_url_for(a)),
        header: Some(super::convert::account_header_url_for(a)),
        header_static: Some(super::convert::account_header_url_for(a)),
        locked: a.locked,
        bot: a.actor_type.as_deref() == Some("Service"),
        hide_collections: a.hide_collections,
        discoverable: a.discoverable,
        indexable: a.indexable,
        attribution_domains: a.attribution_domains.clone().unwrap_or_default(),
        featured_tags,
    }))
}

async fn build_credential_account_response(
    state: &AppState,
    auth: &AuthenticatedUser,
    account: Account,
) -> AppResult<Json<ApiAccount>> {
    let fields = super::convert::fields_from_db(account.fields.as_ref().unwrap_or(&serde_json::json!([])));
    let mut api_account = account_from_db(&account);
    api_account.emojis = fetch_account_emojis(state, &account).await;
    apply_account_stats(state, &mut api_account, account.id).await;
    let follow_requests_count: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM follow_requests WHERE target_account_id = $1 LIMIT 40) sub",
        auth.account_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    // Reflect the user's actual stored posting defaults (Mastodon's
    // CredentialAccountSerializer#source reads the user's settings), not
    // hardcoded values.
    let defaults = user_defaults(state, auth.account_id).await;

    api_account.source = Some(super::types::AccountSource {
        privacy: defaults.privacy,
        sensitive: defaults.sensitive,
        language: defaults.language,
        note: account.note.clone(),
        fields: fields.clone(),
        follow_requests_count,
        discoverable: account.discoverable,
        indexable: account.indexable,
        hide_collections: account.hide_collections,
        attribution_domains: account.attribution_domains.clone().unwrap_or_default(),
        quote_policy: defaults.quote_policy,
    });
    api_account.roles = fetch_account_roles(state, auth.account_id).await;
    api_account.role = fetch_account_role(state, auth.account_id).await;
    Ok(Json(api_account))
}

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
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }
    let params = body.map(|Json(p)| p).unwrap_or_default();
    let hide_notifications = params.notifications.unwrap_or(true);
    let expires_at: Option<chrono::NaiveDateTime> = params.duration
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

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
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
        auth.account_id, target_id
    )
    .execute(&state.db)
    .await?;

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
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
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
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
        let _ = sqlx::query!(
            "UPDATE account_stats SET following_count = GREATEST(following_count - 1, 0), updated_at = now() WHERE account_id = $1",
            row.account_id,
        ).execute(&state.db).await;
        let _ = sqlx::query!(
            "UPDATE account_stats SET followers_count = GREATEST(followers_count - 1, 0), updated_at = now() WHERE account_id = $1",
            row.target_account_id,
        ).execute(&state.db).await;
    }
    // Also delete any pending follow requests in both directions, keeping uris.
    let deleted_requests = sqlx::query!(
        "DELETE FROM follow_requests WHERE (account_id = $1 AND target_account_id = $2) OR (account_id = $2 AND target_account_id = $1) RETURNING account_id, uri",
        auth.account_id, target_id
    )
    .fetch_all(&state.db)
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
    ).fetch_optional(&state.db).await? {
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

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
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
        auth.account_id, target_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();
    if !was_blocking {
        return build_relationship(&state, auth.account_id, target_id).await.map(Json);
    }

    // Send Undo(Block) activity to remote target
    if let Some(target) = sqlx::query!(
        "SELECT uri, inbox_url, shared_inbox_url, domain FROM accounts WHERE id = $1",
        target_id,
    ).fetch_optional(&state.db).await? {
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

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
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
        auth.account_id, max_id, since_id, limit, min_id,
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
    let accounts_ordered: Vec<Account> = target_ids.iter()
        .filter_map(|id| account_map.get(id).cloned())
        .collect();

    let api_accounts = batch_accounts_to_api(&state, &accounts_ordered).await;
    let link = first_block_id.zip(last_block_id).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest, &oldest)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
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
        auth.account_id, max_id, since_id, limit, min_id,
    )
    .fetch_all(&state.db)
    .await?;

    let first_mute_id = rows.first().map(|r| r.mute_id.to_string());
    let last_mute_id = rows.last().map(|r| r.mute_id.to_string());

    let mute_expiries: std::collections::HashMap<i64, Option<chrono::NaiveDateTime>> =
        rows.iter().map(|r| (r.target_account_id, r.expires_at)).collect();
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
    let accounts_ordered: Vec<Account> = target_ids.iter()
        .filter_map(|id| account_map.get(id).cloned())
        .collect();

    let mute_emojis_map = batch_account_emojis(&state, &accounts_ordered).await;
    let mute_roles_map = batch_account_roles(&state, &accounts_ordered).await;
    let api_accounts: Vec<ApiAccount> = accounts_ordered.iter().map(|a| {
        let mut api = account_from_db(a);
        api.emojis = mute_emojis_map.get(&a.id).cloned().unwrap_or_default();
        api.roles = mute_roles_map.get(&a.id).cloned().unwrap_or_default();
        if let Some(expires_at) = mute_expiries.get(&a.id).and_then(|e| *e) {
            api.mute_expires_at = Some(super::convert::mastodon_date(expires_at));
        }
        api
    }).collect();
    let link = first_mute_id.zip(last_mute_id).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest, &oldest)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/preferences ───────────────────────────────────────────────

pub async fn get_preferences(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Preferences>> {
    auth.require_scope("read:accounts")?;
    let d = user_defaults(&state, auth.account_id).await;
    let (privacy, sensitive, language, quote_policy) =
        (d.privacy, d.sensitive, d.language, d.quote_policy);

    Ok(Json(Preferences {
        posting_default_visibility: privacy,
        posting_default_sensitive: sensitive,
        posting_default_language: language,
        posting_default_quote_policy: quote_policy,
        reading_expand_media: "default".into(),
        reading_expand_spoilers: false,
        reading_autoplay_gifs: false,
    }))
}

// ── GET /api/v1/follow_requests ───────────────────────────────────────────

pub async fn get_follow_requests(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<PaginationParams>,
) -> AppResult<impl IntoResponse> {
    auth.require_scope("read:follows")?;
    let limit = q.limit_clamped(40, 80);
    let max_id = q.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let min_id = q.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    // Paginate by follow_request.id (matching Mastodon's FollowRequest.paginate_by_max_id)
    let rows = sqlx::query!(
        r#"SELECT f.id AS req_id, f.account_id FROM follow_requests f
           WHERE f.target_account_id = $1
             AND ($2::bigint IS NULL OR f.id < $2)
             AND ($3::bigint IS NULL OR f.id > $3)
             AND ($5::bigint IS NULL OR f.id > $5)
           ORDER BY f.id DESC LIMIT $4"#,
        auth.account_id, max_id, since_id, limit, min_id
    )
    .fetch_all(&state.db)
    .await?;

    let first_req_id = rows.first().map(|r| r.req_id.to_string());
    let last_req_id = rows.last().map(|r| r.req_id.to_string());
    let account_ids: Vec<i64> = rows.iter().map(|r| r.account_id).collect();

    let accounts = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &account_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let account_map: std::collections::HashMap<i64, Account> =
        accounts.into_iter().map(|a| (a.id, a)).collect();
    let accounts_ordered: Vec<Account> = account_ids.iter()
        .filter_map(|id| account_map.get(id).cloned())
        .collect();

    let api_accounts = batch_accounts_to_api(&state, &accounts_ordered).await;
    let link = first_req_id.zip(last_req_id).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest, &oldest)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── POST /api/v1/follow_requests/:id/authorize ────────────────────────────

pub async fn authorize_follow_request(
    State(state): State<AppState>,
    Path(requester_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:follows")?;
    // Move from follow_requests to follows (atomic: delete pending, insert accepted)
    let deleted = sqlx::query!(
        "DELETE FROM follow_requests WHERE account_id = $1 AND target_account_id = $2 RETURNING account_id, uri",
        requester_id, auth.account_id
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(deleted_row) = deleted {
        sqlx::query!(
            r#"INSERT INTO follows (account_id, target_account_id, created_at, updated_at)
               VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING"#,
            requester_id, auth.account_id
        )
        .execute(&state.db)
        .await?;
        sqlx::query!(
            "INSERT INTO account_stats (account_id, followers_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET followers_count = account_stats.followers_count + 1, updated_at = now()",
            auth.account_id
        )
        .execute(&state.db)
        .await?;

        sqlx::query!(
            "INSERT INTO account_stats (account_id, following_count, created_at, updated_at) VALUES ($1, 1, now(), now()) ON CONFLICT (account_id) DO UPDATE SET following_count = account_stats.following_count + 1, updated_at = now()",
            requester_id
        )
        .execute(&state.db)
        .await?;

        let accepter = fetch_account(&state, auth.account_id).await?;
        push::create_and_push(
            &state,
            requester_id,
            auth.account_id,
            "follow",
            None,
            format!("{} accepted your follow request", accepter.display_name),
            accepter.acct().clone(),
            super::convert::account_avatar_url_for(&accepter),
        ).await;

        if let Some(follow_uri) = deleted_row.uri {
            let requester = fetch_account(&state, requester_id).await?;
            if requester.domain.is_some()
                && accepter.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                    let accepter_actor_url = crate::federation::tag::account_uri_of(&state.instance.domain, &accepter);
                    let key_id = format!("{}#main-key", accepter_actor_url);
                    let accept_id = format!(
                        "https://{}/activities/{}",
                        state.instance.domain,
                        crate::snowflake::next_id()
                    );
                    let activity = crate::federation::activity::accept_follow(
                        &accept_id,
                        &accepter_actor_url,
                        &follow_uri,
                        &requester.uri,
                        &accepter_actor_url,
                    )?;
                    let inbox = requester.inbox_url.clone();
                    if inbox.is_empty() {
                        tracing::warn!(requester_uri = %requester.uri, "cannot deliver Accept: remote actor has no inbox URL");
                    } else {
                        tracing::debug!(inbox, requester_uri = %requester.uri, "enqueueing Accept");
                        if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                            &state,
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

        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let followed_id = auth.account_id;
        if feed::sync_fanout() {
            feed::backfill_follow(&mut redis, &db, requester_id, followed_id).await;
        } else {
            tokio::spawn(async move {
                feed::backfill_follow(&mut redis, &db, requester_id, followed_id).await;
            });
        }
    }

    build_relationship(&state, auth.account_id, requester_id).await.map(Json)
}

// ── POST /api/v1/follow_requests/:id/reject ───────────────────────────────

pub async fn reject_follow_request(
    State(state): State<AppState>,
    Path(requester_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:follows")?;
    let deleted = sqlx::query!(
        "DELETE FROM follow_requests WHERE account_id = $1 AND target_account_id = $2 RETURNING account_id, uri",
        requester_id, auth.account_id
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(deleted_row) = deleted {
        if let Some(follow_uri) = deleted_row.uri {
            let requester = fetch_account(&state, requester_id).await?;
            if requester.domain.is_some() {
                let rejecter = fetch_account(&state, auth.account_id).await?;
                if rejecter.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                    let rejecter_actor_url = crate::federation::tag::account_uri_of(&state.instance.domain, &rejecter);
                    let key_id = format!("{}#main-key", rejecter_actor_url);
                    let reject_id = format!(
                        "https://{}/activities/{}",
                        state.instance.domain,
                        crate::snowflake::next_id()
                    );
                    let activity = crate::federation::activity::reject_follow(
                        &reject_id,
                        &rejecter_actor_url,
                        &follow_uri,
                        &requester.uri,
                        &rejecter_actor_url,
                    )?;
                    let inbox = requester.inbox_url.clone();
                    if !inbox.is_empty() {
                        if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                            &state,
                            activity,
                            vec![inbox],
                            key_id,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to enqueue Reject");
                        }
                    }
                }
            }
        }
    }

    build_relationship(&state, auth.account_id, requester_id).await.map(Json)
}

// ── Helpers ────────────────────────────────────────────────────────────────

pub async fn fetch_account(state: &AppState, id: i64) -> AppResult<Account> {
    sqlx::query_as!(Account, "SELECT * FROM accounts WHERE id = $1", id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn batch_status_media(
    state: &AppState,
    status_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, Vec<crate::db::models::MediaAttachment>>> {
    if status_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = sqlx::query_as!(
        crate::db::models::MediaAttachment,
        "SELECT * FROM media_attachments WHERE status_id = ANY($1::bigint[]) ORDER BY id",
        status_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let mut map: std::collections::HashMap<i64, Vec<_>> = std::collections::HashMap::new();
    for m in rows {
        if let Some(sid) = m.status_id {
            map.entry(sid).or_default().push(m);
        }
    }
    Ok(map)
}

pub async fn batch_reblog_data(
    state: &AppState,
    statuses: &[crate::db::models::Status],
) -> AppResult<std::collections::HashMap<i64, (crate::db::models::Status, crate::db::models::Account, Vec<crate::db::models::MediaAttachment>)>> {
    use std::collections::{HashMap, HashSet};

    let reblog_ids: Vec<i64> = statuses.iter()
        .filter_map(|s| s.reblog_of_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if reblog_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let reblog_statuses = sqlx::query_as!(
        crate::db::models::Status,
        "SELECT * FROM statuses WHERE id = ANY($1::bigint[]) AND deleted_at IS NULL",
        &reblog_ids,
    )
    .fetch_all(&state.db)
    .await?;

    let reblog_account_ids: Vec<i64> = reblog_statuses.iter()
        .map(|s| s.account_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let reblog_accounts = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &reblog_account_ids,
    )
    .fetch_all(&state.db)
    .await?;

    let reblog_account_map: HashMap<i64, Account> = reblog_accounts
        .into_iter()
        .map(|a| (a.id, a))
        .collect();

    let reblog_status_ids: Vec<i64> = reblog_statuses.iter().map(|s| s.id).collect();
    let reblog_media = batch_status_media(state, &reblog_status_ids).await?;

    let reblog_status_map: HashMap<i64, crate::db::models::Status> = reblog_statuses
        .into_iter()
        .map(|s| (s.id, s))
        .collect();

    let mut result = HashMap::new();
    for s in statuses {
        if let Some(reblog_id) = s.reblog_of_id {
            if let Some(rs) = reblog_status_map.get(&reblog_id) {
                if let Some(ra) = reblog_account_map.get(&rs.account_id) {
                    let media = reblog_media.get(&reblog_id).cloned().unwrap_or_default();
                    result.insert(s.id, (rs.clone(), ra.clone(), media));
                }
            }
        }
    }
    Ok(result)
}

/// Batch-fetch quoted statuses for a list of statuses. Returns a map from
/// quoting status ID → fully-built API `Status` (without the quote's own quote).
pub async fn batch_quote_data(
    state: &AppState,
    statuses: &[crate::db::models::Status],
    viewer_id: Option<i64>,
) -> AppResult<std::collections::HashMap<i64, super::types::QuoteInfo>> {
    use std::collections::{HashMap, HashSet};

    let status_ids: Vec<i64> = statuses.iter().map(|s| s.id).collect();

    // Fetch quote relationships from the quotes table
    let quote_rows = sqlx::query!(
        "SELECT status_id, quoted_status_id FROM quotes WHERE status_id = ANY($1::bigint[]) AND quoted_status_id IS NOT NULL",
        &status_ids,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Map from quoting status ID → quoted status ID
    let quote_of: HashMap<i64, i64> = quote_rows.iter()
        .filter_map(|r| r.quoted_status_id.map(|qid| (r.status_id, qid)))
        .collect();

    let quote_ids: Vec<i64> = quote_of.values().cloned().collect::<HashSet<_>>().into_iter().collect();

    if quote_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let quoted_statuses = sqlx::query_as!(
        crate::db::models::Status,
        "SELECT * FROM statuses WHERE id = ANY($1::bigint[]) AND deleted_at IS NULL",
        &quote_ids,
    )
    .fetch_all(&state.db)
    .await?;

    // Also look up any soft-deleted quoted statuses (they exist but have deleted_at set)
    let found_ids: HashSet<i64> = quoted_statuses.iter().map(|s| s.id).collect();
    let deleted_ids: Vec<i64> = quote_ids.iter().filter(|id| !found_ids.contains(*id)).cloned().collect();

    let account_ids: Vec<i64> = quoted_statuses.iter()
        .map(|s| s.account_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let accounts = if !account_ids.is_empty() {
        sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
            &account_ids,
        )
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };
    let account_map: HashMap<i64, Account> = accounts.into_iter().map(|a| (a.id, a)).collect();

    let qs_ids: Vec<i64> = quoted_statuses.iter().map(|s| s.id).collect();
    let (media_map, tags_map, mentions_map, emojis_map, polls_map, cards_map, ctxs) = if !qs_ids.is_empty() {
        let media = batch_status_media(state, &qs_ids).await?;
        let tags = batch_statuses_tags(state, &qs_ids).await?;
        let mentions = batch_status_mentions(state, &qs_ids).await?;
        let emojis = batch_status_emojis(state, &quoted_statuses).await?;
        let polls = batch_status_polls(state, &qs_ids, viewer_id).await?;
        let cards = batch_status_cards(state, &qs_ids).await?;
        let ctxs = if let Some(vid) = viewer_id {
            super::statuses::batch_viewer_contexts(state, vid, &qs_ids).await?
        } else {
            HashMap::new()
        };
        (media, tags, mentions, emojis, polls, cards, ctxs)
    } else {
        (HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new())
    };

    // Fetch quote states for all quoting statuses that have a quoted_status_id in quotes table
    let quoting_ids: Vec<i64> = quote_of.keys().cloned().collect();
    let quote_states: HashMap<i64, String> = if !quoting_ids.is_empty() {
        let rows = sqlx::query!(
            "SELECT status_id, state FROM quotes WHERE status_id = ANY($1::bigint[])",
            &quoting_ids,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        rows.into_iter().map(|r| (r.status_id, crate::db::models::quote_state::to_str(r.state).to_owned())).collect()
    } else {
        HashMap::new()
    };

    // Check block relationships between viewer and quoted status authors (for "unauthorized" state)
    let blocked_author_ids: HashSet<i64> = if let Some(vid) = viewer_id {
        let author_ids: Vec<i64> = quoted_statuses.iter().map(|s| s.account_id).collect::<HashSet<_>>().into_iter().collect();
        if !author_ids.is_empty() {
            sqlx::query_scalar!(
                r#"SELECT target_account_id FROM blocks WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])
                   UNION
                   SELECT account_id FROM blocks WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])"#,
                vid, &author_ids,
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .collect()
        } else {
            HashSet::new()
        }
    } else {
        HashSet::new()
    };

    // Fetch shallow quote states for nested quotes (quoted statuses that themselves quote something).
    let nested_quoting_ids: Vec<i64> = quoted_statuses.iter().map(|qs| qs.id).collect();
    // (status_id → (state, quoted_status_id)) for nested quotes
    let nested_quote_info: HashMap<i64, (String, i64)> = if !nested_quoting_ids.is_empty() {
        let rows = sqlx::query!(
            "SELECT status_id, state, quoted_status_id FROM quotes WHERE status_id = ANY($1::bigint[]) AND quoted_status_id IS NOT NULL",
            &nested_quoting_ids,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        rows.into_iter()
            .filter_map(|r| r.quoted_status_id.map(|qid| (r.status_id, (crate::db::models::quote_state::to_str(r.state).to_owned(), qid))))
            .collect()
    } else {
        HashMap::new()
    };

    // Build a map from quoted status id → API Status
    let mut qs_map: HashMap<i64, super::types::Status> = HashMap::new();
    for qs in &quoted_statuses {
        let Some(account) = account_map.get(&qs.account_id) else { continue };
        let media = media_map.get(&qs.id).cloned().unwrap_or_default();
        let mentions = mentions_map.get(&qs.id).cloned().unwrap_or_default();
        let ctx = ctxs.get(&qs.id).cloned();
        let mut api = super::convert::status_from_db(qs, account, media, None, ctx, &mentions, &[]);
        api.tags = tags_map.get(&qs.id).cloned().unwrap_or_default();
        api.mentions = mentions;
        api.emojis = emojis_map.get(&qs.id).cloned().unwrap_or_default();
        api.poll = polls_map.get(&qs.id).cloned();
        api.card = cards_map.get(&qs.id).cloned();
        // Attach shallow quote info for the nested quote (ShallowQuoteSerializer behavior)
        if let Some((state_str, nested_qid)) = nested_quote_info.get(&qs.id) {
            api.quote = Some(super::types::QuoteInfo {
                state: state_str.clone(),
                quoted_status: None,
                quoted_status_id: Some(nested_qid.to_string()),
            });
        }
        qs_map.insert(qs.id, api);
    }

    // Build the final map keyed by quoting status ID → QuoteInfo.
    // Show QuoteInfo for all states (accepted, pending, revoked, rejected).
    // The effective state is derived from the DB state, with "deleted" and "unauthorized"
    // as viewer-computed overrides per Mastodon's REST::BaseQuoteSerializer logic.
    let mut result: HashMap<i64, super::types::QuoteInfo> = HashMap::new();
    for s in statuses {
        let Some(&qid) = quote_of.get(&s.id) else { continue };
        let state_str = quote_states.get(&s.id).cloned().unwrap_or_else(|| "accepted".to_string());

        // Derive effective display state and whether to include the quoted status body
        let (effective_state, include_status) = if deleted_ids.contains(&qid) {
            ("deleted".to_string(), false)
        } else {
            let quoted_author_id = quoted_statuses.iter().find(|qs| qs.id == qid).map(|qs| qs.account_id);
            let unauthorized = quoted_author_id.map(|aid| blocked_author_ids.contains(&aid)).unwrap_or(false);
            if unauthorized {
                ("unauthorized".to_string(), false)
            } else {
                (state_str.clone(), true)
            }
        };

        let quoted_status = if include_status { qs_map.get(&qid).cloned() } else { None };
        result.insert(s.id, super::types::QuoteInfo {
            state: effective_state,
            quoted_status: quoted_status.map(Box::new),
            quoted_status_id: None,
        });
    }
    Ok(result)
}

pub async fn fetch_status_poll(
    state: &AppState,
    status_id: i64,
    viewer_id: Option<i64>,
) -> AppResult<Option<super::types::Poll>> {
    let row = sqlx::query!(
        "SELECT id, options, multiple, expires_at FROM polls WHERE status_id = $1",
        status_id,
    )
    .fetch_optional(&state.db)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let now = chrono::Utc::now().naive_utc();
    let expired = row.expires_at.is_some_and(|t| t < now);

    let option_titles: Vec<String> = row.options;

    // Compute per-option vote counts live from poll_votes.
    let per_option = sqlx::query!(
        "SELECT choice, COUNT(*)::bigint AS \"cnt!\" FROM poll_votes WHERE poll_id = $1 GROUP BY choice",
        row.id,
    )
    .fetch_all(&state.db)
    .await?;

    let mut per_option_map: std::collections::HashMap<i32, i64> = std::collections::HashMap::new();
    for r in per_option {
        per_option_map.insert(r.choice, r.cnt);
    }

    let options: Vec<super::types::PollOption> = option_titles.iter().enumerate().map(|(i, title)| {
        super::types::PollOption {
            title: title.clone(),
            votes_count: Some(*per_option_map.get(&(i as i32)).unwrap_or(&0)),
        }
    }).collect();

    // Compute aggregate counts live.
    let (votes_count, voters_count) = sqlx::query!(
        r#"SELECT COUNT(*)::bigint AS "votes!", COUNT(DISTINCT account_id)::bigint AS "voters!" FROM poll_votes WHERE poll_id = $1"#,
        row.id,
    )
    .fetch_one(&state.db)
    .await
    .map(|r| (r.votes, r.voters))
    .unwrap_or((0, 0));

    let voters_count = if row.multiple { Some(voters_count) } else { None };

    let (voted, own_votes) = if let Some(vid) = viewer_id {
        let votes = sqlx::query!(
            "SELECT choice FROM poll_votes WHERE poll_id = $1 AND account_id = $2 ORDER BY choice",
            row.id, vid,
        )
        .fetch_all(&state.db)
        .await?;
        if votes.is_empty() {
            (Some(false), None)
        } else {
            let choices: Vec<i32> = votes.iter().map(|v| v.choice).collect();
            (Some(true), Some(choices))
        }
    } else {
        (None, None)
    };

    Ok(Some(super::types::Poll {
        id: row.id.to_string(),
        expires_at: row.expires_at.map(super::convert::mastodon_date),
        expired,
        multiple: row.multiple,
        votes_count,
        voters_count,
        options,
        emojis: vec![],
        voted,
        own_votes,
    }))
}

pub async fn fetch_status_media(
    state: &AppState,
    status_id: i64,
) -> AppResult<Vec<crate::db::models::MediaAttachment>> {
    Ok(sqlx::query_as!(
        crate::db::models::MediaAttachment,
        "SELECT * FROM media_attachments WHERE status_id = $1 ORDER BY id",
        status_id,
    )
    .fetch_all(&state.db)
    .await?)
}

pub async fn fetch_reblog_data(
    state: &AppState,
    status: &crate::db::models::Status,
) -> AppResult<Option<(crate::db::models::Status, Account, Vec<crate::db::models::MediaAttachment>)>> {
    let Some(reblog_id) = status.reblog_of_id else {
        return Ok(None);
    };
    let reblog = sqlx::query_as!(
        crate::db::models::Status,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        reblog_id,
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(reblog) = reblog else {
        return Ok(None);
    };
    let reblog_account = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = $1",
        reblog.account_id,
    )
    .fetch_one(&state.db)
    .await?;
    let reblog_media = fetch_status_media(state, reblog.id).await?;
    Ok(Some((reblog, reblog_account, reblog_media)))
}

async fn batch_build_relationships(state: &AppState, source_id: i64, target_ids: &[i64]) -> AppResult<Vec<Relationship>> {
    struct FollowRow { show_reblogs: bool, notify: bool, languages: Option<Vec<String>> }
    struct MuteRow { hide_notifications: bool, expires_at: Option<chrono::NaiveDateTime> }

    // Accepted follows (outgoing)
    let follows_out = sqlx::query!(
        "SELECT target_account_id, show_reblogs, notify, languages FROM follows WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let follows_out_map: std::collections::HashMap<i64, _> = follows_out.into_iter()
        .map(|r| (r.target_account_id, FollowRow {
            show_reblogs: r.show_reblogs,
            notify: r.notify,
            languages: r.languages.filter(|l| !l.is_empty()),
        }))
        .collect();

    // Pending follow requests (outgoing)
    let follow_requests_out: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT target_account_id FROM follow_requests WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    // Accepted follows (incoming)
    let followed_by_set: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM follows WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    // Pending follow requests (incoming)
    let requested_by_set: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM follow_requests WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let blocks_out: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT target_account_id FROM blocks WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let blocks_in: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM blocks WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let mutes = sqlx::query!(
        "SELECT target_account_id, hide_notifications, expires_at FROM mutes WHERE account_id = $1 AND target_account_id = ANY($2::bigint[]) AND (expires_at IS NULL OR expires_at > now())",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let mutes_map: std::collections::HashMap<i64, MuteRow> = mutes.into_iter()
        .map(|r| (r.target_account_id, MuteRow { hide_notifications: r.hide_notifications, expires_at: r.expires_at }))
        .collect();

    let target_domains: std::collections::HashMap<i64, Option<String>> = sqlx::query!(
        "SELECT id, domain FROM accounts WHERE id = ANY($1::bigint[])",
        target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.domain))
    .collect();

    let domains_to_check: Vec<String> = target_domains.values().filter_map(|d| d.clone()).collect();
    let domain_blocked_set: std::collections::HashSet<String> = if domains_to_check.is_empty() {
        Default::default()
    } else {
        sqlx::query_scalar!(
            "SELECT domain FROM account_domain_blocks WHERE account_id = $1 AND domain = ANY($2)",
            source_id, &domains_to_check,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect()
    };

    let notes: std::collections::HashMap<i64, String> = sqlx::query!(
        "SELECT target_account_id, comment FROM account_notes WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .map(|r| (r.target_account_id, r.comment))
    .collect();

    let endorsed_set: std::collections::HashSet<i64> = sqlx::query_scalar!(
        "SELECT target_account_id FROM account_pins WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
        source_id, target_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let mut results = Vec::with_capacity(target_ids.len());
    for &target_id in target_ids {
        let follow = follows_out_map.get(&target_id);
        let mute = mutes_map.get(&target_id);
        let domain = target_domains.get(&target_id).and_then(|d| d.clone());
        let domain_blocking = domain.is_some_and(|d| domain_blocked_set.contains(&d));
        results.push(Relationship {
            id: target_id.to_string(),
            following: follow.is_some(),
            showing_reblogs: follow.is_some_and(|f| f.show_reblogs),
            notifying: follow.is_some_and(|f| f.notify),
            languages: follow.and_then(|f| f.languages.clone()),
            followed_by: followed_by_set.contains(&target_id),
            blocking: blocks_out.contains(&target_id),
            blocked_by: blocks_in.contains(&target_id),
            muting: mute.is_some(),
            muting_notifications: mute.is_some_and(|m| m.hide_notifications),
            muting_expires_at: mute.and_then(|m| m.expires_at).map(super::convert::mastodon_date),
            requested: follow_requests_out.contains(&target_id),
            requested_by: requested_by_set.contains(&target_id),
            domain_blocking,
            endorsed: endorsed_set.contains(&target_id),
            note: notes.get(&target_id).cloned().unwrap_or_default(),
        });
    }
    Ok(results)
}

async fn build_relationship(state: &AppState, source_id: i64, target_id: i64) -> AppResult<Relationship> {
    // Check accepted follow (source → target)
    let follow = sqlx::query!(
        "SELECT show_reblogs, notify, languages FROM follows WHERE account_id = $1 AND target_account_id = $2",
        source_id, target_id
    )
    .fetch_optional(&state.db)
    .await?;

    // Check pending follow request (source → target)
    let requested = sqlx::query!(
        "SELECT 1 as exists FROM follow_requests WHERE account_id = $1 AND target_account_id = $2",
        source_id, target_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let followed_by = sqlx::query!(
        "SELECT 1 as exists FROM follows WHERE account_id = $1 AND target_account_id = $2",
        target_id, source_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let blocking = sqlx::query!(
        "SELECT 1 as exists FROM blocks WHERE account_id = $1 AND target_account_id = $2",
        source_id, target_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let blocked_by = sqlx::query!(
        "SELECT 1 as exists FROM blocks WHERE account_id = $1 AND target_account_id = $2",
        target_id, source_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let requested_by = sqlx::query!(
        "SELECT 1 as exists FROM follow_requests WHERE account_id = $1 AND target_account_id = $2",
        target_id, source_id
    )
    .fetch_optional(&state.db)
    .await?
    .is_some();

    let muting = sqlx::query!(
        "SELECT hide_notifications, expires_at FROM mutes WHERE account_id = $1 AND target_account_id = $2 AND (expires_at IS NULL OR expires_at > now())",
        source_id, target_id
    )
    .fetch_optional(&state.db)
    .await?;

    // Check if source has domain-blocked target's domain
    let target_domain = sqlx::query_scalar!(
        "SELECT domain FROM accounts WHERE id = $1",
        target_id
    )
    .fetch_optional(&state.db)
    .await?
    .flatten();

    let domain_blocking = if let Some(domain) = target_domain {
        sqlx::query!(
            "SELECT 1 as exists FROM account_domain_blocks WHERE account_id = $1 AND domain = $2",
            source_id, domain
        )
        .fetch_optional(&state.db)
        .await?
        .is_some()
    } else {
        false
    };

    let note = sqlx::query_scalar!(
        "SELECT comment FROM account_notes WHERE account_id = $1 AND target_account_id = $2",
        source_id, target_id
    )
    .fetch_optional(&state.db)
    .await?
    .unwrap_or_default();

    let showing_reblogs = follow.as_ref().is_some_and(|f| f.show_reblogs);
    let notifying = follow.as_ref().is_some_and(|f| f.notify);
    let languages = follow.as_ref().and_then(|f| f.languages.clone().filter(|l| !l.is_empty()));
    let muting_expires_at = muting.as_ref().and_then(|m| m.expires_at)
        .map(super::convert::mastodon_date);

    Ok(Relationship {
        id: target_id.to_string(),
        following: follow.is_some(),
        showing_reblogs,
        notifying,
        languages,
        followed_by,
        blocking,
        blocked_by,
        muting: muting.is_some(),
        muting_notifications: muting.is_some_and(|m| m.hide_notifications),
        muting_expires_at,
        requested,
        requested_by,
        domain_blocking,
        endorsed: sqlx::query!(
            "SELECT 1 AS e FROM account_pins WHERE account_id = $1 AND target_account_id = $2",
            source_id, target_id
        )
        .fetch_optional(&state.db)
        .await?
        .is_some(),
        note,
    })
}

// ── GET /api/v1/suggestions ────────────────────────────────────────────────

pub async fn get_suggestions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Query(params): Query<PaginationParams>,
) -> AppResult<Json<Vec<ApiAccount>>> {
    auth.require_scope("read:accounts")?;
    let limit = params.limit_clamped(40, 80);

    let accounts = sqlx::query_as!(
        Account,
        r#"SELECT a.* FROM accounts a
           JOIN follows f ON f.account_id = a.id
           WHERE f.target_account_id = $1
             AND a.domain IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM follows f2
               WHERE f2.account_id = $1 AND f2.target_account_id = a.id
             )
           ORDER BY f.created_at DESC
           LIMIT $2"#,
        auth.account_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(batch_accounts_to_api(&state, &accounts).await))
}

// ── DELETE /api/v1/suggestions/:account_id ────────────────────────────────

pub async fn dismiss_suggestion(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path(account_id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        r#"INSERT INTO follow_recommendation_mutes (account_id, target_account_id, created_at, updated_at)
           VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING"#,
        auth.account_id, account_id,
    )
    .execute(&state.db)
    .await?;
    Ok(Json(serde_json::json!({})))
}

// ── GET /api/v2/suggestions ───────────────────────────────────────────────

pub async fn get_suggestions_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Query(params): Query<PaginationParams>,
) -> AppResult<Json<Vec<SuggestionV2>>> {
    auth.require_scope("read:accounts")?;
    let limit = params.limit_clamped(40, 80);

    let accounts = sqlx::query_as!(
        Account,
        r#"SELECT a.* FROM accounts a
           JOIN follows f ON f.account_id = a.id
           WHERE f.target_account_id = $1
             AND a.domain IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM follows f2
               WHERE f2.account_id = $1 AND f2.target_account_id = a.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM follow_recommendation_mutes sd
               WHERE sd.account_id = $1 AND sd.target_account_id = a.id
             )
           ORDER BY f.created_at DESC
           LIMIT $2"#,
        auth.account_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    let emojis_map = batch_account_emojis(&state, &accounts).await;
    let roles_map = batch_account_roles(&state, &accounts).await;
    let suggestions = accounts
        .iter()
        .map(|a| {
            let mut api = account_from_db(a);
            api.emojis = emojis_map.get(&a.id).cloned().unwrap_or_default();
            api.roles = roles_map.get(&a.id).cloned().unwrap_or_default();
            SuggestionV2 {
                source: "past_interactions".to_string(),
                sources: vec!["friends_of_friends".to_string()],
                account: api,
            }
        })
        .collect();

    Ok(Json(suggestions))
}

// ── POST /api/v1/accounts/move ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MoveAccountForm {
    pub acct: String,
    pub current_password: String,
}

pub async fn move_account(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    Json(form): Json<MoveAccountForm>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_scope("write:accounts")?;
    // Verify password
    let user = sqlx::query!(
        "SELECT encrypted_password FROM users WHERE account_id = $1",
        auth.account_id
    )
    .fetch_one(&state.db)
    .await?;

    let valid = crate::crypto::verify_password(&form.current_password, &user.encrypted_password).is_ok();
    if !valid {
        return Err(AppError::Unauthorized);
    }

    // Look up target account by URI/acct handle, fetching it from its origin
    // server if we don't know it yet (so cross-instance migration works).
    let target_uri = form.acct.clone();
    let moved_account = sqlx::query_scalar!(
        "SELECT id FROM accounts WHERE uri = $1 OR (username = $1 AND domain IS NULL) LIMIT 1",
        target_uri,
    )
    .fetch_optional(&state.db)
    .await?;
    let moved_account = match moved_account {
        Some(id) => Some(id),
        None if target_uri.starts_with("https://") => {
            crate::api::ap::inbox::resolve_or_fetch_remote_account(&state, &target_uri)
                .await
                .ok()
        }
        None => None,
    };

    // If we can't resolve the target (e.g. an acct handle we haven't federated
    // with yet), record nothing and return success — matching the prior lenient
    // behaviour rather than failing the request.
    let Some(moved_id) = moved_account else {
        return Ok(Json(serde_json::json!({})));
    };

    // The new account must list this account as an alias (alsoKnownAs); Mastodon
    // followers verify the same before honouring the Move.
    let new_uri = sqlx::query_scalar!("SELECT uri FROM accounts WHERE id = $1", moved_id)
        .fetch_one(&state.db)
        .await?;

    sqlx::query!(
        "UPDATE accounts SET moved_to_account_id = $1, updated_at = now() WHERE id = $2",
        moved_id, auth.account_id,
    )
    .execute(&state.db)
    .await?;

    // Announce the Move to followers so they re-follow the new account.
    let mover = sqlx::query!(
        "SELECT username, uri, private_key, id_scheme FROM accounts WHERE id = $1",
        auth.account_id,
    )
    .fetch_one(&state.db)
    .await?;
    if !new_uri.is_empty() && mover.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
        let actor_url = crate::federation::tag::account_uri(&instance.domain, auth.account_id, mover.id_scheme, &mover.username);
        let move_id = format!("{actor_url}#moves/{}", crate::snowflake::next_id());
        let activity = crate::federation::activity::move_actor(&move_id, &actor_url, &actor_url, &new_uri);
        let key_id = format!("{actor_url}#main-key");
        if let Err(e) = crate::federation::delivery::fanout_to_followers(
            &state,
            activity,
            auth.account_id,
            key_id,
        )
        .await
        {
            tracing::warn!(error = %e, "failed to enqueue Move fanout");
        }
    }

    Ok(Json(serde_json::json!({})))
}

// ── GET /api/v1/profile/aliases ───────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AccountAlias {
    pub id: String,
    pub account_id: String,
    pub uri: String,
    pub created_at: String,
}

pub async fn list_aliases(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Vec<AccountAlias>>> {
    auth.require_scope("read:accounts")?;
    let rows = sqlx::query!(
        "SELECT id, account_id, uri, created_at FROM account_aliases WHERE account_id = $1 ORDER BY created_at",
        auth.account_id,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(|r| AccountAlias {
        id: r.id.to_string(),
        account_id: r.account_id.to_string(),
        uri: r.uri,
        created_at: super::convert::mastodon_date(r.created_at),
    }).collect()))
}

#[derive(Debug, Deserialize)]
pub struct CreateAliasForm {
    pub acct: String,
}

pub async fn create_alias(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Json(form): Json<CreateAliasForm>,
) -> AppResult<Json<AccountAlias>> {
    auth.require_scope("write:accounts")?;
    let r = sqlx::query!(
        r#"INSERT INTO account_aliases (account_id, uri, created_at, updated_at) VALUES ($1, $2, now(), now())
           ON CONFLICT (account_id, uri) DO UPDATE SET updated_at = now()
           RETURNING id, account_id, uri, created_at"#,
        auth.account_id, form.acct,
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(AccountAlias {
        id: r.id.to_string(),
        account_id: r.account_id.to_string(),
        uri: r.uri,
        created_at: super::convert::mastodon_date(r.created_at),
    }))
}

pub async fn delete_alias(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        "DELETE FROM account_aliases WHERE id = $1 AND account_id = $2",
        id, auth.account_id,
    )
    .execute(&state.db)
    .await?;
    Ok(Json(serde_json::json!({})))
}

// ── POST /api/v1/accounts/:id/note ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct NoteForm {
    pub comment: Option<String>,
}

pub async fn set_account_note(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    Json(form): Json<NoteForm>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:accounts")?;
    let comment = form.comment.unwrap_or_default();
    // Mastodon AccountNote::COMMENT_SIZE_LIMIT.
    if comment.chars().count() > 2_000 {
        return Err(AppError::Unprocessable(
            "Validation failed: Comment is too long (maximum is 2000 characters)".into(),
        ));
    }
    if comment.trim().is_empty() {
        sqlx::query!(
            "DELETE FROM account_notes WHERE account_id = $1 AND target_account_id = $2",
            auth.account_id, target_id,
        )
        .execute(&state.db)
        .await?;
    } else {
        sqlx::query!(
            r#"INSERT INTO account_notes (account_id, target_account_id, comment, created_at, updated_at)
               VALUES ($1, $2, $3, now(), now())
               ON CONFLICT (account_id, target_account_id)
               DO UPDATE SET comment = EXCLUDED.comment, updated_at = now()"#,
            auth.account_id, target_id, comment,
        )
        .execute(&state.db)
        .await?;
    }

    build_relationship(&state, auth.account_id, target_id).await.map(Json)
}

// ── POST /api/v1/accounts/:id/remove_from_followers ───────────────────────

pub async fn remove_from_followers(
    State(state): State<AppState>,
    Path(requester_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:follows")?;
    let deleted = sqlx::query!(
        "DELETE FROM follows WHERE account_id = $1 AND target_account_id = $2 RETURNING 1 as exists",
        requester_id,
        auth.account_id,
    )
    .fetch_optional(&state.db)
    .await?;

    if deleted.is_some() {
        sqlx::query!(
            "UPDATE account_stats SET followers_count = GREATEST(followers_count - 1, 0), updated_at = now() WHERE account_id = $1",
            auth.account_id
        )
        .execute(&state.db)
        .await?;
        sqlx::query!(
            "UPDATE account_stats SET following_count = GREATEST(following_count - 1, 0), updated_at = now() WHERE account_id = $1",
            requester_id
        )
        .execute(&state.db)
        .await?;
    }

    build_relationship(&state, auth.account_id, requester_id).await.map(Json)
}

// ── POST /api/v1/accounts/:id/endorse ────────────────────────────────────

pub async fn endorse_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        "INSERT INTO account_pins (account_id, target_account_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        auth.account_id, target_id,
    )
    .execute(&state.db)
    .await?;
    build_relationship(&state, auth.account_id, target_id).await.map(Json)
}

// ── POST /api/v1/accounts/:id/unendorse ──────────────────────────────────

pub async fn unendorse_account(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Relationship>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        "DELETE FROM account_pins WHERE account_id = $1 AND target_account_id = $2",
        auth.account_id, target_id,
    )
    .execute(&state.db)
    .await?;
    build_relationship(&state, auth.account_id, target_id).await.map(Json)
}

// ── GET /api/v1/accounts/:id/endorsements ────────────────────────────────

pub async fn get_endorsements(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<PaginationParams>,
) -> AppResult<impl IntoResponse> {
    let limit = q.limit_clamped(40, 80);
    let max_id = q.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    let accounts = sqlx::query_as!(
        Account,
        r#"SELECT a.* FROM accounts a
           JOIN account_pins ap ON ap.target_account_id = a.id
           WHERE ap.account_id = $1
             AND ($2::bigint IS NULL OR a.id < $2)
             AND ($3::bigint IS NULL OR a.id > $3)
             AND a.suspended_at IS NULL
           ORDER BY a.id DESC
           LIMIT $4"#,
        id,
        max_id,
        since_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    let api_accounts = batch_accounts_to_api(&state, &accounts).await;
    let link = api_accounts.first().zip(api_accounts.last()).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(uri.query());
        super::link_header(&req_headers, uri.path(), &extra, &newest.id, &oldest.id)
    });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/endorsements ──────────────────────────────────────────────

pub async fn get_my_endorsements(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    uri: Uri,
    req_headers: HeaderMap,
    Query(q): Query<PaginationParams>,
) -> AppResult<impl IntoResponse> {
    auth.require_scope("read:accounts")?;
    let unlimited = q.limit.as_deref() == Some("0");
    let limit = if unlimited { i64::MAX } else { q.limit_clamped(40, 80) };
    let max_id = q.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = q.since_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    // Paginate by account.id (matching Mastodon's paginate_by_max_id on endorsed_accounts)
    let accounts = sqlx::query_as!(
        Account,
        r#"SELECT a.* FROM accounts a
           JOIN account_pins ap ON ap.target_account_id = a.id
           WHERE ap.account_id = $1
             AND ($2::bigint IS NULL OR a.id < $2)
             AND ($3::bigint IS NULL OR a.id > $3)
             AND a.suspended_at IS NULL
           ORDER BY a.id DESC
           LIMIT $4"#,
        auth.account_id,
        max_id,
        since_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    let api_accounts = batch_accounts_to_api(&state, &accounts).await;
    let link = if unlimited { None } else {
        api_accounts.first().zip(api_accounts.last()).map(|(newest, oldest)| {
            let extra = super::non_pagination_query(uri.query());
            super::link_header(&req_headers, uri.path(), &extra, &newest.id, &oldest.id)
        })
    };
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(api_accounts)))
}

// ── GET /api/v1/accounts/:id/featured_tags ───────────────────────────────

pub async fn get_account_featured_tags(
    State(state): State<AppState>,
    Extension(crate::middleware::ResolvedInstance(instance)): Extension<crate::middleware::ResolvedInstance>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<super::types::FeaturedTag>>> {
    let domain = &instance.domain;
    let rows = sqlx::query!(
        r#"SELECT ft.id, t.name, ft.statuses_count, ft.last_status_at, a.username, a.domain
           FROM featured_tags ft
           JOIN tags t ON t.id = ft.tag_id
           JOIN accounts a ON a.id = ft.account_id
           WHERE ft.account_id = $1
           ORDER BY ft.id"#,
        id,
    )
    .fetch_all(&state.db)
    .await?;
    let tags = rows
        .into_iter()
        .map(|r| {
            let url = if let Some(ref acct_domain) = r.domain {
                format!("https://{}/@{}@{}/tagged/{}", domain, r.username, acct_domain, r.name)
            } else {
                format!("https://{}/@{}/tagged/{}", domain, r.username, r.name)
            };
            super::types::FeaturedTag {
                id: r.id.to_string(),
                name: r.name.clone(),
                url,
                statuses_count: r.statuses_count.to_string(),
                last_status_at: r.last_status_at.map(|t| t.format("%Y-%m-%d").to_string()),
            }
        })
        .collect();
    Ok(Json(tags))
}

// ── GET /api/v1/profile ───────────────────────────────────────────────────

pub async fn get_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(crate::middleware::ResolvedInstance(instance)): Extension<crate::middleware::ResolvedInstance>,
) -> AppResult<Json<super::types::Profile>> {
    auth.require_scope("read:accounts")?;
    Ok(Json(build_profile(&state, &instance.domain, auth.account_id).await?))
}

/// PUT /api/v1/profile — accepts a JSON body and returns the current profile.
/// (Profile field edits go through update_credentials / the multipart PATCH.)
pub async fn put_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(crate::middleware::ResolvedInstance(instance)): Extension<crate::middleware::ResolvedInstance>,
    _body: Option<Json<serde_json::Value>>,
) -> AppResult<Json<super::types::Profile>> {
    auth.require_scope("write:accounts")?;
    Ok(Json(build_profile(&state, &instance.domain, auth.account_id).await?))
}

async fn build_profile(
    state: &AppState,
    domain: &str,
    account_id: i64,
) -> AppResult<super::types::Profile> {
    let account = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = $1",
        account_id,
    )
    .fetch_one(&state.db)
    .await?;

    let domain = &domain.to_string();
    let featured_tag_rows = sqlx::query!(
        r#"SELECT ft.id, t.name, ft.statuses_count, ft.last_status_at
           FROM featured_tags ft
           JOIN tags t ON t.id = ft.tag_id
           WHERE ft.account_id = $1
           ORDER BY ft.id"#,
        account.id,
    )
    .fetch_all(&state.db)
    .await?;

    let featured_tags = featured_tag_rows
        .into_iter()
        .map(|r| super::types::FeaturedTag {
            id: r.id.to_string(),
            name: r.name.clone(),
            url: format!("https://{}/@{}/tagged/{}", domain, account.username, r.name),
            statuses_count: r.statuses_count.to_string(),
            last_status_at: r.last_status_at.map(|t| t.format("%Y-%m-%d").to_string()),
        })
        .collect();

    let a = &account;
    let fields = super::convert::fields_from_db(a.fields.as_ref().unwrap_or(&serde_json::json!([])));
    let formatted_fields = fields.iter().map(|f| super::types::Field {
        name: f.name.clone(),
        value: super::formatting::format_field_value(&f.value),
        verified_at: f.verified_at.clone(),
    }).collect();
    let profile = super::types::Profile {
        id: a.id.to_string(),
        username: a.username.clone(),
        display_name: a.display_name.clone(),
        note: a.note.clone(),
        fields,
        formatted_note: a.note.clone(),
        formatted_fields,
        avatar: Some(super::convert::account_avatar_url_for(a)),
        avatar_static: Some(super::convert::account_avatar_url_for(a)),
        header: Some(super::convert::account_header_url_for(a)),
        header_static: Some(super::convert::account_header_url_for(a)),
        locked: a.locked,
        bot: a.actor_type.as_deref() == Some("Service"),
        hide_collections: a.hide_collections,
        discoverable: a.discoverable,
        indexable: a.indexable,
        attribution_domains: a.attribution_domains.clone().unwrap_or_default(),
        featured_tags,
    };
    Ok(profile)
}

// ── DELETE /api/v1/profile/avatar ────────────────────────────────────────

pub async fn delete_profile_avatar(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
) -> AppResult<Json<super::types::Account>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        "UPDATE accounts SET avatar_file_name = NULL, avatar_content_type = NULL, avatar_file_size = NULL, avatar_updated_at = NULL, updated_at = now() WHERE id = $1",
        auth.account_id,
    )
    .execute(&state.db)
    .await?;
    let account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        auth.account_id,
    )
    .fetch_one(&state.db)
    .await?;
    distribute_account_update(&state, &instance.domain, &account).await;
    let mut api = account_from_db(&account);
    api.emojis = fetch_account_emojis(&state, &account).await;
    api.roles = fetch_account_roles(&state, account.id).await;
    Ok(Json(api))
}

// ── DELETE /api/v1/profile/header ────────────────────────────────────────

pub async fn delete_profile_header(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
) -> AppResult<Json<super::types::Account>> {
    auth.require_scope("write:accounts")?;
    sqlx::query!(
        "UPDATE accounts SET header_file_name = NULL, header_content_type = NULL, header_file_size = NULL, header_updated_at = NULL, updated_at = now() WHERE id = $1",
        auth.account_id,
    )
    .execute(&state.db)
    .await?;
    let account = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = $1",
        auth.account_id,
    )
    .fetch_one(&state.db)
    .await?;
    distribute_account_update(&state, &instance.domain, &account).await;
    let mut api = account_from_db(&account);
    api.emojis = fetch_account_emojis(&state, &account).await;
    api.roles = fetch_account_roles(&state, account.id).await;
    Ok(Json(api))
}

// ── GET /api/v1/accounts/familiar_followers ──────────────────────────────

pub async fn get_familiar_followers(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    RawQuery(qs): RawQuery,
) -> AppResult<Json<Vec<super::types::FamiliarFollowers>>> {
    auth.require_scope("read:follows")?;
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<i64> = url::form_urlencoded::parse(
            qs.as_deref().unwrap_or("").as_bytes()
        )
        .filter(|(k, _)| k == "id[]" || k == "id")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .filter(|id| seen.insert(*id))
        .collect();

    let mut result = Vec::with_capacity(ids.len());
    for target_id in &ids {
        // Find followers of target_id that also follow the viewer (auth.account_id)
        // Find accounts that: (1) follow the target, and (2) are followed by the viewer
        let accounts = sqlx::query_as!(
            crate::db::models::Account,
            r#"SELECT a.* FROM accounts a
               JOIN follows f1 ON f1.account_id = a.id AND f1.target_account_id = $1
               JOIN follows f2 ON f2.account_id = $2 AND f2.target_account_id = a.id
               LIMIT 10"#,
            target_id,
            auth.account_id,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        result.push(super::types::FamiliarFollowers {
            id: target_id.to_string(),
            accounts: batch_accounts_to_api(&state, &accounts).await,
        });
    }
    Ok(Json(result))
}

// ── GET /api/v1/directory ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DirectoryQuery {
    pub offset: Option<i64>,
    pub limit: Option<i64>,
    pub order: Option<String>,
    pub local: Option<bool>,
}

pub async fn get_directory(
    State(state): State<AppState>,
    Query(q): Query<DirectoryQuery>,
) -> AppResult<Json<Vec<ApiAccount>>> {
    let limit = q.limit.unwrap_or(40).clamp(1, 80);
    let offset = q.offset.unwrap_or(0).max(0);
    let local_only = q.local.unwrap_or(true);
    let order = q.order.as_deref().unwrap_or("active");

    let accounts = if order == "new" {
        sqlx::query_as!(
            Account,
            r#"SELECT * FROM accounts
               WHERE discoverable = true
                 AND suspended_at IS NULL
                 AND (NOT $1::bool OR domain IS NULL)
                 AND (domain IS NULL OR NOT EXISTS (
                     SELECT 1 FROM domain_blocks db WHERE db.domain = domain
                 ))
               ORDER BY created_at DESC
               LIMIT $2 OFFSET $3"#,
            local_only, limit, offset,
        )
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as!(
            Account,
            r#"SELECT a.* FROM accounts a
               WHERE a.discoverable = true
                 AND a.suspended_at IS NULL
                 AND (NOT $1::bool OR a.domain IS NULL)
                 AND (a.domain IS NULL OR NOT EXISTS (
                     SELECT 1 FROM domain_blocks db WHERE db.domain = a.domain
                 ))
               ORDER BY (
                   SELECT MAX(s.created_at) FROM statuses s
                   WHERE s.account_id = a.id AND s.deleted_at IS NULL
               ) DESC NULLS LAST
               LIMIT $2 OFFSET $3"#,
            local_only, limit, offset,
        )
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(batch_accounts_to_api(&state, &accounts).await))
}

// ── GET /api/v1/accounts (batch lookup) ──────────────────────────────────

pub async fn get_accounts_batch(
    State(state): State<AppState>,
    RawQuery(qs): RawQuery,
) -> AppResult<Json<Vec<ApiAccount>>> {
    // serde_urlencoded treats id[]=v1&id[]=v2 as a duplicate field → 400.
    // Parse with form_urlencoded which correctly returns each pair separately.
    let ids: Vec<i64> = url::form_urlencoded::parse(
            qs.as_deref().unwrap_or("").as_bytes()
        )
        .filter(|(k, _)| k == "id[]" || k == "id")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    if ids.is_empty() {
        return Ok(Json(vec![]));
    }
    let accounts = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[]) ORDER BY created_at DESC",
        &ids,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(batch_accounts_to_api(&state, &accounts).await))
}

// ── GET /api/v1/accounts/:id/lists ───────────────────────────────────────

pub async fn get_account_lists(
    State(state): State<AppState>,
    Path(target_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Vec<super::types::List>>> {
    auth.require_scope("read:lists")?;
    let rows = sqlx::query!(
        r#"SELECT l.id, l.title, l.exclusive,
                  CASE l.replies_policy WHEN 0 THEN 'followed' WHEN 1 THEN 'list' WHEN 2 THEN 'none' ELSE 'list' END AS "replies_policy!"
           FROM lists l
           JOIN list_accounts la ON la.list_id = l.id
           WHERE l.account_id = $1 AND la.account_id = $2
           ORDER BY l.id"#,
        auth.account_id,
        target_id,
    )
    .fetch_all(&state.db)
    .await?;

    let lists = rows
        .into_iter()
        .map(|r| super::types::List {
            id: r.id.to_string(),
            title: r.title,
            replies_policy: r.replies_policy,
            exclusive: r.exclusive,
        })
        .collect();

    Ok(Json(lists))
}

// ── Tag / mention fetchers ─────────────────────────────────────────────────

pub async fn fetch_statuses_tags(
    state: &AppState,
    status_id: i64,
) -> AppResult<Vec<super::types::StatusTag>> {
    let domain = &state.instance.domain;
    let rows = sqlx::query!(
        r#"SELECT t.name
           FROM tags t
           JOIN statuses_tags st ON st.tag_id = t.id
           WHERE st.status_id = $1
           ORDER BY t.name ASC"#,
        status_id,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|r| {
        let tag_lower = r.name.to_lowercase();
        super::types::StatusTag {
            url: format!("https://{}/tags/{}", domain, urlencoding::encode(&tag_lower)),
            name: r.name,
        }
    }).collect())
}

pub async fn fetch_status_mentions(
    state: &AppState,
    status_id: i64,
) -> AppResult<Vec<super::types::StatusMention>> {
    let rows = sqlx::query!(
        r#"SELECT a.id as account_id, a.username, a.domain, a.url
           FROM accounts a
           JOIN mentions m ON m.account_id = a.id
           WHERE m.status_id = $1
           ORDER BY m.id ASC"#,
        status_id,
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|r| super::types::StatusMention {
        id: r.account_id.to_string(),
        acct: match &r.domain {
            Some(d) => format!("{}@{}", r.username, d),
            None => r.username.clone(),
        },
        url: r.url.unwrap_or_default(),
        username: r.username,
    }).collect())
}

pub async fn batch_statuses_tags(
    state: &AppState,
    status_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, Vec<super::types::StatusTag>>> {
    if status_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let domain = &state.instance.domain;
    let rows = sqlx::query!(
        r#"SELECT st.status_id, t.name
           FROM tags t
           JOIN statuses_tags st ON st.tag_id = t.id
           WHERE st.status_id = ANY($1::bigint[])
           ORDER BY t.name ASC"#,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let mut map: std::collections::HashMap<i64, Vec<super::types::StatusTag>> = std::collections::HashMap::new();
    for r in rows {
        let tag_lower = r.name.to_lowercase();
        map.entry(r.status_id).or_default().push(super::types::StatusTag {
            url: format!("https://{}/tags/{}", domain, urlencoding::encode(&tag_lower)),
            name: r.name,
        });
    }
    Ok(map)
}

pub async fn batch_status_mentions(
    state: &AppState,
    status_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, Vec<super::types::StatusMention>>> {
    if status_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let rows = sqlx::query!(
        r#"SELECT m.status_id, a.id as account_id, a.username, a.domain, a.url
           FROM accounts a
           JOIN mentions m ON m.account_id = a.id
           WHERE m.status_id = ANY($1::bigint[])
           ORDER BY m.id ASC"#,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let mut map: std::collections::HashMap<i64, Vec<super::types::StatusMention>> = std::collections::HashMap::new();
    for r in rows {
        map.entry(r.status_id).or_default().push(super::types::StatusMention {
            id: r.account_id.to_string(),
            acct: match &r.domain {
                Some(d) => format!("{}@{}", r.username, d),
                None => r.username.clone(),
            },
            url: r.url.unwrap_or_default(),
            username: r.username,
        });
    }
    Ok(map)
}

pub async fn batch_status_emojis(
    state: &AppState,
    statuses: &[crate::db::models::Status],
) -> AppResult<std::collections::HashMap<i64, Vec<super::types::CustomEmoji>>> {
    if statuses.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    fn extract_shortcodes(text: &str) -> Vec<String> {
        let mut codes = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find(':') {
            rest = &rest[start + 1..];
            if let Some(end) = rest.find(':') {
                let code = &rest[..end];
                if !code.is_empty() && code.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    codes.push(code.to_string());
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
        codes
    }

    // Collect all shortcodes per status
    let mut status_codes: Vec<(i64, Vec<String>)> = Vec::new();
    for s in statuses {
        let combined = format!("{} {}", s.spoiler_text, s.text);
        let codes = extract_shortcodes(&combined);
        if !codes.is_empty() {
            status_codes.push((s.id, codes));
        }
    }

    let mut map: std::collections::HashMap<i64, Vec<super::types::CustomEmoji>> = std::collections::HashMap::new();

    if status_codes.is_empty() {
        return Ok(map);
    }

    let all_codes: Vec<String> = status_codes.iter()
        .flat_map(|(_, codes)| codes.iter().cloned())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let rows = sqlx::query!(
        r#"SELECT shortcode, image_remote_url, visible_in_picker
           FROM custom_emojis
           WHERE shortcode = ANY($1) AND domain IS NULL AND NOT disabled"#,
        &all_codes,
    )
    .fetch_all(&state.db)
    .await?;

    let emoji_by_code: std::collections::HashMap<String, super::types::CustomEmoji> = rows
        .into_iter()
        .map(|r| {
            let url = r.image_remote_url.unwrap_or_default();
            (r.shortcode.clone(), super::types::CustomEmoji {
                shortcode: r.shortcode,
                url: url.clone(),
                static_url: url,
                visible_in_picker: r.visible_in_picker,
                category: None,
                featured: None,
            })
        })
        .collect();

    for (status_id, codes) in status_codes {
        let unique_codes: std::collections::HashSet<&String> = codes.iter().collect();
        let emojis: Vec<super::types::CustomEmoji> = unique_codes.iter()
            .filter_map(|c| emoji_by_code.get(*c).cloned())
            .collect();
        if !emojis.is_empty() {
            map.insert(status_id, emojis);
        }
    }

    Ok(map)
}

/// Batch-fetch polls for a list of status IDs. Returns map from status_id → Poll.
pub async fn batch_status_polls(
    state: &AppState,
    status_ids: &[i64],
    viewer_id: Option<i64>,
) -> AppResult<std::collections::HashMap<i64, super::types::Poll>> {
    use std::collections::HashMap;

    if status_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query!(
        r#"SELECT id, status_id, options, multiple, expires_at
           FROM polls WHERE status_id = ANY($1::bigint[])"#,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(HashMap::new());
    }

    let poll_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

    // Batch-fetch per-option vote counts live from poll_votes.
    struct OptionCount { poll_id: i64, choice: i32, cnt: i64 }
    let option_counts: Vec<OptionCount> = sqlx::query_as!(
        OptionCount,
        "SELECT poll_id, choice, COUNT(*)::bigint AS \"cnt!\" FROM poll_votes WHERE poll_id = ANY($1::bigint[]) GROUP BY poll_id, choice",
        &poll_ids,
    )
    .fetch_all(&state.db)
    .await?;

    let mut counts_by_poll_option: HashMap<(i64, i32), i64> = HashMap::new();
    for c in option_counts {
        counts_by_poll_option.insert((c.poll_id, c.choice), c.cnt);
    }

    // Batch-fetch total votes and unique voters per poll.
    struct PollTotals { poll_id: i64, votes: i64, voters: i64 }
    let totals: Vec<PollTotals> = sqlx::query_as!(
        PollTotals,
        r#"SELECT poll_id, COUNT(*)::bigint AS "votes!", COUNT(DISTINCT account_id)::bigint AS "voters!" FROM poll_votes WHERE poll_id = ANY($1::bigint[]) GROUP BY poll_id"#,
        &poll_ids,
    )
    .fetch_all(&state.db)
    .await?;

    let mut totals_map: HashMap<i64, (i64, i64)> = HashMap::new();
    for t in totals {
        totals_map.insert(t.poll_id, (t.votes, t.voters));
    }

    // Batch-fetch the viewer's own votes.
    let vote_rows = if let Some(vid) = viewer_id {
        sqlx::query!(
            "SELECT poll_id, choice FROM poll_votes WHERE poll_id = ANY($1::bigint[]) AND account_id = $2 ORDER BY poll_id, choice",
            &poll_ids, vid,
        )
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };

    let mut votes_by_poll: HashMap<i64, Vec<i32>> = HashMap::new();
    for v in vote_rows {
        votes_by_poll.entry(v.poll_id).or_default().push(v.choice);
    }

    let now = chrono::Utc::now().naive_utc();
    let mut result = HashMap::new();
    for row in rows {
        let expired = row.expires_at.is_some_and(|t| t < now);
        let option_titles: Vec<String> = row.options;
        let options: Vec<super::types::PollOption> = option_titles.iter().enumerate().map(|(i, title)| {
            let cnt = *counts_by_poll_option.get(&(row.id, i as i32)).unwrap_or(&0);
            super::types::PollOption { title: title.clone(), votes_count: Some(cnt) }
        }).collect();

        let (votes_count, voters_count) = totals_map.get(&row.id)
            .map(|&(v, u)| (v, u))
            .unwrap_or((0, 0));
        let voters_count = if row.multiple { Some(voters_count) } else { None };

        let (voted, own_votes) = if viewer_id.is_some() {
            let votes = votes_by_poll.get(&row.id).cloned().unwrap_or_default();
            if votes.is_empty() {
                (Some(false), None)
            } else {
                (Some(true), Some(votes))
            }
        } else {
            (None, None)
        };
        result.insert(row.status_id, super::types::Poll {
            id: row.id.to_string(),
            expires_at: row.expires_at.map(super::convert::mastodon_date),
            expired,
            multiple: row.multiple,
            votes_count,
            voters_count,
            options,
            emojis: vec![],
            voted,
            own_votes,
        });
    }
    Ok(result)
}

/// Batch-fetch preview cards for a list of status IDs. Returns map from status_id → PreviewCard.
pub async fn batch_status_cards(
    state: &AppState,
    status_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, super::types::PreviewCard>> {
    use std::collections::HashMap;

    if status_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query!(
        r#"SELECT spc.status_id, pc.url, pc.title, pc.description,
                  CASE pc.type WHEN 1 THEN 'photo' WHEN 2 THEN 'video' WHEN 3 THEN 'rich' ELSE 'link' END as "card_type!",
                  NULL::text as image_url, pc.author_name, pc.author_url,
                  pc.provider_name, pc.provider_url, pc.html, pc.width, pc.height,
                  pc.embed_url, pc.blurhash
           FROM preview_cards_statuses spc
           JOIN preview_cards pc ON pc.id = spc.preview_card_id
           WHERE spc.status_id = ANY($1::bigint[])"#,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?;

    let mut result = HashMap::new();
    for r in rows {
        result.entry(r.status_id).or_insert_with(|| super::types::PreviewCard {
            url: r.url,
            title: r.title,
            description: r.description,
            language: None,
            card_type: r.card_type,
            author_name: r.author_name,
            author_url: r.author_url,
            provider_name: r.provider_name,
            provider_url: r.provider_url,
            html: r.html,
            width: r.width,
            height: r.height,
            image: r.image_url,
            image_description: String::new(),
            embed_url: r.embed_url,
            blurhash: r.blurhash,
            published_at: None,
            authors: vec![],
            missing_attribution: None,
            history: None,
        });
    }
    Ok(result)
}

/// Builds a `Status` API object with tags and mentions populated from the DB.
pub async fn build_status(
    state: &AppState,
    s: &crate::db::models::Status,
    account: &Account,
    media: Vec<crate::db::models::MediaAttachment>,
    reblog: Option<(crate::db::models::Status, Account, Vec<crate::db::models::MediaAttachment>)>,
    viewer_ctx: Option<super::convert::StatusViewerContext>,
) -> AppResult<super::types::Status> {
    build_status_with_app(state, s, account, media, reblog, viewer_ctx, None).await
}

pub async fn build_status_with_app(
    state: &AppState,
    s: &crate::db::models::Status,
    account: &Account,
    media: Vec<crate::db::models::MediaAttachment>,
    reblog: Option<(crate::db::models::Status, Account, Vec<crate::db::models::MediaAttachment>)>,
    viewer_ctx: Option<super::convert::StatusViewerContext>,
    application: Option<super::types::Application>,
) -> AppResult<super::types::Status> {
    let viewer_account_id = viewer_ctx.as_ref().map(|c| c.account_id);

    // Pre-fetch mentions and emojis for content rendering and API fields
    let mentions = fetch_status_mentions(state, s.id).await?;
    let status_emojis = fetch_status_emojis(state, s).await;
    let (reblog_mentions, reblog_emojis) = if let Some((ref rs, _, _)) = reblog {
        (
            fetch_status_mentions(state, rs.id).await?,
            fetch_status_emojis(state, rs).await,
        )
    } else {
        (vec![], vec![])
    };

    let mut api = super::convert::status_from_db_with_app(
        s, account, media, reblog, viewer_ctx, application, &mentions, &reblog_mentions,
    );
    let id: i64 = api.id.parse().unwrap_or(0);
    api.account.emojis = fetch_account_emojis(state, account).await;
    api.account.roles = fetch_account_roles(state, account.id).await;
    api.tags = fetch_statuses_tags(state, id).await?;
    api.mentions = mentions;
    api.emojis = status_emojis;
    api.poll = fetch_status_poll(state, id, viewer_account_id).await?;
    api.card = fetch_status_card(state, id).await;
    if let Some(st) = sqlx::query!(
        "SELECT replies_count, reblogs_count, favourites_count, quotes_count
         FROM status_stats WHERE status_id = $1",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    {
        api.replies_count = st.replies_count;
        api.reblogs_count = st.reblogs_count;
        api.favourites_count = st.favourites_count;
        api.quotes_count = st.quotes_count;
    }
    // Populate quoted status if present (check quotes table)
    {
        let quote_statuses = vec![s.clone()];
        let qmap = batch_quote_data(state, &quote_statuses, viewer_account_id).await?;
        if let Some(qi) = qmap.into_values().next() {
            api.quote = Some(qi);
        }
    }
    if let Some(ref mut rb) = api.reblog {
        let rid: i64 = rb.id.parse().unwrap_or(0);
        let rb_account_id: i64 = rb.account.id.parse().unwrap_or(0);
        if rb_account_id != 0 {
            if let Ok(rb_db_acct) = fetch_account(state, rb_account_id).await {
                rb.account.emojis = fetch_account_emojis(state, &rb_db_acct).await;
                rb.account.roles = fetch_account_roles(state, rb_account_id).await;
            }
        }
        rb.tags = fetch_statuses_tags(state, rid).await?;
        rb.mentions = reblog_mentions;
        rb.emojis = reblog_emojis;
        rb.poll = fetch_status_poll(state, rid, None).await?;
        rb.card = fetch_status_card(state, rid).await;
        if let Some(st) = sqlx::query!(
            "SELECT replies_count, reblogs_count, favourites_count, quotes_count
             FROM status_stats WHERE status_id = $1",
            rid,
        )
        .fetch_optional(&state.db)
        .await?
        {
            rb.replies_count = st.replies_count;
            rb.reblogs_count = st.reblogs_count;
            rb.favourites_count = st.favourites_count;
            rb.quotes_count = st.quotes_count;
        }
    }
    Ok(api)
}

/// Extract `:shortcode:` patterns from status text + spoiler and look them up
/// in `custom_emojis` for the status's instance.
async fn fetch_status_emojis(
    state: &AppState,
    s: &crate::db::models::Status,
) -> Vec<super::types::CustomEmoji> {
    let combined = format!("{} {}", s.spoiler_text, s.text);
    let shortcodes: Vec<&str> = {
        let mut v = Vec::new();
        let mut rest = combined.as_str();
        while let Some(start) = rest.find(':') {
            rest = &rest[start + 1..];
            if let Some(end) = rest.find(':') {
                let code = &rest[..end];
                if !code.is_empty() && code.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    v.push(code);
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
        v
    };

    if shortcodes.is_empty() {
        return vec![];
    }

    let rows = sqlx::query!(
        r#"SELECT shortcode, image_remote_url, visible_in_picker
           FROM custom_emojis
           WHERE shortcode = ANY($1) AND domain IS NULL AND NOT disabled"#,
        &shortcodes.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    rows.into_iter().map(|r| {
        let url = r.image_remote_url.unwrap_or_default();
        super::types::CustomEmoji {
            shortcode: r.shortcode,
            url: url.clone(),
            static_url: url,
            visible_in_picker: r.visible_in_picker,
            category: None,
            featured: None,
        }
    }).collect()
}

/// Extract `:shortcode:` patterns from account profile fields and look them up.
pub async fn fetch_account_emojis(
    state: &AppState,
    a: &Account,
) -> Vec<super::types::CustomEmoji> {
    let mut combined = format!("{} {}", a.display_name, a.note);
    if let Some(fields) = a.fields.as_ref().and_then(|f| f.as_array()) {
        for f in fields {
            if let (Some(n), Some(v)) = (f["name"].as_str(), f["value"].as_str()) {
                combined.push(' ');
                combined.push_str(n);
                combined.push(' ');
                combined.push_str(v);
            }
        }
    }
    let mut shortcodes: Vec<String> = Vec::new();
    let mut rest = combined.as_str();
    while let Some(start) = rest.find(':') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find(':') {
            let code = &rest[..end];
            if !code.is_empty() && code.chars().all(|c| c.is_alphanumeric() || c == '_') {
                shortcodes.push(code.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    if shortcodes.is_empty() {
        return vec![];
    }
    let rows = sqlx::query!(
        r#"SELECT shortcode, image_remote_url, visible_in_picker
           FROM custom_emojis
           WHERE shortcode = ANY($1) AND domain IS NULL AND NOT disabled"#,
        &shortcodes,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    rows.into_iter().map(|r| {
        let url = r.image_remote_url.unwrap_or_default();
        super::types::CustomEmoji {
            shortcode: r.shortcode,
            url: url.clone(),
            static_url: url,
            visible_in_picker: r.visible_in_picker,
            category: None,
            featured: None,
        }
    }).collect()
}

/// Extract emoji shortcodes from account profile text.
fn extract_account_shortcodes(a: &Account) -> Vec<String> {
    let mut combined = format!("{} {}", a.display_name, a.note);
    if let Some(fields) = a.fields.as_ref().and_then(|f| f.as_array()) {
        for f in fields {
            if let (Some(n), Some(v)) = (f["name"].as_str(), f["value"].as_str()) {
                combined.push(' ');
                combined.push_str(n);
                combined.push(' ');
                combined.push_str(v);
            }
        }
    }
    let mut codes = Vec::new();
    let mut rest = combined.as_str();
    while let Some(start) = rest.find(':') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find(':') {
            let code = &rest[..end];
            if !code.is_empty() && code.chars().all(|c| c.is_alphanumeric() || c == '_') {
                codes.push(code.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    codes
}

/// Batch-fetch account profile emojis for multiple accounts in one DB query.
pub async fn batch_account_emojis(
    state: &AppState,
    accounts: &[Account],
) -> std::collections::HashMap<i64, Vec<super::types::CustomEmoji>> {
    if accounts.is_empty() {
        return std::collections::HashMap::new();
    }

    // Collect all shortcodes per account
    let mut account_shortcodes: std::collections::HashMap<i64, Vec<String>> =
        std::collections::HashMap::new();
    let mut all_shortcodes_set: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for a in accounts {
        let codes = extract_account_shortcodes(a);
        if !codes.is_empty() {
            all_shortcodes_set.extend(codes.iter().cloned());
            account_shortcodes.insert(a.id, codes);
        }
    }

    if all_shortcodes_set.is_empty() {
        return std::collections::HashMap::new();
    }

    let all_shortcodes: Vec<String> = all_shortcodes_set.into_iter().collect();

    let rows = sqlx::query!(
        r#"SELECT shortcode, image_remote_url, visible_in_picker
           FROM custom_emojis
           WHERE disabled = false
             AND domain IS NULL
             AND shortcode = ANY($1)"#,
        &all_shortcodes as &[String],
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    // Build a map: shortcode → CustomEmoji
    let emoji_lookup: std::collections::HashMap<String, super::types::CustomEmoji> = rows
        .into_iter()
        .map(|r| {
            let url = r.image_remote_url.unwrap_or_default();
            let emoji = super::types::CustomEmoji {
                shortcode: r.shortcode.clone(),
                url: url.clone(),
                static_url: url,
                visible_in_picker: r.visible_in_picker,
                category: None,
                featured: None,
            };
            (r.shortcode, emoji)
        })
        .collect();

    // Build result map: account_id → emojis
    let mut result = std::collections::HashMap::new();
    for a in accounts {
        if let Some(codes) = account_shortcodes.get(&a.id) {
            let mut seen = std::collections::HashSet::new();
            let emojis: Vec<_> = codes.iter()
                .filter(|code| seen.insert(*code))
                .filter_map(|code| emoji_lookup.get(code).cloned())
                .collect();
            if !emojis.is_empty() {
                result.insert(a.id, emojis);
            }
        }
    }
    result
}

/// Batch-fetch role badges for a set of accounts. Only local accounts (domain IS NULL)
/// can have roles. Returns a map of account_id → role list (empty for non-admin/moderator).
pub async fn batch_account_roles(
    state: &AppState,
    accounts: &[Account],
) -> std::collections::HashMap<i64, Vec<super::types::AccountRole>> {
    let local_ids: Vec<i64> = accounts.iter()
        .filter(|a| a.domain.is_none())
        .map(|a| a.id)
        .collect();
    if local_ids.is_empty() {
        return std::collections::HashMap::new();
    }
    sqlx::query!(
        r#"SELECT u.account_id, ur.id AS "role_id!", ur.name AS "role_name!", ur.color AS "role_color!"
           FROM users u
           JOIN user_roles ur ON ur.id = u.role_id
           WHERE u.account_id = ANY($1::bigint[])"#,
        &local_ids,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| {
        let roles = vec![super::types::AccountRole {
            id: r.role_id.to_string(),
            name: r.role_name,
            color: r.role_color,
        }];
        (r.account_id, roles)
    })
    .collect()
}

/// Look up an already-cached preview card for a status. Never does network I/O.
pub(super) async fn fetch_status_card(
    state: &AppState,
    status_id: i64,
) -> Option<super::types::PreviewCard> {
    let r = sqlx::query!(
        r#"SELECT pc.url, pc.title, pc.description,
                  CASE pc.type WHEN 1 THEN 'photo' WHEN 2 THEN 'video' WHEN 3 THEN 'rich' ELSE 'link' END as "card_type!",
                  NULL::text as image_url, pc.author_name, pc.author_url,
                  pc.provider_name, pc.provider_url, pc.html, pc.width, pc.height,
                  pc.embed_url, pc.blurhash
           FROM preview_cards pc
           JOIN preview_cards_statuses spc ON spc.preview_card_id = pc.id
           WHERE spc.status_id = $1
           LIMIT 1"#,
        status_id,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()?;

    Some(super::types::PreviewCard {
        url: r.url,
        title: r.title,
        description: r.description,
        language: None,
        card_type: r.card_type,
        author_name: r.author_name,
        author_url: r.author_url,
        provider_name: r.provider_name,
        provider_url: r.provider_url,
        html: r.html,
        width: r.width,
        height: r.height,
        embed_url: r.embed_url,
        image: r.image_url,
        image_description: String::new(),
        blurhash: r.blurhash,
        published_at: None,
        authors: vec![],
        missing_attribution: None,
        history: None,
    })
}

/// Convert a slice of DB accounts to API accounts with profile emojis and roles populated.
pub async fn batch_accounts_to_api(
    state: &AppState,
    accounts: &[Account],
) -> Vec<super::types::Account> {
    let emojis_map = batch_account_emojis(state, accounts).await;
    let roles_map = batch_account_roles(state, accounts).await;
    let ids: Vec<i64> = accounts.iter().map(|a| a.id).collect();
    let stats_map: std::collections::HashMap<i64, (i64, i64, i64)> = sqlx::query!(
        "SELECT account_id, statuses_count, following_count, followers_count
         FROM account_stats WHERE account_id = ANY($1::bigint[])",
        &ids,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| (r.account_id, (r.statuses_count, r.following_count, r.followers_count)))
    .collect();
    accounts.iter().map(|a| {
        let mut api = super::convert::account_from_db(a);
        api.emojis = emojis_map.get(&a.id).cloned().unwrap_or_default();
        api.roles = roles_map.get(&a.id).cloned().unwrap_or_default();
        if let Some(&(s, fg, fr)) = stats_map.get(&a.id) {
            api.statuses_count = s;
            api.following_count = fg;
            api.followers_count = fr;
        }
        api
    }).collect()
}

/// Read a user's stored preferences from `users.settings` (a JSON object).
pub async fn user_settings_json(state: &AppState, account_id: i64) -> serde_json::Value {
    let raw = sqlx::query!("SELECT settings FROM users WHERE account_id = $1", account_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.settings);
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// A user's default posting preferences, read from `users.settings`.
pub struct UserDefaults {
    pub privacy: String,
    pub sensitive: bool,
    pub language: Option<String>,
    pub quote_policy: String,
}

pub async fn user_defaults(state: &AppState, account_id: i64) -> UserDefaults {
    let s = user_settings_json(state, account_id).await;
    UserDefaults {
        privacy: s.get("privacy").and_then(|v| v.as_str()).unwrap_or("public").to_string(),
        sensitive: s.get("sensitive").and_then(|v| v.as_bool()).unwrap_or(false),
        language: s.get("language").and_then(|v| v.as_str()).map(String::from),
        quote_policy: s.get("quote_policy").and_then(|v| v.as_str()).unwrap_or("public").to_string(),
    }
}

/// Populate an account entity's `statuses_count` / `following_count` /
/// `followers_count` from the `account_stats` table.
pub async fn apply_account_stats(state: &AppState, api: &mut super::types::Account, account_id: i64) {
    if let Ok(Some(st)) = sqlx::query!(
        "SELECT statuses_count, following_count, followers_count
         FROM account_stats WHERE account_id = $1",
        account_id,
    )
    .fetch_optional(&state.db)
    .await
    {
        api.statuses_count = st.statuses_count;
        api.following_count = st.following_count;
        api.followers_count = st.followers_count;
    }
}

/// Spawn a background task to fetch a preview card for a newly-created status.
/// Only fetches the first external URL found in the HTML content.
pub fn spawn_card_fetch(state: &AppState, status_id: i64, content: String) {
    let urls = crate::preview_card::extract_urls_from_content(&content);
    let url = match urls.into_iter().next() {
        Some(u) => u,
        None => return,
    };
    let state = state.clone();
    tokio::spawn(async move {
        let Some(card_id) = crate::preview_card::fetch_and_store(&state.db, &state.fetch, &url).await
        else {
            return;
        };
        let _ = sqlx::query!(
            "INSERT INTO preview_cards_statuses (status_id, preview_card_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            status_id,
            card_id,
        )
        .execute(&state.db)
        .await;
    });
}

// ── DELETE /api/v1/accounts ────────────────────────────────────────────────

pub async fn delete_account(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<serde_json::Value>>,
) -> AppResult<axum::http::StatusCode> {
    auth.require_scope("write:accounts")?;
    let password = body.as_ref()
        .and_then(|b| b.get("password"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let user = sqlx::query!(
        "SELECT encrypted_password FROM users WHERE account_id = $1",
        auth.account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    crate::crypto::verify_password(password, &user.encrypted_password)?;

    // Soft-delete: mark account as suspended, revoke tokens, remove user row.
    // Hard delete of statuses/follows is deferred (could be a background job).
    let mut tx = state.db.begin().await?;
    sqlx::query!(
        "UPDATE statuses SET deleted_at = now() WHERE account_id = $1 AND deleted_at IS NULL",
        auth.account_id,
    ).execute(&mut *tx).await?;
    sqlx::query!(
        r#"UPDATE oauth_access_tokens t
           SET revoked_at = now()
           FROM users u
           WHERE u.id = t.resource_owner_id
             AND u.account_id = $1
             AND t.revoked_at IS NULL"#,
        auth.account_id,
    ).execute(&mut *tx).await?;
    sqlx::query!(
        "UPDATE accounts SET suspended_at = now() WHERE id = $1",
        auth.account_id,
    ).execute(&mut *tx).await?;
    sqlx::query!(
        "DELETE FROM users WHERE account_id = $1",
        auth.account_id,
    ).execute(&mut *tx).await?;
    tx.commit().await?;

    Ok(axum::http::StatusCode::OK)
}

// ── GET /api/v1/donation_campaigns ───────────────────────────────────────

pub async fn list_donation_campaigns() -> Json<Vec<serde_json::Value>> {
    Json(vec![])
}

// ── GET /api/v1/accounts/:id/identity_proofs ─────────────────────────────

pub async fn get_account_identity_proofs(
    Path(_id): Path<i64>,
) -> Json<Vec<serde_json::Value>> {
    Json(vec![])
}
