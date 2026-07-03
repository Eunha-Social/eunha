use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::objects::CONTENT_TYPE;
use crate::{error::AppResult, middleware::ResolvedInstance, state::AppState};

#[derive(Deserialize)]
pub struct OutboxQuery {
    pub page: Option<bool>,
    pub min_id: Option<i64>,
    pub max_id: Option<i64>,
}

pub async fn get_outbox(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(username): Path<String>,
    Query(q): Query<OutboxQuery>,
) -> AppResult<Response> {
    outbox_for(
        &state,
        &instance.domain,
        super::objects::AccountRef::Username(&username),
        q,
    )
    .await
}

/// Numeric-scheme outbox (`/ap/users/{id}/outbox`).
pub async fn get_outbox_by_id(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Path(id): Path<i64>,
    Query(q): Query<OutboxQuery>,
) -> AppResult<Response> {
    outbox_for(
        &state,
        &instance.domain,
        super::objects::AccountRef::Id(id),
        q,
    )
    .await
}

async fn outbox_for(
    state: &AppState,
    domain: &str,
    who: super::objects::AccountRef<'_>,
    q: OutboxQuery,
) -> AppResult<Response> {
    let account = super::objects::load_local_account(state, who).await?;
    let statuses_count = sqlx::query_scalar!(
        "SELECT COALESCE(statuses_count, 0) FROM account_stats WHERE account_id = $1",
        account.id,
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    .unwrap_or(0);

    let base_url = format!(
        "{}/outbox",
        crate::federation::tag::account_uri_of(domain, &account)
    );

    if q.page != Some(true) {
        // Return the OrderedCollection summary
        let outbox = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": base_url,
            "type": "OrderedCollection",
            "totalItems": statuses_count,
            "first": format!("{}?page=true", base_url),
            "last": format!("{}?page=true&min_id=0", base_url),
        });
        return Ok((
            StatusCode::OK,
            [(header::CONTENT_TYPE, CONTENT_TYPE)],
            Json(outbox),
        )
            .into_response());
    }

    // Only self-authored, public/unlisted, non-boost statuses appear in the outbox.
    let status_ids: Vec<i64> = sqlx::query_scalar!(
        r#"SELECT s.id
           FROM statuses s
           WHERE s.account_id = $1
             AND s.deleted_at IS NULL
             AND s.reblog_of_id IS NULL
             AND s.visibility IN (0, 1) /* vis::PUBLIC, vis::UNLISTED */
             AND ($2::bigint IS NULL OR s.id < $2)
             AND ($3::bigint IS NULL OR s.id > $3)
           ORDER BY s.id DESC
           LIMIT 20"#,
        account.id,
        q.max_id,
        q.min_id,
    )
    .fetch_all(&state.db)
    .await?;

    let mut items: Vec<Value> = Vec::with_capacity(status_ids.len());
    for id in &status_ids {
        if let Some(bundle) = super::note::build_note(state, domain, *id).await? {
            items.push(bundle.into_create());
        }
    }

    let first_id = status_ids.first().copied();
    let last_id = status_ids.last().copied();

    let page = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{}?page=true", base_url),
        "type": "OrderedCollectionPage",
        "partOf": base_url,
        "prev": first_id.map(|id| format!("{}?page=true&min_id={}", base_url, id)),
        "next": last_id.map(|id| format!("{}?page=true&max_id={}", base_url, id)),
        "orderedItems": items,
    });

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, CONTENT_TYPE)],
        Json(page),
    )
        .into_response())
}
