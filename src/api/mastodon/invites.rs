use crate::{
    error::{AppError, AppResult},
    middleware::{AuthenticatedUser, ResolvedInstance},
    state::AppState,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct InviteResponse {
    pub id: String,
    pub code: String,
    pub expires_at: Option<NaiveDateTime>,
    pub max_uses: Option<i32>,
    pub uses: i32,
    pub url: String,
    pub autofollow: bool,
    pub created_at: NaiveDateTime,
}

pub async fn list_invites(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Vec<InviteResponse>>> {
    auth.require_scope("read:accounts")?;
    let rows = sqlx::query!(
        r#"SELECT id, code, expires_at, max_uses, uses, created_at
           FROM invites
           WHERE user_id = (SELECT id FROM users WHERE account_id = $1)
           ORDER BY created_at DESC"#,
        auth.account_id,
    )
    .fetch_all(&state.db)
    .await?;

    let invites = rows
        .into_iter()
        .map(|r| InviteResponse {
            url: invite_url(&instance.domain, &r.code),
            id: r.id.to_string(),
            code: r.code,
            expires_at: r.expires_at,
            max_uses: r.max_uses,
            uses: r.uses,
            autofollow: false,
            created_at: r.created_at,
        })
        .collect();

    Ok(Json(invites))
}

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub max_uses: Option<i32>,
    /// Seconds from now until expiry; None = never expires.
    pub expires_in: Option<i64>,
}

pub async fn create_invite(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<CreateInviteRequest>>,
) -> AppResult<Json<InviteResponse>> {
    auth.require_scope("write:accounts")?;
    let req = body.map(|Json(b)| b).unwrap_or(CreateInviteRequest {
        max_uses: None,
        expires_in: None,
    });

    let code = generate_code();
    let expires_at = req
        .expires_in
        .map(|s| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(s));

    let row = sqlx::query!(
        r#"INSERT INTO invites (code, user_id, max_uses, expires_at, created_at, updated_at)
           VALUES ($1, (SELECT id FROM users WHERE account_id = $2), $3, $4, now(), now())
           RETURNING id, code, expires_at, max_uses, uses, created_at"#,
        code,
        auth.account_id,
        req.max_uses,
        expires_at,
    )
    .fetch_one(&state.db)
    .await?;

    Ok(Json(InviteResponse {
        url: invite_url(&instance.domain, &row.code),
        id: row.id.to_string(),
        code: row.code,
        expires_at: row.expires_at,
        max_uses: row.max_uses,
        uses: row.uses,
        autofollow: false,
        created_at: row.created_at,
    }))
}

pub async fn delete_invite(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    auth.require_scope("write:accounts")?;
    let deleted = sqlx::query!(
        "DELETE FROM invites WHERE id = $1 AND user_id = (SELECT id FROM users WHERE account_id = $2)",
        id,
        auth.account_id,
    )
    .execute(&state.db)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::OK)
}

// ── helpers ────────────────────────────────────────────────────────────────

pub fn generate_code() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..12)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

pub fn invite_url(domain: &str, code: &str) -> String {
    format!("https://{domain}/auth/signup?invite={code}")
}
