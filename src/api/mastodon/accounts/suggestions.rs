//! Follow suggestions (`/api/v1` and `/api/v2`) and dismissing a suggestion.

use super::*;

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
             AND a.suspended_at IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM follows f2
               WHERE f2.account_id = $1 AND f2.target_account_id = a.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM follow_recommendation_mutes sd
               WHERE sd.account_id = $1 AND sd.target_account_id = a.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM blocks b
               WHERE (b.account_id = $1 AND b.target_account_id = a.id)
                  OR (b.account_id = a.id AND b.target_account_id = $1)
             )
             AND NOT EXISTS (
               SELECT 1 FROM mutes m
               WHERE m.account_id = $1 AND m.target_account_id = a.id
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
             AND a.suspended_at IS NULL
             AND NOT EXISTS (
               SELECT 1 FROM follows f2
               WHERE f2.account_id = $1 AND f2.target_account_id = a.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM follow_recommendation_mutes sd
               WHERE sd.account_id = $1 AND sd.target_account_id = a.id
             )
             AND NOT EXISTS (
               SELECT 1 FROM blocks b
               WHERE (b.account_id = $1 AND b.target_account_id = a.id)
                  OR (b.account_id = a.id AND b.target_account_id = $1)
             )
             AND NOT EXISTS (
               SELECT 1 FROM mutes m
               WHERE m.account_id = $1 AND m.target_account_id = a.id
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
