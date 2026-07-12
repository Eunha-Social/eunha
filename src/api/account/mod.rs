use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{
    crypto::{generate_token, hash_password, verify_password},
    locale::Locale,
    middleware::ResolvedInstance,
    state::AppState,
    templates,
};

const COOKIE_NAME: &str = "account_session";
const COOKIE_MAX_AGE: u32 = 2_592_000; // 30 days

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/account", get(account_home))
        .route("/account/login", get(login_page).post(login_post))
        .route("/account/logout", post(logout_post))
        .route("/account/sso", post(sso_post))
        .route("/account/password", get(password_page).post(password_post))
        .route("/account/invites", get(invites_page))
        .with_state(state)
}

// ── Session lookup ─────────────────────────────────────────────────────────────

struct AccountSession {
    user_id: i64,
    username: String,
}

fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(val.to_string());
        }
    }
    None
}

async fn get_session(headers: &HeaderMap, state: &AppState) -> Option<AccountSession> {
    let token = extract_session_token(headers)?;
    let row = sqlx::query!(
        r#"SELECT u.id as user_id, a.username
           FROM oauth_access_tokens t
           JOIN users u ON u.id = t.resource_owner_id
           JOIN accounts a ON a.id = u.account_id
           WHERE t.token = $1
             AND t.revoked_at IS NULL
             AND (t.expires_in IS NULL OR t.created_at + t.expires_in * interval '1 second' > now())
             AND u.disabled = false
             AND a.domain IS NULL"#,
        token,
    )
    .fetch_optional(&state.db)
    .await
    .ok()??;

    Some(AccountSession {
        user_id: row.user_id,
        username: row.username,
    })
}

fn set_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={COOKIE_MAX_AGE}")
}

fn clear_cookie() -> &'static str {
    "account_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"
}

fn accept_language(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("HX-Request").and_then(|v| v.to_str().ok()) == Some("true")
}

// ── GET /account ───────────────────────────────────────────────────────────────

pub async fn account_home(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));

    let Some(session) = get_session(&headers, &state).await else {
        return Redirect::to("/account/login").into_response();
    };

    let domain = instance.domain.clone();

    let html = templates::render(
        "account_home.html",
        minijinja::context! {
            lang => locale.as_str(),
            domain,
            username => session.username,
            t_account => locale.t("account"),
            t_invite_tree => locale.t("invite_tree"),
            t_change_password => locale.t("change_password"),
            t_sign_out => locale.t("sign_out"),
            t_go_to_timeline => locale.t("go_to_timeline"),
        },
    );
    Html(html).into_response()
}

// ── GET /account/login ─────────────────────────────────────────────────────────

pub async fn login_page(
    axum::extract::Extension(ResolvedInstance(instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));
    let domain = instance.domain.clone();

    let html = templates::render(
        "account_login.html",
        minijinja::context! {
            lang => locale.as_str(),
            domain,
            error => "",
            t_email => locale.t("email"),
            t_password => locale.t("password"),
            t_sign_in => locale.t("sign_in"),
            t_account => locale.t("account"),
        },
    );
    Html(html).into_response()
}

// ── POST /account/login ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub email: String,
    pub password: String,
}

pub async fn login_post(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));
    let domain = instance.domain.clone();
    let htmx = is_htmx(&headers);

    let render_error = |error: &'static str| -> Response {
        if htmx {
            return Html(format!("<div class=\"error\">{error}</div>")).into_response();
        }
        let html = templates::render(
            "account_login.html",
            minijinja::context! {
                lang => locale.as_str(),
                domain => domain.clone(),
                error,
                t_email => locale.t("email"),
                t_password => locale.t("password"),
                t_sign_in => locale.t("sign_in"),
                t_account => locale.t("account"),
            },
        );
        Html(html).into_response()
    };

    let row = match sqlx::query!(
        r#"SELECT u.id, u.encrypted_password, a.id as account_id, a.username
           FROM users u
           JOIN accounts a ON a.id = u.account_id
           WHERE lower(u.email) = lower($1)
             AND u.confirmed_at IS NOT NULL
             AND u.disabled = false
             AND a.domain IS NULL"#,
        form.email.trim(),
    )
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(r)) => r,
        _ => return render_error(locale.t("invalid_credentials")),
    };

    if verify_password(&form.password, &row.encrypted_password).is_err() {
        return render_error(locale.t("invalid_credentials"));
    }

    // Reuse an existing non-revoked OAuth token, or mint a new one.
    let token = match sqlx::query_scalar!(
        r#"SELECT token FROM oauth_access_tokens
           WHERE resource_owner_id = $1
             AND revoked_at IS NULL
             AND (expires_in IS NULL OR created_at + expires_in * interval '1 second' > now())
           LIMIT 1"#,
        row.id,
    )
    .fetch_optional(&state.db)
    .await
    {
        Ok(Some(t)) => t,
        _ => {
            let t = generate_token(64);
            if sqlx::query!(
                "INSERT INTO oauth_access_tokens (resource_owner_id, token, scopes, created_at) VALUES ($1, $2, 'read write follow push', now())",
                row.id,
                t,
            )
            .execute(&state.db)
            .await
            .is_err()
            {
                return render_error(locale.t("err_server"));
            }
            t
        }
    };

    if htmx {
        let mut h = HeaderMap::new();
        h.insert(header::SET_COOKIE, set_cookie(&token).parse().unwrap());
        h.insert(
            HeaderName::from_static("hx-redirect"),
            HeaderValue::from_static("/account"),
        );
        return (h, "").into_response();
    }
    (
        [(header::SET_COOKIE, set_cookie(&token))],
        Redirect::to("/account"),
    )
        .into_response()
}

// ── POST /account/sso ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SsoForm {
    pub token: String,
}

pub async fn sso_post(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(_instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    Form(form): Form<SsoForm>,
) -> Response {
    let valid = sqlx::query!(
        r#"SELECT 1 as "exists!"
           FROM oauth_access_tokens t
           JOIN users u ON u.id = t.resource_owner_id
           JOIN accounts a ON a.id = u.account_id
           WHERE t.token = $1
             AND t.revoked_at IS NULL
             AND (t.expires_in IS NULL OR t.created_at + t.expires_in * interval '1 second' > now())
             AND a.domain IS NULL"#,
        form.token,
    )
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .is_some();

    if !valid {
        return Redirect::to("/account/login").into_response();
    }

    (
        [(header::SET_COOKIE, set_cookie(&form.token))],
        Redirect::to("/account"),
    )
        .into_response()
}

// ── POST /account/logout ───────────────────────────────────────────────────────

pub async fn logout_post(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = extract_session_token(&headers) {
        let _ = sqlx::query!(
            "UPDATE oauth_access_tokens SET revoked_at = now() WHERE token = $1",
            token,
        )
        .execute(&state.db)
        .await;
    }

    if is_htmx(&headers) {
        // Client JS (hx-on::after-request) clears Elk IDB/localStorage and redirects.
        return ([(header::SET_COOKIE, clear_cookie())], "").into_response();
    }

    // Non-HTMX fallback: inline JS page.
    let html = r#"<!doctype html><html><head><meta charset="utf-8"></head><body><script>
Object.keys(localStorage).filter(k=>k.startsWith('elk-')).forEach(k=>localStorage.removeItem(k));
var r=indexedDB.open('keyval-store');
r.onsuccess=function(e){var t=e.target.result.transaction('keyval','readwrite');t.objectStore('keyval').delete('elk-users');t.oncomplete=go;t.onerror=go};
r.onerror=go;
function go(){location.replace('/')}
</script></body></html>"#;

    ([(header::SET_COOKIE, clear_cookie())], Html(html)).into_response()
}

// ── GET /account/password ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PasswordQuery {
    pub ok: Option<String>,
    pub err: Option<String>,
    pub mismatch: Option<String>,
}

pub async fn password_page(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
    Query(query): Query<PasswordQuery>,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));

    let Some(_session) = get_session(&headers, &state).await else {
        return Redirect::to("/account/login").into_response();
    };

    let domain = instance.domain.clone();
    let ok = query.ok.as_deref() == Some("1");
    let err = query.err.as_deref() == Some("1");
    let mismatch = query.mismatch.as_deref() == Some("1");

    let html = templates::render(
        "account_password.html",
        minijinja::context! {
            lang => locale.as_str(),
            domain,
            ok,
            err,
            mismatch,
            t_account => locale.t("account"),
            t_change_password => locale.t("change_password"),
            t_current_password => locale.t("current_password"),
            t_new_password => locale.t("new_password"),
            t_confirm_password => locale.t("confirm_new_password"),
            t_sign_out => locale.t("sign_out"),
            t_back_to_account => locale.t("back_to_account"),
            t_password_changed => locale.t("password_changed"),
            t_password_error => locale.t("password_error"),
            t_password_mismatch => locale.t("password_mismatch"),
        },
    );
    Html(html).into_response()
}

// ── POST /account/password ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PasswordForm {
    pub current_password: String,
    pub new_password: String,
    pub new_password_confirm: String,
}

pub async fn password_post(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(_instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
    Form(form): Form<PasswordForm>,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));
    let htmx = is_htmx(&headers);

    macro_rules! err {
        ($msg:expr, $url:expr) => {{
            if htmx {
                return Html(format!("<div class=\"error\">{}</div>", $msg)).into_response();
            }
            return Redirect::to($url).into_response();
        }};
    }

    let Some(session) = get_session(&headers, &state).await else {
        return Redirect::to("/account/login").into_response();
    };

    if form.new_password != form.new_password_confirm {
        err!(
            locale.t("password_mismatch"),
            "/account/password?mismatch=1"
        );
    }

    if form.new_password.len() < 8 {
        err!(locale.t("password_error"), "/account/password?err=1");
    }

    let row = match sqlx::query!(
        "SELECT encrypted_password FROM users WHERE id = $1",
        session.user_id,
    )
    .fetch_one(&state.db)
    .await
    {
        Ok(r) => r,
        Err(_) => err!(locale.t("password_error"), "/account/password?err=1"),
    };

    if verify_password(&form.current_password, &row.encrypted_password).is_err() {
        err!(locale.t("password_error"), "/account/password?err=1");
    }

    let new_hash = match hash_password(&form.new_password) {
        Ok(h) => h,
        Err(_) => err!(locale.t("password_error"), "/account/password?err=1"),
    };

    match sqlx::query!(
        "UPDATE users SET encrypted_password = $1, updated_at = now() WHERE id = $2",
        new_hash,
        session.user_id,
    )
    .execute(&state.db)
    .await
    {
        Ok(_) => {
            if htmx {
                return Html(format!(
                    "<div class=\"success\">{}</div>",
                    locale.t("password_changed")
                ))
                .into_response();
            }
            Redirect::to("/account/password?ok=1").into_response()
        }
        Err(_) => err!(locale.t("password_error"), "/account/password?err=1"),
    }
}

// ── GET /account/invites ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct MemberView {
    username: String,
    invited_by: Option<String>,
    depth: usize,
}

pub async fn invites_page(
    State(state): State<AppState>,
    axum::extract::Extension(ResolvedInstance(instance)): axum::extract::Extension<
        ResolvedInstance,
    >,
    headers: HeaderMap,
) -> Response {
    let locale = Locale::detect(None, accept_language(&headers));

    let Some(_session) = get_session(&headers, &state).await else {
        return Redirect::to("/account/login").into_response();
    };

    let domain = instance.domain.clone();

    let rows = match sqlx::query!(
        r#"SELECT a.username, inv_a.username AS "invited_by?: String"
           FROM users u
           JOIN accounts a ON a.id = u.account_id
           LEFT JOIN invites i ON i.id = u.invite_id
           LEFT JOIN users inv_u ON inv_u.id = i.user_id
           LEFT JOIN accounts inv_a ON inv_a.id = inv_u.account_id
           WHERE a.domain IS NULL
           ORDER BY u.created_at ASC"#,
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    // Build children map: parent (None = root) → sorted list of children.
    let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
    let mut invited_by_map: HashMap<String, Option<String>> = HashMap::new();
    for row in &rows {
        children
            .entry(row.invited_by.clone())
            .or_default()
            .push(row.username.clone());
        invited_by_map.insert(row.username.clone(), row.invited_by.clone());
    }
    for kids in children.values_mut() {
        kids.sort();
    }

    // Pre-order DFS to produce a depth-annotated list.
    let mut members: Vec<MemberView> = Vec::with_capacity(rows.len());
    let mut stack: Vec<(String, usize)> = children
        .get(&None)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|u| (u, 0))
        .collect();

    while let Some((username, depth)) = stack.pop() {
        let invited_by = invited_by_map.get(&username).cloned().flatten();
        members.push(MemberView {
            username: username.clone(),
            invited_by,
            depth,
        });
        if let Some(kids) = children.get(&Some(username)) {
            for kid in kids.iter().rev() {
                stack.push((kid.clone(), depth + 1));
            }
        }
    }

    let html = templates::render(
        "account_invites.html",
        minijinja::context! {
            lang => locale.as_str(),
            domain,
            members,
            t_account => locale.t("account"),
            t_invite_tree => locale.t("invite_tree"),
            t_sign_out => locale.t("sign_out"),
            t_back_to_account => locale.t("back_to_account"),
            t_no_members => locale.t("no_members"),
        },
    );
    Html(html).into_response()
}
