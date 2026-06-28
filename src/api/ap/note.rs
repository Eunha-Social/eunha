//! Shared ActivityPub `Note` construction for locally-authored statuses.
//!
//! Both the outbox and the per-status dereferencing endpoint build their `Note`
//! objects here so the wire representation (content, media `attachment`, and the
//! `tag` array of mentions/hashtags/custom emoji) stays identical regardless of
//! how a remote server discovers the post.

use serde_json::{json, Value};

use crate::api::mastodon::convert;
use crate::api::mastodon::formatting::render_content;
use crate::db::models::{self, vis};
use crate::{error::AppResult, state::AppState};

/// JSON-LD `@context` for a `Create(Note)` / `Note` we serve. Declares the Toot,
/// hashtag/emoji, and FEP-044f quote terms used below.
pub fn note_context() -> Value {
    json!([
        "https://www.w3.org/ns/activitystreams",
        {
            "sensitive": "as:sensitive",
            "toot": "http://joinmastodon.org/ns#",
            "blurhash": "toot:blurhash",
            "Hashtag": "as:Hashtag",
            "Emoji": "toot:Emoji",
            "focalPoint": { "@container": "@list", "@id": "toot:focalPoint" },
            "fep": "https://w3id.org/fep/044f#",
            "quote": { "@id": "fep:quote", "@type": "@id" },
            "quoteUrl": { "@id": "fep:quote", "@type": "@id" },
            "_misskey_quote": "https://misskey-hub.net/ns#quoteUri",
        }
    ])
}

/// A built `Note` plus the addressing needed to wrap it in a `Create`.
pub struct NoteBundle {
    /// The `Note` object, without an `@context` (suitable for embedding).
    pub note: Value,
    /// The local author's actor URI.
    pub actor_url: String,
    /// The canonical AP id of the note (`actor/statuses/{id}`).
    pub note_uri: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl NoteBundle {
    /// Wrap the note in a `Create` activity (id `{note_uri}/activity`), with the
    /// full `@context`.
    pub fn into_create(self) -> Value {
        let activity_id = format!("{}/activity", self.note_uri);
        json!({
            "@context": note_context(),
            "id": activity_id,
            "type": "Create",
            "actor": self.actor_url,
            "published": self.created_at.to_rfc3339(),
            "to": self.to,
            "cc": self.cc,
            "object": self.note,
        })
    }

    /// The standalone `Note` object with its `@context`, for serving at the
    /// note's own URI.
    pub fn into_note(mut self) -> Value {
        self.note["@context"] = note_context();
        self.note
    }
}

/// Build the AP `Note` for a local, non-reblog status. Returns `Ok(None)` if the
/// status doesn't exist, is deleted, is remote, or is a boost.
pub async fn build_note(
    state: &AppState,
    domain: &str,
    status_id: i64,
) -> AppResult<Option<NoteBundle>> {
    let s = sqlx::query!(
        r#"SELECT s.id, s.account_id, s.text, s.spoiler_text, s.visibility, s.sensitive,
                  s.created_at, s.edited_at, s.uri, s.url, s.in_reply_to_id, s.language,
                  s.quote_approval_policy,
                  a.username, a.uri AS account_uri,
                  quoted_s.uri AS "quote_uri?"
           FROM statuses s
           JOIN accounts a ON a.id = s.account_id
           LEFT JOIN quotes qr ON qr.status_id = s.id AND qr.state = 1
           LEFT JOIN statuses quoted_s ON quoted_s.id = qr.quoted_status_id AND quoted_s.deleted_at IS NULL
           WHERE s.id = $1 AND s.deleted_at IS NULL AND a.domain IS NULL
             AND s.reblog_of_id IS NULL"#,
        status_id,
    )
    .fetch_optional(&state.db)
    .await?;
    let Some(s) = s else { return Ok(None) };

    let actor_url = if s.account_uri.is_empty() {
        format!("https://{domain}/users/{}", s.username)
    } else {
        s.account_uri.clone()
    };
    let note_uri = s
        .uri
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("{actor_url}/statuses/{}", s.id));
    let note_url = s.url.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| note_uri.clone());
    let followers_url = format!("{actor_url}/followers");

    // ── inReplyTo ───────────────────────────────────────────────────────────
    let in_reply_to: Option<String> = if let Some(parent) = s.in_reply_to_id {
        sqlx::query_scalar!("SELECT uri FROM statuses WHERE id = $1", parent)
            .fetch_optional(&state.db)
            .await?
            .flatten()
            .filter(|u| !u.is_empty())
    } else {
        None
    };

    // ── Mentions (for tag + addressing) ─────────────────────────────────────
    let mention_rows = sqlx::query!(
        r#"SELECT a.username, a.domain, a.uri AS account_uri, a.url AS "url?"
           FROM mentions m JOIN accounts a ON a.id = m.account_id
           WHERE m.status_id = $1"#,
        s.id,
    )
    .fetch_all(&state.db)
    .await?;

    let mut mention_map: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    let mut mention_uris: Vec<String> = Vec::new();
    let mut mention_tags: Vec<Value> = Vec::new();
    for m in &mention_rows {
        let href = if !m.account_uri.is_empty() {
            m.account_uri.clone()
        } else if let Some(u) = m.url.clone().filter(|u| !u.is_empty()) {
            u
        } else {
            format!("https://{domain}/users/{}", m.username)
        };
        let acct = match &m.domain {
            Some(d) => format!("{}@{}", m.username, d),
            None => m.username.clone(),
        };
        // Keys used by render_content's linkifier (lowercase user / user@domain).
        let url_for_render = m.url.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| href.clone());
        mention_map
            .entry(m.username.to_lowercase())
            .or_insert_with(|| (url_for_render.clone(), acct.clone()));
        if let Some(d) = &m.domain {
            mention_map
                .entry(format!("{}@{}", m.username.to_lowercase(), d.to_lowercase()))
                .or_insert_with(|| (url_for_render.clone(), acct.clone()));
        }
        mention_uris.push(href.clone());
        mention_tags.push(json!({
            "type": "Mention",
            "href": href,
            "name": format!("@{acct}"),
        }));
    }

    // ── Hashtags ────────────────────────────────────────────────────────────
    let hashtag_rows = sqlx::query!(
        r#"SELECT t.name FROM statuses_tags st JOIN tags t ON t.id = st.tag_id
           WHERE st.status_id = $1 ORDER BY t.name"#,
        s.id,
    )
    .fetch_all(&state.db)
    .await?;
    let hashtag_tags: Vec<Value> = hashtag_rows
        .iter()
        .map(|t| {
            json!({
                "type": "Hashtag",
                "href": format!("https://{domain}/tags/{}", t.name),
                "name": format!("#{}", t.name),
            })
        })
        .collect();

    // ── Custom emoji (best effort: only those with a resolvable image) ───────
    let emoji_tags = emoji_tags_for(state, &s.text, &s.spoiler_text).await?;

    let mut tag: Vec<Value> = mention_tags;
    tag.extend(hashtag_tags);
    tag.extend(emoji_tags);

    // ── Media attachments ───────────────────────────────────────────────────
    let media = sqlx::query_as!(
        models::MediaAttachment,
        r#"SELECT m.* FROM media_attachments m
           JOIN LATERAL (
               SELECT COALESCE(array_position(s.ordered_media_attachment_ids, m.id), 2147483647) AS ord
               FROM statuses s WHERE s.id = $1
           ) o ON true
           WHERE m.status_id = $1
           ORDER BY o.ord, m.id"#,
        s.id,
    )
    .fetch_all(&state.db)
    .await?;
    let attachment: Vec<Value> = media.iter().filter_map(media_attachment_ap).collect();

    // ── Content + addressing ────────────────────────────────────────────────
    let content = render_content(&s.text, domain, &mention_map);
    let (to, cc) = vis::audience(s.visibility, &followers_url, &mention_uris);

    let mut note = json!({
        "id": note_uri,
        "type": "Note",
        "summary": if s.spoiler_text.is_empty() { None } else { Some(s.spoiler_text.clone()) },
        "inReplyTo": in_reply_to,
        "published": s.created_at.and_utc().to_rfc3339(),
        "url": note_url,
        "attributedTo": actor_url,
        "to": to.clone(),
        "cc": cc.clone(),
        "sensitive": s.sensitive,
        "content": content,
        "attachment": attachment,
        "tag": tag,
    });

    if let Some(lang) = s.language.as_deref().filter(|l| !l.is_empty()) {
        note["contentMap"] = json!({ lang: note["content"].clone() });
    }
    if let Some(edited) = s.edited_at {
        note["updated"] = json!(edited.and_utc().to_rfc3339());
    }

    // FEP-044f quote linkage.
    if let Some(q) = s.quote_uri.clone().filter(|u| !u.is_empty()) {
        note["quote"] = json!(q);
        note["quoteUrl"] = json!(q);
        note["_misskey_quote"] = json!(q);
    }

    // Quote interaction policy advertisement.
    note["interactionPolicy"] = json!({
        "canQuote": quote_interaction_policy(s.quote_approval_policy, s.visibility, &followers_url),
    });

    Ok(Some(NoteBundle {
        note,
        actor_url,
        note_uri,
        to,
        cc,
        created_at: s.created_at.and_utc(),
    }))
}

/// Map our `quote_approval_policy` to a FEP-044f `canQuote` policy.
fn quote_interaction_policy(policy: i32, visibility: i32, followers_url: &str) -> Value {
    use crate::db::models::quote_policy;
    const PUBLIC_URI: &str = "https://www.w3.org/ns/activitystreams#Public";
    let public_post = matches!(visibility, vis::PUBLIC | vis::UNLISTED);
    let (automatic, manual): (Vec<String>, Vec<String>) = match policy {
        quote_policy::PUBLIC if public_post => (vec![PUBLIC_URI.to_string()], vec![]),
        quote_policy::FOLLOWERS => (vec![followers_url.to_string()], vec![]),
        quote_policy::MANUAL if public_post => (vec![], vec![PUBLIC_URI.to_string()]),
        _ => (vec![], vec![]),
    };
    json!({
        "automaticApproval": automatic,
        "manualApproval": manual,
    })
}

/// Build an AP `attachment` entry for one media attachment, or `None` if it has
/// no resolvable URL.
fn media_attachment_ap(m: &models::MediaAttachment) -> Option<Value> {
    let url = convert::media_url(m)?;
    let ap_type = match m.r#type.unwrap_or(4) {
        0 => "Image",
        1 | 2 => "Video", // gifv + video
        3 => "Audio",
        _ => "Document",
    };
    let mut obj = json!({
        "type": ap_type,
        "url": url,
        "mediaType": m.file_content_type,
        "name": m.description,
        "blurhash": m.blurhash,
    });
    if let Some(preview) = convert::media_preview_url(m) {
        obj["icon"] = json!({ "type": "Image", "url": preview });
    }
    // Surface original width/height when known (helps remote layout).
    if let Some(orig) = m.file_meta.as_ref().and_then(|v| v.get("original")) {
        if let Some(w) = orig.get("width").and_then(Value::as_i64) {
            obj["width"] = json!(w);
        }
        if let Some(h) = orig.get("height").and_then(Value::as_i64) {
            obj["height"] = json!(h);
        }
    }
    Some(obj)
}

/// Scan text and spoiler for `:shortcode:` tokens and emit `Emoji` tags for any
/// matching enabled local custom emoji that has a resolvable image URL.
async fn emoji_tags_for(state: &AppState, text: &str, spoiler: &str) -> AppResult<Vec<Value>> {
    let mut shortcodes: Vec<String> = Vec::new();
    for src in [text, spoiler] {
        for cap in EMOJI_RE.captures_iter(src) {
            let sc = cap[1].to_string();
            if !shortcodes.contains(&sc) {
                shortcodes.push(sc);
            }
        }
    }
    if shortcodes.is_empty() {
        return Ok(vec![]);
    }

    let rows = sqlx::query!(
        r#"SELECT shortcode, image_remote_url, uri, updated_at
           FROM custom_emojis
           WHERE domain IS NULL AND disabled = false AND shortcode = ANY($1)"#,
        &shortcodes,
    )
    .fetch_all(&state.db)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let image = r.image_remote_url.filter(|u| !u.is_empty())?;
            Some(json!({
                "type": "Emoji",
                "id": r.uri,
                "name": format!(":{}:", r.shortcode),
                "updated": r.updated_at.and_utc().to_rfc3339(),
                "icon": { "type": "Image", "url": image },
            }))
        })
        .collect())
}

static EMOJI_RE: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| regex::Regex::new(r":([a-zA-Z0-9_]+):").unwrap());
