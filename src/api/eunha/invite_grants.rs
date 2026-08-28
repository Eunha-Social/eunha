//! Handing invites to somebody else.
//!
//! Mastodon has no equivalent. There an invite is made by the person who will
//! hand it out, and `invite_users` is the whole of the question — either a
//! member may make as many as they like, or none at all. An instance that wants
//! to say "you may bring two people" has nothing to say it with.
//!
//! So an admin here mints the codes *into the member's own account*: they
//! appear on that member's invite page for them to pass on, and whoever signs
//! up through one lands under **them** in the invite tree rather than under the
//! admin who minted it. The count is the limit — there is no allowance to keep
//! books on, because the codes themselves are the allowance.

use axum::{extract::State, routing::post, Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::{
    api::mastodon::{admin, invites::generate_code},
    error::{AppError, AppResult},
    middleware::AuthenticatedUser,
    state::AppState,
};

/// Codes minted per account in one request. Nothing wants more, and a mistyped
/// count across a whole userbase should not write ten thousand rows.
const MAX_COUNT: i32 = 25;
/// The largest of Mastodon's `Invite::MAX_USES_COUNTS`.
const MAX_USES: i32 = 100;
/// Mastodon's `Invite::COMMENT_SIZE_LIMIT`.
const COMMENT_SIZE_LIMIT: usize = 420;

#[derive(Debug, Deserialize)]
pub struct GrantRequest {
    /// Whose account to mint them into. Absent means every local member.
    pub account_id: Option<String>,
    /// How many codes each of those accounts gets.
    pub count: i32,
    /// Uses per code. One by default: "three invites" should mean three people.
    pub max_uses: Option<i32>,
    /// Seconds until the codes expire; absent for never.
    pub expires_in: Option<i64>,
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GrantResponse {
    /// Codes created.
    pub granted: i64,
    /// Accounts they were created for.
    pub accounts: i64,
}

/// POST /api/eunha/v1/invite_grants
pub async fn grant_invites(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedUser>>,
    Json(req): Json<GrantRequest>,
) -> AppResult<Json<GrantResponse>> {
    let Some(Extension(auth)) = auth else {
        return Err(AppError::Unauthorized);
    };
    auth.require_scope("write:accounts")?;
    // `manage_invites` is the permission Mastodon uses for acting on invites
    // that are not your own, which is what this does to the furthest extent:
    // it creates them.
    admin::require_permission(&state, auth.account_id, admin::perm::MANAGE_INVITES).await?;

    if !(1..=MAX_COUNT).contains(&req.count) {
        return Err(AppError::Unprocessable(format!(
            "Count must be between 1 and {MAX_COUNT}"
        )));
    }
    let max_uses = req.max_uses.unwrap_or(1);
    if !(1..=MAX_USES).contains(&max_uses) {
        return Err(AppError::Unprocessable(format!(
            "Uses per invite must be between 1 and {MAX_USES}"
        )));
    }
    let comment = req
        .comment
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty());
    if comment
        .as_ref()
        .is_some_and(|c| c.chars().count() > COMMENT_SIZE_LIMIT)
    {
        return Err(AppError::Unprocessable(format!(
            "Validation failed: Comment is too long (maximum is {COMMENT_SIZE_LIMIT} characters)"
        )));
    }
    let account_id: Option<i64> = match req.account_id.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(id) => Some(
            id.parse()
                .map_err(|_| AppError::Unprocessable("Invalid account id".into()))?,
        ),
    };
    let expires_at = req
        .expires_in
        .map(|s| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(s));

    // Members only: the same set the invite tree counts. An unconfirmed or
    // unapproved signup has not joined yet, and a suspended one has left.
    let targets: Vec<i64> = sqlx::query_scalar!(
        r#"SELECT u.id
           FROM users u
           JOIN accounts a ON a.id = u.account_id
           WHERE a.domain IS NULL
             AND u.approved
             AND u.confirmed_at IS NOT NULL
             AND a.suspended_at IS NULL
             AND a.requested_deletion_at IS NULL
             AND ($1::bigint IS NULL OR a.id = $1)
           ORDER BY u.id"#,
        account_id,
    )
    .fetch_all(&state.db)
    .await?;

    if targets.is_empty() {
        return Err(match account_id {
            Some(_) => AppError::NotFound,
            None => AppError::Unprocessable("This instance has no members yet".into()),
        });
    }

    // One row per code, built here rather than in a loop of statements: 25
    // codes across a whole userbase is a single insert either way.
    let mut user_ids = Vec::with_capacity(targets.len() * req.count as usize);
    let mut codes = Vec::with_capacity(targets.len() * req.count as usize);
    for user_id in &targets {
        for _ in 0..req.count {
            user_ids.push(*user_id);
            codes.push(generate_code());
        }
    }

    // `ON CONFLICT DO NOTHING` for the same reason Mastodon's `set_code` loops
    // until the code is free: the codes are random, and a collision should cost
    // one code rather than the whole grant. `granted` counts what landed.
    let granted = sqlx::query!(
        r#"INSERT INTO invites
             (user_id, code, max_uses, expires_at, autofollow, comment, created_at, updated_at)
           SELECT t.user_id, t.code, $3, $4, false, $5, now(), now()
           FROM unnest($1::bigint[], $2::text[]) AS t(user_id, code)
           ON CONFLICT (code) DO NOTHING"#,
        &user_ids,
        &codes,
        max_uses,
        expires_at,
        comment,
    )
    .execute(&state.db)
    .await?
    .rows_affected() as i64;

    Ok(Json(GrantResponse {
        granted,
        accounts: targets.len() as i64,
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/eunha/v1/invite_grants", post(grant_invites))
}
