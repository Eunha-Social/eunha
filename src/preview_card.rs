use scraper::{Html, Selector};
use sqlx::PgPool;

/// Fetch and upsert a preview card for a URL, return the card id.
/// Fails silently — callers ignore errors.
pub async fn fetch_and_store(db: &PgPool, http: &reqwest::Client, url: &str) -> Option<i64> {
    // `http` must be the SSRF-guarded client; this also rejects literal-IP and
    // non-HTTP(S) targets before we issue the request.
    crate::federation::safe_fetch::validate_url(url).ok()?;

    let resp = http
        .get(url)
        .header("User-Agent", "eunha/1.0 (link preview bot)")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if !content_type.contains("text/html") {
        return None;
    }

    let html = resp.text().await.ok()?;
    let (title, description, _image_url, card_type) = extract_og(&html, url);
    let card_type = match card_type.as_str() {
        "photo" => 1,
        "video" => 2,
        "rich" => 3,
        _ => 0,
    };

    let id: i64 = sqlx::query_scalar(
        r#"INSERT INTO preview_cards (url, title, description, type, created_at, updated_at)
           VALUES ($1, $2, $3, $4, now(), now())
           ON CONFLICT (url) DO UPDATE
             SET title = $2, description = $3, type = $4, updated_at = now()
           RETURNING id"#,
    )
    .bind(url)
    .bind(&title)
    .bind(&description)
    .bind(&card_type)
    .fetch_one(db)
    .await
    .ok()?;

    Some(id)
}

fn extract_og(html: &str, url: &str) -> (String, String, Option<String>, String) {
    let doc = Html::parse_document(html);

    let meta_sel = Selector::parse("meta").unwrap();
    let title_sel = Selector::parse("title").unwrap();

    let mut title = String::new();
    let mut description = String::new();
    let mut image_url: Option<String> = None;
    let mut card_type = "link".to_string();

    for meta in doc.select(&meta_sel) {
        let property = meta
            .value()
            .attr("property")
            .or_else(|| meta.value().attr("name"))
            .unwrap_or("");
        let content = meta.value().attr("content").unwrap_or("");

        match property {
            "og:title" if title.is_empty() => title = content.to_string(),
            "og:description" if description.is_empty() => description = content.to_string(),
            "og:image" if image_url.is_none() => image_url = Some(content.to_string()),
            "og:type" => {
                card_type = match content {
                    "video" | "video.movie" | "video.episode" => "video".to_string(),
                    "music.song" | "music.album" => "link".to_string(),
                    _ => "link".to_string(),
                };
            }
            "twitter:title" if title.is_empty() => title = content.to_string(),
            "twitter:description" if description.is_empty() => description = content.to_string(),
            "twitter:image" if image_url.is_none() => image_url = Some(content.to_string()),
            "description" if description.is_empty() => description = content.to_string(),
            _ => {}
        }
    }

    if title.is_empty() {
        if let Some(t) = doc.select(&title_sel).next() {
            title = t.text().collect::<String>().trim().to_string();
        }
    }

    if title.is_empty() {
        title = url.to_string();
    }

    (title, description, image_url, card_type)
}

/// Extract all plain-text URLs from HTML status content (href attributes on <a> tags
/// that are NOT mentions or hashtags).
pub fn extract_urls_from_content(content: &str) -> Vec<String> {
    let doc = Html::parse_document(content);
    let a_sel = Selector::parse("a").unwrap();

    doc.select(&a_sel)
        .filter_map(|a| {
            let href = a.value().attr("href")?;
            // Skip mentions and hashtag links
            let class = a.value().attr("class").unwrap_or("");
            if class.contains("mention") || class.contains("hashtag") {
                return None;
            }
            if href.starts_with("http://") || href.starts_with("https://") {
                Some(href.to_string())
            } else {
                None
            }
        })
        .collect()
}
