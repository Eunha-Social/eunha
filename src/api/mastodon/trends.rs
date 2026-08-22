use axum::{
    extract::{Extension, Query, State},
    Json,
};
use serde::Deserialize;

use super::{
    accounts::{batch_account_emojis, batch_account_roles},
    convert::status_from_db,
    status_serialize::{
        batch_reblog_data, batch_status_cards, batch_status_emojis, batch_status_media,
        batch_status_mentions, batch_status_polls, batch_statuses_tags, hydrate_status_stats,
    },
    types::{Status, Tag},
};
use crate::{
    error::AppResult,
    middleware::{AuthenticatedUser, ResolvedInstance},
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct TrendParams {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// ── GET /api/v1/trends/tags  &  GET /api/v1/trends ────────────────────────

pub async fn trending_tags(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Query(params): Query<TrendParams>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<Tag>>> {
    let limit = params.limit.unwrap_or(10).clamp(1, 20);
    let offset = params.offset.unwrap_or(0).max(0);
    let domain = &instance.domain;
    let viewer_id = auth.map(|Extension(a)| a.account_id);

    // Tags with most status uses in the last 7 days
    let rows = sqlx::query!(
        r#"SELECT t.id, t.name, COUNT(st.status_id) AS uses
           FROM tags t
           JOIN statuses_tags st ON st.tag_id = t.id
           JOIN statuses s ON s.id = st.status_id
           WHERE s.deleted_at IS NULL
             AND s.visibility = 0
             AND s.created_at > now() - interval '7 days'
           GROUP BY t.id, t.name
           ORDER BY uses DESC, t.name ASC
           LIMIT $1 OFFSET $2"#,
        limit,
        offset,
    )
    .fetch_all(&state.db)
    .await?;

    let tag_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let histories = super::tags::fetch_tags_histories(&state.db, &tag_ids).await;

    let (following_set, featuring_set) =
        if let Some(vid) = viewer_id {
            let followed: std::collections::HashSet<i64> = sqlx::query_scalar!(
            "SELECT tag_id FROM tag_follows WHERE account_id = $1 AND tag_id = ANY($2::bigint[])",
            vid, &tag_ids,
        )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

            let featured: std::collections::HashSet<i64> = sqlx::query_scalar!(
            "SELECT tag_id FROM featured_tags WHERE account_id = $1 AND tag_id = ANY($2::bigint[])",
            vid, &tag_ids,
        )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

            (Some(followed), Some(featured))
        } else {
            (None, None)
        };

    let tags: Vec<Tag> = rows
        .into_iter()
        .map(|r| {
            let name_lower = r.name.to_lowercase();
            let following = following_set.as_ref().map(|s| s.contains(&r.id));
            let featuring = featuring_set.as_ref().map(|s| s.contains(&r.id));
            Tag {
                id: r.id.to_string(),
                history: histories.get(&r.id).cloned().unwrap_or_default(),
                name: r.name,
                url: format!(
                    "https://{}/tags/{}",
                    domain,
                    urlencoding::encode(&name_lower)
                ),
                following,
                featuring,
            }
        })
        .collect();

    Ok(Json(tags))
}

// ── GET /api/v1/trends/statuses ───────────────────────────────────────────

pub async fn trending_statuses(
    State(state): State<AppState>,
    Query(params): Query<TrendParams>,
    auth: Option<Extension<crate::middleware::AuthenticatedUser>>,
) -> AppResult<Json<Vec<Status>>> {
    let limit = params.limit.unwrap_or(20).clamp(1, 40);
    let offset = params.offset.unwrap_or(0).max(0);
    let viewer_id = auth.map(|Extension(a)| a.account_id);

    let rows = sqlx::query_as!(
        crate::db::models::Status,
        r#"SELECT s.* FROM statuses s
           JOIN accounts a ON a.id = s.account_id
           WHERE s.deleted_at IS NULL
             AND s.visibility = 0
             AND s.reblog_of_id IS NULL
             AND s.created_at > now() - interval '2 days'
             AND a.suspended_at IS NULL AND a.requested_deletion_at IS NULL
             AND ($3::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM blocks b
                 WHERE (b.account_id = $3 AND b.target_account_id = s.account_id)
                    OR (b.account_id = s.account_id AND b.target_account_id = $3)
             ))
             AND ($3::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mutes mu
                 WHERE mu.account_id = $3 AND mu.target_account_id = s.account_id
                   AND (mu.expires_at IS NULL OR mu.expires_at > now())
             ))
           ORDER BY (COALESCE((SELECT favourites_count FROM status_stats WHERE status_id = s.id), 0)
                   + COALESCE((SELECT reblogs_count FROM status_stats WHERE status_id = s.id), 0) * 2) DESC, s.created_at DESC
           LIMIT $1 OFFSET $2"#,
        limit,
        offset,
        viewer_id,
    )
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(Json(vec![]));
    }

    let all_ids: Vec<i64> = rows.iter().map(|s| s.id).collect();
    let media_map = batch_status_media(&state, &all_ids).await?;
    let reblog_map = batch_reblog_data(&state, &rows).await?;
    let reblog_ids: Vec<i64> = reblog_map.values().map(|(rs, _, _)| rs.id).collect();
    let mut enrich_ids = all_ids.clone();
    enrich_ids.extend_from_slice(&reblog_ids);
    let tags_map = batch_statuses_tags(&state, &enrich_ids).await?;
    let mentions_map = batch_status_mentions(&state, &enrich_ids).await?;
    let all_statuses_for_emoji: Vec<crate::db::models::Status> = rows
        .iter()
        .cloned()
        .chain(reblog_map.values().map(|(rs, _, _)| rs.clone()))
        .collect();
    let emojis_map = batch_status_emojis(&state, &all_statuses_for_emoji).await?;
    let polls_map = batch_status_polls(&state, &enrich_ids, viewer_id).await?;
    let cards_map = batch_status_cards(&state, &enrich_ids).await?;
    let ctxs = if let Some(vid) = viewer_id {
        super::statuses::batch_viewer_contexts(&state, vid, &all_ids).await?
    } else {
        std::collections::HashMap::new()
    };

    let account_ids: Vec<i64> = rows
        .iter()
        .map(|s| s.account_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let accounts: Vec<crate::db::models::Account> = sqlx::query_as!(
        crate::db::models::Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &account_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let account_map: std::collections::HashMap<i64, crate::db::models::Account> =
        accounts.iter().cloned().map(|a| (a.id, a)).collect();

    // Batch-fetch profile emojis for all accounts
    let all_accounts_for_emoji: Vec<crate::db::models::Account> = {
        let mut seen = std::collections::HashSet::new();
        account_map
            .values()
            .chain(reblog_map.values().map(|(_, ra, _)| ra))
            .filter(|a| seen.insert(a.id))
            .cloned()
            .collect()
    };
    let account_emojis_map = batch_account_emojis(&state, &all_accounts_for_emoji).await;
    let account_roles_map = batch_account_roles(&state, &all_accounts_for_emoji).await;

    let mut result = Vec::with_capacity(rows.len());
    for s in &rows {
        let Some(account) = account_map.get(&s.account_id) else {
            continue;
        };
        let media = media_map.get(&s.id).cloned().unwrap_or_default();
        let reblog = reblog_map.get(&s.id).cloned();
        let mentions = mentions_map.get(&s.id).cloned().unwrap_or_default();
        let rb_mentions = reblog
            .as_ref()
            .and_then(|(rs, _, _)| mentions_map.get(&rs.id))
            .cloned()
            .unwrap_or_default();
        let ctx = ctxs.get(&s.id).cloned();
        let mut api = status_from_db(s, account, media, reblog, ctx, &mentions, &rb_mentions);
        api.account.emojis = account_emojis_map
            .get(&account.id)
            .cloned()
            .unwrap_or_default();
        api.account.roles = account_roles_map
            .get(&account.id)
            .cloned()
            .unwrap_or_default();
        api.tags = tags_map.get(&s.id).cloned().unwrap_or_default();
        api.mentions = mentions;
        api.emojis = emojis_map.get(&s.id).cloned().unwrap_or_default();
        api.poll = polls_map.get(&s.id).cloned();
        api.card = cards_map.get(&s.id).cloned();
        if let Some(ref mut rb) = api.reblog {
            let rid: i64 = rb.id.parse().unwrap_or(0);
            let rb_id: i64 = rb.account.id.parse().unwrap_or(0);
            rb.account.emojis = account_emojis_map.get(&rb_id).cloned().unwrap_or_default();
            rb.account.roles = account_roles_map.get(&rb_id).cloned().unwrap_or_default();
            rb.tags = tags_map.get(&rid).cloned().unwrap_or_default();
            rb.mentions = rb_mentions;
            rb.emojis = emojis_map.get(&rid).cloned().unwrap_or_default();
            rb.poll = polls_map.get(&rid).cloned();
            rb.card = cards_map.get(&rid).cloned();
        }
        result.push(api);
    }
    hydrate_status_stats(&state, result.iter_mut()).await;

    Ok(Json(result))
}

// ── GET /api/v1/trends/links ──────────────────────────────────────────────

pub async fn trending_links(
    State(state): State<AppState>,
    Query(params): Query<TrendParams>,
) -> AppResult<Json<Vec<super::types::PreviewCard>>> {
    let limit = params.limit.unwrap_or(10).clamp(1, 40);
    let offset = params.offset.unwrap_or(0).max(0);

    let rows = sqlx::query!(
        r#"SELECT pc.url, pc.title, pc.description,
                  CASE pc.type WHEN 1 THEN 'photo' WHEN 2 THEN 'video' WHEN 3 THEN 'rich' ELSE 'link' END as "card_type!",
                  pc.author_name, pc.author_url, pc.provider_name, pc.provider_url,
                  pc.html, pc.width, pc.height, NULL::text as image_url, pc.embed_url, pc.blurhash,
                  COUNT(s.id) AS uses
           FROM preview_cards pc
           JOIN preview_cards_statuses spc ON spc.preview_card_id = pc.id
           JOIN statuses s ON s.id = spc.status_id
           WHERE s.deleted_at IS NULL
             AND s.visibility = 0
             AND s.created_at > now() - interval '7 days'
           GROUP BY pc.url, pc.title, pc.description, pc.type,
                    pc.author_name, pc.author_url, pc.provider_name, pc.provider_url,
                    pc.html, pc.width, pc.height, pc.embed_url, pc.blurhash
           ORDER BY uses DESC
           LIMIT $1 OFFSET $2"#,
        limit, offset,
    )
    .fetch_all(&state.db)
    .await?;

    let cards = rows
        .into_iter()
        .map(|r| super::types::PreviewCard {
            url: r.url,
            title: r.title,
            description: r.description,
            card_type: r.card_type,
            author_name: r.author_name,
            author_url: r.author_url,
            provider_name: r.provider_name,
            provider_url: r.provider_url,
            html: r.html,
            width: r.width,
            height: r.height,
            image: r.image_url,
            embed_url: r.embed_url,
            blurhash: r.blurhash,
            language: None,
            published_at: None,
            authors: vec![],
            image_description: String::new(),
            missing_attribution: None,
            history: Some(vec![]),
        })
        .collect();

    Ok(Json(cards))
}
