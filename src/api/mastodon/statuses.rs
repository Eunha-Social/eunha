use axum::{
    extract::{Extension, FromRequest, Multipart, Path, Query, RawQuery, State},
    http::{header, HeaderMap, Uri},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use std::collections::HashMap;

use super::scheduled_statuses::ScheduledStatusResponse;
use super::{
    accounts::{batch_account_emojis, batch_account_roles, batch_accounts_to_api},
    convert::{account_from_db, status_from_db},
    formatting::{render_content, HASHTAG_RE, MENTION_RE},
    status_serialize::{
        batch_reblog_data, batch_status_cards, batch_status_emojis, batch_status_media,
        batch_status_mentions, batch_status_polls, batch_statuses_tags, build_status,
        fetch_reblog_data, fetch_status_media, hydrate_status_stats, spawn_card_fetch,
    },
    types::{PaginationParams, Status, StatusContext, StatusEdit, StatusSource},
};
use crate::{
    db::models::{Account, Status as DbStatus},
    error::{AppError, AppResult},
    feed,
    middleware::{AuthenticatedUser, ResolvedInstance},
    push,
    state::AppState,
    streaming::Event,
};

#[derive(Debug, Deserialize, Default)]
pub struct PollForm {
    pub options: Vec<String>,
    pub expires_in: Option<i64>,
    pub multiple: Option<bool>,
    pub hide_totals: Option<bool>,
}

/// Embed the (context-less) quote `note` as a QuoteRequest's `instrument`,
/// folding the Note's JSON-LD term definitions into the request's compound
/// `@context` so the embedded terms (`quote`, `Hashtag`, `sensitive`, …) still
/// resolve. Mirrors how [`crate::api::ap::note::NoteBundle::into_create`] hoists
/// the note context to the activity's top level.
fn inline_quote_instrument(request: &mut serde_json::Value, note: serde_json::Value) {
    let note_ctx = crate::api::ap::note::note_context();
    if let (Some(req_terms), Some(note_terms)) = (
        request
            .get_mut("@context")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|ctx| ctx.get_mut(1))
            .and_then(serde_json::Value::as_object_mut),
        note_ctx.as_array().and_then(|ctx| ctx.get(1)).and_then(serde_json::Value::as_object),
    ) {
        for (key, value) in note_terms {
            req_terms
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }
    request["instrument"] = note;
}

/// Maximum media attachments per status (Mastodon `Status::MEDIA_ATTACHMENTS_LIMIT`).
const MEDIA_ATTACHMENTS_LIMIT: usize = 4;

/// Poll limits, matching Mastodon's `PollOptionsValidator` /
/// `PollExpirationValidator`.
const POLL_MAX_OPTIONS: usize = 4;
const POLL_MAX_OPTION_CHARS: usize = 50;
const POLL_MIN_EXPIRATION: i64 = 5 * 60; // 5 minutes
const POLL_MAX_EXPIRATION: i64 = 2_629_746; // ActiveSupport `1.month`

/// Validate a poll submission the way Mastodon validates the `Poll` model on a
/// local status: option count, non-blank, per-option length, uniqueness, and
/// expiration presence/bounds. Used by both the create and edit paths.
fn validate_poll_form(poll: &PollForm) -> AppResult<()> {
    use unicode_segmentation::UnicodeSegmentation;

    if poll.options.len() < 2 {
        return Err(AppError::Unprocessable(
            "Validation failed: Poll must have at least 2 options".into(),
        ));
    }
    if poll.options.len() > POLL_MAX_OPTIONS {
        return Err(AppError::Unprocessable(format!(
            "Validation failed: Poll can have at most {POLL_MAX_OPTIONS} options"
        )));
    }
    if poll.options.iter().any(|o| o.trim().is_empty()) {
        return Err(AppError::Unprocessable(
            "Validation failed: Poll options cannot be blank".into(),
        ));
    }
    if poll
        .options
        .iter()
        .any(|o| o.graphemes(true).count() > POLL_MAX_OPTION_CHARS)
    {
        return Err(AppError::Unprocessable(format!(
            "Validation failed: Poll options cannot be longer than {POLL_MAX_OPTION_CHARS} characters"
        )));
    }
    // Duplicate options (Mastodon: `options.uniq.size == options.size`).
    let mut seen = std::collections::HashSet::new();
    if !poll.options.iter().all(|o| seen.insert(o)) {
        return Err(AppError::Unprocessable(
            "Validation failed: Poll options must be unique".into(),
        ));
    }
    // Local polls require an expiration, bounded to [5 minutes, 1 month].
    match poll.expires_in {
        None => {
            return Err(AppError::Unprocessable(
                "Validation failed: Poll expiration can't be blank".into(),
            ))
        }
        Some(secs) if secs < POLL_MIN_EXPIRATION => {
            return Err(AppError::Unprocessable(
                "Validation failed: Poll duration is too short".into(),
            ));
        }
        Some(secs) if secs > POLL_MAX_EXPIRATION => {
            return Err(AppError::Unprocessable(
                "Validation failed: Poll duration is too long".into(),
            ));
        }
        Some(_) => {}
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
pub struct PostStatusForm {
    pub status: Option<String>,
    pub in_reply_to_id: Option<String>,
    #[serde(alias = "quote_id")]
    pub quoted_status_id: Option<String>,
    pub quote_approval_policy: Option<String>,
    pub spoiler_text: Option<String>,
    pub sensitive: Option<bool>,
    pub language: Option<String>,
    pub visibility: Option<String>,
    pub media_ids: Option<Vec<String>>,
    pub poll: Option<PollForm>,
    pub scheduled_at: Option<String>,
    pub allowed_mentions: Option<Vec<String>>,
}

// ── POST /api/v1/statuses ──────────────────────────────────────────────────

pub async fn post_status(
    State(state): State<AppState>,
    Extension(ResolvedInstance(instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    request: axum::extract::Request,
) -> AppResult<axum::response::Response> {
    use axum::response::IntoResponse;
    auth.require_scope("write:statuses")?;

    // Capture the Idempotency-Key header before the request body is consumed.
    let idempotency_key = request
        .headers()
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let form = extract_post_status_form(request).await?;
    let account = fetch_account(&state, auth.account_id).await?;

    // If we've already processed a request with this key, replay the stored status.
    if let Some(ref ik) = idempotency_key {
        use redis::AsyncCommands;
        let redis_key = format!("idempotency:{}:{}", auth.account_id, ik);
        let mut redis = state.redis.clone();
        if let Ok(Some(existing_id)) = redis.get::<_, Option<i64>>(&redis_key).await {
            if let Some(status) = sqlx::query_as!(
                DbStatus,
                "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
                existing_id,
            )
            .fetch_optional(&state.db)
            .await?
            {
                let media = fetch_status_media(&state, status.id).await?;
                let viewer_ctx = build_viewer_context(&state, auth.account_id, status.id)
                    .await
                    .ok();
                let api_status = super::status_serialize::build_status_with_app(
                    &state, &status, &account, media, None, viewer_ctx, None,
                )
                .await?;
                return Ok((axum::http::StatusCode::OK, Json(api_status)).into_response());
            }
        }
    }
    let mut text = form.status.clone().unwrap_or_default();
    let mut spoiler_text = form.spoiler_text.clone().unwrap_or_default();
    // Mastodon PostStatusService#preprocess_attributes promotes a lone content
    // warning (no body, no quote) into the body, leaving no CW. `sensitive` is
    // still forced on below because the CW was present when it was evaluated.
    let spoiler_was_present = !spoiler_text.is_empty();
    if text.is_empty() && spoiler_was_present && form.quoted_status_id.is_none() {
        text = std::mem::take(&mut spoiler_text);
    }
    if text.is_empty()
        && form.media_ids.as_ref().is_none_or(|m| m.is_empty())
        && form.poll.is_none()
    {
        return Err(AppError::Unprocessable(
            "Status must have text or media".into(),
        ));
    }
    // Mastodon StatusLengthValidator: spoiler + body, URLs as 23 chars, mentions
    // without their domain, counted in grapheme clusters.
    if super::formatting::countable_length(&text, &spoiler_text) > 500 {
        return Err(AppError::Unprocessable(
            "Validation failed: Text character limit of 500 exceeded".into(),
        ));
    }

    // Validate poll options before inserting anything
    if let Some(ref poll_form) = form.poll {
        validate_poll_form(poll_form)?;
    }

    // Handle scheduled statuses. Mastodon's PostStatusService ignores a
    // scheduled_at in the past (posts immediately); otherwise ScheduledStatus
    // must be at least MINIMUM_OFFSET (5 min) in the future and is bounded by
    // total (300) and daily (25) per-account limits.
    if let Some(ref scheduled_at_str) = form.scheduled_at {
        let scheduled_at = chrono::DateTime::parse_from_rfc3339(scheduled_at_str)
            .map(|t| t.with_timezone(&chrono::Utc).naive_utc())
            .map_err(|_| AppError::Unprocessable("Invalid scheduled_at format".into()))?;
        let now = chrono::Utc::now().naive_utc();
        // Past dates fall through and post immediately.
        if scheduled_at > now {
            if scheduled_at <= now + chrono::Duration::minutes(5) {
                return Err(AppError::Unprocessable(
                    "Validation failed: Scheduled date must be in the future".into(),
                ));
            }
            let total = sqlx::query_scalar!(
                "SELECT COUNT(*) FROM scheduled_statuses WHERE account_id = $1",
                account.id,
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(0);
            if total >= 300 {
                return Err(AppError::Unprocessable(
                    "Validation failed: Total number of scheduled statuses exceeded".into(),
                ));
            }
            let daily = sqlx::query_scalar!(
                "SELECT COUNT(*) FROM scheduled_statuses WHERE account_id = $1 AND scheduled_at::date = $2",
                account.id,
                scheduled_at.date(),
            )
            .fetch_one(&state.db)
            .await?
            .unwrap_or(0);
            if daily >= 25 {
                return Err(AppError::Unprocessable(
                    "Validation failed: Daily number of scheduled statuses exceeded".into(),
                ));
            }
            let params = serde_json::json!({
                "text": text,
                "visibility": form.visibility,
                "spoiler_text": spoiler_text,
                "sensitive": form.sensitive,
                "language": form.language,
                "in_reply_to_id": form.in_reply_to_id,
                "media_ids": form.media_ids,
                "poll": form.poll.as_ref().map(|p| serde_json::json!({
                    "options": p.options,
                    "expires_in": p.expires_in,
                    "multiple": p.multiple,
                    "hide_totals": p.hide_totals,
                })),
            });
            let row = sqlx::query!(
                r#"INSERT INTO scheduled_statuses (account_id, scheduled_at, params)
                   VALUES ($1, $2, $3)
                   RETURNING id, scheduled_at"#,
                account.id,
                scheduled_at,
                params,
            )
            .fetch_one(&state.db)
            .await?;
            let resp = ScheduledStatusResponse {
                id: row.id.to_string(),
                scheduled_at: row.scheduled_at.map(super::convert::mastodon_date),
                params,
                media_attachments: vec![],
            };
            return Ok((axum::http::StatusCode::CREATED, Json(resp)).into_response());
        }
    }

    // Reject an unrecognized visibility rather than silently coercing it (the
    // fallback maps unknown strings to `direct`, which would turn a typo into a
    // DM). Mastodon only accepts these client-settable visibilities.
    if let Some(v) = form.visibility.as_deref() {
        if !matches!(v, "public" | "unlisted" | "private" | "direct") {
            return Err(AppError::Unprocessable(format!(
                "Validation failed: Visibility is not included in the list: {v}"
            )));
        }
    }

    // Fall back to the user's stored posting defaults when the form omits them.
    let defaults = super::accounts::user_defaults(&state, auth.account_id).await;
    let mut visibility = form
        .visibility
        .as_deref()
        .map(str::to_owned)
        .unwrap_or(defaults.privacy);
    // A silenced account cannot post publicly: Mastodon downgrades public to
    // unlisted so the post stays out of public and federated timelines.
    if visibility == "public" && account.silenced_at.is_some() {
        visibility = "unlisted".to_string();
    }
    // Mastodon forces sensitive when a content warning is present
    // (PostStatusService: `sensitive || spoiler_text.present?`).
    let sensitive = form.sensitive.unwrap_or(defaults.sensitive) || spoiler_was_present;
    let language = form.language.clone().or(defaults.language);
    let in_reply_to_id = form
        .in_reply_to_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());

    // Look up the parent author for in_reply_to_account_id
    let in_reply_to_account_id: Option<i64> = if let Some(parent_id) = in_reply_to_id {
        let account_id = sqlx::query_scalar!(
            "SELECT account_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
            parent_id,
        )
        .fetch_optional(&state.db)
        .await?;
        if account_id.is_none() {
            return Err(AppError::Unprocessable(
                "in_reply_to_id does not exist".into(),
            ));
        }
        account_id
    } else {
        None
    };

    // Validate quoted_status_id
    let mut quoted_author_id: Option<i64> = None;
    let quote_of_id: Option<i64> = if let Some(ref qid_str) = form.quoted_status_id {
        let qid = qid_str
            .parse::<i64>()
            .map_err(|_| AppError::Unprocessable("invalid quoted_status_id".into()))?;
        let quoted = sqlx::query!(
            "SELECT id, account_id, visibility, reblog_of_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
            qid,
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Unprocessable("quoted_status_id does not exist".into()))?;
        // Cannot quote direct messages
        if quoted.visibility == 3 {
            return Err(AppError::Unprocessable(
                "cannot quote a direct message".into(),
            ));
        }
        // Cannot quote a reblog; must quote the original post directly
        if quoted.reblog_of_id.is_some() {
            return Err(AppError::Unprocessable("cannot quote a reblog".into()));
        }
        // Quoting a followers-only post forces the quote down to followers-only,
        // so the quoted content is never exposed to a wider audience than the
        // original (Mastodon PostStatusService#preprocess_attributes).
        if quoted.visibility == crate::db::models::vis::PRIVATE
            && matches!(visibility.as_str(), "public" | "unlisted")
        {
            visibility = "private".to_string();
        }
        // Block check against quoted author
        let blocked = sqlx::query_scalar!(
            r#"SELECT 1 FROM blocks
               WHERE (account_id = $1 AND target_account_id = $2)
                  OR (account_id = $2 AND target_account_id = $1)
               LIMIT 1"#,
            account.id,
            quoted.account_id,
        )
        .fetch_optional(&state.db)
        .await?;
        if blocked.is_some() {
            return Err(AppError::Unprocessable(
                "not allowed to interact with this post".into(),
            ));
        }
        quoted_author_id = Some(quoted.account_id);
        Some(quoted.id)
    } else {
        None
    };

    let hashtags = extract_hashtags(&text);
    let mention_handles = extract_mention_handles(&text);
    let resolved = resolve_mention_accounts(&state, &mention_handles, &instance.domain).await;

    // Mastodon safeguard_private_mention_quote!: a direct post that quotes
    // someone else's status must mention that author, otherwise they would be
    // quoted into a conversation they cannot see.
    if visibility == "direct" {
        if let Some(qauthor) = quoted_author_id {
            if qauthor != account.id && !resolved.iter().any(|(_, a)| a.id == qauthor) {
                return Err(AppError::Unprocessable(
                    "Validation failed: The quoted user must be mentioned in a direct message"
                        .into(),
                ));
            }
        }
    }

    // Safeguard: if the caller passed allowed_mentions, reject the post if any resolved
    // mentions are not in that list (mirrors Mastodon's PostStatusService#safeguard_mentions!).
    if let Some(ref allowed_ids) = form.allowed_mentions {
        let unexpected: Vec<serde_json::Value> = resolved
            .iter()
            .filter(|(_, acct)| !allowed_ids.iter().any(|aid| aid == &acct.id.to_string()))
            .map(|(_, acct)| serde_json::json!({ "id": acct.id.to_string(), "acct": acct.acct() }))
            .collect();
        if !unexpected.is_empty() {
            let body = serde_json::json!({
                "error": "These accounts will be mentioned, but you did not explicitly select them",
                "unexpected_accounts": unexpected,
            });
            return Ok((axum::http::StatusCode::UNPROCESSABLE_ENTITY, Json(body)).into_response());
        }
    }

    let mention_map = build_mention_map(&resolved, &instance.domain);
    let content = render_content(&text, &instance.domain, &mention_map);

    let status_id = crate::snowflake::next_id();
    let uri = crate::federation::tag::status_uri(
        &instance.domain,
        account.id,
        account.id_scheme,
        &account.username,
        status_id,
    );
    // Human permalink — always the /@username form, independent of id_scheme.
    let human_url = format!(
        "https://{}/@{}/{}",
        instance.domain, account.username, status_id
    );

    // Validate media_ids before inserting the status (Mastodon
    // PostStatusService#validate_media!) — fail early so no cleanup is needed.
    let parsed_media_ids: Vec<i64> = if let Some(ref ids) = form.media_ids {
        // Reject more than the 4-attachment limit outright.
        if ids.len() > MEDIA_ATTACHMENTS_LIMIT {
            return Err(AppError::Unprocessable(format!(
                "Validation failed: Cannot attach more than {MEDIA_ATTACHMENTS_LIMIT} files"
            )));
        }
        let mut parsed = Vec::with_capacity(ids.len());
        let mut has_audio_or_video = false;
        let mut any_not_ready = false;
        for id_str in ids {
            let media_id = id_str.parse::<i64>().map_err(|_| {
                AppError::Unprocessable(format!("media_ids: invalid id '{}'", id_str))
            })?;
            let row = sqlx::query!(
                r#"SELECT "type", processing FROM media_attachments
                   WHERE id = $1 AND account_id = $2 AND status_id IS NULL"#,
                media_id,
                account.id,
            )
            .fetch_optional(&state.db)
            .await?;
            let Some(row) = row else {
                return Err(AppError::Unprocessable(format!(
                    "media_ids: '{}' not found, already attached, or not owned by you",
                    id_str
                )));
            };
            // audio(3)/video(2) can't be combined with other media (gifv is exempt,
            // matching MediaAttachment#audio_or_video?).
            if matches!(row.r#type, 2 | 3) {
                has_audio_or_video = true;
            }
            // processing set and not complete(2) means still processing/failed.
            if row.processing.is_some_and(|p| p != 2) {
                any_not_ready = true;
            }
            parsed.push(media_id);
        }
        if parsed.len() > 1 && has_audio_or_video {
            return Err(AppError::Unprocessable(
                "Validation failed: Cannot attach a video or audio file to a post that contains other media".into(),
            ));
        }
        if any_not_ready {
            return Err(AppError::Unprocessable(
                "Validation failed: Cannot attach files that have not finished processing. Try again in a moment!".into(),
            ));
        }
        parsed
    } else {
        vec![]
    };

    let is_reply = in_reply_to_id.is_some();
    let visibility_int = crate::db::models::vis::from_str(&visibility);
    let quote_policy_int = crate::db::models::quote_policy::from_str(
        form.quote_approval_policy
            .as_deref()
            .unwrap_or(&defaults.quote_policy),
    );
    let status = sqlx::query_as!(
        DbStatus,
        r#"INSERT INTO statuses
             (id, account_id, application_id, text, spoiler_text, visibility,
              language, sensitive, in_reply_to_id, in_reply_to_account_id, reply, uri, url,
              quote_approval_policy, local, created_at, updated_at)
           VALUES ($1,$2,$10,$3,$4,$5,$6,$7,$8,$9,$12,$11,$14,$13, true, now(), now())
           RETURNING *"#,
        status_id,
        account.id,
        text,
        spoiler_text,
        visibility_int,
        language,
        sensitive,
        in_reply_to_id,
        in_reply_to_account_id,
        auth.application_id,
        uri,
        is_reply,
        quote_policy_int,
        human_url,
    )
    .fetch_one(&state.db)
    .await?;

    // Create a quotes record if this is a quote post. When the quoted author is
    // remote we ask for consent via a FEP-044f QuoteRequest (sent below) and keep
    // the quote pending until they Accept; local quotes accept by visibility.
    let mut quote_request_activity_uri: Option<String> = None;
    if let Some(qid) = quote_of_id {
        let quoted = sqlx::query!(
            "SELECT s.account_id, s.visibility, s.quote_approval_policy, a.domain
             FROM statuses s JOIN accounts a ON a.id = s.account_id
             WHERE s.id = $1",
            qid,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();

        let quoted_is_remote = quoted.as_ref().and_then(|q| q.domain.clone()).is_some();
        let quoted_account_id = quoted.as_ref().map(|q| q.account_id).unwrap_or(account.id);

        let (quote_state, activity_uri) = if quoted_is_remote {
            let au = format!(
                "https://{}/users/{}/quote_requests/{}",
                instance.domain,
                account.username,
                crate::snowflake::next_id()
            );
            quote_request_activity_uri = Some(au.clone());
            (crate::db::models::quote_state::PENDING, Some(au))
        } else {
            use crate::db::models::quote_policy;
            let policy = quoted
                .as_ref()
                .map(|q| q.quote_approval_policy)
                .unwrap_or(quote_policy::PUBLIC);
            // The author's own quotes are always accepted; a manual-approval
            // policy holds others' quotes pending until the author approves.
            let st = if quoted_account_id == account.id {
                crate::db::models::quote_state::ACCEPTED
            } else if policy == quote_policy::MANUAL || policy == quote_policy::NOBODY {
                crate::db::models::quote_state::PENDING
            } else {
                match quoted.as_ref().map(|q| q.visibility) {
                    Some(0) | Some(1) => crate::db::models::quote_state::ACCEPTED, // public, unlisted
                    _ => crate::db::models::quote_state::PENDING,
                }
            };
            (st, None)
        };

        let quote_row_id = crate::snowflake::next_id();
        let _ = sqlx::query!(
            r#"INSERT INTO quotes (id, status_id, quoted_status_id, account_id, quoted_account_id, activity_uri, state, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, now(), now())
               ON CONFLICT DO NOTHING"#,
            quote_row_id,
            status.id,
            qid,
            account.id,
            quoted_account_id,
            activity_uri,
            quote_state,
        )
        .execute(&state.db)
        .await;

        // Accepted quotes count toward the quoted status's quotes_count.
        if quote_state == crate::db::models::quote_state::ACCEPTED {
            let _ = sqlx::query!(
                r#"INSERT INTO status_stats (status_id, quotes_count, created_at, updated_at)
                   VALUES ($1, 1, now(), now())
                   ON CONFLICT (status_id) DO UPDATE
                     SET quotes_count = status_stats.quotes_count + 1, updated_at = now()"#,
                qid,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Store tags and mentions
    store_statuses_tags(&state, status.id, account.id, &hashtags).await?;
    store_status_mentions(&state, status.id, &resolved).await?;

    // Mastodon assigns a conversation_id to every status. For replies, inherit
    // the parent's conversation; otherwise create a new one.
    let conv_id = if let Some(parent_id) = in_reply_to_id {
        sqlx::query_scalar!(
            "SELECT conversation_id FROM statuses WHERE id = $1",
            parent_id
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .flatten()
    } else {
        None
    };

    let conv_id = if let Some(cid) = conv_id {
        cid
    } else {
        sqlx::query_scalar!(
            "INSERT INTO conversations (created_at, updated_at) VALUES (now(), now()) RETURNING id",
        )
        .fetch_one(&state.db)
        .await?
    };

    sqlx::query!(
        "UPDATE statuses SET conversation_id = $1 WHERE id = $2",
        conv_id,
        status.id
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        "UPDATE conversations SET updated_at = now() WHERE id = $1",
        conv_id
    )
    .execute(&state.db)
    .await?;

    // For direct messages, also manage the account_conversations inbox.
    if visibility == "direct" {
        // Build sorted participant ID lists for each party's account_conversations row.
        // Mastodon convention: participant_account_ids = everyone else in the conversation.
        let mut mentioned_ids: Vec<i64> = resolved
            .iter()
            .map(|(_, m)| m.id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        mentioned_ids.sort_unstable();

        // Sender sees the mentioned accounts as participants.
        sqlx::query!(
            r#"INSERT INTO account_conversations
                   (account_id, conversation_id, participant_account_ids, status_ids, last_status_id, unread)
               VALUES ($1, $2, $3, ARRAY[$4::bigint], $4, false)
               ON CONFLICT (account_id, conversation_id, participant_account_ids) DO UPDATE
                   SET unread         = false,
                       last_status_id = EXCLUDED.last_status_id,
                       status_ids     = array_append(account_conversations.status_ids, EXCLUDED.last_status_id),
                       lock_version   = account_conversations.lock_version + 1"#,
            account.id, conv_id, &mentioned_ids, status.id
        )
        .execute(&state.db)
        .await?;

        // Each recipient sees the sender (plus other recipients) as participants.
        for (_, mentioned) in &resolved {
            let mut recipient_participants: Vec<i64> = std::iter::once(account.id)
                .chain(
                    resolved
                        .iter()
                        .filter(|(_, m)| m.id != mentioned.id)
                        .map(|(_, m)| m.id),
                )
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            recipient_participants.sort_unstable();

            sqlx::query!(
                r#"INSERT INTO account_conversations
                       (account_id, conversation_id, participant_account_ids, status_ids, last_status_id, unread)
                   VALUES ($1, $2, $3, ARRAY[$4::bigint], $4, true)
                   ON CONFLICT (account_id, conversation_id, participant_account_ids) DO UPDATE
                       SET unread         = true,
                           last_status_id = EXCLUDED.last_status_id,
                           status_ids     = array_append(account_conversations.status_ids, EXCLUDED.last_status_id),
                           lock_version   = account_conversations.lock_version + 1"#,
                mentioned.id, conv_id, &recipient_participants, status.id
            )
            .execute(&state.db)
            .await?;
        }
    }

    // Advance last_status_at and increment statuses_count in account_stats
    sqlx::query!(
        r#"INSERT INTO account_stats (account_id, statuses_count, last_status_at, created_at, updated_at)
           VALUES ($1, 1, $2, now(), now())
           ON CONFLICT (account_id) DO UPDATE
             SET statuses_count = account_stats.statuses_count + 1,
                 last_status_at = GREATEST(account_stats.last_status_at, $2),
                 updated_at = now()"#,
        account.id,
        status.created_at,
    )
    .execute(&state.db)
    .await?;

    // Increment parent's replies_count in status_stats if this is a reply
    if let Some(parent_id) = in_reply_to_id {
        let _ = sqlx::query!(
            r#"INSERT INTO status_stats (status_id, replies_count, created_at, updated_at)
               VALUES ($1, 1, now(), now())
               ON CONFLICT (status_id) DO UPDATE
                 SET replies_count = status_stats.replies_count + 1,
                     updated_at = now()"#,
            parent_id
        )
        .execute(&state.db)
        .await;
    }

    // Attach media (IDs already validated above)
    for media_id in &parsed_media_ids {
        sqlx::query!(
            "UPDATE media_attachments SET status_id = $1
             WHERE id = $2 AND account_id = $3 AND status_id IS NULL",
            status.id,
            media_id,
            account.id
        )
        .execute(&state.db)
        .await?;
    }

    // Create poll if requested (options already validated above)
    if let Some(ref poll_form) = form.poll {
        let expires_at = poll_form
            .expires_in
            .map(|secs| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(secs));
        let poll_options: Vec<String> = poll_form.options.clone();
        let poll_id = sqlx::query_scalar!(
            r#"INSERT INTO polls
                 (status_id, account_id, options, multiple, hide_totals, expires_at, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, now(), now())
               RETURNING id"#,
            status.id, account.id, &poll_options as &[String],
            poll_form.multiple.unwrap_or(false),
            poll_form.hide_totals.unwrap_or(false),
            expires_at,
        )
        .fetch_one(&state.db)
        .await?;
        // Link the poll back onto the status, mirroring the federation ingest
        // path so `statuses.poll_id` is consistently populated for local polls.
        sqlx::query!(
            "UPDATE statuses SET poll_id = $1 WHERE id = $2",
            poll_id,
            status.id,
        )
        .execute(&state.db)
        .await?;
    }

    let mut status = status;
    status.uri = Some(uri.clone());

    // Load the application that created this status (for the author's view)
    let application = if let Some(app_id) = auth.application_id {
        sqlx::query!(
            "SELECT name, website FROM oauth_applications WHERE id = $1",
            app_id,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| super::types::Application {
            name: r.name,
            website: r.website,
        })
    } else {
        None
    };

    let media = fetch_status_media(&state, status.id).await?;
    let viewer_ctx = build_viewer_context(&state, auth.account_id, status.id)
        .await
        .ok();
    let api_status = super::status_serialize::build_status_with_app(
        &state,
        &status,
        &account,
        media,
        None,
        viewer_ctx,
        application,
    )
    .await?;

    spawn_card_fetch(&state, status.id, content.clone());

    if matches!(visibility.as_str(), "public" | "unlisted" | "private") {
        if let Ok(payload) = serde_json::to_string(&api_status) {
            let hashtags: Vec<String> = api_status.tags.iter().map(|t| t.name.clone()).collect();
            state.streaming.publish(Event::NewStatus {
                author_id: account.id,
                is_public: visibility == "public",
                is_direct: visibility == "direct",
                status_id: status.id,
                hashtags,
                has_media: !api_status.media_attachments.is_empty(),
                payload: std::sync::Arc::new(payload),
            });
        }
    }

    // Notify the author of the parent status if this is a reply
    let mut notified = std::collections::HashSet::new();
    if let Some(parent_id) = in_reply_to_id {
        if let Ok(Some(parent)) = sqlx::query!(
            "SELECT account_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
            parent_id,
        )
        .fetch_optional(&state.db)
        .await
        {
            push::create_and_push(
                &state,
                parent.account_id,
                account.id,
                "mention",
                Some(status.id),
                format!("{} mentioned you", account.display_name),
                account.acct().clone(),
                super::convert::account_avatar_url_for(&account),
            )
            .await;
            notified.insert(parent.account_id);
        }
    }

    // Notify each mentioned account not already notified above
    for (_, mentioned) in &resolved {
        if mentioned.id == account.id || notified.contains(&mentioned.id) {
            continue;
        }
        push::create_and_push(
            &state,
            mentioned.id,
            account.id,
            "mention",
            Some(status.id),
            format!("{} mentioned you", account.display_name),
            account.acct().clone(),
            super::convert::account_avatar_url_for(&account),
        )
        .await;
        notified.insert(mentioned.id);
    }

    // Notify followers who opted in to per-account posting notifications (the
    // "bell"). Mastodon's FeedInsertWorker#notify? excludes replies to other
    // accounts (self-replies still notify), reblogs, and edits.
    let is_reply_to_other = in_reply_to_id.is_some() && in_reply_to_account_id != Some(account.id);
    if (visibility == "public" || visibility == "unlisted") && !is_reply_to_other {
        if let Ok(followers) = sqlx::query!(
            r#"SELECT account_id FROM follows
               WHERE target_account_id = $1 AND notify = true"#,
            account.id,
        )
        .fetch_all(&state.db)
        .await
        {
            for row in followers {
                if notified.contains(&row.account_id) {
                    continue;
                }
                push::create_and_push(
                    &state,
                    row.account_id,
                    account.id,
                    "status",
                    Some(status.id),
                    format!("{} posted a new status", account.display_name),
                    account.acct().clone(),
                    super::convert::account_avatar_url_for(&account),
                )
                .await;
            }
        }
    }

    // Fan-out to follower feeds and list feeds in background (non-blocking)
    {
        let tag_ids: Vec<i64> = sqlx::query_scalar!(
            "SELECT tag_id FROM statuses_tags WHERE status_id = $1",
            status.id
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();

        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let author_id = account.id;
        let status_id = status.id;
        let reply_to_account = in_reply_to_account_id;
        let vis = visibility.clone();
        if feed::sync_fanout() {
            feed::fanout_new_status(&mut redis, &db, author_id, status_id, &tag_ids).await;
            feed::fanout_to_lists(
                &mut redis,
                &db,
                author_id,
                status_id,
                reply_to_account,
                &vis,
            )
            .await;
        } else {
            tokio::spawn(async move {
                feed::fanout_new_status(&mut redis, &db, author_id, status_id, &tag_ids).await;
                feed::fanout_to_lists(
                    &mut redis,
                    &db,
                    author_id,
                    status_id,
                    reply_to_account,
                    &vis,
                )
                .await;
            });
        }
    }

    // Federate outgoing statuses to remote inboxes
    if matches!(
        visibility.as_str(),
        "public" | "unlisted" | "private" | "direct"
    ) && account
        .private_key
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        let domain = &instance.domain;
        let actor_url = crate::federation::tag::account_uri_of(domain, &account);
        let key_id = format!("{}#main-key", actor_url);

        // Build the Create(Note) from the persisted status so the wire shape
        // matches what we serve at the note's own URI (content, media
        // attachments, and the mention/hashtag/emoji tag array).
        let Some(bundle) = crate::api::ap::note::build_note(&state, domain, status.id).await?
        else {
            return Err(AppError::Internal(anyhow::anyhow!(
                "failed to build Note for status {}",
                status.id
            )));
        };
        // Keep a copy of the (context-less) Note to inline as the QuoteRequest
        // `instrument` below, before the bundle is consumed by `into_create`.
        let quote_note = quote_of_id.map(|_| bundle.note.clone());
        let activity = bundle.into_create();

        // FEP-044f: ask a remote quoted author for consent to quote them.
        if let (Some(qr_uri), Some(qid)) = (&quote_request_activity_uri, quote_of_id) {
            let quoted_uri: Option<String> =
                sqlx::query_scalar!("SELECT uri FROM statuses WHERE id = $1", qid)
                    .fetch_optional(&state.db)
                    .await
                    .ok()
                    .flatten()
                    .flatten();
            if let (Some(quoted_status_uri), Ok(Some(qa))) = (
                quoted_uri,
                sqlx::query!(
                    "SELECT a.inbox_url, a.shared_inbox_url
                         FROM statuses s JOIN accounts a ON a.id = s.account_id
                         WHERE s.id = $1",
                    qid,
                )
                .fetch_optional(&state.db)
                .await,
            ) {
                let qinbox = if !qa.shared_inbox_url.is_empty() {
                    qa.shared_inbox_url
                } else {
                    qa.inbox_url
                };
                if !qinbox.is_empty() {
                    if let Ok(mut qr) = crate::federation::consent::quote_request(
                        qr_uri,
                        &actor_url,
                        &quoted_status_uri,
                        &uri,
                    ) {
                        // Inline the quote Note as `instrument` (Mastodon's
                        // `allow_post_inlining`): the quoted author's server
                        // validates the request against the embedded object
                        // rather than dereferencing it, so quoting works even
                        // for non-public posts it cannot fetch from us.
                        if let Some(note) = quote_note.clone() {
                            inline_quote_instrument(&mut qr, note);
                        }
                        if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                            &state,
                            qr,
                            vec![qinbox],
                            key_id.clone(),
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "failed to enqueue QuoteRequest");
                        }
                    }
                }
            }
        }

        // Reach the full status audience (StatusReachFinder): followers +
        // mentions + replied-to author + quoted author + relays (public).
        use crate::db::models::vis;
        let vis_int = vis::from_str(&visibility);
        let inboxes = crate::federation::delivery::status_reach_inboxes(
            &state,
            status.id,
            account.id,
            in_reply_to_account_id,
            matches!(vis_int, vis::PUBLIC | vis::UNLISTED),
            false,
            vis_int == vis::PUBLIC,
            matches!(vis_int, vis::PUBLIC | vis::UNLISTED | vis::PRIVATE),
            None,
            &[],
        )
        .await
        .unwrap_or_default();
        if !inboxes.is_empty() {
            if let Err(e) =
                crate::federation::delivery::deliver_to_inboxes(&state, activity, inboxes, key_id)
                    .await
            {
                tracing::warn!(error = %e, "failed to enqueue status delivery");
            }
        }
    }

    // Record the idempotency mapping so a retried request replays this status.
    if let Some(ref ik) = idempotency_key {
        use redis::AsyncCommands;
        let redis_key = format!("idempotency:{}:{}", auth.account_id, ik);
        let mut redis = state.redis.clone();
        let _: redis::RedisResult<()> = redis.set_ex(redis_key, status.id, 21600).await;
    }

    Ok((axum::http::StatusCode::OK, Json(api_status)).into_response())
}

async fn extract_post_status_form(request: axum::extract::Request) -> AppResult<PostStatusForm> {
    let ct = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if ct.contains("application/json") {
        return axum::extract::Json::<PostStatusForm>::from_request(request, &())
            .await
            .map(|axum::extract::Json(f)| f)
            .map_err(|e| AppError::Unprocessable(e.to_string()));
    }

    if ct.contains("multipart/form-data") {
        let mut multipart = Multipart::from_request(request, &())
            .await
            .map_err(|e| AppError::Unprocessable(e.to_string()))?;
        let mut form = PostStatusForm::default();
        let mut media_ids: Vec<String> = Vec::new();
        while let Some(field) = multipart
            .next_field()
            .await
            .map_err(|e| AppError::Unprocessable(e.to_string()))?
        {
            let name = field.name().unwrap_or("").to_string();
            let text = field
                .text()
                .await
                .map_err(|e| AppError::Unprocessable(e.to_string()))?;
            match name.as_str() {
                "status" => form.status = Some(text),
                "in_reply_to_id" => {
                    form.in_reply_to_id = if text.is_empty() { None } else { Some(text) }
                }
                "quoted_status_id" | "quote_id" => {
                    form.quoted_status_id = if text.is_empty() { None } else { Some(text) }
                }
                "quote_approval_policy" => {
                    form.quote_approval_policy = if text.is_empty() { None } else { Some(text) }
                }
                "spoiler_text" => {
                    form.spoiler_text = if text.is_empty() { None } else { Some(text) }
                }
                "visibility" => form.visibility = Some(text),
                "language" => form.language = if text.is_empty() { None } else { Some(text) },
                "sensitive" => form.sensitive = Some(text == "true" || text == "1"),
                "scheduled_at" => {
                    form.scheduled_at = if text.is_empty() { None } else { Some(text) }
                }
                "media_ids[]" | "media_ids" => {
                    if !text.is_empty() {
                        media_ids.push(text);
                    }
                }
                name if name.starts_with("poll[options]") || name == "poll[options][]" => {
                    if !text.is_empty() {
                        let p = form.poll.get_or_insert_with(PollForm::default);
                        p.options.push(text);
                    }
                }
                "poll[expires_in]" => {
                    if let Ok(n) = text.parse::<i64>() {
                        form.poll.get_or_insert_with(PollForm::default).expires_in = Some(n);
                    }
                }
                "poll[multiple]" => {
                    form.poll.get_or_insert_with(PollForm::default).multiple =
                        Some(text == "true" || text == "1");
                }
                "poll[hide_totals]" => {
                    form.poll.get_or_insert_with(PollForm::default).hide_totals =
                        Some(text == "true" || text == "1");
                }
                _ => {}
            }
        }
        if !media_ids.is_empty() {
            form.media_ids = Some(media_ids);
        }
        return Ok(form);
    }

    // Fall back to URL-encoded form
    axum::extract::Form::<PostStatusForm>::from_request(request, &())
        .await
        .map(|axum::extract::Form(f)| f)
        .map_err(|e| AppError::Unprocessable(e.to_string()))
}

// ── GET /api/v1/statuses/:id ───────────────────────────────────────────────

// ── GET /api/v1/statuses (batch) ──────────────────────────────────────────

pub async fn get_statuses_batch(
    State(state): State<AppState>,
    RawQuery(qs): RawQuery,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<Status>>> {
    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);

    let ids: Vec<i64> = url::form_urlencoded::parse(qs.as_deref().unwrap_or("").as_bytes())
        .filter(|(k, _)| k == "id[]" || k == "id")
        .filter_map(|(_, v)| v.parse::<i64>().ok())
        .collect();

    if ids.len() > 20 {
        return Err(AppError::Unprocessable("Too many IDs requested".into()));
    }

    if ids.is_empty() {
        return Ok(Json(vec![]));
    }

    let statuses: Vec<DbStatus> = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = ANY($1::bigint[]) AND deleted_at IS NULL",
        &ids,
    )
    .fetch_all(&state.db)
    .await?;

    if statuses.is_empty() {
        return Ok(Json(vec![]));
    }

    // Batch block check
    let blocked_account_ids: std::collections::HashSet<i64> = if let Some(vid) = viewer_id {
        let other_ids: Vec<i64> = statuses
            .iter()
            .filter(|s| s.account_id != vid)
            .map(|s| s.account_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if other_ids.is_empty() {
            std::collections::HashSet::new()
        } else {
            sqlx::query_scalar!(
                r#"SELECT target_account_id FROM blocks WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])
                   UNION
                   SELECT account_id FROM blocks WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])"#,
                vid, &other_ids,
            )
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .flatten()
            .collect()
        }
    } else {
        std::collections::HashSet::new()
    };

    // Batch follow check for private statuses
    let private_author_ids: Vec<i64> = statuses
        .iter()
        .filter(|s| {
            s.visibility == crate::db::models::vis::PRIVATE && viewer_id != Some(s.account_id)
        })
        .map(|s| s.account_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let followed_ids: std::collections::HashSet<i64> = if let (Some(vid), false) =
        (viewer_id, private_author_ids.is_empty())
    {
        sqlx::query_scalar!(
            "SELECT target_account_id FROM follows WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
            vid, &private_author_ids,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };

    // Batch mention check for statuses whose visibility can be granted by mention.
    let mention_checked_ids: Vec<i64> = statuses
        .iter()
        .filter(|s| {
            matches!(
                s.visibility,
                crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
            ) && viewer_id != Some(s.account_id)
        })
        .map(|s| s.id)
        .collect();
    let mentioned_status_ids: std::collections::HashSet<i64> = if let (Some(vid), false) =
        (viewer_id, mention_checked_ids.is_empty())
    {
        sqlx::query_scalar!(
            "SELECT status_id FROM mentions WHERE account_id = $1 AND status_id = ANY($2::bigint[])",
            vid, &mention_checked_ids,
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };

    let visible: Vec<DbStatus> = statuses
        .into_iter()
        .filter(|s| {
            if viewer_id != Some(s.account_id) && blocked_account_ids.contains(&s.account_id) {
                return false;
            }
            match s.visibility {
                crate::db::models::vis::PRIVATE => {
                    viewer_id == Some(s.account_id)
                        || followed_ids.contains(&s.account_id)
                        || mentioned_status_ids.contains(&s.id)
                }
                crate::db::models::vis::DIRECT => {
                    viewer_id == Some(s.account_id) || mentioned_status_ids.contains(&s.id)
                }
                _ => true,
            }
        })
        .collect();

    if visible.is_empty() {
        return Ok(Json(vec![]));
    }

    let account_ids: Vec<i64> = visible
        .iter()
        .map(|s| s.account_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let accounts_vec: Vec<Account> = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
        &account_ids,
    )
    .fetch_all(&state.db)
    .await?;
    let account_map: HashMap<i64, Account> = accounts_vec.into_iter().map(|a| (a.id, a)).collect();

    let all_ids: Vec<i64> = visible.iter().map(|s| s.id).collect();
    let media_map = batch_status_media(&state, &all_ids).await?;
    let reblog_map = batch_reblog_data(&state, &visible).await?;
    let reblog_ids: Vec<i64> = reblog_map.values().map(|(rs, _, _)| rs.id).collect();
    let mut enrich_ids = all_ids.clone();
    enrich_ids.extend_from_slice(&reblog_ids);
    let tags_map = batch_statuses_tags(&state, &enrich_ids).await?;
    let mentions_map = batch_status_mentions(&state, &enrich_ids).await?;
    let all_for_emoji: Vec<DbStatus> = visible
        .iter()
        .cloned()
        .chain(reblog_map.values().map(|(rs, _, _)| rs.clone()))
        .collect();
    let emojis_map = batch_status_emojis(&state, &all_for_emoji).await?;
    let polls_map = batch_status_polls(&state, &enrich_ids, viewer_id).await?;
    let cards_map = batch_status_cards(&state, &enrich_ids).await?;
    let viewer_ctxs = if let Some(vid) = viewer_id {
        batch_viewer_contexts(&state, vid, &all_ids).await?
    } else {
        HashMap::new()
    };
    // Preserve original request order
    let id_order: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let mut indexed: Vec<(usize, Status)> = Vec::with_capacity(visible.len());
    for s in &visible {
        let Some(account) = account_map.get(&s.account_id) else {
            continue;
        };
        let media = media_map.get(&s.id).cloned().unwrap_or_default();
        let reblog = reblog_map.get(&s.id).cloned();
        let mentions = mentions_map.get(&s.id).cloned().unwrap_or_default();
        let rb_mentions = reblog
            .as_ref()
            .and_then(|(rs, _, _)| mentions_map.get(&rs.id))
            .cloned()
            .unwrap_or_default();
        let ctx = viewer_ctxs.get(&s.id).cloned();
        let mut api = status_from_db(s, account, media, reblog, ctx, &mentions, &rb_mentions);
        api.tags = tags_map.get(&s.id).cloned().unwrap_or_default();
        api.mentions = mentions;
        api.emojis = emojis_map.get(&s.id).cloned().unwrap_or_default();
        api.poll = polls_map.get(&s.id).cloned();
        api.card = cards_map.get(&s.id).cloned();
        if let Some(ref mut rb) = api.reblog {
            let rid: i64 = rb.id.parse().unwrap_or(0);
            rb.tags = tags_map.get(&rid).cloned().unwrap_or_default();
            rb.mentions = rb_mentions;
            rb.emojis = emojis_map.get(&rid).cloned().unwrap_or_default();
            rb.poll = polls_map.get(&rid).cloned();
            rb.card = cards_map.get(&rid).cloned();
        }
        let order = id_order.get(&s.id).copied().unwrap_or(usize::MAX);
        indexed.push((order, api));
    }
    indexed.sort_by_key(|(i, _)| *i);
    let mut out: Vec<Status> = indexed.into_iter().map(|(_, s)| s).collect();
    hydrate_status_stats(&state, out.iter_mut()).await;
    Ok(Json(out))
}

// ── GET /api/v1/statuses/:id ──────────────────────────────────────────────

pub async fn get_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Status>> {
    // Check existence including deleted rows so we can return 410 vs 404 correctly.
    let deleted_at = sqlx::query_scalar!("SELECT deleted_at FROM statuses WHERE id = $1", id)
        .fetch_optional(&state.db)
        .await?;
    match deleted_at {
        None => return Err(AppError::NotFound),
        Some(Some(_)) => return Err(AppError::Gone("Status has been deleted".into())),
        Some(None) => {}
    }
    let (status, account) = fetch_status_with_account(&state, id).await?;

    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);

    // Block check: if viewer is not the author and there's a block in either direction, 404.
    if let Some(vid) = viewer_id {
        if vid != status.account_id {
            let blocked = sqlx::query_scalar!(
                r#"SELECT 1 FROM blocks
                   WHERE (account_id = $1 AND target_account_id = $2)
                      OR (account_id = $2 AND target_account_id = $1)"#,
                vid,
                status.account_id
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if blocked {
                return Err(AppError::NotFound);
            }
        }
    }

    match status.visibility {
        crate::db::models::vis::PRIVATE => {
            let is_author = viewer_id == Some(status.account_id);
            let is_follower = if let Some(vid) = viewer_id {
                sqlx::query_scalar!(
                    "SELECT 1 as e FROM follows WHERE account_id = $1 AND target_account_id = $2",
                    vid,
                    status.account_id
                )
                .fetch_optional(&state.db)
                .await?
                .is_some()
            } else {
                false
            };
            let is_mentioned = if let Some(vid) = viewer_id {
                sqlx::query_scalar!(
                    "SELECT 1 as e FROM mentions WHERE status_id = $1 AND account_id = $2",
                    id,
                    vid,
                )
                .fetch_optional(&state.db)
                .await?
                .is_some()
            } else {
                false
            };
            if !is_author && !is_follower && !is_mentioned {
                return Err(AppError::NotFound);
            }
        }
        crate::db::models::vis::DIRECT if viewer_id != Some(status.account_id) => {
            let is_mentioned = if let Some(vid) = viewer_id {
                sqlx::query_scalar!(
                    "SELECT 1 as e FROM mentions WHERE status_id = $1 AND account_id = $2",
                    id,
                    vid,
                )
                .fetch_optional(&state.db)
                .await?
                .is_some()
            } else {
                false
            };
            if !is_mentioned {
                return Err(AppError::NotFound);
            }
        }
        _ => {}
    }

    let media = fetch_status_media(&state, id).await?;
    let reblog = fetch_reblog_data(&state, &status).await?;
    let viewer_ctx = if let Some(Extension(auth)) = auth {
        Some(build_viewer_context(&state, auth.account_id, id).await?)
    } else {
        None
    };
    let application = if let Some(app_id) = status.application_id {
        sqlx::query!(
            "SELECT name, website FROM oauth_applications WHERE id = $1",
            app_id,
        )
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .map(|r| super::types::Application {
            name: r.name,
            website: r.website,
        })
    } else {
        None
    };

    let s = super::status_serialize::build_status_with_app(
        &state,
        &status,
        &account,
        media,
        reblog,
        viewer_ctx,
        application,
    )
    .await?;
    Ok(Json(s))
}

// ── DELETE /api/v1/statuses/:id ────────────────────────────────────────────

pub async fn delete_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:statuses")?;
    let (status, account) = fetch_status_with_account(&state, id).await?;
    // Mastodon scopes to `current_account.statuses.find`, so another user's
    // status is a 404 (not 403) — it also avoids confirming the status exists.
    if status.account_id != auth.account_id {
        return Err(AppError::NotFound);
    }

    // Cascade-delete any reblogs of this status before soft-deleting the original.
    // Mastodon deletes reblogs when the original is removed.
    let reblogger_ids: Vec<i64> = sqlx::query_scalar!(
        "UPDATE statuses SET deleted_at = now() WHERE reblog_of_id = $1 AND deleted_at IS NULL RETURNING account_id",
        id
    )
    .fetch_all(&state.db)
    .await?;

    for reblogger_id in &reblogger_ids {
        let _ = sqlx::query!(
            r#"UPDATE account_stats SET
                 statuses_count = GREATEST(statuses_count - 1, 0), updated_at = now()
               WHERE account_id = $1"#,
            reblogger_id
        )
        .execute(&state.db)
        .await;
    }

    sqlx::query!("UPDATE statuses SET deleted_at = now() WHERE id = $1", id)
        .execute(&state.db)
        .await?;

    sqlx::query!(
        r#"UPDATE account_stats SET statuses_count = GREATEST(statuses_count - 1, 0), updated_at = now()
           WHERE account_id = $1"#,
        account.id
    )
    .execute(&state.db)
    .await?;

    // Decrement parent's replies_count if this was a reply
    if let Some(parent_id) = status.in_reply_to_id {
        let _ = sqlx::query!(
            r#"UPDATE status_stats SET replies_count = GREATEST(replies_count - 1, 0), updated_at = now()
               WHERE status_id = $1"#,
            parent_id
        )
        .execute(&state.db)
        .await;
    }

    // Decrement the quoted status's quotes_count if this was an accepted quote.
    if let Some(quoted_id) = sqlx::query_scalar!(
        "SELECT quoted_status_id FROM quotes WHERE status_id = $1 AND state = 1",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .flatten()
    {
        let _ = sqlx::query!(
            r#"UPDATE status_stats SET quotes_count = GREATEST(quotes_count - 1, 0), updated_at = now()
               WHERE status_id = $1"#,
            quoted_id,
        )
        .execute(&state.db)
        .await;
    }

    // Decrement original's reblogs_count if this was a boost
    if let Some(original_id) = status.reblog_of_id {
        let _ = sqlx::query!(
            r#"UPDATE status_stats SET reblogs_count = GREATEST(reblogs_count - 1, 0), updated_at = now()
               WHERE status_id = $1"#,
            original_id
        )
        .execute(&state.db)
        .await;
    }

    // Recalculate featured_tags counts now that this status is soft-deleted
    sqlx::query!(
        r#"UPDATE featured_tags ft
           SET statuses_count = (
               SELECT COUNT(*) FROM statuses_tags st
               JOIN statuses s ON s.id = st.status_id
               WHERE st.tag_id = ft.tag_id AND s.account_id = $1 AND s.deleted_at IS NULL
           ),
           last_status_at = (
               SELECT MAX(s.created_at) FROM statuses_tags st
               JOIN statuses s ON s.id = st.status_id
               WHERE st.tag_id = ft.tag_id AND s.account_id = $1 AND s.deleted_at IS NULL
           )
           WHERE ft.account_id = $1"#,
        account.id,
    )
    .execute(&state.db)
    .await?;

    state
        .streaming
        .publish(Event::DeleteStatus { status_id: id });

    // Remove from follower feeds and list feeds in background
    {
        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let author_id = account.id;
        if feed::sync_fanout() {
            feed::fanout_remove_status(&mut redis, &db, author_id, id).await;
            feed::fanout_remove_from_lists(&mut redis, &db, author_id, id).await;
        } else {
            tokio::spawn(async move {
                feed::fanout_remove_status(&mut redis, &db, author_id, id).await;
                feed::fanout_remove_from_lists(&mut redis, &db, author_id, id).await;
            });
        }
    }

    // Mastodon destroys the pin when a status is deleted.
    let _ = sqlx::query!("DELETE FROM status_pins WHERE status_id = $1", id)
        .execute(&state.db)
        .await;

    // Federate the removal (Mastodon RemoveStatusService): a reblog sends
    // Undo(Announce); any other status sends Delete(Tombstone). Reach is the
    // full StatusReachFinder (unsafe) audience.
    if account
        .private_key
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        let domain = &state.instance.domain;
        let actor_url = crate::federation::tag::account_uri_of(domain, &account);
        let key_id = format!("{}#main-key", actor_url);
        use crate::db::models::vis;
        let distributable = matches!(status.visibility, vis::PUBLIC | vis::UNLISTED);
        let is_public = status.visibility == vis::PUBLIC;
        let followers_allowed = matches!(
            status.visibility,
            vis::PUBLIC | vis::UNLISTED | vis::PRIVATE
        );

        let plan: Option<(serde_json::Value, Option<i64>)> =
            if let Some(original_id) = status.reblog_of_id {
                let original = sqlx::query!(
                    "SELECT account_id, uri FROM statuses WHERE id = $1",
                    original_id,
                )
                .fetch_optional(&state.db)
                .await?;
                let original_uri = original
                    .as_ref()
                    .and_then(|r| r.uri.clone())
                    .unwrap_or_default();
                let announce_id = format!("{actor_url}/statuses/{}/activity", id);
                let undo_id = format!("{announce_id}#undo");
                let undo = crate::federation::activity::undo_announce(
                    &undo_id,
                    &actor_url,
                    &announce_id,
                    &original_uri,
                )?;
                Some((undo, original.map(|r| r.account_id)))
            } else if let Some(ref status_uri) = status.uri {
                let mut activity = crate::federation::activity::delete(
                    &format!("{status_uri}#delete"),
                    &actor_url,
                    status_uri,
                )?;
                activity["to"] = serde_json::json!([crate::federation::activity::AS_PUBLIC]);
                if let Some(obj) = activity.get_mut("object").and_then(|o| o.as_object_mut()) {
                    obj.insert("atomUri".to_string(), serde_json::json!(status_uri));
                }
                Some((activity, None))
            } else {
                None
            };

        if let Some((activity, reblog_of_account_id)) = plan {
            let inboxes = crate::federation::delivery::status_reach_inboxes(
                &state,
                id,
                account.id,
                status.in_reply_to_account_id,
                distributable,
                true,
                is_public,
                followers_allowed,
                reblog_of_account_id,
                &reblogger_ids,
            )
            .await
            .unwrap_or_default();
            if !inboxes.is_empty() {
                if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                    &state, activity, inboxes, key_id,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to enqueue status removal delivery");
                }
            }
        }
    }

    let mut s = serialize_status(&state, &status, None).await?;
    s.text = Some(status.text.clone());
    Ok(Json(s))
}

// ── POST /api/v1/statuses/:id/favourite ───────────────────────────────────

pub async fn favourite_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:favourites")?;
    let (s, _) = fetch_status_with_account(&state, id).await?;
    check_status_visible(&state, &s, auth.account_id).await?;

    sqlx::query!(
        "INSERT INTO favourites (account_id, status_id, created_at, updated_at) VALUES ($1,$2, now(), now()) ON CONFLICT DO NOTHING",
        auth.account_id, id
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        r#"INSERT INTO status_stats (status_id, favourites_count, created_at, updated_at)
           VALUES ($1, 1, now(), now())
           ON CONFLICT (status_id) DO UPDATE
             SET favourites_count = (SELECT COUNT(*) FROM favourites WHERE status_id = $1),
                 updated_at = now()"#,
        id
    )
    .execute(&state.db)
    .await?;

    let (status, account) = fetch_status_with_account(&state, id).await?;

    // Notify status author
    let from_account = fetch_account(&state, auth.account_id).await?;
    push::create_and_push(
        &state,
        status.account_id,
        auth.account_id,
        "favourite",
        Some(id),
        format!("{} favourited your post", from_account.display_name),
        from_account.acct().clone(),
        super::convert::account_avatar_url_for(&from_account),
    )
    .await;

    // Send Like to remote status author
    if account.domain.is_some()
        && from_account
            .private_key
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    {
        let domain = state.instance.domain.clone();
        let actor_url = crate::federation::tag::account_uri_of(&domain, &from_account);
        let like_id = format!(
            "https://{}/users/{}/likes/{}",
            domain, from_account.username, id
        );
        let status_uri = status.uri.clone().unwrap_or_default();
        let like = crate::federation::activity::like(&like_id, &actor_url, &status_uri)?;
        let key_id = format!("{}#main-key", actor_url);
        let inbox = if !account.shared_inbox_url.is_empty() {
            account.shared_inbox_url.clone()
        } else {
            account.inbox_url.clone()
        };
        if let Err(e) =
            crate::federation::delivery::deliver_to_inboxes(&state, like, vec![inbox], key_id).await
        {
            tracing::warn!(error = %e, "failed to enqueue Like delivery");
        }
    }

    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/unfavourite ─────────────────────────────────

pub async fn unfavourite_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:favourites")?;
    let (s, account) = fetch_status_with_account(&state, id).await?;
    check_status_visible(&state, &s, auth.account_id).await?;

    sqlx::query!(
        "DELETE FROM favourites WHERE account_id = $1 AND status_id = $2",
        auth.account_id,
        id
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        r#"UPDATE status_stats SET favourites_count = (SELECT COUNT(*) FROM favourites WHERE status_id = $1),
               updated_at = now()
           WHERE status_id = $1"#,
        id
    )
    .execute(&state.db)
    .await?;

    // Send Undo(Like) to remote status author
    if account.domain.is_some() {
        if let Some(actor_row) = sqlx::query!(
            "SELECT username, private_key, inbox_url, shared_inbox_url, id_scheme FROM accounts WHERE id = $1 AND domain IS NULL",
            auth.account_id,
        ).fetch_optional(&state.db).await? {
            if actor_row.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                let domain = state.instance.domain.clone();
                let actor_url = crate::federation::tag::account_uri(&domain, auth.account_id, actor_row.id_scheme, &actor_row.username);
                let like_id = format!("{actor_url}/likes/{id}");
                let status_uri = s.uri.clone().unwrap_or_default();
                let undo_id = format!("{}#undo", like_id);
                let undo = crate::federation::activity::undo_like(&undo_id, &actor_url, &like_id, &status_uri)?;
                let key_id = format!("{}#main-key", actor_url);
                let inbox = if !account.shared_inbox_url.is_empty() {
                    account.shared_inbox_url.clone()
                } else {
                    account.inbox_url.clone()
                };
                if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                    &state,
                    undo,
                    vec![inbox],
                    key_id,
                )
                .await
                {
                    tracing::warn!(error = %e, "failed to enqueue Undo(Like) delivery");
                }
            }
        }
    }

    let (status, _) = fetch_status_with_account(&state, id).await?;
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/reblog ──────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct ReblogForm {
    pub visibility: Option<String>,
}

pub async fn reblog_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<ReblogForm>>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:statuses")?;
    let (fetched, _) = fetch_status_with_account(&state, id).await?;
    // If this is itself a reblog, boost the original instead
    let original_id = fetched.reblog_of_id.unwrap_or(id);
    let original = if original_id != id {
        let (o, _) = fetch_status_with_account(&state, original_id).await?;
        o
    } else {
        fetched
    };
    // visibility check: 404 if not visible, 403 if visible but not rebloggable
    check_status_visible(&state, &original, auth.account_id).await?;
    // direct messages are never rebloggable; private statuses only by their author
    if original.visibility == crate::db::models::vis::DIRECT
        || (original.visibility == crate::db::models::vis::PRIVATE
            && original.account_id != auth.account_id)
    {
        return Err(AppError::Forbidden);
    }

    // Reject an unrecognized requested visibility rather than coercing to direct.
    if let Some(v) = body.as_ref().and_then(|b| b.visibility.as_deref()) {
        if !matches!(v, "public" | "unlisted" | "private" | "direct") {
            return Err(AppError::Unprocessable(format!(
                "Validation failed: Visibility is not included in the list: {v}"
            )));
        }
    }

    let boost_account = fetch_account(&state, auth.account_id).await?;

    // Determine visibility: hidden originals keep their own visibility;
    // otherwise use the requested visibility or fall back to the user's default.
    let boost_visibility = if matches!(
        original.visibility,
        crate::db::models::vis::PRIVATE
            | crate::db::models::vis::DIRECT
            | crate::db::models::vis::LIMITED
    ) {
        // Hidden originals keep their own visibility (Mastodon: reblogged_status.hidden?).
        original.visibility
    } else {
        match body.as_ref().and_then(|b| b.visibility.as_deref()) {
            Some(v) => crate::db::models::vis::from_str(v),
            // Mastodon falls back to the booster's default posting privacy.
            None => {
                let defaults = super::accounts::user_defaults(&state, auth.account_id).await;
                crate::db::models::vis::from_str(&defaults.privacy)
            }
        }
    };

    // Idempotent: if already reblogged, return the existing boost
    let existing = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE account_id = $1 AND reblog_of_id = $2 AND deleted_at IS NULL",
        auth.account_id,
        original_id,
    )
    .fetch_optional(&state.db)
    .await?;
    if let Some(boost) = existing {
        let ctx = build_viewer_context(&state, auth.account_id, original_id).await?;
        let media = fetch_status_media(&state, boost.id).await?;
        let reblog = fetch_reblog_data(&state, &boost).await?;
        return Ok(Json(
            build_status(&state, &boost, &boost_account, media, reblog, Some(ctx)).await?,
        ));
    }

    let boost_id = crate::snowflake::next_id();
    let boost = sqlx::query_as!(
        DbStatus,
        r#"INSERT INTO statuses (id, account_id, text, visibility, reblog_of_id, local, created_at, updated_at)
           VALUES ($1,$2,'',$3,$4, true, now(), now())
           RETURNING *"#,
        boost_id,
        auth.account_id,
        boost_visibility,
        original_id,
    )
    .fetch_one(&state.db)
    .await?;

    sqlx::query!(
        r#"INSERT INTO status_stats (status_id, reblogs_count, created_at, updated_at)
           VALUES ($1, 1, now(), now())
           ON CONFLICT (status_id) DO UPDATE
             SET reblogs_count = status_stats.reblogs_count + 1,
                 updated_at = now()"#,
        original_id
    )
    .execute(&state.db)
    .await?;

    sqlx::query!(
        r#"INSERT INTO account_stats (account_id, statuses_count, created_at, updated_at)
           VALUES ($1, 1, now(), now())
           ON CONFLICT (account_id) DO UPDATE
             SET statuses_count = account_stats.statuses_count + 1,
                 updated_at = now()"#,
        auth.account_id
    )
    .execute(&state.db)
    .await?;

    // Notify original author
    push::create_and_push(
        &state,
        original.account_id,
        auth.account_id,
        "reblog",
        Some(original_id),
        format!("{} boosted your post", boost_account.display_name),
        boost_account.acct().clone(),
        super::convert::account_avatar_url_for(&boost_account),
    )
    .await;

    // Build viewer context against the ORIGINAL so the nested reblog object
    // carries correct favourited/bookmarked/reblogged flags for the iOS client.
    let ctx = build_viewer_context(&state, auth.account_id, original_id).await?;
    let media = fetch_status_media(&state, boost.id).await?;
    let reblog = fetch_reblog_data(&state, &boost).await?;
    let api_boost = build_status(&state, &boost, &boost_account, media, reblog, Some(ctx)).await?;

    if let Ok(payload) = serde_json::to_string(&api_boost) {
        let hashtags: Vec<String> = api_boost.tags.iter().map(|t| t.name.clone()).collect();
        state.streaming.publish(Event::NewStatus {
            author_id: boost_account.id,
            is_public: original.visibility == crate::db::models::vis::PUBLIC,
            is_direct: false,
            status_id: boost.id,
            hashtags,
            has_media: !api_boost.media_attachments.is_empty(),
            payload: std::sync::Arc::new(payload),
        });
    }

    // Fan the boost into followers' home feeds (mirrors the post path) so it
    // appears immediately, not only after a feed repopulate.
    {
        let mut redis = state.redis.clone();
        let db = state.db.clone();
        let booster_id = boost_account.id;
        let bid = boost.id;
        if feed::sync_fanout() {
            feed::fanout_new_status(&mut redis, &db, booster_id, bid, &[]).await;
        } else {
            tokio::spawn(async move {
                feed::fanout_new_status(&mut redis, &db, booster_id, bid, &[]).await;
            });
        }
    }

    // Send Announce activity to followers and original status author (if remote)
    if boost_account
        .private_key
        .as_deref()
        .is_some_and(|s| !s.is_empty())
    {
        let domain = state.instance.domain.clone();
        let actor_url = crate::federation::tag::account_uri_of(&domain, &boost_account);
        let followers_url = format!("{}/followers", actor_url);
        let announce_id = format!(
            "https://{}/users/{}/statuses/{}/activity",
            domain, boost_account.username, boost_id
        );
        let original_uri = original.uri.clone().unwrap_or_default();
        let original_account = sqlx::query!(
            "SELECT uri, inbox_url, shared_inbox_url, domain FROM accounts WHERE id = $1",
            original.account_id,
        )
        .fetch_optional(&state.db)
        .await?;
        let original_author_url = original_account
            .as_ref()
            .map(|a| a.uri.clone())
            .unwrap_or_default();
        // Address the Announce by the boost's visibility (Mastodon TagManager),
        // always cc'ing the original author.
        let (to_strs, mut cc_strs) =
            crate::db::models::vis::audience(boost_visibility, &followers_url, &[]);
        if !original_author_url.is_empty() {
            cc_strs.push(original_author_url.clone());
        }
        let to_refs: Vec<&str> = to_strs.iter().map(String::as_str).collect();
        let cc_refs: Vec<&str> = cc_strs.iter().map(String::as_str).collect();
        let published = boost.created_at.and_utc().to_rfc3339();
        let announce = crate::federation::activity::announce(
            &announce_id,
            &actor_url,
            &original_uri,
            &to_refs,
            &cc_refs,
            &published,
        )?;
        let key_id = format!("{}#main-key", actor_url);

        // Reach the reblog audience (StatusReachFinder reblog branch): the
        // original author + the booster's followers + relays (public).
        use crate::db::models::vis;
        let inboxes = crate::federation::delivery::status_reach_inboxes(
            &state,
            boost.id,
            boost_account.id,
            None,
            matches!(boost_visibility, vis::PUBLIC | vis::UNLISTED),
            false,
            boost_visibility == vis::PUBLIC,
            matches!(boost_visibility, vis::PUBLIC | vis::UNLISTED | vis::PRIVATE),
            Some(original.account_id),
            &[],
        )
        .await
        .unwrap_or_default();
        if !inboxes.is_empty() {
            if let Err(e) =
                crate::federation::delivery::deliver_to_inboxes(&state, announce, inboxes, key_id)
                    .await
            {
                tracing::warn!(error = %e, "failed to enqueue Announce delivery");
            }
        }
    }

    Ok(Json(api_boost))
}

// ── GET /api/v1/statuses/:id/context ──────────────────────────────────────

pub async fn get_status_context(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<StatusContext>> {
    let root = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let viewer_id = auth.map(|Extension(a)| a.account_id);

    // Enforce the same visibility rules as GET /api/v1/statuses/:id
    match viewer_id {
        Some(vid) => check_status_visible(&state, &root, vid).await?,
        None => {
            if !matches!(
                root.visibility,
                crate::db::models::vis::PUBLIC | crate::db::models::vis::UNLISTED
            ) {
                return Err(AppError::NotFound);
            }
        }
    }

    // Mastodon limits: authenticated=4096 each; unauthenticated=40 ancestors, 60 descendants (depth 20).
    let (ancestor_limit, descendant_limit, depth_limit): (i64, i64, i64) = if viewer_id.is_some() {
        (4096, 4096, 4096)
    } else {
        (40, 60, 20)
    };

    let ancestor_rows = sqlx::query_as::<_, DbStatus>(
        r#"WITH RECURSIVE ancestor_chain AS (
             SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL
             UNION ALL
             SELECT s.* FROM statuses s
               JOIN ancestor_chain a ON s.id = a.in_reply_to_id
             WHERE s.deleted_at IS NULL
           )
           SELECT * FROM ancestor_chain WHERE id != $1 ORDER BY id ASC LIMIT $2"#,
    )
    .bind(id)
    .bind(ancestor_limit)
    .fetch_all(&state.db)
    .await?;

    // Descendants are ordered by tree path (depth-first pre-order) so each
    // subtree stays contiguous, matching Mastodon's `descendant_ids` ORDER BY path.
    let descendant_rows = sqlx::query_as::<_, DbStatus>(
        r#"WITH RECURSIVE reply_tree(id, path, depth) AS (
             SELECT id, ARRAY[id]::bigint[] AS path, 1::int AS depth FROM statuses
             WHERE in_reply_to_id = $1 AND deleted_at IS NULL
             UNION ALL
             SELECT s.id, r.path || s.id, r.depth + 1 FROM statuses s
               JOIN reply_tree r ON s.in_reply_to_id = r.id
             WHERE s.deleted_at IS NULL AND r.depth < $3 AND NOT s.id = ANY(r.path)
           ),
           bounded AS (SELECT id, path FROM reply_tree ORDER BY path LIMIT $2)
           SELECT s.* FROM statuses s JOIN bounded b ON s.id = b.id ORDER BY b.path"#,
    )
    .bind(id)
    .bind(descendant_limit)
    .bind(depth_limit)
    .fetch_all(&state.db)
    .await?;

    // Collect blocked account IDs for the viewer (batch query, avoids n+1 per status).
    let blocked_accounts: std::collections::HashSet<i64> = if let Some(vid) = viewer_id {
        let all_account_ids: Vec<i64> = ancestor_rows
            .iter()
            .chain(descendant_rows.iter())
            .map(|s| s.account_id)
            .filter(|aid| *aid != vid)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if all_account_ids.is_empty() {
            Default::default()
        } else {
            sqlx::query_scalar!(
                r#"SELECT target_account_id FROM blocks
                   WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])
                   UNION
                   SELECT account_id FROM blocks
                   WHERE target_account_id = $1 AND account_id = ANY($2::bigint[])"#,
                vid,
                &all_account_ids,
            )
            .fetch_all(&state.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .collect()
        }
    } else {
        Default::default()
    };

    // Filter by visibility first, then apply "thread" context custom filters.
    let visible_ancestors: Vec<&DbStatus> = ancestor_rows
        .iter()
        .filter(|s| {
            if viewer_id.is_some_and(|vid| vid != s.account_id)
                && blocked_accounts.contains(&s.account_id)
            {
                return false;
            }
            if matches!(
                s.visibility,
                crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
            ) {
                viewer_id.is_some()
            } else {
                true
            }
        })
        .collect();
    let visible_descendants: Vec<&DbStatus> = {
        let filtered = descendant_rows.iter().filter(|s| {
            if viewer_id.is_some_and(|vid| vid != s.account_id)
                && blocked_accounts.contains(&s.account_id)
            {
                return false;
            }
            if matches!(
                s.visibility,
                crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
            ) {
                viewer_id.is_some()
            } else {
                true
            }
        });
        // Mastodon `promote: true` — self-replies (author continuing their own
        // thread) are pulled to the front, preserving relative order (a stable
        // partition), so the OP's thread reads first.
        let (self_replies, others): (Vec<&DbStatus>, Vec<&DbStatus>) =
            filtered.partition(|s| s.in_reply_to_account_id == Some(s.account_id));
        self_replies.into_iter().chain(others).collect()
    };

    // For private/direct: do the per-status visibility check and compute thread filters.
    let anc_owned: Vec<DbStatus> = {
        let mut v = Vec::new();
        for s in &visible_ancestors {
            if matches!(
                s.visibility,
                crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
            ) {
                if let Some(vid) = viewer_id {
                    if check_status_visible(&state, s, vid).await.is_err() {
                        continue;
                    }
                }
            }
            v.push((*s).clone());
        }
        v
    };
    let desc_owned: Vec<DbStatus> = {
        let mut v = Vec::new();
        for s in &visible_descendants {
            if matches!(
                s.visibility,
                crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
            ) {
                if let Some(vid) = viewer_id {
                    if check_status_visible(&state, s, vid).await.is_err() {
                        continue;
                    }
                }
            }
            v.push((*s).clone());
        }
        v
    };

    let (anc_filters, desc_filters) = if let Some(vid) = viewer_id {
        let af = super::timelines::compute_filter_results(&state, vid, &anc_owned, "thread").await;
        let df = super::timelines::compute_filter_results(&state, vid, &desc_owned, "thread").await;
        (af, df)
    } else {
        (Default::default(), Default::default())
    };

    // Build ancestors and descendants using batch fetches instead of N+1 queries.
    let build_batch = |statuses: Vec<DbStatus>,
                       filters: HashMap<i64, (bool, serde_json::Value)>| {
        let state = state.clone();
        async move {
            if statuses.is_empty() {
                return Ok::<Vec<Status>, crate::error::AppError>(vec![]);
            }
            let visible: Vec<DbStatus> = statuses
                .into_iter()
                .filter(|s| !filters.get(&s.id).is_some_and(|(hide, _)| *hide))
                .collect();
            if visible.is_empty() {
                return Ok(vec![]);
            }

            let account_ids: Vec<i64> = visible
                .iter()
                .map(|s| s.account_id)
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let accounts_vec: Vec<Account> = sqlx::query_as!(
                Account,
                "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
                &account_ids,
            )
            .fetch_all(&state.db)
            .await?;
            let account_map: HashMap<i64, Account> =
                accounts_vec.into_iter().map(|a| (a.id, a)).collect();

            let all_ids: Vec<i64> = visible.iter().map(|s| s.id).collect();
            let media_map = batch_status_media(&state, &all_ids).await?;
            let reblog_map = batch_reblog_data(&state, &visible).await?;
            let reblog_ids: Vec<i64> = reblog_map.values().map(|(rs, _, _)| rs.id).collect();
            let mut enrich_ids = all_ids.clone();
            enrich_ids.extend_from_slice(&reblog_ids);
            let tags_map = batch_statuses_tags(&state, &enrich_ids).await?;
            let mentions_map = batch_status_mentions(&state, &enrich_ids).await?;
            let all_statuses_for_emoji: Vec<DbStatus> = visible
                .iter()
                .cloned()
                .chain(reblog_map.values().map(|(rs, _, _)| rs.clone()))
                .collect();
            let emojis_map = batch_status_emojis(&state, &all_statuses_for_emoji).await?;
            let polls_map = batch_status_polls(&state, &enrich_ids, viewer_id).await?;
            let cards_map = batch_status_cards(&state, &enrich_ids).await?;
            let viewer_ctxs = if let Some(vid) = viewer_id {
                batch_viewer_contexts(&state, vid, &all_ids).await?
            } else {
                HashMap::new()
            };
            let all_accounts_for_emoji: Vec<Account> = {
                let mut seen = std::collections::HashSet::new();
                account_map
                    .values()
                    .chain(reblog_map.values().map(|(_, ra, _)| ra))
                    .filter(|a| seen.insert(a.id))
                    .cloned()
                    .collect()
            };
            let account_emojis_map = batch_account_emojis(&state, &all_accounts_for_emoji).await;
            let account_roles_map = batch_account_roles(&state, &all_accounts_for_emoji).await;

            let mut result = Vec::with_capacity(visible.len());
            for s in &visible {
                let Some(account) = account_map.get(&s.account_id) else {
                    continue;
                };
                let media = media_map.get(&s.id).cloned().unwrap_or_default();
                let reblog = reblog_map.get(&s.id).cloned();
                let mentions = mentions_map.get(&s.id).cloned().unwrap_or_default();
                let rb_mentions = reblog
                    .as_ref()
                    .and_then(|(rs, _, _)| mentions_map.get(&rs.id))
                    .cloned()
                    .unwrap_or_default();
                let ctx = viewer_ctxs.get(&s.id).cloned();
                let mut api =
                    status_from_db(s, account, media, reblog, ctx, &mentions, &rb_mentions);
                api.account.emojis = account_emojis_map
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default();
                api.account.roles = account_roles_map
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default();
                api.tags = tags_map.get(&s.id).cloned().unwrap_or_default();
                api.mentions = mentions;
                api.emojis = emojis_map.get(&s.id).cloned().unwrap_or_default();
                api.poll = polls_map.get(&s.id).cloned();
                api.card = cards_map.get(&s.id).cloned();
                if let Some(ref mut rb) = api.reblog {
                    let rid: i64 = rb.id.parse().unwrap_or(0);
                    let rb_id: i64 = rb.account.id.parse().unwrap_or(0);
                    rb.account.emojis = account_emojis_map.get(&rb_id).cloned().unwrap_or_default();
                    rb.account.roles = account_roles_map.get(&rb_id).cloned().unwrap_or_default();
                    rb.tags = tags_map.get(&rid).cloned().unwrap_or_default();
                    rb.mentions = rb_mentions;
                    rb.emojis = emojis_map.get(&rid).cloned().unwrap_or_default();
                    rb.poll = polls_map.get(&rid).cloned();
                    rb.card = cards_map.get(&rid).cloned();
                }
                if let Some((_, ref fj)) = filters.get(&s.id) {
                    if let Some(arr) = fj.as_array() {
                        if !arr.is_empty() {
                            api.filtered = Some(arr.clone());
                        }
                    }
                }
                result.push(api);
            }
            hydrate_status_stats(&state, result.iter_mut()).await;
            Ok(result)
        }
    };

    let ancestors = build_batch(anc_owned, anc_filters).await?;
    let descendants = build_batch(desc_owned, desc_filters).await?;

    Ok(Json(StatusContext {
        ancestors,
        descendants,
    }))
}

// ── POST /api/v1/statuses/:id/unreblog ────────────────────────────────────

pub async fn unreblog_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:statuses")?;
    let (status_raw, _) = fetch_status_with_account(&state, id).await?;
    check_status_visible(&state, &status_raw, auth.account_id).await?;

    // Accept both the original status ID and the reblog's own ID.
    // When iOS sends the reblog wrapper's ID, resolve it to the original.
    let original_id = status_raw.reblog_of_id.unwrap_or(id);

    let deleted = sqlx::query!(
        "DELETE FROM statuses WHERE account_id = $1 AND reblog_of_id = $2 AND deleted_at IS NULL RETURNING id",
        auth.account_id, original_id
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(ref del) = deleted {
        sqlx::query!(
            r#"UPDATE status_stats SET reblogs_count = GREATEST(reblogs_count - 1, 0), updated_at = now()
               WHERE status_id = $1"#,
            original_id
        )
        .execute(&state.db)
        .await?;
        sqlx::query!(
            r#"UPDATE account_stats SET statuses_count = GREATEST(statuses_count - 1, 0), updated_at = now()
               WHERE account_id = $1"#,
            auth.account_id
        )
        .execute(&state.db)
        .await?;

        // Send Undo(Announce) to followers and original status author (if remote)
        let boost_id = del.id;
        if let Some(actor_row) = sqlx::query!(
            "SELECT username, private_key, id_scheme FROM accounts WHERE id = $1 AND domain IS NULL",
            auth.account_id,
        ).fetch_optional(&state.db).await? {
            if actor_row.private_key.as_deref().is_some_and(|s| !s.is_empty()) {
                let domain = state.instance.domain.clone();
                let actor_url = crate::federation::tag::account_uri(&domain, auth.account_id, actor_row.id_scheme, &actor_row.username);
                let announce_id = format!("{actor_url}/statuses/{boost_id}/activity");
                let original_uri = sqlx::query_scalar!("SELECT uri FROM statuses WHERE id = $1", original_id)
                    .fetch_optional(&state.db).await?.flatten().unwrap_or_default();
                let undo_id = format!("{}#undo", announce_id);
                let undo = crate::federation::activity::undo_announce(&undo_id, &actor_url, &announce_id, &original_uri)?;
                let key_id = format!("{}#main-key", actor_url);

                // Deliver to remote original author's inbox
                if let Some(orig_acc) = sqlx::query!(
                    "SELECT inbox_url, shared_inbox_url, domain FROM accounts WHERE id = (SELECT account_id FROM statuses WHERE id = $1)",
                    original_id,
                ).fetch_optional(&state.db).await? {
                    if orig_acc.domain.is_some() {
                        let inbox = if !orig_acc.shared_inbox_url.is_empty() { orig_acc.shared_inbox_url } else { orig_acc.inbox_url };
                        if !inbox.is_empty() {
                            if let Err(e) = crate::federation::delivery::deliver_to_inboxes(
                                &state,
                                undo.clone(),
                                vec![inbox],
                                key_id.clone(),
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "failed to enqueue Undo(Announce) to original author");
                            }
                        }
                    }
                }

                if let Err(e) = crate::federation::delivery::fanout_to_followers(&state, undo, auth.account_id, key_id).await {
                    tracing::warn!(error = %e, "failed to enqueue Undo(Announce) fanout");
                }
            }
        }
    }

    let (original, _) = fetch_status_with_account(&state, original_id).await?;
    Ok(Json(
        serialize_status(&state, &original, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/bookmark ────────────────────────────────────

pub async fn bookmark_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:bookmarks")?;
    let (s, _) = fetch_status_with_account(&state, id).await?;
    check_status_visible(&state, &s, auth.account_id).await?;

    sqlx::query!(
        "INSERT INTO bookmarks (account_id, status_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        auth.account_id, id
    )
    .execute(&state.db)
    .await?;

    let (status, _) = fetch_status_with_account(&state, id).await?;
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/unbookmark ──────────────────────────────────

pub async fn unbookmark_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:bookmarks")?;
    let (s, _) = fetch_status_with_account(&state, id).await?;
    check_status_visible(&state, &s, auth.account_id).await?;

    sqlx::query!(
        "DELETE FROM bookmarks WHERE account_id = $1 AND status_id = $2",
        auth.account_id,
        id
    )
    .execute(&state.db)
    .await?;

    let (status, _) = fetch_status_with_account(&state, id).await?;
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/pin ─────────────────────────────────────────

/// Federate an `Add`/`Remove` of a status to/from the actor's featured (pinned)
/// collection, delivered to followers (Mastodon PinsController). No-op for
/// remote authors or accounts without a signing key.
async fn federate_pin_change(state: &AppState, account: &Account, status: &DbStatus, is_add: bool) {
    if account.domain.is_some() || account.private_key.as_deref().is_none_or(|s| s.is_empty()) {
        return;
    }
    let Some(status_uri) = status.uri.clone().filter(|s| !s.is_empty()) else {
        return;
    };
    let domain = &state.instance.domain;
    let actor_url = crate::federation::tag::account_uri_of(domain, account);
    let target = format!("{actor_url}/collections/featured");
    let activity_id = format!(
        "https://{}/activities/{}",
        domain,
        crate::snowflake::next_id()
    );
    let activity = if is_add {
        crate::federation::activity::add_to_collection(
            &activity_id,
            &actor_url,
            &status_uri,
            &target,
        )
    } else {
        crate::federation::activity::remove_from_collection(
            &activity_id,
            &actor_url,
            &status_uri,
            &target,
        )
    };
    let Ok(activity) = activity else { return };
    let key_id = format!("{actor_url}#main-key");
    if let Err(e) =
        crate::federation::delivery::fanout_to_followers(state, activity, account.id, key_id).await
    {
        tracing::warn!(error = %e, "failed to enqueue pin Add/Remove delivery");
    }
}

pub async fn pin_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:accounts")?;
    let (status, account) = fetch_status_with_account(&state, id).await?;
    if status.account_id != auth.account_id {
        return Err(AppError::Unprocessable(
            "Validation failed: You can only pin your own statuses".into(),
        ));
    }
    if status.reblog_of_id.is_some() {
        return Err(AppError::Unprocessable(
            "Validation failed: Reblogs cannot be pinned".into(),
        ));
    }
    let pin_count = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM status_pins WHERE account_id = $1",
        auth.account_id
    )
    .fetch_one(&state.db)
    .await?
    .unwrap_or(0);
    if pin_count >= 5 {
        return Err(AppError::Unprocessable(
            "Validation failed: You have already pinned the maximum number of statuses".into(),
        ));
    }
    let inserted = sqlx::query!(
        "INSERT INTO status_pins (account_id, status_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        auth.account_id, id
    )
    .execute(&state.db)
    .await?;
    if inserted.rows_affected() > 0 {
        federate_pin_change(&state, &account, &status, true).await;
    }
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/unpin ───────────────────────────────────────

pub async fn unpin_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:accounts")?;
    let (status, account) = fetch_status_with_account(&state, id).await?;
    let deleted = sqlx::query!(
        "DELETE FROM status_pins WHERE account_id = $1 AND status_id = $2",
        auth.account_id,
        id
    )
    .execute(&state.db)
    .await?;
    if deleted.rows_affected() > 0 {
        federate_pin_change(&state, &account, &status, false).await;
    }
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/mute ────────────────────────────────────────

pub async fn mute_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:mutes")?;
    let (status, _) = fetch_status_with_account(&state, id).await?;
    // Every status now has a conversation_id assigned at creation time.
    let cid = status.conversation_id.ok_or(AppError::NotFound)?;
    sqlx::query!(
        "INSERT INTO conversation_mutes (account_id, conversation_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        auth.account_id, cid
    )
    .execute(&state.db)
    .await?;
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── POST /api/v1/statuses/:id/unmute ──────────────────────────────────────

pub async fn unmute_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:mutes")?;
    let (status, _) = fetch_status_with_account(&state, id).await?;
    sqlx::query!(
        "DELETE FROM conversation_mutes WHERE account_id = $1 AND conversation_id = (SELECT conversation_id FROM statuses WHERE id = $2)",
        auth.account_id, id
    )
    .execute(&state.db)
    .await?;
    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── GET /api/v1/statuses/:id/favourited_by ────────────────────────────────

pub async fn favourited_by(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(pagination): Query<PaginationParams>,
    uri: Uri,
    req_headers: HeaderMap,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<impl IntoResponse> {
    let (status, _) = fetch_status_with_account(&state, id).await?;
    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);
    if let Some(vid) = viewer_id {
        check_status_visible(&state, &status, vid).await?;
    } else if matches!(
        status.visibility,
        crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
    ) {
        return Err(AppError::NotFound);
    }

    let limit = pagination.limit_clamped(40, 80);
    let max_id = pagination
        .max_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());
    let since_id = pagination
        .since_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());
    let min_id = pagination
        .min_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());

    // Paginate by favourite.id (matching Mastodon's Favourite.paginate_by_max_id)
    let fav_rows = sqlx::query!(
        r#"SELECT f.id AS fav_id, f.account_id FROM favourites f
           JOIN accounts a ON a.id = f.account_id
           WHERE f.status_id = $1
             AND a.suspended_at IS NULL
             AND ($2::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM blocks
                 WHERE (account_id = $2 AND target_account_id = a.id)
                    OR (account_id = a.id AND target_account_id = $2)
             ))
             AND ($2::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mutes WHERE account_id = $2 AND target_account_id = a.id
             ))
             AND ($3::bigint IS NULL OR f.id < $3)
             AND ($4::bigint IS NULL OR f.id > $4)
             AND ($5::bigint IS NULL OR f.id > $5)
           ORDER BY f.id DESC LIMIT $6"#,
        id,
        viewer_id,
        max_id,
        since_id,
        min_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    let first_fav_id = fav_rows.first().map(|r| r.fav_id.to_string());
    let last_fav_id = fav_rows.last().map(|r| r.fav_id.to_string());
    let account_ids: Vec<i64> = fav_rows.iter().map(|r| r.account_id).collect();
    let account_map: std::collections::HashMap<i64, Account> = if account_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
            &account_ids
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|a| (a.id, a))
        .collect()
    };
    let accounts: Vec<Account> = fav_rows
        .iter()
        .filter_map(|r| account_map.get(&r.account_id).cloned())
        .collect();

    let result = batch_accounts_to_api(&state, &accounts).await;
    let bounds = first_fav_id.zip(last_fav_id);
    let resp_headers = super::link_headers(
        &req_headers,
        &uri,
        bounds.as_ref().map(|(n, o)| (n.as_str(), o.as_str())),
    );
    Ok((resp_headers, Json(result)))
}

// ── GET /api/v1/statuses/:id/reblogged_by ─────────────────────────────────

pub async fn reblogged_by(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(pagination): Query<PaginationParams>,
    uri: Uri,
    req_headers: HeaderMap,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<impl IntoResponse> {
    let (status, _) = fetch_status_with_account(&state, id).await?;
    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);
    if let Some(vid) = viewer_id {
        check_status_visible(&state, &status, vid).await?;
    } else if matches!(
        status.visibility,
        crate::db::models::vis::PRIVATE | crate::db::models::vis::DIRECT
    ) {
        return Err(AppError::NotFound);
    }

    let limit = pagination.limit_clamped(40, 80);
    let max_id = pagination
        .max_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());
    let since_id = pagination
        .since_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());
    let min_id = pagination
        .min_id
        .as_deref()
        .and_then(|s| s.parse::<i64>().ok());

    // Paginate by reblog status.id (matching Mastodon's Status.paginate_by_max_id)
    let reblog_rows = sqlx::query!(
        r#"SELECT s.id AS reblog_id, s.account_id FROM statuses s
           JOIN accounts a ON a.id = s.account_id
           WHERE s.reblog_of_id = $1 AND s.deleted_at IS NULL
             AND s.visibility IN (0, 1)
             AND a.suspended_at IS NULL
             AND ($2::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM blocks
                 WHERE (account_id = $2 AND target_account_id = a.id)
                    OR (account_id = a.id AND target_account_id = $2)
             ))
             AND ($2::bigint IS NULL OR NOT EXISTS (
                 SELECT 1 FROM mutes WHERE account_id = $2 AND target_account_id = a.id
             ))
             AND ($3::bigint IS NULL OR s.id < $3)
             AND ($4::bigint IS NULL OR s.id > $4)
             AND ($5::bigint IS NULL OR s.id > $5)
           ORDER BY s.id DESC LIMIT $6"#,
        id,
        viewer_id,
        max_id,
        since_id,
        min_id,
        limit,
    )
    .fetch_all(&state.db)
    .await?;

    let first_reblog_id = reblog_rows.first().map(|r| r.reblog_id.to_string());
    let last_reblog_id = reblog_rows.last().map(|r| r.reblog_id.to_string());
    let account_ids: Vec<i64> = reblog_rows.iter().map(|r| r.account_id).collect();
    let account_map: std::collections::HashMap<i64, Account> = if account_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        sqlx::query_as!(
            Account,
            "SELECT * FROM accounts WHERE id = ANY($1::bigint[])",
            &account_ids
        )
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .map(|a| (a.id, a))
        .collect()
    };
    let accounts: Vec<Account> = reblog_rows
        .iter()
        .filter_map(|r| account_map.get(&r.account_id).cloned())
        .collect();

    let result = batch_accounts_to_api(&state, &accounts).await;
    let bounds = first_reblog_id.zip(last_reblog_id);
    let resp_headers = super::link_headers(
        &req_headers,
        &uri,
        bounds.as_ref().map(|(n, o)| (n.as_str(), o.as_str())),
    );
    Ok((resp_headers, Json(result)))
}

// ── PUT /api/v1/statuses/:id ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct EditMediaAttribute {
    pub id: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EditStatusForm {
    pub status: Option<String>,
    pub spoiler_text: Option<String>,
    pub sensitive: Option<bool>,
    pub language: Option<String>,
    pub media_ids: Option<Vec<String>>,
    pub media_attributes: Option<Vec<EditMediaAttribute>>,
    // Double-option so we can tell an absent `poll` (no change) from an explicit
    // `poll: null` (remove the poll) — Mastodon keys off `options.key?(:poll)`.
    #[serde(default, deserialize_with = "double_option")]
    pub poll: Option<Option<PollForm>>,
}

fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

pub async fn edit_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    Json(form): Json<EditStatusForm>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:statuses")?;
    let (status, account) = fetch_status_with_account(&state, id).await?;
    // Mastodon scopes to `current_account.statuses.find` → 404 for another
    // user's status.
    if status.account_id != auth.account_id {
        return Err(AppError::NotFound);
    }
    if status.reblog_of_id.is_some() {
        return Err(AppError::Unprocessable("Reblogs cannot be edited".into()));
    }

    let instance_domain = state.instance.domain.clone();

    // Compute the proposed new values.
    let new_text = form.status.clone().unwrap_or_else(|| status.text.clone());
    let new_spoiler = form
        .spoiler_text
        .clone()
        .unwrap_or_else(|| status.spoiler_text.clone());
    // Mastodon StatusLengthValidator: spoiler + body, URLs as 23 chars, mentions
    // without their domain, counted in grapheme clusters.
    if super::formatting::countable_length(&new_text, &new_spoiler) > 500 {
        return Err(AppError::Unprocessable(
            "Validation failed: Text character limit of 500 exceeded".into(),
        ));
    }
    // Mastodon forces sensitive when a content warning is present.
    let new_sensitive = form.sensitive.unwrap_or(status.sensitive) || !new_spoiler.is_empty();
    let new_language = form.language.clone().or(status.language.clone());

    // Detect whether the attached media set changes (description edits via
    // media_attributes also count as a change).
    let media_changed = form.media_attributes.is_some()
        || match form.media_ids {
            Some(ref ids) => {
                let mut parsed: Vec<i64> =
                    ids.iter().filter_map(|s| s.parse::<i64>().ok()).collect();
                let mut current: Vec<i64> = sqlx::query_scalar!(
                    "SELECT id FROM media_attachments WHERE status_id = $1",
                    id,
                )
                .fetch_all(&state.db)
                .await
                .unwrap_or_default();
                parsed.sort_unstable();
                current.sort_unstable();
                parsed != current
            }
            None => false,
        };

    // Poll editing (Mastodon UpdateStatusService#update_poll!): a poll in the
    // request creates or updates one; changing options resets votes.
    if let Some(Some(pf)) = &form.poll {
        validate_poll_form(pf)?;
    }
    let existing_poll = sqlx::query!(
        "SELECT id, options, multiple, hide_totals FROM polls WHERE status_id = $1",
        id,
    )
    .fetch_optional(&state.db)
    .await?;
    let poll_changed = match (&form.poll, &existing_poll) {
        (Some(Some(pf)), Some(ep)) => {
            pf.options != ep.options
                || pf.multiple.unwrap_or(false) != ep.multiple
                || pf.hide_totals.unwrap_or(false) != ep.hide_totals
        }
        (Some(Some(_)), None) => true, // adding a poll
        (Some(None), Some(_)) => true, // explicit poll:null removes it
        (Some(None), None) => false,
        (None, _) => false, // absent: no change
    };

    // Mastodon only records an edit (and bumps edited_at / notifies) when the
    // submission actually changes the status; a no-op edit returns it as-is.
    let significant = new_text != status.text
        || new_spoiler != status.spoiler_text
        || new_sensitive != status.sensitive
        || new_language != status.language
        || media_changed
        || poll_changed;

    if !significant {
        return Ok(Json(
            serialize_status(&state, &status, Some(auth.account_id)).await?,
        ));
    }

    // Save the current version to the edit history before updating. The snapshot
    // is stamped with the version's own creation time (Mastodon snapshots with
    // `at_time: edited_at || created_at`), not the moment it is superseded, and
    // carries that version's media order and poll options so `/history` renders
    // each past version faithfully.
    let snapshot_at = status.edited_at.unwrap_or(status.created_at);
    let snapshot_media = status.ordered_media_attachment_ids.clone();
    let snapshot_poll = existing_poll.as_ref().map(|p| p.options.clone());
    sqlx::query!(
        r#"INSERT INTO status_edits (status_id, account_id, text, spoiler_text, sensitive, ordered_media_attachment_ids, poll_options, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())"#,
        id, auth.account_id, status.text, status.spoiler_text, status.sensitive,
        snapshot_media.as_deref(), snapshot_poll.as_deref(), snapshot_at,
    )
    .execute(&state.db)
    .await?;

    let hashtags = extract_hashtags(&new_text);
    let mention_handles = extract_mention_handles(&new_text);
    let resolved = resolve_mention_accounts(&state, &mention_handles, &instance_domain).await;
    let mention_map = build_mention_map(&resolved, &instance_domain);
    let new_content = render_content(&new_text, &instance_domain, &mention_map);

    sqlx::query!(
        "UPDATE statuses SET text = $1, spoiler_text = $2, sensitive = $3, language = $4, edited_at = now() WHERE id = $5",
        new_text, new_spoiler, new_sensitive, new_language, id,
    )
    .execute(&state.db)
    .await?;

    store_statuses_tags(&state, id, auth.account_id, &hashtags).await?;
    store_status_mentions(&state, id, &resolved).await?;
    spawn_card_fetch(&state, id, new_content);

    // Update media: change descriptions and/or reorder/replace attached media.
    if let Some(ref attrs) = form.media_attributes {
        for attr in attrs {
            if let Ok(media_id) = attr.id.parse::<i64>() {
                if let Some(ref desc) = attr.description {
                    let _ = sqlx::query!(
                        "UPDATE media_attachments SET description = $1 WHERE id = $2 AND account_id = $3",
                        desc, media_id, auth.account_id,
                    )
                    .execute(&state.db)
                    .await;
                }
            }
        }
    }
    if let Some(ref ids) = form.media_ids {
        let parsed: Vec<i64> = ids.iter().filter_map(|s| s.parse::<i64>().ok()).collect();
        // Detach old media not in the new set
        let _ = sqlx::query!(
            "UPDATE media_attachments SET status_id = NULL WHERE status_id = $1 AND id != ALL($2::bigint[])",
            id, &parsed,
        )
        .execute(&state.db)
        .await;
        // Attach new media (must be owned by same account)
        for media_id in &parsed {
            let _ = sqlx::query!(
                "UPDATE media_attachments SET status_id = $1 WHERE id = $2 AND account_id = $3 AND (status_id IS NULL OR status_id = $1)",
                id, media_id, auth.account_id,
            )
            .execute(&state.db)
            .await;
        }
    }

    // Apply the poll change (Mastodon resets votes when options change; an
    // explicit poll:null removes the poll).
    match &form.poll {
        Some(Some(pf)) => {
            let expires_at = pf
                .expires_in
                .map(|secs| chrono::Utc::now().naive_utc() + chrono::Duration::seconds(secs));
            let opts: Vec<String> = pf.options.clone();
            match &existing_poll {
                Some(ep) => {
                    let options_changed =
                        ep.options != opts || ep.multiple != pf.multiple.unwrap_or(false);
                    if options_changed {
                        let _ = sqlx::query!("DELETE FROM poll_votes WHERE poll_id = $1", ep.id)
                            .execute(&state.db)
                            .await;
                    }
                    let _ = sqlx::query!(
                        r#"UPDATE polls
                             SET options = $2, multiple = $3, hide_totals = $4, expires_at = $5,
                                 votes_count = (SELECT COUNT(*) FROM poll_votes WHERE poll_id = $1),
                                 cached_tallies = '{}', updated_at = now()
                           WHERE id = $1"#,
                        ep.id,
                        &opts as &[String],
                        pf.multiple.unwrap_or(false),
                        pf.hide_totals.unwrap_or(false),
                        expires_at,
                    )
                    .execute(&state.db)
                    .await;
                }
                None => {
                    if let Ok(poll_id) = sqlx::query_scalar!(
                        r#"INSERT INTO polls (status_id, account_id, options, multiple, hide_totals, expires_at, created_at, updated_at)
                           VALUES ($1, $2, $3, $4, $5, $6, now(), now())
                           RETURNING id"#,
                        id,
                        auth.account_id,
                        &opts as &[String],
                        pf.multiple.unwrap_or(false),
                        pf.hide_totals.unwrap_or(false),
                        expires_at,
                    )
                    .fetch_one(&state.db)
                    .await
                    {
                        let _ = sqlx::query!(
                            "UPDATE statuses SET poll_id = $1 WHERE id = $2",
                            poll_id, id,
                        )
                        .execute(&state.db)
                        .await;
                    }
                }
            }
        }
        Some(None) => {
            if let Some(ep) = &existing_poll {
                let _ = sqlx::query!("DELETE FROM poll_votes WHERE poll_id = $1", ep.id)
                    .execute(&state.db)
                    .await;
                let _ = sqlx::query!("UPDATE statuses SET poll_id = NULL WHERE id = $1", id)
                    .execute(&state.db)
                    .await;
                let _ = sqlx::query!("DELETE FROM polls WHERE id = $1", ep.id)
                    .execute(&state.db)
                    .await;
            }
        }
        None => {}
    }

    // Notify accounts who reblogged this status (Mastodon notify_about_update!).
    let interacted: Vec<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM statuses WHERE reblog_of_id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let notify_title = format!("{} edited a status", account.display_name);
    for recipient_id in interacted {
        push::create_and_push(
            &state,
            recipient_id,
            auth.account_id,
            "update",
            Some(id),
            notify_title.clone(),
            "".into(),
            super::convert::account_avatar_url_for(&account),
        )
        .await;
    }

    // Notify accounts whose accepted quotes point at this status (Mastodon's
    // quoted_update). The notification references the quoting status.
    if let Ok(quoters) = sqlx::query!(
        "SELECT account_id, status_id FROM quotes WHERE quoted_status_id = $1 AND state = 1",
        id,
    )
    .fetch_all(&state.db)
    .await
    {
        let quote_title = format!("{} edited a quoted post", account.display_name);
        for q in quoters {
            push::create_and_push(
                &state,
                q.account_id,
                auth.account_id,
                "quoted_update",
                Some(q.status_id),
                quote_title.clone(),
                "".into(),
                super::convert::account_avatar_url_for(&account),
            )
            .await;
        }
    }

    let (updated_status, _) = fetch_status_with_account(&state, id).await?;
    let api_status = serialize_status(&state, &updated_status, Some(auth.account_id)).await?;

    if matches!(
        updated_status.visibility,
        crate::db::models::vis::PUBLIC
            | crate::db::models::vis::UNLISTED
            | crate::db::models::vis::PRIVATE
    ) {
        if let Ok(payload) = serde_json::to_string(&api_status) {
            let hashtags: Vec<String> = api_status.tags.iter().map(|t| t.name.clone()).collect();
            state.streaming.publish(Event::StatusUpdate {
                author_id: account.id,
                is_public: updated_status.visibility == crate::db::models::vis::PUBLIC,
                status_id: id,
                hashtags,
                has_media: !api_status.media_attachments.is_empty(),
                payload: std::sync::Arc::new(payload),
            });
        }
    }

    if let Err(e) = federate_status_update(&state, id, &account, &updated_status).await {
        tracing::warn!(status_id = id, error = %e, "failed to enqueue ActivityPub status update");
    }

    Ok(Json(api_status))
}

// ── GET /api/v1/statuses/:id/history ──────────────────────────────────────

pub async fn get_status_history(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<Vec<StatusEdit>>> {
    let status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);
    match viewer_id {
        Some(vid) => check_status_visible(&state, &status, vid).await?,
        None => {
            if !matches!(
                status.visibility,
                crate::db::models::vis::PUBLIC | crate::db::models::vis::UNLISTED
            ) {
                return Err(AppError::NotFound);
            }
        }
    }

    let account = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = $1",
        status.account_id,
    )
    .fetch_one(&state.db)
    .await?;

    let edits = sqlx::query_as!(
        crate::db::models::StatusEdit,
        "SELECT * FROM status_edits WHERE status_id = $1 ORDER BY created_at ASC",
        id,
    )
    .fetch_all(&state.db)
    .await?;

    // Render current version content on the fly
    let current_mentions = super::status_serialize::fetch_status_mentions(&state, id)
        .await
        .unwrap_or_default();
    let current_content = if account.domain.is_none() {
        let instance_domain = state.instance.domain.clone();
        let map = super::formatting::mention_map_from_api(&current_mentions, &instance_domain);
        super::formatting::render_content(&status.text, &instance_domain, &map)
    } else {
        ammonia::clean(&status.text)
    };

    let account_emojis = batch_account_emojis(&state, std::slice::from_ref(&account)).await;
    let account_roles = batch_account_roles(&state, std::slice::from_ref(&account)).await;
    let mut api_account = account_from_db(&account);
    api_account.emojis = account_emojis.get(&account.id).cloned().unwrap_or_default();
    api_account.roles = account_roles.get(&account.id).cloned().unwrap_or_default();
    super::accounts::apply_account_stats(&state, &mut api_account, account.id).await;

    // Collect all media attachment IDs needed across all edits, then batch-fetch them.
    let all_media_ids: Vec<i64> = edits
        .iter()
        .filter_map(|e| e.ordered_media_attachment_ids.as_ref())
        .flat_map(|ids| ids.iter().copied())
        .chain(
            status
                .ordered_media_attachment_ids
                .iter()
                .flat_map(|ids| ids.iter().copied()),
        )
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let fetched_media: Vec<crate::db::models::MediaAttachment> = if all_media_ids.is_empty() {
        vec![]
    } else {
        sqlx::query_as!(
            crate::db::models::MediaAttachment,
            "SELECT * FROM media_attachments WHERE id = ANY($1)",
            &all_media_ids,
        )
        .fetch_all(&state.db)
        .await?
    };
    let media_map: std::collections::HashMap<i64, &crate::db::models::MediaAttachment> =
        fetched_media.iter().map(|m| (m.id, m)).collect();

    let ordered_media = |ids: Option<&Vec<i64>>| -> Vec<super::types::MediaAttachment> {
        ids.map(|list| {
            list.iter()
                .filter_map(|id| media_map.get(id))
                .map(|m| super::convert::media_from_db(m))
                .filter(|m| {
                    m.url.is_some() || m.remote_url.as_deref().is_some_and(|u| !u.is_empty())
                })
                .collect()
        })
        .unwrap_or_default()
    };

    let mut result: Vec<StatusEdit> = edits.iter().map(|e| {
        let poll = e.poll_options.as_ref().filter(|o| !o.is_empty()).map(|opts| {
            serde_json::json!({ "options": opts.iter().map(|t| serde_json::json!({"title": t})).collect::<Vec<_>>() })
        });
        StatusEdit {
            content: ammonia::clean(&e.text),
            spoiler_text: e.spoiler_text.clone(),
            sensitive: e.sensitive.unwrap_or(false),
            created_at: super::convert::mastodon_date(e.created_at),
            account: api_account.clone(),
            media_attachments: ordered_media(e.ordered_media_attachment_ids.as_ref()),
            emojis: vec![],
            poll,
            quote: None,
        }
    }).collect();

    // Current version poll — render its options so the latest history entry
    // matches Mastodon (which snapshots poll_options on every edit).
    let current_poll = if status.poll_id.is_some() {
        sqlx::query_scalar!(
            "SELECT options FROM polls WHERE status_id = $1",
            id,
        )
        .fetch_optional(&state.db)
        .await?
        .map(|opts: Vec<String>| {
            serde_json::json!({
                "options": opts.iter().map(|t| serde_json::json!({ "title": t })).collect::<Vec<_>>()
            })
        })
    } else {
        None
    };

    // Append current version
    result.push(StatusEdit {
        content: current_content,
        spoiler_text: status.spoiler_text.clone(),
        sensitive: status.sensitive,
        created_at: super::convert::mastodon_date(status.edited_at.unwrap_or(status.created_at)),
        account: api_account,
        media_attachments: ordered_media(status.ordered_media_attachment_ids.as_ref()),
        emojis: vec![],
        poll: current_poll,
        quote: None,
    });

    Ok(Json(result))
}

// ── GET /api/v1/statuses/:id/source ───────────────────────────────────────

pub async fn get_status_source(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<Json<StatusSource>> {
    auth.require_scope("read:statuses")?;
    let status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Mastodon allows any authenticated user who has visibility to the status
    // to read its source — not just the author.
    match status.visibility {
        crate::db::models::vis::PRIVATE => {
            let is_author = status.account_id == auth.account_id;
            let is_follower = sqlx::query_scalar!(
                "SELECT 1 as e FROM follows WHERE account_id = $1 AND target_account_id = $2",
                auth.account_id,
                status.account_id,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if !is_author && !is_follower {
                return Err(AppError::NotFound);
            }
        }
        crate::db::models::vis::DIRECT => {
            let is_author = status.account_id == auth.account_id;
            let is_mentioned = sqlx::query_scalar!(
                "SELECT 1 as e FROM mentions WHERE status_id = $1 AND account_id = $2",
                id,
                auth.account_id,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if !is_author && !is_mentioned {
                return Err(AppError::NotFound);
            }
        }
        _ => {}
    }

    Ok(Json(StatusSource {
        id: status.id.to_string(),
        text: status.text,
        spoiler_text: status.spoiler_text,
    }))
}

// ── POST /api/v1/statuses/:id/translate ───────────────────────────────────

pub async fn translate_status(
    Path(_id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
) -> AppResult<()> {
    auth.require_scope("read:statuses")?;
    // No translation service is configured. Mastodon rescues
    // TranslationService::NotConfiguredError with `not_found` (404); 503 is only
    // for quota/rate-limit errors when translation *is* configured.
    Err(crate::error::AppError::NotFound)
}

// ── GET /api/v1/statuses/:id/card ─────────────────────────────────────────

pub async fn get_status_card(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    auth: Option<Extension<AuthenticatedUser>>,
) -> AppResult<Json<serde_json::Value>> {
    let status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let viewer_id = auth.as_ref().map(|Extension(a)| a.account_id);

    match viewer_id {
        Some(vid) => check_status_visible(&state, &status, vid).await?,
        None => {
            if !matches!(
                status.visibility,
                crate::db::models::vis::PUBLIC | crate::db::models::vis::UNLISTED
            ) {
                return Err(AppError::NotFound);
            }
        }
    }

    let card = super::status_serialize::fetch_status_card(&state, id).await;
    Ok(Json(match card {
        Some(c) => serde_json::to_value(c).unwrap_or(serde_json::Value::Null),
        None => serde_json::Value::Null,
    }))
}

// ── PATCH /api/v1/statuses/:id/interaction_policy ─────────────────────────

#[derive(Debug, serde::Deserialize, Default)]
pub struct InteractionPolicyCanQuote {
    pub always: Option<Vec<String>>,
    pub with_approval: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct InteractionPolicyForm {
    pub can_quote: Option<InteractionPolicyCanQuote>,
}

pub async fn update_interaction_policy(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    body: Option<Json<InteractionPolicyForm>>,
) -> AppResult<Json<Status>> {
    auth.require_scope("write:statuses")?;
    // Verify the status exists and belongs to the authenticated user
    let status_meta = sqlx::query!(
        "SELECT account_id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    if status_meta.account_id != auth.account_id {
        return Err(AppError::Forbidden);
    }

    // Translate the can_quote interaction policy into our quote_approval_policy.
    if let Some(Json(form)) = body {
        if let Some(can_quote) = form.can_quote {
            use crate::db::models::quote_policy;
            let always = can_quote.always.unwrap_or_default();
            let with_approval = can_quote.with_approval.unwrap_or_default();
            let policy = if !always.is_empty() {
                // Someone may quote automatically — map by the broadest audience.
                if always.iter().any(|p| p.ends_with("#Public")) {
                    quote_policy::PUBLIC
                } else if always.iter().any(|p| p.ends_with("/followers")) {
                    quote_policy::FOLLOWERS
                } else {
                    quote_policy::PUBLIC
                }
            } else if !with_approval.is_empty() {
                quote_policy::MANUAL
            } else {
                quote_policy::NOBODY
            };
            sqlx::query!(
                "UPDATE statuses SET quote_approval_policy = $1 WHERE id = $2",
                policy,
                id,
            )
            .execute(&state.db)
            .await?;
        }
    }

    // Re-fetch
    let status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(
        serialize_status(&state, &status, Some(auth.account_id)).await?,
    ))
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Return NotFound if `viewer_id` cannot see `status` (private/direct visibility).
async fn check_status_visible(
    state: &AppState,
    status: &DbStatus,
    viewer_id: i64,
) -> AppResult<()> {
    match status.visibility {
        crate::db::models::vis::PRIVATE => {
            if status.account_id == viewer_id {
                return Ok(());
            }
            let is_follower = sqlx::query_scalar!(
                "SELECT 1 as e FROM follows WHERE account_id = $1 AND target_account_id = $2",
                viewer_id,
                status.account_id,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            let is_mentioned = sqlx::query_scalar!(
                "SELECT 1 as e FROM mentions WHERE status_id = $1 AND account_id = $2",
                status.id,
                viewer_id,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if !is_follower && !is_mentioned {
                return Err(AppError::NotFound);
            }
        }
        crate::db::models::vis::DIRECT if status.account_id != viewer_id => {
            let is_mentioned = sqlx::query_scalar!(
                "SELECT 1 as e FROM mentions WHERE status_id = $1 AND account_id = $2",
                status.id,
                viewer_id,
            )
            .fetch_optional(&state.db)
            .await?
            .is_some();
            if !is_mentioned {
                return Err(AppError::NotFound);
            }
        }
        _ => {}
    }
    Ok(())
}

async fn fetch_status_with_account(state: &AppState, id: i64) -> AppResult<(DbStatus, Account)> {
    let status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let account = sqlx::query_as!(
        Account,
        "SELECT * FROM accounts WHERE id = $1",
        status.account_id
    )
    .fetch_one(&state.db)
    .await?;

    Ok((status, account))
}

async fn fetch_account(state: &AppState, id: i64) -> AppResult<Account> {
    sqlx::query_as!(Account, "SELECT * FROM accounts WHERE id = $1", id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

pub(crate) async fn federate_status_update(
    state: &AppState,
    status_id: i64,
    account: &Account,
    status: &DbStatus,
) -> anyhow::Result<()> {
    if account.domain.is_some() || status.reblog_of_id.is_some() {
        return Ok(());
    }
    if !matches!(
        status.visibility,
        crate::db::models::vis::PUBLIC
            | crate::db::models::vis::UNLISTED
            | crate::db::models::vis::PRIVATE
            | crate::db::models::vis::DIRECT
    ) {
        return Ok(());
    }

    if account.private_key.as_deref().is_none_or(|s| s.is_empty()) {
        return Ok(());
    }

    let bundle =
        match crate::api::ap::note::build_note(state, &state.instance.domain, status_id).await? {
            Some(bundle) => bundle,
            None => return Ok(()),
        };

    let updated_at = status
        .edited_at
        .unwrap_or_else(|| chrono::Utc::now().naive_utc())
        .and_utc();
    let update_id = format!("{}#updates/{}", bundle.note_uri, updated_at.timestamp());
    let activity = serde_json::json!({
        "@context": crate::api::ap::note::note_context(),
        "id": update_id,
        "type": "Update",
        "actor": bundle.actor_url,
        "published": updated_at.to_rfc3339(),
        "to": bundle.to,
        "cc": bundle.cc,
        "object": bundle.note,
    });
    let key_id = crate::federation::tag::key_id_of(&state.instance.domain, account);

    // Reach the same audience that received the original (StatusReachFinder).
    use crate::db::models::vis;
    let inboxes = crate::federation::delivery::status_reach_inboxes(
        state,
        status_id,
        account.id,
        status.in_reply_to_account_id,
        matches!(status.visibility, vis::PUBLIC | vis::UNLISTED),
        false,
        status.visibility == vis::PUBLIC,
        matches!(
            status.visibility,
            vis::PUBLIC | vis::UNLISTED | vis::PRIVATE
        ),
        None,
        &[],
    )
    .await?;
    if !inboxes.is_empty() {
        crate::federation::delivery::deliver_to_inboxes(state, activity, inboxes, key_id).await?;
    }

    Ok(())
}

/// Batch-fetch viewer context for a list of status IDs in 5 queries.
/// Returns a map from status_id → StatusViewerContext.
pub(super) async fn batch_viewer_contexts(
    state: &AppState,
    viewer_id: i64,
    status_ids: &[i64],
) -> AppResult<std::collections::HashMap<i64, super::convert::StatusViewerContext>> {
    use super::convert::StatusViewerContext;
    use std::collections::{HashMap, HashSet};

    if status_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let fav_set: HashSet<i64> = sqlx::query_scalar!(
        "SELECT status_id FROM favourites WHERE account_id = $1 AND status_id = ANY($2::bigint[])",
        viewer_id,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let reb_set: HashSet<i64> = sqlx::query_scalar!(
        r#"SELECT reblog_of_id as "reblog_of_id!: i64" FROM statuses
           WHERE account_id = $1 AND reblog_of_id = ANY($2::bigint[]) AND deleted_at IS NULL"#,
        viewer_id,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let book_set: HashSet<i64> = sqlx::query_scalar!(
        "SELECT status_id FROM bookmarks WHERE account_id = $1 AND status_id = ANY($2::bigint[])",
        viewer_id,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let mute_set: HashSet<i64> = sqlx::query_scalar!(
        "SELECT s.id FROM statuses s JOIN conversation_mutes cm ON cm.conversation_id = s.conversation_id WHERE cm.account_id = $1 AND s.id = ANY($2::bigint[])",
        viewer_id, status_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let pin_set: HashSet<i64> = sqlx::query_scalar!(
        "SELECT status_id FROM status_pins WHERE account_id = $1 AND status_id = ANY($2::bigint[])",
        viewer_id,
        status_ids,
    )
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();

    let status_author_rows = sqlx::query!(
        "SELECT id as status_id, account_id FROM statuses WHERE id = ANY($1::bigint[])",
        status_ids,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let status_to_author: HashMap<i64, i64> = status_author_rows
        .into_iter()
        .map(|r| (r.status_id, r.account_id))
        .collect();

    let author_ids: Vec<i64> = status_to_author
        .values()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let viewer_follows_set: HashSet<i64> = if !author_ids.is_empty() {
        sqlx::query_scalar!(
            "SELECT target_account_id FROM follows WHERE account_id = $1 AND target_account_id = ANY($2::bigint[])",
            viewer_id, &author_ids,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
    } else {
        HashSet::new()
    };

    let author_follows_set: HashSet<i64> = if !author_ids.is_empty() {
        sqlx::query_scalar!(
            "SELECT account_id FROM follows WHERE account_id = ANY($1::bigint[]) AND target_account_id = $2",
            &author_ids, viewer_id,
        )
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
    } else {
        HashSet::new()
    };

    let mut result = HashMap::with_capacity(status_ids.len());
    for &id in status_ids {
        let author_id = status_to_author.get(&id).cloned().unwrap_or(0);
        result.insert(
            id,
            StatusViewerContext {
                account_id: viewer_id,
                follows_author: viewer_follows_set.contains(&author_id),
                author_follows: author_follows_set.contains(&author_id),
                favourited: fav_set.contains(&id),
                reblogged: reb_set.contains(&id),
                bookmarked: book_set.contains(&id),
                muted: mute_set.contains(&id),
                pinned: pin_set.contains(&id),
            },
        );
    }
    Ok(result)
}

// ── GET /api/v1/statuses/:id/quotes ──────────────────────────────────────

pub async fn get_status_quotes(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Extension(auth): Extension<AuthenticatedUser>,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
    req_headers: axum::http::HeaderMap,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    Query(params): Query<PaginationParams>,
) -> AppResult<impl axum::response::IntoResponse> {
    auth.require_scope("read:statuses")?;
    let viewer_id = Some(auth.account_id);
    let limit: i64 = params.limit_clamped(20, 40);
    let max_id: Option<i64> = params.max_id.as_deref().and_then(|s| s.parse().ok());
    let since_id: Option<i64> = params.since_id.as_deref().and_then(|s| s.parse().ok());
    let min_id: Option<i64> = params.min_id.as_deref().and_then(|s| s.parse().ok());

    // Verify the quoted status exists
    let _ = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Only return accepted quotes; private quoting statuses are hidden from non-owners
    let quoted_owner: Option<i64> =
        sqlx::query_scalar!("SELECT account_id FROM statuses WHERE id = $1", id,)
            .fetch_optional(&state.db)
            .await?;
    let viewer_is_owner = viewer_id.is_some() && viewer_id == quoted_owner;

    let quotes = sqlx::query_as!(
        DbStatus,
        r#"SELECT s.* FROM statuses s
           JOIN quotes q ON q.status_id = s.id AND q.quoted_status_id = $1
           WHERE s.deleted_at IS NULL
             AND q.state = 1
             AND (s.visibility IN (0, 1) OR (s.visibility = 2 AND $6::bool))
             AND ($2::bigint IS NULL OR q.id < $2)
             AND ($3::bigint IS NULL OR q.id > $3)
             AND ($4::bigint IS NULL OR q.id > $4)
           ORDER BY q.id DESC
           LIMIT $5"#,
        id,
        max_id,
        since_id,
        min_id,
        limit,
        viewer_is_owner,
    )
    .fetch_all(&state.db)
    .await?;

    use super::timelines::build_status_list_with_context;
    let result = build_status_list_with_context(&state, quotes, viewer_id, "public").await?;

    let link = result.first().zip(result.last()).map(|(newest, oldest)| {
        let extra = super::non_pagination_query(raw_query.as_deref());
        super::link_header(&req_headers, uri.path(), &extra, &newest.id, &oldest.id)
    });
    let mut headers = axum::http::HeaderMap::new();
    if let Some(v) = link {
        if let Ok(val) = v.parse() {
            headers.insert(axum::http::header::LINK, val);
        }
    }
    Ok((headers, Json(result)))
}

// ── POST /api/v1/statuses/:status_id/quotes/:id/revoke ────────────────────

pub async fn revoke_quote(
    State(state): State<AppState>,
    Extension(ResolvedInstance(_instance)): Extension<ResolvedInstance>,
    Extension(auth): Extension<AuthenticatedUser>,
    Path((quoted_status_id, quoting_status_id)): Path<(i64, i64)>,
) -> AppResult<impl axum::response::IntoResponse> {
    auth.require_scope("write:statuses")?;

    // Find the quote record; the caller must be the quoted status's author
    let quote = sqlx::query!(
        r#"SELECT q.id, q.status_id, q.quoted_status_id, q.quoted_account_id, q.state
           FROM quotes q
           WHERE q.quoted_status_id = $1 AND q.status_id = $2 AND q.state != 3"#,
        quoted_status_id,
        quoting_status_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    if quote.quoted_account_id != Some(auth.account_id) {
        return Err(AppError::Forbidden);
    }

    sqlx::query!("UPDATE quotes SET state = 3 WHERE id = $1", quote.id,)
        .execute(&state.db)
        .await?;

    // Return the quoting status
    let quoting_status = sqlx::query_as!(
        DbStatus,
        "SELECT * FROM statuses WHERE id = $1 AND deleted_at IS NULL",
        quoting_status_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let account = fetch_account(&state, quoting_status.account_id).await?;
    let media = fetch_status_media(&state, quoting_status.id).await?;
    let reblog = fetch_reblog_data(&state, &quoting_status).await?;
    let ctx = build_viewer_context(&state, auth.account_id, quoting_status.id)
        .await
        .ok();
    let api_status = build_status(&state, &quoting_status, &account, media, reblog, ctx).await?;
    Ok(Json(api_status))
}

/// Fully serialize a single status for an optional viewer.
///
/// Performs the account / media / reblog / viewer-context fetches that every
/// single-status endpoint needs, then delegates to [`build_status`]. Pass
/// `viewer_id` as `None` for unauthenticated serialization (omits per-viewer
/// flags such as `favourited`). This is the single entry point single-status
/// endpoints should use instead of re-inlining the fetch quintet.
pub async fn serialize_status(
    state: &AppState,
    status: &DbStatus,
    viewer_id: Option<i64>,
) -> AppResult<super::types::Status> {
    let account = fetch_account(state, status.account_id).await?;
    let media = fetch_status_media(state, status.id).await?;
    let reblog = fetch_reblog_data(state, status).await?;
    let viewer_ctx = match viewer_id {
        Some(vid) => Some(build_viewer_context(state, vid, status.id).await?),
        None => None,
    };
    build_status(state, status, &account, media, reblog, viewer_ctx).await
}

pub async fn build_viewer_context(
    state: &AppState,
    viewer_id: i64,
    status_id: i64,
) -> AppResult<super::convert::StatusViewerContext> {
    // Delegate to the batched implementation so the per-viewer flag logic
    // (favourited / reblogged / bookmarked / muted / pinned + follow
    // relationships) lives in exactly one place. `batch_viewer_contexts`
    // inserts an entry for every requested id, so the fallback is a safety
    // net that only fires if the id list is somehow dropped.
    Ok(batch_viewer_contexts(state, viewer_id, &[status_id])
        .await?
        .remove(&status_id)
        .unwrap_or(super::convert::StatusViewerContext {
            account_id: viewer_id,
            follows_author: false,
            author_follows: false,
            favourited: false,
            reblogged: false,
            muted: false,
            bookmarked: false,
            pinned: false,
        }))
}

pub fn extract_hashtags(text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    HASHTAG_RE
        .captures_iter(text)
        .filter_map(|c| {
            let tag = c[2].to_lowercase();
            if seen.insert(tag.clone()) {
                Some(tag)
            } else {
                None
            }
        })
        .collect()
}

pub fn extract_mention_handles(text: &str) -> Vec<(String, Option<String>)> {
    let mut seen = std::collections::HashSet::new();
    MENTION_RE
        .captures_iter(text)
        .filter_map(|c| {
            let username = c[2].to_lowercase();
            let domain = c.get(3).map(|m| m.as_str().to_lowercase());
            let key = match &domain {
                Some(d) => format!("{}@{}", username, d),
                None => username.clone(),
            };
            if seen.insert(key) {
                Some((username, domain))
            } else {
                None
            }
        })
        .collect()
}

pub async fn resolve_mention_accounts(
    state: &AppState,
    handles: &[(String, Option<String>)],
    local_domain: &str,
) -> Vec<(String, Account)> {
    let mut result = Vec::new();
    for (username, domain) in handles {
        // A mention that names this instance's own domain refers to a local
        // account, which is stored with domain IS NULL (mirrors Mastodon's
        // TagManager#local_domain? normalization).
        let domain = domain
            .as_deref()
            .filter(|d| local_domain.is_empty() || !d.eq_ignore_ascii_case(local_domain));

        let account = if let Some(d) = domain {
            sqlx::query_as!(
                Account,
                "SELECT * FROM accounts WHERE LOWER(username) = $1 AND domain = $2 LIMIT 1",
                username,
                d,
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
        } else {
            sqlx::query_as!(
                Account,
                "SELECT * FROM accounts WHERE LOWER(username) = $1 AND domain IS NULL LIMIT 1",
                username,
            )
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten()
        };

        // Unknown remote account: resolve it via WebFinger and fetch the actor,
        // mirroring Mastodon's ProcessMentionsService, so that mentioning a user
        // this instance has never seen still creates the mention and federates.
        let account = match account {
            Some(acct) => Some(acct),
            None => match domain {
                Some(d) => match crate::federation::webfinger::resolve(&state.fetch, username, d)
                    .await
                {
                    Ok(actor_url) => match crate::api::ap::inbox::resolve_or_fetch_remote_account(
                        state, &actor_url,
                    )
                    .await
                    {
                        Ok(id) => {
                            sqlx::query_as!(Account, "SELECT * FROM accounts WHERE id = $1", id)
                                .fetch_optional(&state.db)
                                .await
                                .ok()
                                .flatten()
                        }
                        Err(e) => {
                            tracing::debug!(handle = %format!("{username}@{d}"), error = %e, "mention actor fetch failed");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::debug!(handle = %format!("{username}@{d}"), error = %e, "mention webfinger failed");
                        None
                    }
                },
                None => None,
            },
        };

        if let Some(acct) = account {
            result.push((username.clone(), acct));
        }
    }
    result
}

pub fn build_mention_map(
    resolved: &[(String, Account)],
    local_domain: &str,
) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    for (username_lower, account) in resolved {
        let url = account.url.clone().unwrap_or_default();
        let display = account.acct();
        map.insert(username_lower.clone(), (url.clone(), display.clone()));
        if let Some(ref d) = account.domain {
            map.insert(
                format!("{}@{}", username_lower, d.to_lowercase()),
                (url, display),
            );
        } else if !local_domain.is_empty() {
            // Local accounts are stored with domain NULL, but users may still
            // write the fully-qualified `@alice@this.instance` form; map that key
            // too so it renders as a link instead of plain text.
            map.insert(
                format!("{}@{}", username_lower, local_domain.to_lowercase()),
                (url, display),
            );
        }
    }
    map
}

pub async fn store_statuses_tags(
    state: &AppState,
    status_id: i64,
    account_id: i64,
    hashtags: &[String],
) -> AppResult<()> {
    sqlx::query!("DELETE FROM statuses_tags WHERE status_id = $1", status_id)
        .execute(&state.db)
        .await?;
    for tag_name in hashtags {
        let tag_id = sqlx::query_scalar!(
            "INSERT INTO tags (name, created_at, updated_at) VALUES ($1, now(), now())
             ON CONFLICT ((lower(name))) DO UPDATE SET updated_at = now()
             RETURNING id",
            tag_name,
        )
        .fetch_one(&state.db)
        .await?;
        sqlx::query!(
            "INSERT INTO statuses_tags (status_id, tag_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            status_id,
            tag_id,
        )
        .execute(&state.db)
        .await?;
    }
    // Recalculate statuses_count and last_status_at for all featured tags of this account
    sqlx::query!(
        r#"UPDATE featured_tags ft
           SET statuses_count = (
               SELECT COUNT(*) FROM statuses_tags st
               JOIN statuses s ON s.id = st.status_id
               WHERE st.tag_id = ft.tag_id AND s.account_id = $1 AND s.deleted_at IS NULL
           ),
           last_status_at = (
               SELECT MAX(s.created_at) FROM statuses_tags st
               JOIN statuses s ON s.id = st.status_id
               WHERE st.tag_id = ft.tag_id AND s.account_id = $1 AND s.deleted_at IS NULL
           )
           WHERE ft.account_id = $1"#,
        account_id,
    )
    .execute(&state.db)
    .await?;
    Ok(())
}

pub async fn store_status_mentions(
    state: &AppState,
    status_id: i64,
    resolved: &[(String, Account)],
) -> AppResult<()> {
    sqlx::query!("DELETE FROM mentions WHERE status_id = $1", status_id)
        .execute(&state.db)
        .await?;
    for (_, account) in resolved {
        sqlx::query!(
            "INSERT INTO mentions (status_id, account_id, created_at, updated_at) VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
            status_id, account.id,
        )
        .execute(&state.db)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod inline_quote_tests {
    use super::inline_quote_instrument;
    use serde_json::json;

    #[test]
    fn inlines_note_and_merges_context() {
        // A QuoteRequest as feder-vocab emits it: compound @context declaring the
        // FEP-044f `QuoteRequest` term, with `instrument` as a bare URI.
        let mut request = json!({
            "@context": [
                "https://www.w3.org/ns/activitystreams",
                { "QuoteRequest": "https://w3id.org/fep/044f#QuoteRequest" }
            ],
            "type": "QuoteRequest",
            "id": "https://seoul.earth/users/sohu/quote_requests/1",
            "actor": "https://seoul.earth/users/sohu",
            "object": "https://hackers.pub/ap/notes/abc",
            "instrument": "https://seoul.earth/users/sohu/statuses/1",
        });
        // The context-less Note we inline (as `NoteBundle::note` is built).
        let note = json!({
            "id": "https://seoul.earth/users/sohu/statuses/1",
            "type": "Note",
            "attributedTo": "https://seoul.earth/users/sohu",
            "quote": "https://hackers.pub/ap/notes/abc",
            "quoteUrl": "https://hackers.pub/ap/notes/abc",
        });

        inline_quote_instrument(&mut request, note.clone());

        // The instrument is now the embedded Note object, not a URI.
        assert_eq!(request["instrument"], note);
        assert_eq!(request["instrument"]["quote"], "https://hackers.pub/ap/notes/abc");

        // The request context keeps the QuoteRequest term and gains the Note's
        // JSON-LD terms so the embedded fields resolve.
        let terms = &request["@context"][1];
        assert_eq!(terms["QuoteRequest"], "https://w3id.org/fep/044f#QuoteRequest");
        assert_eq!(terms["quote"], json!({ "@id": "fep:quote", "@type": "@id" }));
        assert_eq!(terms["Hashtag"], "as:Hashtag");
        assert_eq!(terms["sensitive"], "as:sensitive");
    }
}
