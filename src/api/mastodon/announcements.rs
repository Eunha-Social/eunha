use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};

use crate::{
    error::AppResult,
    middleware::AuthenticatedUser,
    state::AppState,
};
use super::formatting::text_to_html;
use super::types::{Announcement, AnnouncementReaction};

// ── GET /api/v1/announcements ─────────────────────────────────────────────

pub async fn get_announcements(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<Announcement>>> {
    let viewer_id = auth.map(|Extension(a)| a.account_id);

    let rows = sqlx::query!(
        r#"SELECT id, text, published, all_day, starts_at, ends_at,
                  published_at, created_at, updated_at
           FROM announcements
           WHERE published = true
             AND (ends_at IS NULL OR ends_at > now())
           ORDER BY published_at DESC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let ann_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();

    // Batch-fetch dismissed announcements for the viewer
    let dismissed_set: std::collections::HashSet<i64> = if let Some(vid) = viewer_id {
        sqlx::query_scalar!(
            "SELECT announcement_id FROM announcement_mutes WHERE account_id = $1 AND announcement_id = ANY($2::bigint[])",
            vid, &ann_ids,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Batch-fetch all reactions for all announcements
    let all_reactions = sqlx::query!(
        r#"SELECT ar.announcement_id, ar.name,
                  COUNT(*) AS "count!",
                  ce.image_remote_url AS "image_remote_url?"
           FROM announcement_reactions ar
           LEFT JOIN custom_emojis ce ON ce.id = ar.custom_emoji_id
           WHERE ar.announcement_id = ANY($1::bigint[])
           GROUP BY ar.announcement_id, ar.name, ce.image_remote_url
           ORDER BY ar.announcement_id, ar.name"#,
        &ann_ids,
    )
    .fetch_all(&state.db)
    .await?;

    // Batch-fetch the viewer's own reactions
    let my_reactions: std::collections::HashSet<(i64, String)> = if let Some(vid) = viewer_id {
        sqlx::query!(
            "SELECT announcement_id, name FROM announcement_reactions WHERE account_id = $1 AND announcement_id = ANY($2::bigint[])",
            vid, &ann_ids,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|r| (r.announcement_id, r.name))
        .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Group reactions by announcement_id
    let mut reactions_by_ann: std::collections::HashMap<i64, Vec<AnnouncementReaction>> =
        std::collections::HashMap::new();
    for row in all_reactions {
        let me = my_reactions.contains(&(row.announcement_id, row.name.clone()));
        let url = row.image_remote_url;
        reactions_by_ann.entry(row.announcement_id).or_default().push(AnnouncementReaction {
            name: row.name,
            count: row.count,
            me,
            url: url.clone(),
            static_url: url,
        });
    }

    let mut result = Vec::with_capacity(rows.len());
    for r in &rows {
        result.push(Announcement {
            id: r.id.to_string(),
            content: text_to_html(&r.text),
            all_day: r.all_day,
            starts_at: r.starts_at.map(super::convert::mastodon_date),
            ends_at: r.ends_at.map(super::convert::mastodon_date),
            published_at: r.published_at.map(super::convert::mastodon_date).unwrap_or_default(),
            updated_at: super::convert::mastodon_date(r.updated_at),
            read: if viewer_id.is_some() { Some(dismissed_set.contains(&r.id)) } else { None },
            reactions: reactions_by_ann.remove(&r.id).unwrap_or_default(),
            statuses: vec![],
            tags: vec![],
            emojis: vec![],
            mentions: vec![],
        });
    }

    Ok(Json(result))
}

// ── POST /api/v1/announcements/:id/dismiss ────────────────────────────────

pub async fn dismiss_announcement(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<StatusCode> {
    auth.require_scope("write:accounts")?;

    let exists = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM announcements WHERE id = $1 AND published = true)",
        id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if !exists {
        return Err(crate::error::AppError::NotFound);
    }

    sqlx::query!(
        "INSERT INTO announcement_mutes (announcement_id, account_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        id, auth.account_id,
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::OK)
}

// ── PUT /api/v1/announcements/:id/reactions/:name ─────────────────────────

pub async fn add_reaction(
    State(state): State<AppState>,
    Path((id, name)): Path<(i64, String)>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<StatusCode> {
    auth.require_scope("write:favourites")?;

    // Mastodon's set_announcement only finds published announcements (404 otherwise).
    let published = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM announcements WHERE id = $1 AND published = true)",
        id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if !published {
        return Err(crate::error::AppError::NotFound);
    }

    // AnnouncementReaction#set_custom_emoji resolves only local, enabled emoji
    // (CustomEmoji.local.enabled): remote copies must not back a reaction.
    let custom_emoji_id = sqlx::query_scalar!(
        r#"SELECT id FROM custom_emojis
           WHERE shortcode = $1
             AND domain IS NULL
             AND NOT disabled"#,
        name,
    )
    .fetch_optional(&state.db)
    .await?;

    // ReactionValidator: a name that is neither a known custom emoji nor a
    // supported unicode emoji is rejected (422).
    if custom_emoji_id.is_none() && emojis::get(&name).is_none() {
        return Err(crate::error::AppError::Unprocessable("Unrecognized emoji".into()));
    }

    // ReactionValidator LIMIT = 8 distinct reaction names per announcement,
    // enforced only when introducing a brand-new reaction name.
    let name_exists = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM announcement_reactions WHERE announcement_id = $1 AND name = $2)",
        id, name,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);
    if !name_exists {
        let distinct = sqlx::query_scalar!(
            "SELECT COUNT(DISTINCT name) FROM announcement_reactions WHERE announcement_id = $1 AND name <> $2",
            id, name,
        )
        .fetch_one(&state.db)
        .await?
        .unwrap_or(0);
        if distinct >= 8 {
            return Err(crate::error::AppError::Unprocessable("Reaction limit reached".into()));
        }
    }

    sqlx::query!(
        r#"INSERT INTO announcement_reactions (announcement_id, account_id, name, custom_emoji_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, now(), now())
           ON CONFLICT (announcement_id, account_id, name) DO NOTHING"#,
        id, auth.account_id, name, custom_emoji_id,
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::OK)
}

// ── DELETE /api/v1/announcements/:id/reactions/:name ─────────────────────

pub async fn remove_reaction(
    State(state): State<AppState>,
    Path((id, name)): Path<(i64, String)>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<StatusCode> {
    auth.require_scope("write:favourites")?;
    sqlx::query!(
        "DELETE FROM announcement_reactions WHERE announcement_id = $1 AND account_id = $2 AND name = $3",
        id, auth.account_id, name,
    )
    .execute(&state.db)
    .await?;

    Ok(StatusCode::OK)
}
