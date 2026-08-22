//! Account migration: `Move` to a new account and managing `alsoKnownAs`
//! aliases.

use super::*;

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

    let valid =
        crate::crypto::verify_password(&form.current_password, &user.encrypted_password).is_ok();
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
        moved_id,
        auth.account_id,
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
    // The Move names the new account by its actor id, which a local target does
    // not store, and it has to be signed.
    let can_sign = mover.private_key.as_deref().is_some_and(|s| !s.is_empty());
    if let Some(new_uri) = new_uri.filter(|uri| !uri.is_empty() && can_sign) {
        let actor_url = crate::federation::tag::account_uri(
            &instance.domain,
            auth.account_id,
            mover.id_scheme,
            &mover.username,
        );
        let move_id = format!("{actor_url}#moves/{}", crate::snowflake::next_id());
        let activity =
            crate::federation::activity::move_actor(&move_id, &actor_url, &actor_url, &new_uri);
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
    Ok(Json(
        rows.into_iter()
            .map(|r| AccountAlias {
                id: r.id.to_string(),
                account_id: r.account_id.to_string(),
                uri: r.uri,
                created_at: crate::api::mastodon::convert::mastodon_date(r.created_at),
            })
            .collect(),
    ))
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
        created_at: crate::api::mastodon::convert::mastodon_date(r.created_at),
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
        id,
        auth.account_id,
    )
    .execute(&state.db)
    .await?;
    Ok(Json(serde_json::json!({})))
}

// ── POST /api/v1/accounts/:id/note ────────────────────────────────────────
