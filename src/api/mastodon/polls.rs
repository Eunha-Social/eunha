use axum::{
    extract::{Extension, Json, Path, State},
};
use serde::Deserialize;

use crate::{
    db::models,
    error::{AppError, AppResult},
    middleware::AuthenticatedUser,
    state::AppState,
};
use super::types::{Poll, PollOption};

// ── GET /api/v1/polls/:id ─────────────────────────────────────────────────

pub async fn get_poll(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Poll>> {
    let poll = fetch_poll(&state, id).await?;
    let viewer_id = auth.map(|Extension(a)| a.account_id);
    poll_from_db(&state, &poll, viewer_id).await.map(Json)
}

// ── POST /api/v1/polls/:id/votes ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PollVoteForm {
    pub choices: Vec<i32>,
}

pub async fn vote_poll(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    Json(form): Json<PollVoteForm>,
) -> AppResult<Json<Poll>> {
    auth.require_scope("write:statuses")?;
    let poll = fetch_poll(&state, id).await?;

    let expired = poll.expires_at.map(|e| e < chrono::Utc::now().naive_utc()).unwrap_or(false);
    if expired {
        return Err(AppError::Unprocessable("Poll has expired".into()));
    }

    if poll.account_id == auth.account_id {
        return Err(AppError::Unprocessable("You cannot vote on your own poll".into()));
    }

    let option_count = poll.options.len() as i32;
    if !poll.multiple && form.choices.len() > 1 {
        return Err(AppError::Unprocessable("Multiple choices not allowed".into()));
    }

    // Single-choice: block re-voting entirely. Multi-choice: only block same choice (ON CONFLICT).
    if !poll.multiple {
        let already_voted = sqlx::query_scalar!(
            "SELECT EXISTS(SELECT 1 FROM poll_votes WHERE poll_id = $1 AND account_id = $2)",
            id, auth.account_id,
        )
        .fetch_one(&state.db)
        .await?
        .unwrap_or(false);
        if already_voted {
            return Err(AppError::Unprocessable("Already voted".into()));
        }
    }

    let was_first_vote = sqlx::query_scalar!(
        "SELECT NOT EXISTS(SELECT 1 FROM poll_votes WHERE poll_id = $1 AND account_id = $2)",
        id, auth.account_id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(true);

    for choice in &form.choices {
        if *choice < 0 || *choice >= option_count {
            return Err(AppError::Unprocessable("Invalid choice index".into()));
        }
        sqlx::query!(
            "INSERT INTO poll_votes (poll_id, account_id, choice) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            id, auth.account_id, choice,
        )
        .execute(&state.db)
        .await?;
    }

    // Recount votes_count; increment voters_count for multi-choice if this is a new voter.
    sqlx::query!(
        "UPDATE polls SET votes_count = (SELECT COUNT(*) FROM poll_votes WHERE poll_id = $1) WHERE id = $1",
        id,
    )
    .execute(&state.db)
    .await?;
    if poll.multiple && was_first_vote {
        sqlx::query!(
            "UPDATE polls SET voters_count = COALESCE(voters_count, 0) + 1 WHERE id = $1",
            id,
        )
        .execute(&state.db)
        .await?;
    }

    let poll = fetch_poll(&state, id).await?;
    poll_from_db(&state, &poll, Some(auth.account_id)).await.map(Json)
}

// ── Helpers ────────────────────────────────────────────────────────────────

async fn fetch_poll(state: &AppState, id: i64) -> AppResult<models::Poll> {
    sqlx::query_as!(
        models::Poll,
        "SELECT * FROM polls WHERE id = $1",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)
}

async fn poll_from_db(state: &AppState, poll: &models::Poll, viewer_id: Option<i64>) -> AppResult<Poll> {
    let option_titles: Vec<String> = poll.options.clone();

    let expired = poll.expires_at.map(|e| e < chrono::Utc::now().naive_utc()).unwrap_or(false);
    let viewer_is_owner = viewer_id.map(|vid| vid == poll.account_id).unwrap_or(false);

    let (voted, own_votes) = if let Some(vid) = viewer_id {
        let votes = sqlx::query!(
            "SELECT choice FROM poll_votes WHERE poll_id = $1 AND account_id = $2 ORDER BY choice",
            poll.id, vid,
        )
        .fetch_all(&state.db)
        .await?;
        if votes.is_empty() {
            // Poll owner counts as having voted (matches Mastodon's voted? logic)
            (Some(viewer_is_owner), if viewer_is_owner { Some(vec![]) } else { None })
        } else {
            let choices: Vec<i32> = votes.iter().map(|v| v.choice).collect();
            (Some(true), Some(choices))
        }
    } else {
        (None, None)
    };

    // Per Mastodon show_totals_now?: expired? || !hide_totals?
    let show_results = expired || !poll.hide_totals;

    let per_option_counts = if show_results {
        sqlx::query!(
            "SELECT choice, COUNT(*) as cnt FROM poll_votes WHERE poll_id = $1 GROUP BY choice",
            poll.id,
        )
        .fetch_all(&state.db)
        .await?
    } else {
        vec![]
    };

    let options: Vec<PollOption> = option_titles.iter().enumerate().map(|(i, title)| {
        let votes_count = if show_results {
            let cnt = per_option_counts.iter()
                .find(|r| r.choice == i as i32)
                .and_then(|r| r.cnt)
                .unwrap_or(0);
            Some(cnt)
        } else {
            None
        };
        PollOption { title: title.clone(), votes_count }
    }).collect();

    let voters_count = sqlx::query_scalar!(
        "SELECT COUNT(DISTINCT account_id) FROM poll_votes WHERE poll_id = $1",
        poll.id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    let votes_count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM poll_votes WHERE poll_id = $1",
        poll.id,
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);

    Ok(Poll {
        id: poll.id.to_string(),
        expires_at: poll.expires_at.map(super::convert::mastodon_date),
        expired,
        multiple: poll.multiple,
        votes_count,
        voters_count: if poll.multiple { Some(voters_count) } else { None },
        options,
        emojis: vec![],
        voted,
        own_votes,
    })
}
