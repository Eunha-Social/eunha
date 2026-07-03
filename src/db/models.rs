use chrono::NaiveDateTime;
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow)]
pub struct Account {
    pub id: i64,
    pub username: String,
    pub domain: Option<String>,
    pub display_name: String,
    pub note: String,
    pub url: Option<String>,
    pub uri: String,
    pub private_key: Option<String>,
    pub public_key: String,
    pub locked: bool,
    pub discoverable: Option<bool>,
    pub indexable: bool,
    pub inbox_url: String,
    pub outbox_url: String,
    pub shared_inbox_url: String,
    pub suspended_at: Option<NaiveDateTime>,
    pub silenced_at: Option<NaiveDateTime>,
    pub sensitized_at: Option<NaiveDateTime>,
    pub hide_collections: Option<bool>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub fields: Option<serde_json::Value>,
    pub attribution_domains: Option<Vec<String>>,
    pub actor_type: Option<String>,
    pub also_known_as: Option<Vec<String>>,
    pub featured_collection_url: Option<String>,
    pub followers_url: String,
    pub following_url: String,
    pub last_webfingered_at: Option<NaiveDateTime>,
    pub memorial: bool,
    pub moved_to_account_id: Option<i64>,
    pub protocol: i32,
    pub requested_review_at: Option<NaiveDateTime>,
    pub reviewed_at: Option<NaiveDateTime>,
    pub suspension_origin: Option<i32>,
    pub trendable: Option<bool>,
    pub id_scheme: Option<i32>,
    // Paperclip/ActiveStorage compat columns (added in migration 067)
    pub avatar_file_name: Option<String>,
    pub avatar_content_type: Option<String>,
    pub avatar_file_size: Option<i32>,
    pub avatar_updated_at: Option<NaiveDateTime>,
    pub header_file_name: Option<String>,
    pub header_content_type: Option<String>,
    pub header_file_size: Option<i32>,
    pub header_updated_at: Option<NaiveDateTime>,
    pub avatar_remote_url: Option<String>,
    pub header_remote_url: String,
    pub avatar_storage_schema_version: Option<i32>,
    pub header_storage_schema_version: Option<i32>,
    // Added in Mastodon v4.6.0
    pub avatar_description: String,
    pub header_description: String,
    pub show_featured: bool,
    pub show_media: bool,
    pub show_media_replies: bool,
    pub feature_approval_policy: i32,
    pub collections_url: Option<String>,
}

impl Account {
    pub fn is_local(&self) -> bool {
        self.domain.is_none()
    }

    pub fn acct(&self) -> String {
        match &self.domain {
            None => self.username.clone(),
            Some(d) => format!("{}@{}", self.username, d),
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct User {
    pub id: i64,
    pub account_id: i64,
    pub email: String,
    pub encrypted_password: String,
    pub confirmed_at: Option<NaiveDateTime>,
    pub invite_id: Option<i64>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct Status {
    pub id: i64,
    pub account_id: i64,
    pub application_id: Option<i64>,
    pub text: String,
    pub spoiler_text: String,
    pub in_reply_to_id: Option<i64>,
    pub in_reply_to_account_id: Option<i64>,
    pub reblog_of_id: Option<i64>,
    pub visibility: i32,
    pub language: Option<String>,
    pub sensitive: bool,
    pub url: Option<String>,
    pub uri: Option<String>,
    pub deleted_at: Option<NaiveDateTime>,
    pub edited_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub reply: bool,
    pub conversation_id: Option<i64>,
    // Added in migration 065
    pub fetched_replies_at: Option<NaiveDateTime>,
    pub local: Option<bool>,
    pub ordered_media_attachment_ids: Option<Vec<i64>>,
    pub poll_id: Option<i64>,
    pub quote_approval_policy: i32,
    pub trendable: Option<bool>,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, FromRow)]
pub struct MediaAttachment {
    pub id: i64,
    pub account_id: Option<i64>,
    pub status_id: Option<i64>,
    pub remote_url: Option<String>,
    pub description: Option<String>,
    pub blurhash: Option<String>,
    pub created_at: NaiveDateTime,
    // Mastodon compat columns (added in migration 067)
    pub updated_at: NaiveDateTime,
    pub shortcode: Option<String>,
    pub r#type: Option<i32>,
    pub file_meta: Option<serde_json::Value>,
    pub scheduled_status_id: Option<i64>,
    pub processing: Option<i32>,
    pub file_storage_schema_version: Option<i32>,
    pub file_file_name: Option<String>,
    pub file_content_type: Option<String>,
    pub file_file_size: Option<i32>,
    pub file_updated_at: Option<NaiveDateTime>,
    pub thumbnail_file_name: Option<String>,
    pub thumbnail_content_type: Option<String>,
    pub thumbnail_file_size: Option<i32>,
    pub thumbnail_updated_at: Option<NaiveDateTime>,
    pub thumbnail_remote_url: Option<String>,
    // Added in Mastodon v4.6.0
    pub thumbnail_storage_schema_version: Option<i32>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Follow {
    pub id: i64,
    pub account_id: i64,
    pub target_account_id: i64,
    pub show_reblogs: bool,
    pub notify: bool,
    pub languages: Vec<String>,
    pub uri: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct Notification {
    pub id: i64,
    pub account_id: i64,
    pub from_account_id: i64,
    pub r#type: Option<String>,
    pub created_at: NaiveDateTime,
    // Added in migration 065
    pub filtered: bool,
    pub group_key: Option<String>,
    // Mastodon polymorphic association columns
    pub activity_id: Option<i64>,
    pub activity_type: Option<String>,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct OauthApplication {
    pub id: i64,
    pub name: String,
    pub uid: String,
    pub secret: String,
    pub redirect_uri: String,
    pub scopes: Option<String>,
    pub website: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    // Added in migration 065
    pub confidential: bool,
    pub superapp: bool,
    pub updated_at: Option<NaiveDateTime>,
    pub owner_type: Option<String>,
    pub owner_id: Option<i64>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OauthAccessToken {
    pub id: i64,
    pub application_id: Option<i64>,
    pub resource_owner_id: Option<i64>,
    pub token: String,
    pub refresh_token: Option<String>,
    pub scopes: Option<String>,
    pub expires_in: Option<i32>,
    pub revoked_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
    pub last_used_ip: Option<std::net::IpAddr>,
}

#[derive(Debug, Clone, FromRow)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct CustomEmoji {
    pub id: i64,
    pub shortcode: String,
    pub domain: Option<String>,
    pub image_remote_url: Option<String>,
    pub visible_in_picker: bool,
    pub disabled: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct Favourite {
    pub id: i64,
    pub account_id: i64,
    pub status_id: i64,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct List {
    pub id: i64,
    pub account_id: i64,
    pub title: String,
    pub replies_policy: i32,
    pub exclusive: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct StatusEdit {
    pub id: i64,
    pub status_id: i64,
    pub account_id: Option<i64>,
    pub text: String,
    pub spoiler_text: String,
    pub sensitive: Option<bool>,
    pub created_at: NaiveDateTime,
    // Added in migration 065
    pub media_descriptions: Option<Vec<String>>,
    pub ordered_media_attachment_ids: Option<Vec<i64>>,
    pub poll_options: Option<Vec<String>>,
    pub quote_id: Option<i64>,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct Poll {
    pub id: i64,
    pub status_id: i64,
    pub account_id: i64,
    pub options: Vec<String>,
    pub votes_count: i64,
    pub voters_count: Option<i64>,
    pub multiple: bool,
    pub expires_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub cached_tallies: Vec<i64>,
    pub hide_totals: bool,
    pub last_fetched_at: Option<NaiveDateTime>,
    pub lock_version: i32,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct UserDomainBlock {
    pub id: i64,
    pub account_id: i64,
    pub domain: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, FromRow)]
pub struct WebPushSubscription {
    pub id: i64,
    pub account_id: i64,
    pub access_token_id: i64,
    pub endpoint: String,
    pub key_p256dh: String,
    pub key_auth: String,
    pub data: serde_json::Value,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Integer-to-text helpers for statuses.visibility (public=0 unlisted=1 private=2 direct=3 limited=4).
pub mod vis {
    pub const PUBLIC: i32 = 0;
    pub const UNLISTED: i32 = 1;
    pub const PRIVATE: i32 = 2;
    pub const DIRECT: i32 = 3;
    /// Limited (group-delivery) visibility — serialized as "private" per Mastodon API contract.
    pub const LIMITED: i32 = 4;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "public" => PUBLIC,
            "unlisted" => UNLISTED,
            "private" => PRIVATE,
            _ => DIRECT,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        // Mastodon masks "limited" (4) as "private" so clients don't need to handle it.
        match v {
            PUBLIC => "public",
            UNLISTED => "unlisted",
            PRIVATE | LIMITED => "private",
            _ => "direct",
        }
    }

    /// Derive visibility from an object's `to`/`cc` audience (portable logic in feder-core).
    pub fn from_audience<S: AsRef<str>, T: AsRef<str>>(to: &[S], cc: &[T]) -> i32 {
        use feder_core::addressing::Visibility;
        match feder_core::addressing::visibility_from_audience(to, cc) {
            Visibility::Public => PUBLIC,
            Visibility::Unlisted => UNLISTED,
            Visibility::Private => PRIVATE,
            Visibility::Direct => DIRECT,
        }
    }

    /// Compute the `(to, cc)` audience for an outgoing status of this visibility.
    pub fn audience(v: i32, followers: &str, mentioned: &[String]) -> (Vec<String>, Vec<String>) {
        use feder_core::addressing::Visibility;
        let vis = match v {
            PUBLIC => Visibility::Public,
            UNLISTED => Visibility::Unlisted,
            PRIVATE => Visibility::Private,
            // Mastodon's TagManager serializes "limited" like "direct": the
            // recipients go in `to` and `cc` stays empty.
            LIMITED => return (mentioned.to_vec(), Vec::new()),
            _ => Visibility::Direct,
        };
        feder_core::addressing::audience_for(vis, followers, mentioned)
    }
}

/// Integer-to-text helpers for lists.replies_policy (followed=0 list=1 none=2).
pub mod replies {
    pub const FOLLOWED: i32 = 0;
    pub const LIST: i32 = 1;
    pub const NONE: i32 = 2;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "followed" => FOLLOWED,
            "list" => LIST,
            _ => NONE,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        match v {
            FOLLOWED => "followed",
            LIST => "list",
            _ => "none",
        }
    }
}

/// Integer-to-text helpers for quotes.state (pending=0 accepted=1 rejected=2 revoked=3).
pub mod quote_state {
    pub const PENDING: i32 = 0;
    pub const ACCEPTED: i32 = 1;
    pub const REJECTED: i32 = 2;
    pub const REVOKED: i32 = 3;

    pub fn to_str(v: i32) -> &'static str {
        match v {
            ACCEPTED => "accepted",
            REJECTED => "rejected",
            REVOKED => "revoked",
            _ => "pending",
        }
    }
}

/// Integer encoding of statuses.quote_approval_policy (who may quote automatically).
pub mod quote_policy {
    pub const PUBLIC: i32 = 0;
    pub const FOLLOWERS: i32 = 1;
    pub const NOBODY: i32 = 2;
    /// Anyone may quote, but only with the author's manual approval.
    pub const MANUAL: i32 = 3;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "followers" => FOLLOWERS,
            "nobody" => NOBODY,
            "manual" => MANUAL,
            _ => PUBLIC,
        }
    }

    /// Labels for the `quote_approval.automatic` API field (who may quote
    /// without asking).
    pub fn automatic_labels(v: i32) -> Vec<&'static str> {
        match v {
            FOLLOWERS => vec!["followers"],
            NOBODY | MANUAL => vec![],
            _ => vec!["public"],
        }
    }

    /// Labels for the `quote_approval.manual` API field (who may quote only
    /// with the author's approval).
    pub fn manual_labels(v: i32) -> Vec<&'static str> {
        match v {
            MANUAL => vec!["public"],
            _ => vec![],
        }
    }
}

/// Integer-to-text helpers for custom_filters.action (warn=0 hide=1).
pub mod filter_action {
    pub const WARN: i32 = 0;
    pub const HIDE: i32 = 1;
    pub const BLUR: i32 = 2;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "hide" => HIDE,
            "blur" => BLUR,
            _ => WARN,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        match v {
            HIDE => "hide",
            BLUR => "blur",
            _ => "warn",
        }
    }
}

/// Integer-to-text helpers for domain_blocks/ip_blocks severity.
pub mod domain_severity {
    pub const NOOP: i32 = 0;
    pub const SILENCE: i32 = 1;
    pub const SUSPEND: i32 = 2;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "noop" => NOOP,
            "silence" => SILENCE,
            "suspend" => SUSPEND,
            _ => NOOP,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        match v {
            NOOP => "noop",
            SILENCE => "silence",
            SUSPEND => "suspend",
            _ => "noop",
        }
    }
}

/// Integer-to-text helpers for ip_blocks.severity (noop=0 sign_up_requires_approval=1 sign_up_block=2 block=3).
pub mod ip_severity {
    pub const NOOP: i32 = 0;
    pub const SIGN_UP_REQUIRES_APPROVAL: i32 = 1;
    pub const SIGN_UP_BLOCK: i32 = 2;
    pub const BLOCK: i32 = 3;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "noop" => NOOP,
            "sign_up_requires_approval" => SIGN_UP_REQUIRES_APPROVAL,
            "sign_up_block" => SIGN_UP_BLOCK,
            "block" => BLOCK,
            _ => NOOP,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        match v {
            NOOP => "noop",
            SIGN_UP_REQUIRES_APPROVAL => "sign_up_requires_approval",
            SIGN_UP_BLOCK => "sign_up_block",
            BLOCK => "block",
            _ => "noop",
        }
    }
}

/// Integer-to-text helpers for reports.category (other=0 spam=1 violation=2).
pub mod report_category {
    pub const OTHER: i32 = 0;
    pub const SPAM: i32 = 1;
    pub const VIOLATION: i32 = 2;

    pub fn from_str(s: &str) -> i32 {
        match s {
            "spam" => SPAM,
            "violation" => VIOLATION,
            _ => OTHER,
        }
    }

    pub fn to_str(v: i32) -> &'static str {
        match v {
            SPAM => "spam",
            VIOLATION => "violation",
            _ => "other",
        }
    }
}
