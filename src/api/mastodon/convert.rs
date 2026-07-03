/// Conversions from DB models → Mastodon API types.
use std::sync::OnceLock;
use crate::db::models;
use super::formatting::{format_field_value, mention_map_from_api, render_content};
use super::types;

static DEFAULT_AVATAR: OnceLock<String> = OnceLock::new();
static DEFAULT_HEADER: OnceLock<String> = OnceLock::new();
static MEDIA_BASE_URL: OnceLock<String> = OnceLock::new();
static LOCAL_DOMAIN: OnceLock<String> = OnceLock::new();

/// Call once at startup to record this instance's domain, used to build the
/// human-facing `/@username` URLs for local accounts and statuses.
pub fn init_local_domain(domain: String) {
    LOCAL_DOMAIN.set(domain).ok();
}

fn local_domain() -> &'static str {
    LOCAL_DOMAIN.get().map(|s| s.as_str()).unwrap_or("")
}

/// Call once at startup (before serving requests) to set the default avatar/header
/// URLs from the configured media storage base URL.
pub fn init_media_defaults(avatar: String, header: String) {
    if let Some(base) = avatar.strip_suffix("/avatars/original/missing.png") {
        MEDIA_BASE_URL.set(base.to_string()).ok();
    }
    DEFAULT_AVATAR.set(avatar).ok();
    DEFAULT_HEADER.set(header).ok();
}

pub(super) fn missing_avatar() -> &'static str {
    DEFAULT_AVATAR.get().map(|s| s.as_str()).unwrap_or("avatars/original/missing.png")
}

pub(super) fn missing_header() -> &'static str {
    DEFAULT_HEADER.get().map(|s| s.as_str()).unwrap_or("headers/original/missing.png")
}

fn media_base_url() -> &'static str {
    MEDIA_BASE_URL.get().map(|s| s.as_str()).unwrap_or("")
}

pub fn account_avatar_url_for(a: &models::Account) -> String {
    account_avatar_url(a)
}

pub fn account_header_url_for(a: &models::Account) -> String {
    account_header_url(a)
}

fn account_avatar_url(a: &models::Account) -> String {
    if let Some(url) = &a.avatar_remote_url {
        if !url.is_empty() {
            return url.clone();
        }
    }
    if let Some(filename) = &a.avatar_file_name {
        if !filename.is_empty() {
            return format!("{}/accounts/avatars/{}/original/{}", media_base_url(), crate::media::int_to_path(a.id), filename);
        }
    }
    missing_avatar().to_string()
}

fn account_header_url(a: &models::Account) -> String {
    if !a.header_remote_url.is_empty() {
        return a.header_remote_url.clone();
    }
    if let Some(filename) = &a.header_file_name {
        if !filename.is_empty() {
            return format!("{}/accounts/headers/{}/original/{}", media_base_url(), crate::media::int_to_path(a.id), filename);
        }
    }
    missing_header().to_string()
}

pub fn media_url(m: &models::MediaAttachment) -> Option<String> {
    if let Some(filename) = &m.file_file_name {
        if !filename.is_empty() {
            return Some(format!("{}/media_attachments/files/{}/original/{}", media_base_url(), crate::media::int_to_path(m.id), filename));
        }
    }
    m.remote_url.as_deref().filter(|s| !s.is_empty()).map(str::to_string)
}

pub fn media_preview_url(m: &models::MediaAttachment) -> Option<String> {
    if let Some(filename) = &m.thumbnail_file_name {
        if !filename.is_empty() {
            return Some(format!("{}/media_attachments/files/{}/small/{}", media_base_url(), crate::media::int_to_path(m.id), filename));
        }
    }
    if let Some(filename) = &m.file_file_name {
        if !filename.is_empty() {
            return Some(format!("{}/media_attachments/files/{}/small/{}", media_base_url(), crate::media::int_to_path(m.id), filename));
        }
    }
    m.thumbnail_remote_url.as_deref().filter(|s| !s.is_empty()).map(str::to_string)
}

pub trait MastodonTimestamp {
    fn format_mastodon(self) -> String;
}

impl MastodonTimestamp for chrono::NaiveDateTime {
    fn format_mastodon(self) -> String {
        self.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }
}

impl MastodonTimestamp for chrono::DateTime<chrono::Utc> {
    fn format_mastodon(self) -> String {
        self.naive_utc().format_mastodon()
    }
}

/// Format a timestamp in the Mastodon-standard format: `YYYY-MM-DDTHH:MM:SS.mmmZ`.
/// Mastodon stores UTC timestamps without time zones and serializes them with a `Z` suffix.
pub fn mastodon_date<T: MastodonTimestamp>(t: T) -> String {
    t.format_mastodon()
}

fn status_url_from_uri(uri: &str) -> Option<String> {
    let (base, rest) = uri.split_once("/users/")?;
    let (username, id) = rest.split_once("/statuses/")?;
    Some(format!("{}/@{}/{}", base, username, id))
}

pub fn account_from_db(a: &models::Account) -> types::Account {
    let (url, uri) = if a.domain.is_none() {
        // Local accounts: the human url is /@username; the AP uri follows the
        // account's id_scheme (/users/{username} or /ap/users/{id}).
        (
            format!("https://{}/@{}", local_domain(), a.username),
            crate::federation::tag::account_uri(local_domain(), a.id, a.id_scheme, &a.username),
        )
    } else {
        (a.url.clone().unwrap_or_default(), a.uri.clone())
    };

    let suspended = a.suspended_at.is_some();

    types::Account {
        id: a.id.to_string(),
        username: a.username.clone(),
        acct: a.acct(),
        display_name: if suspended { String::new() } else { a.display_name.clone() },
        locked: if suspended { false } else { a.locked },
        bot: !suspended && a.actor_type.as_deref() == Some("Service"),
        group: !suspended && a.actor_type.as_deref() == Some("Group"),
        discoverable: if suspended { Some(false) } else { a.discoverable },
        indexable: !suspended && a.indexable,
        hide_collections: a.hide_collections,
        created_at: a.created_at.format("%Y-%m-%dT00:00:00.000Z").to_string(),
        note: if suspended { String::new() } else { a.note.clone() },
        url,
        uri,
        avatar: if suspended { missing_avatar().to_string() } else { account_avatar_url(a) },
        avatar_static: if suspended { missing_avatar().to_string() } else { account_avatar_url(a) },
        header: if suspended { missing_header().to_string() } else { account_header_url(a) },
        header_static: if suspended { missing_header().to_string() } else { account_header_url(a) },
        followers_count: 0,
        following_count: 0,
        statuses_count: 0,
        last_status_at: None,
        emojis: vec![],
        fields: if suspended {
            vec![]
        } else if a.domain.is_none() {
            // Local accounts: format field values as HTML (linkify URLs) matching Mastodon's FieldSerializer
            fields_from_db(a.fields.as_ref().unwrap_or(&serde_json::json!([]))).into_iter().map(|f| types::Field {
                name: f.name,
                value: format_field_value(&f.value),
                verified_at: f.verified_at,
            }).collect()
        } else {
            fields_from_db(a.fields.as_ref().unwrap_or(&serde_json::json!([])))
        },
        roles: vec![],
        moved: None,
        suspended: if suspended { Some(true) } else { None },
        limited: if a.silenced_at.is_some() { Some(true) } else { None },
        noindex: if a.domain.is_none() { Some(!a.indexable) } else { None },
        memorial: if a.memorial { Some(true) } else { None },
        mute_expires_at: None,
        source: None,
        role: None,
    }
}

pub fn fields_from_db(fields: &serde_json::Value) -> Vec<types::Field> {
    fields
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    Some(types::Field {
                        name: f["name"].as_str()?.to_string(),
                        value: f["value"].as_str()?.to_string(),
                        verified_at: f["verified_at"].as_str().map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn media_from_db(m: &models::MediaAttachment) -> types::MediaAttachment {
    types::MediaAttachment {
        id: m.id.to_string(),
        media_type: super::media::media_type_str(m.r#type).to_string(),
        url: media_url(m),
        preview_url: media_preview_url(m),
        remote_url: m.remote_url.as_deref().filter(|s| !s.is_empty()).map(str::to_string),
        preview_remote_url: m.thumbnail_remote_url.as_deref().filter(|s| !s.is_empty()).map(str::to_string),
        text_url: None,
        description: m.description.clone(),
        blurhash: m.blurhash.clone(),
        meta: Some(m.file_meta.clone().unwrap_or_else(|| serde_json::json!({}))),
    }
}

/// Render status content from raw text, matching the Mastodon convention:
/// - local statuses: render from plaintext (linkify mentions/hashtags/URLs)
/// - remote statuses: sanitize the ActivityPub HTML
fn render_status_content(
    s: &models::Status,
    account: &models::Account,
    mentions: &[types::StatusMention],
) -> String {
    if account.domain.is_none() {
        // Local: text is raw plaintext, render to annotated HTML
        let domain = s.uri.as_deref()
            .and_then(|uri| uri.strip_prefix("https://"))
            .and_then(|rest| rest.split('/').next())
            .unwrap_or("");
        let map = mention_map_from_api(mentions, domain);
        render_content(&s.text, domain, &map)
    } else {
        // Remote: text is ActivityPub HTML, sanitize before serving
        ammonia::clean(&s.text)
    }
}

fn build_quote_approval(
    s: &models::Status,
    viewer: Option<&StatusViewerContext>,
) -> types::QuoteApproval {
    use crate::db::models::quote_policy;
    let policy = s.quote_approval_policy;
    let automatic: Vec<String> = quote_policy::automatic_labels(policy)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let manual: Vec<String> = quote_policy::manual_labels(policy)
        .into_iter()
        .map(str::to_owned)
        .collect();

    let current_user = match viewer {
        None => "unknown".to_string(),
        Some(ctx) => match policy {
            quote_policy::PUBLIC => "automatic".to_string(),
            quote_policy::FOLLOWERS if ctx.follows_author => "automatic".to_string(),
            quote_policy::MANUAL => "manual".to_string(),
            _ => "denied".to_string(),
        },
    };

    types::QuoteApproval {
        automatic,
        manual,
        current_user,
    }
}

pub fn status_from_db(
    s: &models::Status,
    account: &models::Account,
    media: Vec<models::MediaAttachment>,
    reblog: Option<(models::Status, models::Account, Vec<models::MediaAttachment>)>,
    viewer_context: Option<StatusViewerContext>,
    mentions: &[types::StatusMention],
    reblog_mentions: &[types::StatusMention],
) -> types::Status {
    status_from_db_with_app(s, account, media, reblog, viewer_context, None, mentions, reblog_mentions)
}

#[allow(clippy::too_many_arguments)]
pub fn status_from_db_with_app(
    s: &models::Status,
    account: &models::Account,
    media: Vec<models::MediaAttachment>,
    reblog: Option<(models::Status, models::Account, Vec<models::MediaAttachment>)>,
    viewer_context: Option<StatusViewerContext>,
    application: Option<types::Application>,
    mentions: &[types::StatusMention],
    reblog_mentions: &[types::StatusMention],
) -> types::Status {
    let content = render_status_content(s, account, mentions);
    let reblog_status = reblog.map(|(rs, ra, rm)| {
        Box::new(status_from_db(&rs, &ra, rm, None, viewer_context.clone(), reblog_mentions, &[]))
    });

    // Mastodon: the author always sees their own raw `sensitive` flag; sensitization
    // from account-level flags is only applied to other viewers.
    let is_author = viewer_context.as_ref().map(|c| c.account_id) == Some(account.id);
    let sensitive = if is_author {
        s.sensitive
    } else {
        s.sensitive || account.sensitized_at.is_some()
    };

    // Mastodon omits viewer-dependent fields entirely for unauthenticated responses.
    // `pinned` is further restricted to the author's own view.
    let (favourited, reblogged, muted, bookmarked, pinned, filtered) =
        if let Some(ref ctx) = viewer_context {
            (
                Some(ctx.favourited),
                Some(ctx.reblogged),
                Some(ctx.muted),
                Some(ctx.bookmarked),
                if is_author
                    && s.reblog_of_id.is_none()
                    && matches!(
                        s.visibility,
                        crate::db::models::vis::PUBLIC
                            | crate::db::models::vis::UNLISTED
                            | crate::db::models::vis::PRIVATE
                    )
                {
                    Some(ctx.pinned)
                } else {
                    None
                },
                Some(vec![]),
            )
        } else {
            (None, None, None, None, None, None)
        };

    types::Status {
        id: s.id.to_string(),
        created_at: s.created_at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        in_reply_to_id: s.in_reply_to_id.map(|i| i.to_string()),
        in_reply_to_account_id: s.in_reply_to_account_id.map(|i| i.to_string()),
        sensitive,
        spoiler_text: s.spoiler_text.clone(),
        visibility: crate::db::models::vis::to_str(s.visibility).to_owned(),
        language: s.language.clone(),
        uri: s.uri.clone().unwrap_or_else(|| s.id.to_string()),
        url: if account.domain.is_none() {
            // Local status: human permalink is /@username/{id}; prefer a stored
            // non-AP url, otherwise derive from the (id_scheme-independent) username.
            Some(
                s.url.clone().filter(|u| !u.is_empty() && Some(u.as_str()) != s.uri.as_deref())
                    .unwrap_or_else(|| format!("https://{}/@{}/{}", local_domain(), account.username, s.id)),
            )
        } else {
            let uri_str = s.uri.as_deref();
            s.url.as_deref()
                .filter(|&u| uri_str != Some(u))
                .map(String::from)
                .or_else(|| status_url_from_uri(uri_str?))
        },
        replies_count: 0,
        reblogs_count: 0,
        favourites_count: 0,
        quotes_count: 0,
        edited_at: s.edited_at.map(|t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()),
        content,
        reblog: reblog_status,
        application,
        account: account_from_db(account),
        media_attachments: media.iter()
            .map(media_from_db)
            .filter(|m| m.url.is_some() || m.remote_url.as_deref().is_some_and(|u| !u.is_empty()))
            .collect(),
        mentions: mentions.to_vec(),
        tags: vec![],
        emojis: vec![],
        card: None,
        poll: None,
        quote: None,
        quote_approval: build_quote_approval(s, viewer_context.as_ref()),
        tagged_collections: vec![],
        favourited,
        reblogged,
        muted,
        bookmarked,
        pinned,
        filtered,
        text: None,
    }
}

#[derive(Clone)]
pub struct StatusViewerContext {
    pub account_id: i64,
    pub follows_author: bool,
    pub author_follows: bool,
    pub favourited: bool,
    pub reblogged: bool,
    pub muted: bool,
    pub bookmarked: bool,
    pub pinned: bool,
}
