use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{config::InstanceConfig, error::AppError, state::AppState};

/// Resolved instance config, injected into request extensions by [`resolve_instance`].
#[derive(Clone)]
pub struct ResolvedInstance(pub InstanceConfig);

/// Injects the single-tenant instance config into every request's extensions.
pub async fn resolve_instance(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    req.extensions_mut()
        .insert(ResolvedInstance((*state.instance).clone()));
    Ok(next.run(req).await)
}

/// Resolved OAuth token + account, injected by [`authenticate`].
#[derive(Clone)]
pub struct AuthenticatedUser {
    pub account_id: i64,
    pub user_id: Option<i64>,
    pub token_id: i64,
    pub scopes: Vec<String>,
    pub application_id: Option<i64>,
}

impl AuthenticatedUser {
    /// Returns `Err(AppError::Forbidden)` if the token does not cover `required`.
    ///
    /// Scope hierarchy:
    /// - `"read"` covers every `"read:*"` sub-scope.
    /// - `"write"` covers every `"write:*"` sub-scope.
    /// - `"follow"` covers all social-graph operations (read+write on follows/blocks/mutes).
    pub fn require_scope(&self, required: &str) -> crate::error::AppResult<()> {
        if self.has_scope(required) {
            Ok(())
        } else {
            Err(crate::error::AppError::Forbidden)
        }
    }

    fn has_scope(&self, required: &str) -> bool {
        if self.scopes.iter().any(|s| s == required) {
            return true;
        }
        // Parent scope covers child: "read" → "read:*", "write" → "write:*"
        if let Some(parent) = required.split(':').next() {
            if self.scopes.iter().any(|s| s == parent) {
                return true;
            }
        }
        // "follow" covers all social-graph operations (read + write on follows/blocks/mutes)
        if matches!(
            required,
            "write:follows"
                | "write:blocks"
                | "write:mutes"
                | "read:follows"
                | "read:blocks"
                | "read:mutes"
        ) && self.scopes.iter().any(|s| s == "follow")
        {
            return true;
        }
        // "profile" is a narrow scope that covers read:accounts (for verify_credentials)
        if required == "read:accounts" && self.scopes.iter().any(|s| s == "profile") {
            return true;
        }
        false
    }
}

/// Bearer token authentication. Attaches `AuthenticatedUser` if a valid token
/// is present; passes through unauthenticated requests so endpoints can decide
/// whether auth is required.
pub async fn authenticate(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    if let Some(token) = extract_bearer(&req) {
        if let Some(tok) = sqlx::query!(
            r#"SELECT t.id, u.account_id, t.application_id, t.scopes,
                      t.expires_in, t.created_at, t.revoked_at, u.id as "user_id?",
                      u.disabled as "disabled?", a.suspended_at AS "suspended_at?"
               FROM oauth_access_tokens t
               LEFT JOIN users u ON u.id = t.resource_owner_id
               LEFT JOIN accounts a ON a.id = u.account_id
               WHERE t.token = $1"#,
            token
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        {
            // App-only tokens (client_credentials) have no user, so `disabled` is
            // NULL there and must stay valid; a disabled user's tokens are rejected.
            let user_disabled = tok.disabled.unwrap_or(false);
            // Mastodon's `Api::BaseController#require_not_suspended!`: a
            // suspended account cannot act, but its tokens stay intact so
            // unsuspending restores access without a new sign-in.
            let account_suspended = tok.suspended_at.is_some();
            let valid = tok.revoked_at.is_none()
                && token_not_expired(tok.created_at, tok.expires_in)
                && !user_disabled
                && !account_suspended;

            if valid {
                let user = AuthenticatedUser {
                    account_id: tok.account_id,
                    user_id: tok.user_id,
                    token_id: tok.id,
                    scopes: tok
                        .scopes
                        .as_deref()
                        .unwrap_or("read")
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    application_id: tok.application_id,
                };
                req.extensions_mut().insert(user);
            }
        }
    }
    next.run(req).await
}

/// Log failed requests (4xx/5xx) with method, path, and request body preview.
/// Skips body buffering for multipart uploads to avoid memory pressure.
pub async fn log_failures(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let content_type = req
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    let is_text = content_type.contains("json") || content_type.contains("x-www-form-urlencoded");
    let is_multipart = content_type.contains("multipart");

    let (parts, body) = req.into_parts();
    // Buffer text bodies fully so the handler always receives the complete body.
    // The log preview is capped to 2 KB; the 4 MB limit guards against runaway requests.
    let (body_preview, rebuilt) = if is_text && !is_multipart {
        match axum::body::to_bytes(body, 4 * 1024 * 1024).await {
            Ok(bytes) => {
                let preview = if bytes.len() > 2048 {
                    format!(
                        "{}…({} bytes)",
                        String::from_utf8_lossy(&bytes[..2048]),
                        bytes.len()
                    )
                } else {
                    String::from_utf8_lossy(&bytes).into_owned()
                };
                let new_body = axum::body::Body::from(bytes);
                (Some(preview), Request::from_parts(parts, new_body))
            }
            Err(_) => (None, Request::from_parts(parts, axum::body::Body::empty())),
        }
    } else {
        (None, Request::from_parts(parts, body))
    };

    let response = next.run(rebuilt).await;
    let status = response.status();

    if status.is_client_error() || status.is_server_error() {
        tracing::warn!(
            method = %method,
            path = %path,
            status = %status,
            body = body_preview.as_deref().unwrap_or(""),
            "request failed",
        );
    }

    response
}

fn extract_bearer(req: &Request) -> Option<String> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    header.strip_prefix("Bearer ").map(str::to_string)
}

fn token_not_expired(created_at: chrono::NaiveDateTime, expires_in: Option<i32>) -> bool {
    expires_in
        .map(|seconds| {
            created_at + chrono::Duration::seconds(seconds as i64) > chrono::Utc::now().naive_utc()
        })
        .unwrap_or(true)
}
