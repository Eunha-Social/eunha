use axum::{extract::State, Extension, Json};
use serde::Serialize;
use std::collections::HashMap;

use crate::{
    api::mastodon::convert,
    error::{AppError, AppResult},
    middleware::AuthenticatedUser,
    state::AppState,
};

#[derive(Debug, Serialize)]
pub struct TreeAccount {
    pub id: String,
    pub username: String,
    pub acct: String,
    pub display_name: String,
    pub avatar: String,
    /// When the account joined (its user row's created_at), ISO 8601.
    pub invited_at: String,
}

#[derive(Debug, Serialize)]
pub struct InviteNode {
    #[serde(flatten)]
    pub account: TreeAccount,
    pub children: Vec<InviteNode>,
}

#[derive(Debug, Serialize)]
pub struct InviteTreeResponse {
    /// Members who were not invited by a local account (registration roots).
    pub roots: Vec<InviteNode>,
    /// Total number of local members represented in the tree.
    pub total: usize,
}

/// GET /api/eunha/v1/invite_tree
///
/// eunha-specific endpoint (Mastodon has no invite-tree API). Returns the
/// instance's local members as a forest keyed on "who invited whom": each node's
/// `children` are the accounts that signed up through one of its invites. Any
/// authenticated local member may view it, matching the existing server-rendered
/// `/account/invites` page.
pub async fn invite_tree(
    State(state): State<AppState>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<InviteTreeResponse>> {
    // Require an authenticated local user (app-only tokens have no user_id).
    match auth {
        Some(Extension(auth)) if auth.user_id.is_some() => {}
        _ => return Err(AppError::Unauthorized),
    }

    // The inviter is reached via users.invite_id -> invites.user_id -> users ->
    // accounts (Mastodon's `invite.user.account` path).
    // Only genuine members: confirmed, admin-approved, and not suspended. This
    // keeps pending/unapproved signups and deleted (suspended) accounts out of
    // the tree.
    let rows = sqlx::query!(
        r#"SELECT a.id, a.username, a.display_name,
                  a.avatar_file_name, a.avatar_remote_url, u.created_at,
                  inv_a.id AS "invited_by_id?"
           FROM users u
           JOIN accounts a ON a.id = u.account_id
           LEFT JOIN invites i ON i.id = u.invite_id
           LEFT JOIN users inv_u ON inv_u.id = i.user_id
           LEFT JOIN accounts inv_a ON inv_a.id = inv_u.account_id
           WHERE a.domain IS NULL
             AND u.approved
             AND u.confirmed_at IS NOT NULL
             AND a.suspended_at IS NULL
           ORDER BY u.created_at ASC"#,
    )
    .fetch_all(&state.db)
    .await?;

    let total = rows.len();

    // First pass: materialize every member, remembering its raw inviter id.
    let mut accounts: HashMap<i64, TreeAccount> = HashMap::with_capacity(total);
    let mut order: Vec<(i64, Option<i64>)> = Vec::with_capacity(total);
    for r in rows {
        let id = r.id;
        order.push((id, r.invited_by_id));
        accounts.insert(
            id,
            TreeAccount {
                id: id.to_string(),
                acct: r.username.clone(),
                username: r.username,
                display_name: r.display_name,
                avatar: convert::account_avatar_url_parts(
                    id,
                    r.avatar_file_name.as_deref(),
                    r.avatar_remote_url.as_deref(),
                ),
                invited_at: convert::mastodon_date(r.created_at),
            },
        );
    }

    // Second pass: group children under their inviter, preserving creation
    // order. An inviter that was filtered out (unapproved/suspended) isn't in
    // the member set, so its invitees are promoted to roots rather than lost.
    let mut children: HashMap<Option<i64>, Vec<i64>> = HashMap::new();
    for (id, invited_by) in order {
        let parent = invited_by.filter(|pid| accounts.contains_key(pid));
        children.entry(parent).or_default().push(id);
    }

    let root_ids = children.get(&None).cloned().unwrap_or_default();
    let mut visited = std::collections::HashSet::new();
    let roots = build_nodes(&root_ids, &mut accounts, &children, &mut visited);

    Ok(Json(InviteTreeResponse { roots, total }))
}

/// Assemble nodes for `ids`, recursing into each account's invitees. `visited`
/// guards against cycles in pathological data (the graph is normally a forest).
fn build_nodes(
    ids: &[i64],
    accounts: &mut HashMap<i64, TreeAccount>,
    children: &HashMap<Option<i64>, Vec<i64>>,
    visited: &mut std::collections::HashSet<i64>,
) -> Vec<InviteNode> {
    let mut nodes = Vec::with_capacity(ids.len());
    for &id in ids {
        if !visited.insert(id) {
            continue;
        }
        let Some(account) = accounts.remove(&id) else {
            continue;
        };
        let child_ids = children.get(&Some(id)).cloned().unwrap_or_default();
        let child_nodes = build_nodes(&child_ids, accounts, children, visited);
        nodes.push(InviteNode {
            account,
            children: child_nodes,
        });
    }
    nodes
}
