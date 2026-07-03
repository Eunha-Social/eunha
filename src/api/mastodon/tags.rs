use super::types::{Tag, TagHistory};
use crate::{
    error::{AppError, AppResult},
    middleware::{AuthenticatedUser, ResolvedInstance},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, Uri},
    response::{IntoResponse, Json},
    Extension,
};
use std::collections::HashMap;

// ── Tag history helpers ────────────────────────────────────────────────────

/// Returns 7 days of daily use/account counts for the given tags, newest day first.
pub(super) async fn fetch_tags_histories(
    db: &sqlx::PgPool,
    tag_ids: &[i64],
) -> HashMap<i64, Vec<TagHistory>> {
    if tag_ids.is_empty() {
        return HashMap::new();
    }
    let cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(7);
    let rows = sqlx::query!(
        r#"SELECT st.tag_id,
                  date_trunc('day', s.created_at)::date AS day,
                  COUNT(*)::bigint AS uses,
                  COUNT(DISTINCT s.account_id)::bigint AS accounts
           FROM statuses_tags st
           JOIN statuses s ON s.id = st.status_id
           WHERE st.tag_id = ANY($1::bigint[])
             AND s.deleted_at IS NULL
             AND s.visibility = 0
             AND s.created_at >= $2
           GROUP BY st.tag_id, date_trunc('day', s.created_at)::date"#,
        tag_ids as &[i64],
        cutoff,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    // Build tag_id → NaiveDate → (uses, accounts)
    let mut raw: HashMap<i64, HashMap<chrono::NaiveDate, (i64, i64)>> = HashMap::new();
    for r in rows {
        if let Some(day) = r.day {
            raw.entry(r.tag_id)
                .or_default()
                .insert(day, (r.uses.unwrap_or(0), r.accounts.unwrap_or(0)));
        }
    }

    let today = chrono::Utc::now().date_naive();
    tag_ids
        .iter()
        .map(|&tid| {
            let day_map = raw.get(&tid).cloned().unwrap_or_default();
            let history = (0..7i64)
                .map(|i| {
                    let day = today - chrono::Duration::days(i);
                    let (uses, accounts) = day_map.get(&day).copied().unwrap_or((0, 0));
                    let ts = day
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp()
                        .to_string();
                    TagHistory {
                        day: ts,
                        uses: uses.to_string(),
                        accounts: accounts.to_string(),
                    }
                })
                .collect();
            (tid, history)
        })
        .collect()
}

pub(super) async fn fetch_tag_history(db: &sqlx::PgPool, tag_id: i64) -> Vec<TagHistory> {
    fetch_tags_histories(db, &[tag_id])
        .await
        .remove(&tag_id)
        .unwrap_or_default()
}

fn tag_url(domain: &str, name: &str) -> String {
    format!("https://{domain}/tags/{name}")
}

// ── GET /api/v1/tags/:name ────────────────────────────────────────────────

pub async fn get_tag(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(name): Path<String>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Tag>> {
    let domain = &instance.domain;
    let name = name.to_lowercase();

    // Mastodon's TagsController#show uses Tag.find_normalized! which raises
    // RecordNotFound (→ 404) for hashtags that do not exist.
    let tag = sqlx::query!("SELECT id FROM tags WHERE name = $1", name,)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let (following, featuring, history, id_str) = {
        let t = &tag;
        let history = fetch_tag_history(&state.db, t.id).await;
        let (following, featuring) = if let Some(Extension(ref auth)) = auth {
            let following = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                   SELECT 1 FROM tag_follows tf
                   WHERE tf.account_id = $1 AND tf.tag_id = $2
                )"#,
                auth.account_id,
                t.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(false);

            let featuring = sqlx::query_scalar!(
                "SELECT EXISTS(SELECT 1 FROM featured_tags WHERE account_id = $1 AND tag_id = $2)",
                auth.account_id,
                t.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(false);

            (Some(following), Some(featuring))
        } else {
            (None, None)
        };
        (following, featuring, history, t.id.to_string())
    };

    Ok(Json(Tag {
        id: id_str,
        url: tag_url(domain, &name),
        name,
        history,
        following,
        featuring,
    }))
}

#[derive(Debug, serde::Deserialize)]
pub struct FollowedTagsParams {
    limit: Option<i64>,
    max_id: Option<String>,
    since_id: Option<String>,
    min_id: Option<String>,
}

pub async fn list_followed_tags(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    Query(params): Query<FollowedTagsParams>,
    uri: Uri,
    req_headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    auth.require_scope("read:follows")?;
    let domain = &instance.domain;
    let limit = params.limit.unwrap_or(100).clamp(1, 200);
    let max_id = params.max_id.as_deref().and_then(|s| s.parse::<i64>().ok());
    let since_id = params
        .since_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());
    let min_id = params.min_id.as_deref().and_then(|s| s.parse::<i64>().ok());

    let rows = sqlx::query!(
        r#"SELECT tf.id AS follow_id, t.id, t.name
           FROM tag_follows tf
           JOIN tags t ON t.id = tf.tag_id
           WHERE tf.account_id = $1
             AND ($2::bigint IS NULL OR tf.id < $2)
             AND ($3::bigint IS NULL OR tf.id > $3)
             AND ($4::bigint IS NULL OR tf.id > $4)
           ORDER BY tf.id DESC
           LIMIT $5"#,
        auth.account_id,
        max_id,
        since_id,
        min_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    // Use tag_follow id (bigint) as the pagination cursor, not the tag UUID.
    let first_follow_id = rows.first().map(|r| r.follow_id.to_string());
    let last_follow_id = rows.last().map(|r| r.follow_id.to_string());

    let tag_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let histories = fetch_tags_histories(&state.db, &tag_ids).await;

    let result: Vec<Tag> = rows
        .into_iter()
        .map(|r| Tag {
            id: r.id.to_string(),
            url: tag_url(domain, &r.name),
            history: histories.get(&r.id).cloned().unwrap_or_default(),
            name: r.name,
            following: Some(true),
            featuring: None,
        })
        .collect();

    let link = first_follow_id
        .zip(last_follow_id)
        .map(|(newest_fid, oldest_fid)| {
            let extra = super::non_pagination_query(uri.query());
            super::link_header(&req_headers, uri.path(), &extra, &newest_fid, &oldest_fid)
        });
    let mut resp_headers = HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            resp_headers.insert(header::LINK, val);
        }
    }
    Ok((resp_headers, Json(result)))
}

pub async fn follow_tag(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> AppResult<Json<Tag>> {
    auth.require_scope("write:follows")?;
    let domain = &instance.domain;
    let name = name.to_lowercase();

    // Create the tag if it doesn't already exist (matches Mastodon behaviour).
    let tag_id = sqlx::query_scalar!(
        r#"INSERT INTO tags (name, created_at, updated_at) VALUES ($1, now(), now())
           ON CONFLICT ((lower(name))) DO UPDATE SET name = EXCLUDED.name
           RETURNING id"#,
        name,
    )
    .fetch_one(&state.db)
    .await?;

    sqlx::query!(
        "INSERT INTO tag_follows (account_id, tag_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        auth.account_id,
        tag_id,
    )
    .execute(&state.db)
    .await?;

    let history = fetch_tag_history(&state.db, tag_id).await;

    let featuring = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM featured_tags WHERE account_id = $1 AND tag_id = $2)",
        auth.account_id,
        tag_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);

    Ok(Json(Tag {
        id: tag_id.to_string(),
        url: tag_url(domain, &name),
        name,
        history,
        following: Some(true),
        featuring: Some(featuring),
    }))
}

pub async fn unfollow_tag(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> AppResult<Json<Tag>> {
    auth.require_scope("write:follows")?;
    let domain = &instance.domain;
    let name = name.to_lowercase();

    let tag = sqlx::query!("SELECT id FROM tags WHERE name = $1", name,)
        .fetch_optional(&state.db)
        .await?;

    // If tag doesn't exist there's nothing to unfollow; return empty-id tag (matches Mastodon).
    let Some(tag) = tag else {
        return Ok(Json(Tag {
            id: String::new(),
            url: tag_url(domain, &name),
            name,
            history: vec![],
            following: Some(false),
            featuring: Some(false),
        }));
    };

    sqlx::query!(
        "DELETE FROM tag_follows WHERE account_id = $1 AND tag_id = $2",
        auth.account_id,
        tag.id,
    )
    .execute(&state.db)
    .await?;

    let history = fetch_tag_history(&state.db, tag.id).await;

    let featuring = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM featured_tags WHERE account_id = $1 AND tag_id = $2)",
        auth.account_id,
        tag.id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(false);

    Ok(Json(Tag {
        id: tag.id.to_string(),
        url: tag_url(domain, &name),
        name,
        history,
        following: Some(false),
        featuring: Some(featuring),
    }))
}
