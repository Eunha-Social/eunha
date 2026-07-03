use super::types::CustomEmoji;
use crate::{error::AppResult, state::AppState};
use axum::{extract::State, response::Json};

pub async fn list_custom_emojis(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<CustomEmoji>>> {
    let rows = sqlx::query!(
        r#"SELECT ce.shortcode, ce.image_remote_url, ce.visible_in_picker,
                  ecc.name AS "category_name?"
           FROM custom_emojis ce
           LEFT JOIN custom_emoji_categories ecc ON ecc.id = ce.category_id
           WHERE ce.domain IS NULL
             AND ce.disabled = false
           ORDER BY ce.shortcode"#,
    )
    .fetch_all(&state.db)
    .await?;

    let emojis = rows
        .into_iter()
        .map(|r| {
            let url = r.image_remote_url.unwrap_or_default();
            CustomEmoji {
                shortcode: r.shortcode,
                url: url.clone(),
                static_url: url,
                visible_in_picker: r.visible_in_picker,
                category: r.category_name,
                featured: None,
            }
        })
        .collect();

    Ok(Json(emojis))
}
