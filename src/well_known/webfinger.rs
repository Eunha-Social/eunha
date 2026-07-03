use axum::{
    extract::{Extension, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    middleware::ResolvedInstance,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct WebFingerQuery {
    pub resource: String,
}

#[derive(Debug, Serialize)]
pub struct WebFingerResponse {
    pub subject: String,
    pub aliases: Vec<String>,
    pub links: Vec<WebFingerLink>,
}

#[derive(Debug, Serialize)]
pub struct WebFingerLink {
    pub rel: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub link_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

pub async fn webfinger(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Query(q): Query<WebFingerQuery>,
) -> AppResult<Response> {
    // resource should be "acct:user@domain" or a URL
    let resource = &q.resource;

    let username = if let Some(acct) = resource.strip_prefix("acct:") {
        // acct:user@domain — verify the domain matches
        let (user, domain) = acct.split_once('@').unwrap_or((acct, ""));
        if !domain.is_empty() && domain != instance.domain {
            return Err(AppError::NotFound);
        }
        user.to_string()
    } else if let Ok(url) = url::Url::parse(resource) {
        // URL like https://domain/users/username
        url.path_segments()
            .and_then(|mut s| {
                if s.next()? == "users" {
                    s.next().map(str::to_owned)
                } else {
                    None
                }
            })
            .ok_or(AppError::NotFound)?
    } else {
        return Err(AppError::NotFound);
    };

    let account = sqlx::query!(
        "SELECT id, username, id_scheme FROM accounts WHERE username = $1 AND domain IS NULL",
        username,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let actor_url = crate::federation::tag::account_uri(
        &instance.domain,
        account.id,
        account.id_scheme,
        &account.username,
    );
    let profile_url = format!("https://{}/@{}", instance.domain, account.username);
    let subject = format!("acct:{}@{}", account.username, instance.domain);

    let response = WebFingerResponse {
        subject: subject.clone(),
        aliases: vec![actor_url.clone(), profile_url.clone()],
        links: vec![
            WebFingerLink {
                rel: "http://webfinger.net/rel/profile-page".to_string(),
                link_type: Some("text/html".to_string()),
                href: Some(profile_url),
                template: None,
            },
            WebFingerLink {
                rel: "self".to_string(),
                link_type: Some("application/activity+json".to_string()),
                href: Some(actor_url),
                template: None,
            },
            WebFingerLink {
                rel: "http://ostatus.org/schema/1.0/subscribe".to_string(),
                link_type: None,
                href: None,
                template: Some(format!(
                    "https://{}/@{}/authorize_interaction?uri={{uri}}",
                    instance.domain, account.username
                )),
            },
        ],
    };

    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/jrd+json; charset=utf-8",
        )],
        Json(response),
    )
        .into_response())
}
