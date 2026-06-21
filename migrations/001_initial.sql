-- Squashed migration: final schema state (merges original migrations 001–008).
-- Fresh installs only; no data migration.

-- ── Sequences ────────────────────────────────────────────────────────────────
-- Named to match Mastodon's sequence (notifications_id_seq, plural)
CREATE SEQUENCE notifications_id_seq START 1;
CREATE SEQUENCE follows_id_seq;
CREATE SEQUENCE custom_emojis_id_seq;
CREATE SEQUENCE tags_id_seq;
CREATE SEQUENCE polls_id_seq;
CREATE SEQUENCE users_id_seq;
CREATE SEQUENCE invites_id_seq;
CREATE SEQUENCE oauth_access_tokens_id_seq;
CREATE SEQUENCE oauth_applications_id_seq;
-- Sequences for BIGINT PRIMARY KEY tables (Mastodon creates these even for Snowflake-ID tables)
CREATE SEQUENCE accounts_id_seq;
CREATE SEQUENCE statuses_id_seq;
CREATE SEQUENCE media_attachments_id_seq;
CREATE SEQUENCE quotes_id_seq;
CREATE SEQUENCE user_roles_id_seq;
CREATE SEQUENCE encrypted_messages_id_seq;

-- ── console_users ─────────────────────────────────────────────────────────────
CREATE TABLE console_users (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email               TEXT NOT NULL,
    email_normalized    TEXT NOT NULL UNIQUE,
    password_hash       TEXT,
    locale              TEXT NOT NULL DEFAULT 'en',
    confirmed_at        TIMESTAMPTZ,
    confirmation_token  TEXT UNIQUE,
    request_token       UUID NOT NULL DEFAULT gen_random_uuid(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── console_sessions ──────────────────────────────────────────────────────────
CREATE TABLE console_sessions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    console_user_id UUID NOT NULL REFERENCES console_users(id) ON DELETE CASCADE,
    token           TEXT NOT NULL UNIQUE,
    expires_at      TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── user_roles ────────────────────────────────────────────────────────────────
CREATE TABLE user_roles (
    id               BIGINT PRIMARY KEY,
    name             TEXT NOT NULL DEFAULT '',
    color            TEXT NOT NULL DEFAULT '',
    position         INTEGER NOT NULL DEFAULT 0,
    permissions      BIGINT NOT NULL DEFAULT 0,
    highlighted      BOOLEAN NOT NULL DEFAULT false,
    require_2fa      BOOLEAN NOT NULL DEFAULT false,
    collection_limit INTEGER NOT NULL DEFAULT 10,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);


-- ── oauth_applications ────────────────────────────────────────────────────────
CREATE TABLE oauth_applications (
    id           BIGINT PRIMARY KEY DEFAULT nextval('oauth_applications_id_seq'),
    name         TEXT NOT NULL,
    uid          TEXT NOT NULL,
    secret       TEXT NOT NULL,
    redirect_uri TEXT NOT NULL DEFAULT 'urn:ietf:wg:oauth:2.0:oob',
    scopes       TEXT,
    website      TEXT,
    confidential BOOLEAN NOT NULL DEFAULT true,
    superapp     BOOLEAN NOT NULL DEFAULT false,
    owner_type   TEXT,
    owner_id     BIGINT,
    created_at   TIMESTAMPTZ,
    updated_at   TIMESTAMPTZ
);
ALTER SEQUENCE oauth_applications_id_seq OWNED BY oauth_applications.id;

CREATE UNIQUE INDEX oauth_applications_uid_key ON oauth_applications(uid);
CREATE INDEX index_oauth_applications_on_owner_id_and_owner_type
    ON oauth_applications(owner_id, owner_type) WHERE owner_id IS NOT NULL;
CREATE INDEX index_oauth_applications_on_superapp
    ON oauth_applications(superapp) WHERE superapp = true;

-- ── accounts ──────────────────────────────────────────────────────────────────
CREATE TABLE accounts (
    id                              BIGINT PRIMARY KEY,
    username                        TEXT NOT NULL,
    domain                          TEXT,
    display_name                    TEXT NOT NULL DEFAULT '',
    note                            TEXT NOT NULL DEFAULT '',
    url                             TEXT,
    uri                             TEXT NOT NULL DEFAULT '',
    private_key                     TEXT,
    public_key                      TEXT NOT NULL DEFAULT '',
    locked                          BOOLEAN NOT NULL DEFAULT false,
    discoverable                    BOOLEAN,
    indexable                       BOOLEAN NOT NULL DEFAULT false,
    inbox_url                       TEXT NOT NULL DEFAULT '',
    outbox_url                      TEXT NOT NULL DEFAULT '',
    shared_inbox_url                TEXT NOT NULL DEFAULT '',
    suspended_at                    TIMESTAMPTZ,
    silenced_at                     TIMESTAMPTZ,
    sensitized_at                   TIMESTAMPTZ,
    hide_collections                BOOLEAN,
    fields                          JSONB,
    attribution_domains             TEXT[] NOT NULL DEFAULT '{}',
    also_known_as                   TEXT[],
    actor_type                      TEXT,
    featured_collection_url         TEXT,
    followers_url                   TEXT NOT NULL DEFAULT '',
    following_url                   TEXT NOT NULL DEFAULT '',
    last_webfingered_at             TIMESTAMPTZ,
    memorial                        BOOLEAN NOT NULL DEFAULT false,
    moved_to_account_id             BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    protocol                        INTEGER NOT NULL DEFAULT 0,
    requested_review_at             TIMESTAMPTZ,
    reviewed_at                     TIMESTAMPTZ,
    suspension_origin               INTEGER,
    trendable                       BOOLEAN,
    id_scheme                       INTEGER DEFAULT 1,
    avatar_file_name                TEXT,
    avatar_content_type             TEXT,
    avatar_file_size                INTEGER,
    avatar_updated_at               TIMESTAMPTZ,
    header_file_name                TEXT,
    header_content_type             TEXT,
    header_file_size                INTEGER,
    header_updated_at               TIMESTAMPTZ,
    avatar_remote_url               TEXT,
    header_remote_url               TEXT NOT NULL DEFAULT '',
    avatar_storage_schema_version   INTEGER,
    header_storage_schema_version   INTEGER,
    created_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- v4.6.0 columns. Kept at the end of the table to match the physical
    -- column order produced by ALTER ADD COLUMN on already-deployed databases,
    -- so `SELECT *` positional decoding stays consistent everywhere.
    avatar_description              TEXT NOT NULL DEFAULT '',
    header_description              TEXT NOT NULL DEFAULT '',
    show_featured                   BOOLEAN NOT NULL DEFAULT true,
    show_media                      BOOLEAN NOT NULL DEFAULT true,
    show_media_replies              BOOLEAN NOT NULL DEFAULT true,
    feature_approval_policy         INTEGER NOT NULL DEFAULT 0,
    collections_url                 TEXT,
    CONSTRAINT accounts_local_unique UNIQUE NULLS NOT DISTINCT (username, domain)
);

CREATE UNIQUE INDEX accounts_uri_unique ON accounts(uri) WHERE uri != '';
CREATE INDEX accounts_by_domain ON accounts(domain) WHERE domain IS NOT NULL;

-- ── invites ───────────────────────────────────────────────────────────────────
-- user_id FK to users is added after users table via ALTER TABLE below
CREATE TABLE invites (
    id          BIGINT PRIMARY KEY DEFAULT nextval('invites_id_seq'),
    code        TEXT NOT NULL UNIQUE,
    user_id     BIGINT,
    max_uses    INT,
    uses        INT NOT NULL DEFAULT 0,
    expires_at  TIMESTAMPTZ,
    autofollow  BOOLEAN NOT NULL DEFAULT false,
    comment     TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE invites_id_seq OWNED BY invites.id;

CREATE INDEX invites_by_code ON invites(code);

-- ── users ─────────────────────────────────────────────────────────────────────
CREATE TABLE users (
    id                          BIGINT PRIMARY KEY DEFAULT nextval('users_id_seq'),
    account_id                  BIGINT NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    email                       TEXT NOT NULL,
    encrypted_password          TEXT NOT NULL,
    confirmed_at                TIMESTAMPTZ,
    confirmation_token          TEXT UNIQUE,
    confirmation_sent_at        TIMESTAMPTZ,
    unconfirmed_email           TEXT,
    invite_id                   BIGINT REFERENCES invites(id) ON DELETE SET NULL,
    role_id                     BIGINT REFERENCES user_roles(id) ON DELETE SET NULL,
    locale                      TEXT,
    chosen_languages            TEXT[],
    time_zone                   TEXT,
    settings                    TEXT,
    reset_password_token        TEXT,
    reset_password_sent_at      TIMESTAMPTZ,
    sign_in_count               INTEGER NOT NULL DEFAULT 0,
    current_sign_in_at          TIMESTAMPTZ,
    last_sign_in_at             TIMESTAMPTZ,
    consumed_timestep           INTEGER,
    otp_required_for_login      BOOLEAN NOT NULL DEFAULT false,
    otp_backup_codes            TEXT[],
    otp_secret                  TEXT,
    sign_in_token               TEXT,
    sign_in_token_sent_at       TIMESTAMPTZ,
    skip_sign_in_token          BOOLEAN,
    webauthn_id                 TEXT,
    last_emailed_at             TIMESTAMPTZ,
    disabled                    BOOLEAN NOT NULL DEFAULT false,
    approved                    BOOLEAN NOT NULL DEFAULT true,
    sign_up_ip                  INET,
    created_by_application_id   BIGINT REFERENCES oauth_applications(id) ON DELETE SET NULL,
    age_verified_at             TIMESTAMPTZ,
    require_tos_interstitial    BOOLEAN NOT NULL DEFAULT false,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE users_id_seq OWNED BY users.id;

CREATE UNIQUE INDEX index_users_on_email ON users(lower(email));
CREATE INDEX users_by_invite ON users(invite_id) WHERE invite_id IS NOT NULL;
CREATE INDEX index_users_on_role_id ON users(role_id) WHERE role_id IS NOT NULL;
CREATE INDEX index_users_on_confirmation_token ON users(confirmation_token) WHERE confirmation_token IS NOT NULL;
CREATE UNIQUE INDEX index_users_on_reset_password_token ON users(reset_password_token) WHERE reset_password_token IS NOT NULL;
CREATE INDEX index_users_on_unconfirmed_email ON users(unconfirmed_email) WHERE unconfirmed_email IS NOT NULL;
CREATE INDEX index_users_on_created_by_application_id ON users(created_by_application_id) WHERE created_by_application_id IS NOT NULL;

-- Complete circular FK: invites.user_id → users
ALTER TABLE invites ADD CONSTRAINT fk_invites_user_id FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
CREATE INDEX index_invites_on_user_id ON invites(user_id) WHERE user_id IS NOT NULL;

-- ── instance_user_sessions ────────────────────────────────────────────────────
CREATE TABLE instance_user_sessions (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token      TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ON instance_user_sessions(token);

-- ── pending_signups ───────────────────────────────────────────────────────────
CREATE TABLE pending_signups (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username            TEXT NOT NULL,
    email               TEXT NOT NULL,
    email_normalized    TEXT NOT NULL UNIQUE,
    password_hash       TEXT NOT NULL,
    invite_id           BIGINT REFERENCES invites(id),
    reason              TEXT,
    locale              TEXT NOT NULL DEFAULT 'en',
    app_id              BIGINT REFERENCES oauth_applications(id),
    confirmation_token  TEXT NOT NULL UNIQUE,
    expires_at          TIMESTAMPTZ NOT NULL DEFAULT now() + interval '24 hours',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── conversations ─────────────────────────────────────────────────────────────
CREATE TABLE conversations (
    id                BIGSERIAL PRIMARY KEY,
    uri               TEXT,
    parent_status_id  BIGINT,
    parent_account_id BIGINT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_conversations_on_uri
    ON conversations(uri) WHERE uri IS NOT NULL;
CREATE UNIQUE INDEX index_conversations_on_parent_status_id
    ON conversations(parent_status_id) WHERE parent_status_id IS NOT NULL;

-- ── scheduled_statuses ────────────────────────────────────────────────────────
CREATE TABLE scheduled_statuses (
    id           BIGSERIAL PRIMARY KEY,
    account_id   BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    scheduled_at TIMESTAMPTZ,
    params       JSONB,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_scheduled_statuses_on_scheduled_at ON scheduled_statuses(scheduled_at);

-- ── statuses ──────────────────────────────────────────────────────────────────
CREATE TABLE statuses (
    id                              BIGINT PRIMARY KEY,
    account_id                      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    text                            TEXT NOT NULL DEFAULT '',
    spoiler_text                    TEXT NOT NULL DEFAULT '',
    in_reply_to_id                  BIGINT REFERENCES statuses(id) ON DELETE SET NULL,
    in_reply_to_account_id          BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    reblog_of_id                    BIGINT REFERENCES statuses(id) ON DELETE CASCADE,
    visibility                      INTEGER NOT NULL DEFAULT 0,
    language                        TEXT,
    sensitive                       BOOLEAN NOT NULL DEFAULT false,
    url                             TEXT,
    uri                             TEXT,
    deleted_at                      TIMESTAMPTZ,
    edited_at                       TIMESTAMPTZ,
    application_id                  BIGINT REFERENCES oauth_applications(id) ON DELETE SET NULL,
    reply                           BOOLEAN NOT NULL DEFAULT false,
    conversation_id                 BIGINT REFERENCES conversations(id),
    fetched_replies_at              TIMESTAMPTZ,
    local                           BOOLEAN,
    ordered_media_attachment_ids    BIGINT[],
    poll_id                         BIGINT,
    quote_approval_policy           INTEGER NOT NULL DEFAULT 0,
    trendable                       BOOLEAN,
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX statuses_by_account
    ON statuses(account_id, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX statuses_by_account_id_desc
    ON statuses(account_id, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX statuses_public
    ON statuses(id DESC) WHERE visibility = 0 AND deleted_at IS NULL AND reblog_of_id IS NULL;
CREATE INDEX statuses_public_timeline
    ON statuses(id DESC)
    WHERE visibility = 0
      AND deleted_at IS NULL
      AND reblog_of_id IS NULL
      AND (NOT reply OR in_reply_to_account_id = account_id);
CREATE INDEX statuses_by_reblog
    ON statuses(account_id, reblog_of_id) WHERE reblog_of_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX statuses_by_reply
    ON statuses(in_reply_to_id) WHERE in_reply_to_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_statuses_conversation_id
    ON statuses(conversation_id) WHERE conversation_id IS NOT NULL;

CREATE INDEX statuses_by_account_created_at
    ON statuses(account_id, created_at DESC) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX index_statuses_on_uri
    ON statuses(uri text_pattern_ops) WHERE uri IS NOT NULL;

-- ── account_stats ─────────────────────────────────────────────────────────────
CREATE TABLE account_stats (
    id              BIGSERIAL PRIMARY KEY,
    account_id      BIGINT NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    statuses_count  BIGINT NOT NULL DEFAULT 0,
    following_count BIGINT NOT NULL DEFAULT 0,
    followers_count BIGINT NOT NULL DEFAULT 0,
    last_status_at  TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_account_stats_on_account_id ON account_stats(account_id);
CREATE INDEX index_account_stats_on_last_status_at_and_account_id
    ON account_stats(last_status_at DESC NULLS LAST, account_id);

-- ── quotes ────────────────────────────────────────────────────────────────────
CREATE TABLE quotes (
    id                BIGINT PRIMARY KEY,
    status_id         BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    quoted_status_id  BIGINT REFERENCES statuses(id) ON DELETE CASCADE,
    quoted_account_id BIGINT REFERENCES accounts(id) ON DELETE CASCADE,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    activity_uri      TEXT,
    approval_uri      TEXT,
    state             INTEGER NOT NULL DEFAULT 0,
    legacy            BOOLEAN NOT NULL DEFAULT false,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_quotes_on_status_id ON quotes(status_id);
CREATE UNIQUE INDEX index_quotes_on_activity_uri ON quotes(activity_uri) WHERE activity_uri IS NOT NULL;
CREATE INDEX index_quotes_on_approval_uri ON quotes(approval_uri) WHERE approval_uri IS NOT NULL;
CREATE INDEX index_quotes_on_account_id_and_quoted_account_id_and_id
    ON quotes(account_id, quoted_account_id, id);
CREATE INDEX index_quotes_on_quoted_status_id_and_id ON quotes(quoted_status_id, id);
CREATE INDEX quotes_quoted_status_id_idx ON quotes(quoted_status_id);
CREATE INDEX quotes_account_id_idx ON quotes(account_id);
CREATE INDEX quotes_quoted_account_id_idx ON quotes(quoted_account_id);
CREATE INDEX quotes_state_idx ON quotes(state);

-- ── status_edits ──────────────────────────────────────────────────────────────
CREATE TABLE status_edits (
    id                              BIGSERIAL PRIMARY KEY,
    status_id                       BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    text                            TEXT NOT NULL DEFAULT '',
    spoiler_text                    TEXT NOT NULL DEFAULT '',
    sensitive                       BOOLEAN,
    created_at                      TIMESTAMPTZ NOT NULL DEFAULT now(),
    account_id                      BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    media_descriptions              TEXT[],
    ordered_media_attachment_ids    BIGINT[],
    poll_options                    TEXT[],
    quote_id                        BIGINT REFERENCES quotes(id) ON DELETE SET NULL,
    updated_at                      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── media_attachments ─────────────────────────────────────────────────────────
CREATE TABLE media_attachments (
    id                          BIGINT PRIMARY KEY,
    account_id                  BIGINT REFERENCES accounts(id) ON DELETE CASCADE,
    status_id                   BIGINT REFERENCES statuses(id) ON DELETE SET NULL,
    remote_url                  TEXT NOT NULL DEFAULT '',
    description                 TEXT,
    blurhash                    TEXT,
    type                        INTEGER NOT NULL DEFAULT 0,
    shortcode                   TEXT,
    file_meta                   JSON,
    scheduled_status_id         BIGINT REFERENCES scheduled_statuses(id) ON DELETE SET NULL,
    processing                  INTEGER,
    file_storage_schema_version INTEGER,
    file_file_name              TEXT,
    file_content_type           TEXT,
    file_file_size              INTEGER,
    file_updated_at             TIMESTAMPTZ,
    thumbnail_file_name         TEXT,
    thumbnail_content_type      TEXT,
    thumbnail_file_size         INTEGER,
    thumbnail_updated_at        TIMESTAMPTZ,
    thumbnail_remote_url        TEXT,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- v4.6.0 column, kept at the end to match ALTER-applied prod column order.
    thumbnail_storage_schema_version INTEGER
);

CREATE INDEX media_by_account ON media_attachments(account_id);
CREATE INDEX media_by_status  ON media_attachments(status_id) WHERE status_id IS NOT NULL;
CREATE UNIQUE INDEX index_media_attachments_on_shortcode
    ON media_attachments(shortcode) WHERE shortcode IS NOT NULL;
CREATE INDEX index_media_attachments_on_scheduled_status_id
    ON media_attachments(scheduled_status_id) WHERE scheduled_status_id IS NOT NULL;

-- ── polls ─────────────────────────────────────────────────────────────────────
CREATE TABLE polls (
    id              BIGINT PRIMARY KEY DEFAULT nextval('polls_id_seq'),
    status_id       BIGINT NOT NULL UNIQUE REFERENCES statuses(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    options         TEXT[] NOT NULL DEFAULT '{}',
    votes_count     BIGINT NOT NULL DEFAULT 0,
    voters_count    BIGINT,
    multiple        BOOLEAN NOT NULL DEFAULT false,
    expires_at      TIMESTAMPTZ,
    cached_tallies  BIGINT[] NOT NULL DEFAULT '{}',
    hide_totals     BOOLEAN NOT NULL DEFAULT false,
    last_fetched_at TIMESTAMPTZ,
    lock_version    INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE polls_id_seq OWNED BY polls.id;

CREATE INDEX polls_by_expires_at ON polls(expires_at) WHERE expires_at IS NOT NULL;

-- ── poll_votes ────────────────────────────────────────────────────────────────
CREATE TABLE poll_votes (
    id         BIGSERIAL PRIMARY KEY,
    poll_id    BIGINT NOT NULL REFERENCES polls(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    choice     INT NOT NULL,
    uri        TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (poll_id, account_id, choice)
);

-- ── reports ───────────────────────────────────────────────────────────────────
CREATE TABLE reports (
    id                          BIGSERIAL PRIMARY KEY,
    account_id                  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id           BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    assigned_account_id         BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    action_taken_by_account_id  BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    status_ids                  BIGINT[] NOT NULL DEFAULT '{}',
    comment                     TEXT NOT NULL DEFAULT '',
    forwarded                   BOOLEAN,
    category                    INTEGER NOT NULL DEFAULT 0,
    action_taken_at             TIMESTAMPTZ,
    uri                         TEXT,
    rule_ids                    BIGINT[],
    application_id              BIGINT REFERENCES oauth_applications(id) ON DELETE SET NULL,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── notifications ─────────────────────────────────────────────────────────────
CREATE TABLE notifications (
    id              BIGINT PRIMARY KEY DEFAULT nextval('notifications_id_seq'),
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    from_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    type            TEXT,
    filtered        BOOLEAN NOT NULL DEFAULT false,
    group_key       TEXT,
    activity_id     BIGINT,
    activity_type   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX notifications_by_account ON notifications(account_id, id DESC);
CREATE INDEX index_notifications_on_account_id_and_group_key
    ON notifications(account_id, group_key) WHERE group_key IS NOT NULL;
CREATE INDEX index_notifications_on_account_id_id_type
    ON notifications(account_id, id DESC, type);
CREATE INDEX index_notifications_on_filtered
    ON notifications(account_id, id DESC, type) WHERE filtered = false;
CREATE INDEX index_notifications_on_activity_id_and_activity_type
    ON notifications(activity_id, activity_type)
    WHERE activity_id IS NOT NULL AND activity_type IS NOT NULL;
CREATE INDEX index_notifications_on_from_account_id ON notifications(from_account_id);

-- ── notification_requests ─────────────────────────────────────────────────────
CREATE TABLE notification_requests (
    id                  BIGSERIAL PRIMARY KEY,
    account_id          BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    from_account_id     BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    last_status_id      BIGINT,
    notifications_count BIGINT NOT NULL DEFAULT 0,
    dismissed           BOOLEAN NOT NULL DEFAULT false,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, from_account_id)
);
CREATE INDEX index_notification_requests_on_last_status_id
    ON notification_requests(last_status_id) WHERE last_status_id IS NOT NULL;
CREATE INDEX index_notification_requests_on_from_account_id
    ON notification_requests(from_account_id);

-- ── notification_policies ─────────────────────────────────────────────────────
CREATE TABLE notification_policies (
    id                   BIGSERIAL PRIMARY KEY,
    account_id           BIGINT NOT NULL UNIQUE REFERENCES accounts(id) ON DELETE CASCADE,
    for_not_following    INTEGER NOT NULL DEFAULT 0,
    for_not_followers    INTEGER NOT NULL DEFAULT 0,
    for_new_accounts     INTEGER NOT NULL DEFAULT 0,
    for_private_mentions INTEGER NOT NULL DEFAULT 1,
    for_limited_accounts INTEGER NOT NULL DEFAULT 1,
    for_bots             INTEGER NOT NULL DEFAULT 0,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_notification_policies_on_account_id ON notification_policies(account_id);

-- ── notification_permissions ──────────────────────────────────────────────────
CREATE TABLE notification_permissions (
    id              BIGSERIAL PRIMARY KEY,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    from_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_notification_permissions_on_account_id ON notification_permissions(account_id);
CREATE INDEX index_notification_permissions_on_from_account_id ON notification_permissions(from_account_id);

-- ── report_notes ──────────────────────────────────────────────────────────────
CREATE TABLE report_notes (
    id         BIGSERIAL PRIMARY KEY,
    content    TEXT NOT NULL,
    report_id  BIGINT NOT NULL REFERENCES reports(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── account_warnings ──────────────────────────────────────────────────────────
CREATE TABLE account_warnings (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    target_account_id BIGINT REFERENCES accounts(id) ON DELETE CASCADE,
    action            INTEGER NOT NULL DEFAULT 0,
    text              TEXT NOT NULL DEFAULT '',
    status_ids        BIGINT[],
    report_id         BIGINT REFERENCES reports(id) ON DELETE SET NULL,
    overruled_at      TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── account_warning_presets ───────────────────────────────────────────────────
CREATE TABLE account_warning_presets (
    id         BIGSERIAL PRIMARY KEY,
    text       TEXT NOT NULL DEFAULT '',
    title      TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── account_moderation_notes ──────────────────────────────────────────────────
CREATE TABLE account_moderation_notes (
    id                BIGSERIAL PRIMARY KEY,
    content           TEXT NOT NULL,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_account_moderation_notes_on_account_id
    ON account_moderation_notes(account_id);
CREATE INDEX index_account_moderation_notes_on_target_account_id
    ON account_moderation_notes(target_account_id);

-- ── admin_action_logs ─────────────────────────────────────────────────────────
CREATE TABLE admin_action_logs (
    id               BIGSERIAL PRIMARY KEY,
    account_id       BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    action           TEXT NOT NULL DEFAULT '',
    target_type      TEXT,
    target_id        BIGINT,
    human_identifier TEXT,
    route_param      TEXT,
    permalink        TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_admin_action_logs_on_target_type_and_target_id
    ON admin_action_logs(target_type, target_id)
    WHERE target_type IS NOT NULL AND target_id IS NOT NULL;

-- ── follows ───────────────────────────────────────────────────────────────────
CREATE TABLE follows (
    id                BIGINT PRIMARY KEY DEFAULT nextval('follows_id_seq'),
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uri               TEXT UNIQUE,
    show_reblogs      BOOLEAN NOT NULL DEFAULT true,
    notify            BOOLEAN NOT NULL DEFAULT false,
    languages         TEXT[],
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
ALTER SEQUENCE follows_id_seq OWNED BY follows.id;

CREATE INDEX follows_by_target ON follows(target_account_id);

-- ── follow_requests ───────────────────────────────────────────────────────────
CREATE TABLE follow_requests (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uri               TEXT UNIQUE,
    show_reblogs      BOOLEAN NOT NULL DEFAULT true,
    notify            BOOLEAN NOT NULL DEFAULT false,
    languages         TEXT[],
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
CREATE INDEX index_follow_requests_on_account_id_and_target_account_id
    ON follow_requests(account_id, target_account_id);

-- ── follow_recommendation_mutes ───────────────────────────────────────────────
CREATE TABLE follow_recommendation_mutes (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);

-- ── follow_recommendation_suppressions ───────────────────────────────────────
CREATE TABLE follow_recommendation_suppressions (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id)
);

-- ── blocks ────────────────────────────────────────────────────────────────────
CREATE TABLE blocks (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uri               TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
CREATE INDEX index_blocks_on_target_account_id ON blocks(target_account_id);

-- ── mutes ─────────────────────────────────────────────────────────────────────
CREATE TABLE mutes (
    id                 BIGSERIAL PRIMARY KEY,
    account_id         BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    hide_notifications BOOLEAN NOT NULL DEFAULT true,
    expires_at         TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
CREATE INDEX index_mutes_on_target_account_id ON mutes(target_account_id);

-- ── favourites ────────────────────────────────────────────────────────────────
CREATE TABLE favourites (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    status_id  BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, status_id)
);

-- ── bookmarks ─────────────────────────────────────────────────────────────────
CREATE TABLE bookmarks (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    status_id  BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, status_id)
);

-- ── status_pins ───────────────────────────────────────────────────────────────
CREATE TABLE status_pins (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    status_id  BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, status_id)
);
CREATE INDEX status_pins_by_account ON status_pins(account_id, id DESC);

-- ── status_stats ──────────────────────────────────────────────────────────────
CREATE TABLE status_stats (
    id                          BIGSERIAL PRIMARY KEY,
    status_id                   BIGINT NOT NULL UNIQUE REFERENCES statuses(id) ON DELETE CASCADE,
    replies_count               BIGINT NOT NULL DEFAULT 0,
    reblogs_count               BIGINT NOT NULL DEFAULT 0,
    favourites_count            BIGINT NOT NULL DEFAULT 0,
    quotes_count                BIGINT NOT NULL DEFAULT 0,
    untrusted_favourites_count  BIGINT,
    untrusted_reblogs_count     BIGINT,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_status_stats_on_status_id ON status_stats(status_id);

-- ── ip_blocks ─────────────────────────────────────────────────────────────────
CREATE TABLE ip_blocks (
    id         BIGSERIAL PRIMARY KEY,
    ip         INET NOT NULL UNIQUE DEFAULT '0.0.0.0',
    severity   INTEGER NOT NULL DEFAULT 0,
    comment    TEXT NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── email_domain_blocks ───────────────────────────────────────────────────────
CREATE TABLE email_domain_blocks (
    id                  BIGSERIAL PRIMARY KEY,
    domain              TEXT NOT NULL UNIQUE,
    allow_with_approval BOOLEAN NOT NULL DEFAULT false,
    parent_id           BIGINT REFERENCES email_domain_blocks(id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── canonical_email_blocks ────────────────────────────────────────────────────
CREATE TABLE canonical_email_blocks (
    id                   BIGSERIAL PRIMARY KEY,
    canonical_email_hash TEXT NOT NULL UNIQUE,
    reference_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── domain_blocks ─────────────────────────────────────────────────────────────
CREATE TABLE domain_blocks (
    id              BIGSERIAL PRIMARY KEY,
    domain          TEXT NOT NULL UNIQUE,
    severity        INTEGER,
    reject_media    BOOLEAN NOT NULL DEFAULT false,
    reject_reports  BOOLEAN NOT NULL DEFAULT false,
    private_comment TEXT,
    public_comment  TEXT,
    obfuscate       BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── domain_allows ─────────────────────────────────────────────────────────────
CREATE TABLE domain_allows (
    id         BIGSERIAL PRIMARY KEY,
    domain     TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── account_domain_blocks ─────────────────────────────────────────────────────
CREATE TABLE account_domain_blocks (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    domain     TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, domain)
);
CREATE INDEX index_account_domain_blocks_on_account_id_and_domain
    ON account_domain_blocks(account_id, domain);

-- ── custom_emoji_categories ───────────────────────────────────────────────────
CREATE TABLE custom_emoji_categories (
    id                BIGSERIAL PRIMARY KEY,
    name              TEXT,
    featured_emoji_id BIGINT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_custom_emoji_categories_on_name ON custom_emoji_categories(name);

-- ── custom_emojis ─────────────────────────────────────────────────────────────
CREATE TABLE custom_emojis (
    id                          BIGINT PRIMARY KEY DEFAULT nextval('custom_emojis_id_seq'),
    shortcode                   TEXT NOT NULL,
    domain                      TEXT,
    visible_in_picker           BOOLEAN NOT NULL DEFAULT true,
    disabled                    BOOLEAN NOT NULL DEFAULT false,
    category_id                 BIGINT REFERENCES custom_emoji_categories(id) ON DELETE SET NULL,
    uri                         TEXT,
    image_file_name             TEXT,
    image_content_type          TEXT,
    image_file_size             INTEGER,
    image_updated_at            TIMESTAMPTZ,
    image_remote_url            TEXT,
    image_storage_schema_version INTEGER,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE custom_emojis_id_seq OWNED BY custom_emojis.id;
-- Circular FK: custom_emoji_categories.featured_emoji_id -> custom_emojis (added after custom_emojis exists)
ALTER TABLE custom_emoji_categories ADD CONSTRAINT fk_custom_emoji_categories_featured_emoji_id FOREIGN KEY (featured_emoji_id) REFERENCES custom_emojis(id) ON DELETE SET NULL;
-- Matches Mastodon's index_custom_emojis_on_shortcode_and_domain (remote emojis unique per domain)
CREATE UNIQUE INDEX index_custom_emojis_on_shortcode_and_domain ON custom_emojis (shortcode, domain);
-- Partial index for local emojis (domain IS NULL) — needed for ON CONFLICT in admin upserts
CREATE UNIQUE INDEX index_custom_emojis_on_shortcode_local ON custom_emojis (shortcode) WHERE domain IS NULL;

-- ── announcements ─────────────────────────────────────────────────────────────
CREATE TABLE announcements (
    id                   BIGSERIAL PRIMARY KEY,
    text                 TEXT NOT NULL DEFAULT '',
    published            BOOLEAN NOT NULL DEFAULT false,
    all_day              BOOLEAN NOT NULL DEFAULT false,
    starts_at            TIMESTAMPTZ,
    ends_at              TIMESTAMPTZ,
    published_at         TIMESTAMPTZ,
    scheduled_at         TIMESTAMPTZ,
    status_ids           BIGINT[],
    notification_sent_at TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── announcement_mutes ────────────────────────────────────────────────────────
CREATE TABLE announcement_mutes (
    id              BIGSERIAL PRIMARY KEY,
    announcement_id BIGINT NOT NULL REFERENCES announcements(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, announcement_id)
);
CREATE INDEX index_announcement_mutes_on_account_id_and_announcement_id
    ON announcement_mutes(account_id, announcement_id);

-- ── announcement_reactions ────────────────────────────────────────────────────
CREATE TABLE announcement_reactions (
    id              BIGSERIAL PRIMARY KEY,
    announcement_id BIGINT NOT NULL REFERENCES announcements(id) ON DELETE CASCADE,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    custom_emoji_id BIGINT REFERENCES custom_emojis(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (announcement_id, account_id, name)
);

-- ── tags ──────────────────────────────────────────────────────────────────────
CREATE TABLE tags (
    id                  BIGINT PRIMARY KEY DEFAULT nextval('tags_id_seq'),
    name                TEXT NOT NULL,
    trendable           BOOLEAN,
    usable              BOOLEAN,
    listable            BOOLEAN,
    reviewed_at         TIMESTAMPTZ,
    display_name        TEXT,
    last_status_at      TIMESTAMPTZ,
    max_score           DOUBLE PRECISION,
    max_score_at        TIMESTAMPTZ,
    requested_review_at TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE tags_id_seq OWNED BY tags.id;
CREATE UNIQUE INDEX index_tags_on_name_lower_btree ON tags(lower(name) text_pattern_ops);

-- ── statuses_tags ─────────────────────────────────────────────────────────────
CREATE TABLE statuses_tags (
    status_id BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    tag_id    BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (tag_id, status_id)
);
CREATE INDEX index_statuses_tags_on_status_id ON statuses_tags(status_id);

-- ── accounts_tags ─────────────────────────────────────────────────────────────
CREATE TABLE accounts_tags (
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    tag_id     BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (tag_id, account_id)
);
CREATE INDEX index_accounts_tags_on_account_id_and_tag_id ON accounts_tags(account_id, tag_id);

-- ── tag_follows ───────────────────────────────────────────────────────────────
CREATE TABLE tag_follows (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    tag_id     BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, tag_id)
);
CREATE INDEX tag_follows_by_account ON tag_follows(account_id);

-- ── tag_trends ────────────────────────────────────────────────────────────────
CREATE TABLE tag_trends (
    id       BIGSERIAL PRIMARY KEY,
    tag_id   BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    score    DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    rank     INTEGER NOT NULL DEFAULT 0,
    allowed  BOOLEAN NOT NULL DEFAULT false,
    language TEXT NOT NULL DEFAULT '',
    UNIQUE (tag_id, language)
);

-- ── featured_tags ─────────────────────────────────────────────────────────────
CREATE TABLE featured_tags (
    id             BIGSERIAL PRIMARY KEY,
    account_id     BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    tag_id         BIGINT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    name           TEXT,
    statuses_count BIGINT NOT NULL DEFAULT 0,
    last_status_at TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, tag_id)
);

-- ── mentions ──────────────────────────────────────────────────────────────────
CREATE TABLE mentions (
    id         BIGSERIAL PRIMARY KEY,
    status_id  BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    silent     BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (status_id, account_id)
);

-- ── lists ─────────────────────────────────────────────────────────────────────
CREATE TABLE lists (
    id             BIGSERIAL PRIMARY KEY,
    account_id     BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    title          TEXT NOT NULL DEFAULT '',
    replies_policy INTEGER NOT NULL DEFAULT 0,
    exclusive      BOOLEAN NOT NULL DEFAULT false,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── list_accounts ─────────────────────────────────────────────────────────────
CREATE TABLE list_accounts (
    id                BIGSERIAL PRIMARY KEY,
    list_id           BIGINT NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    follow_id         BIGINT REFERENCES follows(id) ON DELETE SET NULL,
    follow_request_id BIGINT REFERENCES follow_requests(id) ON DELETE SET NULL,
    UNIQUE (list_id, account_id)
);

-- ── custom_filters ────────────────────────────────────────────────────────────
CREATE TABLE custom_filters (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ,
    phrase     TEXT NOT NULL DEFAULT '',
    context    TEXT[] NOT NULL DEFAULT '{}',
    action     INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── custom_filter_keywords ────────────────────────────────────────────────────
CREATE TABLE custom_filter_keywords (
    id               BIGSERIAL PRIMARY KEY,
    custom_filter_id BIGINT NOT NULL REFERENCES custom_filters(id) ON DELETE CASCADE,
    keyword          TEXT NOT NULL DEFAULT '',
    whole_word       BOOLEAN NOT NULL DEFAULT true,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── custom_filter_statuses ────────────────────────────────────────────────────
CREATE TABLE custom_filter_statuses (
    id               BIGSERIAL PRIMARY KEY,
    custom_filter_id BIGINT NOT NULL REFERENCES custom_filters(id) ON DELETE CASCADE,
    status_id        BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_custom_filter_statuses_on_status_id_and_custom_filter_id
    ON custom_filter_statuses(status_id, custom_filter_id);

-- ── preview_cards ─────────────────────────────────────────────────────────────
CREATE TABLE preview_cards (
    id                          BIGSERIAL PRIMARY KEY,
    url                         TEXT NOT NULL UNIQUE,
    title                       TEXT NOT NULL DEFAULT '',
    description                 TEXT NOT NULL DEFAULT '',
    card_type                   TEXT NOT NULL DEFAULT 'link',
    image_url                   TEXT,
    author_name                 TEXT NOT NULL DEFAULT '',
    author_url                  TEXT NOT NULL DEFAULT '',
    provider_name               TEXT NOT NULL DEFAULT '',
    provider_url                TEXT NOT NULL DEFAULT '',
    html                        TEXT NOT NULL DEFAULT '',
    width                       INT NOT NULL DEFAULT 0,
    height                      INT NOT NULL DEFAULT 0,
    embed_url                   TEXT NOT NULL DEFAULT '',
    blurhash                    TEXT,
    type                        INTEGER NOT NULL DEFAULT 0,
    author_account_id           BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    language                    TEXT,
    link_type                   INTEGER,
    max_score                   DOUBLE PRECISION,
    max_score_at                TIMESTAMPTZ,
    published_at                TIMESTAMPTZ,
    trendable                   BOOLEAN,
    image_description           TEXT NOT NULL DEFAULT '',
    image_file_name             TEXT,
    image_content_type          TEXT,
    image_file_size             INTEGER,
    image_updated_at            TIMESTAMPTZ,
    image_storage_schema_version INTEGER,
    unverified_author_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    fetched_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_preview_cards_on_unverified_author_account_id_and_id
    ON preview_cards (unverified_author_account_id, id) WHERE unverified_author_account_id IS NOT NULL;
-- ── preview_card_providers ────────────────────────────────────────────────────
CREATE TABLE preview_card_providers (
    id                  BIGSERIAL PRIMARY KEY,
    domain              TEXT NOT NULL DEFAULT '' UNIQUE,
    trendable           BOOLEAN,
    reviewed_at         TIMESTAMPTZ,
    requested_review_at TIMESTAMPTZ,
    icon_file_name      TEXT,
    icon_content_type   TEXT,
    icon_file_size      BIGINT,
    icon_updated_at     TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── preview_cards_statuses ────────────────────────────────────────────────────
CREATE TABLE preview_cards_statuses (
    status_id       BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    preview_card_id BIGINT NOT NULL REFERENCES preview_cards(id) ON DELETE CASCADE,
    url             TEXT,
    PRIMARY KEY (status_id, preview_card_id)
);

-- ── preview_card_trends ───────────────────────────────────────────────────────
CREATE TABLE preview_card_trends (
    id              BIGSERIAL PRIMARY KEY,
    preview_card_id BIGINT NOT NULL UNIQUE REFERENCES preview_cards(id) ON DELETE CASCADE,
    score           DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    rank            INTEGER NOT NULL DEFAULT 0,
    allowed         BOOLEAN NOT NULL DEFAULT false,
    language        TEXT
);

-- ── account_pins ──────────────────────────────────────────────────────────────
CREATE TABLE account_pins (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
CREATE INDEX index_account_pins_on_target_account_id ON account_pins(target_account_id);

-- ── account_aliases ───────────────────────────────────────────────────────────
CREATE TABLE account_aliases (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uri        TEXT NOT NULL,
    acct       TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, uri)
);

-- ── account_notes ─────────────────────────────────────────────────────────────
CREATE TABLE account_notes (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    target_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    comment           TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, target_account_id)
);
CREATE INDEX index_account_notes_on_target_account_id ON account_notes(target_account_id);

-- ── markers ───────────────────────────────────────────────────────────────────
CREATE TABLE markers (
    id           BIGSERIAL PRIMARY KEY,
    user_id      BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    timeline     TEXT NOT NULL CHECK (timeline IN ('home', 'notifications')),
    last_read_id BIGINT NOT NULL DEFAULT 0,
    lock_version INTEGER NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, timeline)
);

-- ── conversation_mutes ────────────────────────────────────────────────────────
CREATE TABLE conversation_mutes (
    id              BIGSERIAL PRIMARY KEY,
    account_id      BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    conversation_id BIGINT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    UNIQUE (account_id, conversation_id)
);

-- ── account_conversations ─────────────────────────────────────────────────────
CREATE TABLE account_conversations (
    id                      BIGSERIAL PRIMARY KEY,
    account_id              BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    conversation_id         BIGINT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    participant_account_ids BIGINT[] NOT NULL DEFAULT '{}',
    status_ids              BIGINT[] NOT NULL DEFAULT '{}',
    last_status_id          BIGINT,
    lock_version            INTEGER NOT NULL DEFAULT 0,
    unread                  BOOLEAN NOT NULL DEFAULT false,
    UNIQUE (account_id, conversation_id, participant_account_ids)
);
CREATE INDEX index_account_conversations_on_conversation_id
    ON account_conversations(conversation_id);

-- ── status_trends ─────────────────────────────────────────────────────────────
CREATE TABLE status_trends (
    id         BIGSERIAL PRIMARY KEY,
    status_id  BIGINT NOT NULL UNIQUE REFERENCES statuses(id) ON DELETE CASCADE,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    score      DOUBLE PRECISION NOT NULL DEFAULT 0.0,
    rank       INTEGER NOT NULL DEFAULT 0,
    allowed    BOOLEAN NOT NULL DEFAULT false,
    language   TEXT
);
CREATE INDEX index_status_trends_on_account_id ON status_trends(account_id);

-- ── oauth_access_grants ───────────────────────────────────────────────────────
CREATE TABLE oauth_access_grants (
    id                    BIGSERIAL PRIMARY KEY,
    application_id        BIGINT NOT NULL REFERENCES oauth_applications(id) ON DELETE CASCADE,
    resource_owner_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token                 TEXT NOT NULL UNIQUE,
    redirect_uri          TEXT NOT NULL,
    scopes                TEXT,
    code_challenge        TEXT,
    code_challenge_method TEXT,
    expires_in            INTEGER NOT NULL DEFAULT 600,
    revoked_at            TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── oauth_access_tokens ───────────────────────────────────────────────────────
CREATE TABLE oauth_access_tokens (
    id                BIGINT PRIMARY KEY DEFAULT nextval('oauth_access_tokens_id_seq'),
    application_id    BIGINT REFERENCES oauth_applications(id) ON DELETE CASCADE,
    account_id        BIGINT REFERENCES accounts(id) ON DELETE CASCADE,
    token             TEXT NOT NULL UNIQUE,
    refresh_token     TEXT UNIQUE,
    scopes            TEXT,
    expires_at        TIMESTAMPTZ,
    revoked_at        TIMESTAMPTZ,
    expires_in        INTEGER,
    resource_owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    last_used_at      TIMESTAMPTZ,
    last_used_ip      INET,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER SEQUENCE oauth_access_tokens_id_seq OWNED BY oauth_access_tokens.id;

CREATE INDEX tokens_by_account ON oauth_access_tokens(account_id) WHERE revoked_at IS NULL;
CREATE INDEX index_oauth_access_tokens_on_resource_owner_id
    ON oauth_access_tokens(resource_owner_id) WHERE resource_owner_id IS NOT NULL;

-- ── web_push_subscriptions ────────────────────────────────────────────────────
CREATE TABLE web_push_subscriptions (
    id              BIGSERIAL PRIMARY KEY,
    access_token_id BIGINT NOT NULL REFERENCES oauth_access_tokens(id) ON DELETE CASCADE,
    endpoint        TEXT NOT NULL,
    key_p256dh      TEXT NOT NULL DEFAULT '',
    key_auth        TEXT NOT NULL DEFAULT '',
    data            JSON,
    user_id         BIGINT REFERENCES users(id) ON DELETE CASCADE,
    standard        BOOLEAN NOT NULL DEFAULT false,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (access_token_id)
);
CREATE INDEX index_web_push_subscriptions_on_user_id
    ON web_push_subscriptions(user_id) WHERE user_id IS NOT NULL;

-- ── session_activations ───────────────────────────────────────────────────────
CREATE TABLE session_activations (
    id                       BIGSERIAL PRIMARY KEY,
    session_id               TEXT NOT NULL UNIQUE,
    user_id                  BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_agent               TEXT NOT NULL DEFAULT '',
    ip                       INET,
    access_token_id          BIGINT REFERENCES oauth_access_tokens(id) ON DELETE SET NULL,
    web_push_subscription_id BIGINT REFERENCES web_push_subscriptions(id) ON DELETE SET NULL,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_session_activations_on_user_id ON session_activations(user_id);
CREATE INDEX index_session_activations_on_access_token_id
    ON session_activations(access_token_id) WHERE access_token_id IS NOT NULL;

-- ── login_activities ──────────────────────────────────────────────────────────
CREATE TABLE login_activities (
    id                    BIGSERIAL PRIMARY KEY,
    user_id               BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    authentication_method TEXT,
    provider              TEXT,
    success               BOOLEAN,
    failure_reason        TEXT,
    ip                    INET,
    user_agent            TEXT,
    created_at            TIMESTAMPTZ
);
CREATE INDEX index_login_activities_on_user_id ON login_activities(user_id);

-- ── identities ────────────────────────────────────────────────────────────────
CREATE TABLE identities (
    id         BIGSERIAL PRIMARY KEY,
    user_id    BIGINT REFERENCES users(id) ON DELETE CASCADE,
    provider   TEXT NOT NULL DEFAULT '',
    uid        TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_identities_on_user_id ON identities(user_id);
CREATE UNIQUE INDEX index_identities_on_uid_and_provider ON identities(uid, provider);

-- ── backups ───────────────────────────────────────────────────────────────────
CREATE TABLE backups (
    id                BIGSERIAL PRIMARY KEY,
    user_id           BIGINT REFERENCES users(id) ON DELETE SET NULL,
    dump_file         TEXT,
    dump_file_name    TEXT,
    dump_content_type TEXT,
    dump_updated_at   TIMESTAMPTZ,
    dump_file_size    BIGINT,
    processed         BOOLEAN NOT NULL DEFAULT false,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── web_settings ──────────────────────────────────────────────────────────────
CREATE TABLE web_settings (
    id         BIGSERIAL PRIMARY KEY,
    user_id    BIGINT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    data       JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── webauthn_credentials ──────────────────────────────────────────────────────
CREATE TABLE webauthn_credentials (
    id          BIGSERIAL PRIMARY KEY,
    external_id TEXT NOT NULL UNIQUE,
    public_key  TEXT NOT NULL,
    nickname    TEXT NOT NULL,
    sign_count  BIGINT NOT NULL DEFAULT 0,
    user_id     BIGINT REFERENCES users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, nickname)
);

-- ── user_invite_requests ──────────────────────────────────────────────────────
CREATE TABLE user_invite_requests (
    id         BIGSERIAL PRIMARY KEY,
    user_id    BIGINT REFERENCES users(id) ON DELETE CASCADE,
    text       TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_user_invite_requests_on_user_id ON user_invite_requests(user_id);

-- ── bulk_imports ──────────────────────────────────────────────────────────────
CREATE TABLE bulk_imports (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    type              INTEGER NOT NULL DEFAULT 0,
    state             INTEGER NOT NULL DEFAULT 0,
    total_items       INTEGER NOT NULL DEFAULT 0,
    imported_items    INTEGER NOT NULL DEFAULT 0,
    processed_items   INTEGER NOT NULL DEFAULT 0,
    finished_at       TIMESTAMPTZ,
    overwrite         BOOLEAN NOT NULL DEFAULT false,
    likely_mismatched BOOLEAN NOT NULL DEFAULT false,
    missing_status    BOOLEAN NOT NULL DEFAULT false,
    original_filename TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_bulk_imports_on_account_id ON bulk_imports(account_id);
CREATE INDEX index_bulk_imports_unconfirmed ON bulk_imports(id) WHERE state = 0;

CREATE TABLE bulk_import_rows (
    id             BIGSERIAL PRIMARY KEY,
    bulk_import_id BIGINT NOT NULL REFERENCES bulk_imports(id) ON DELETE CASCADE,
    data           JSONB,
    account_id     BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    state          INTEGER NOT NULL DEFAULT 0,
    original_line  INTEGER,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_bulk_import_rows_on_bulk_import_id ON bulk_import_rows(bulk_import_id);

-- ── settings ──────────────────────────────────────────────────────────────────
CREATE TABLE settings (
    id         BIGSERIAL PRIMARY KEY,
    var        TEXT NOT NULL,
    value      TEXT,
    thing_type TEXT,
    thing_id   BIGINT,
    created_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ,
    UNIQUE (thing_type, thing_id, var)
);

-- ── site_uploads ──────────────────────────────────────────────────────────────
CREATE TABLE site_uploads (
    id                BIGSERIAL PRIMARY KEY,
    var               TEXT NOT NULL DEFAULT '' UNIQUE,
    file_url          TEXT,
    meta              JSONB,
    file_file_name    TEXT,
    file_content_type TEXT,
    file_file_size    INTEGER,
    file_updated_at   TIMESTAMPTZ,
    blurhash          TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── software_updates ──────────────────────────────────────────────────────────
CREATE TABLE software_updates (
    id            BIGSERIAL PRIMARY KEY,
    version       TEXT NOT NULL DEFAULT '' UNIQUE,
    urgent        BOOLEAN NOT NULL DEFAULT false,
    type          INTEGER NOT NULL DEFAULT 0,
    release_notes TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── relays ────────────────────────────────────────────────────────────────────
CREATE TABLE relays (
    id                 BIGSERIAL PRIMARY KEY,
    inbox_url          TEXT NOT NULL DEFAULT '',
    follow_activity_id TEXT,
    state              INTEGER NOT NULL DEFAULT 0,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── rules ─────────────────────────────────────────────────────────────────────
CREATE TABLE rules (
    id          BIGSERIAL PRIMARY KEY,
    priority    INTEGER NOT NULL DEFAULT 0,
    deleted_at  TIMESTAMPTZ,
    text        TEXT NOT NULL DEFAULT '',
    hint        TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE rule_translations (
    id         BIGSERIAL PRIMARY KEY,
    rule_id    BIGINT NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    language   TEXT NOT NULL DEFAULT '',
    text       TEXT NOT NULL DEFAULT '',
    hint       TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_rule_translations_on_rule_id ON rule_translations(rule_id);
CREATE UNIQUE INDEX index_rule_translations_on_rule_id_and_language
    ON rule_translations(rule_id, language);

-- ── terms_of_services ─────────────────────────────────────────────────────────
CREATE TABLE terms_of_services (
    id                   BIGSERIAL PRIMARY KEY,
    text                 TEXT NOT NULL DEFAULT '',
    changelog            TEXT NOT NULL DEFAULT '',
    published_at         TIMESTAMPTZ,
    notification_sent_at TIMESTAMPTZ,
    effective_date       DATE,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_terms_of_services_on_published_at
    ON terms_of_services(published_at) WHERE published_at IS NOT NULL;

-- ── tombstones ────────────────────────────────────────────────────────────────
CREATE TABLE tombstones (
    id           BIGSERIAL PRIMARY KEY,
    account_id   BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    uri          TEXT NOT NULL,
    by_moderator BOOLEAN,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_tombstones_on_account_id ON tombstones(account_id);
CREATE INDEX index_tombstones_on_uri ON tombstones(uri);

-- ── unavailable_domains ───────────────────────────────────────────────────────
CREATE TABLE unavailable_domains (
    id         BIGSERIAL PRIMARY KEY,
    domain     TEXT NOT NULL UNIQUE DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── webhooks ──────────────────────────────────────────────────────────────────
CREATE TABLE webhooks (
    id          BIGSERIAL PRIMARY KEY,
    url         TEXT NOT NULL UNIQUE DEFAULT '',
    events      TEXT[] NOT NULL DEFAULT '{}',
    secret      TEXT NOT NULL DEFAULT '',
    enabled     BOOLEAN NOT NULL DEFAULT true,
    template    TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── fasp_providers ────────────────────────────────────────────────────────────
CREATE TABLE fasp_providers (
    id                       BIGSERIAL PRIMARY KEY,
    name                     TEXT NOT NULL DEFAULT '',
    base_url                 TEXT NOT NULL DEFAULT '',
    sign_in_url              TEXT,
    remote_identifier        TEXT NOT NULL DEFAULT '',
    provider_public_key_pem  TEXT NOT NULL DEFAULT '',
    server_private_key_pem   TEXT NOT NULL DEFAULT '',
    capabilities             JSONB NOT NULL DEFAULT '[]',
    privacy_policy           JSONB,
    contact_email            TEXT,
    fediverse_account        TEXT,
    delivery_last_failed_at  TIMESTAMPTZ,
    confirmed                BOOLEAN NOT NULL DEFAULT false,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_fasp_providers_on_base_url ON fasp_providers(base_url);

CREATE TABLE fasp_subscriptions (
    id                  BIGSERIAL PRIMARY KEY,
    fasp_provider_id    BIGINT NOT NULL REFERENCES fasp_providers(id) ON DELETE CASCADE,
    category            TEXT NOT NULL,
    subscription_type   TEXT NOT NULL DEFAULT '',
    max_batch_size      INTEGER NOT NULL DEFAULT 0,
    threshold_timeframe INTEGER,
    threshold_shares    INTEGER,
    threshold_likes     INTEGER,
    threshold_replies   INTEGER,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_fasp_subscriptions_on_fasp_provider_id
    ON fasp_subscriptions(fasp_provider_id);

CREATE TABLE fasp_backfill_requests (
    id               BIGSERIAL PRIMARY KEY,
    fasp_provider_id BIGINT NOT NULL REFERENCES fasp_providers(id) ON DELETE CASCADE,
    max_count        INTEGER NOT NULL DEFAULT 0,
    fulfilled        BOOLEAN NOT NULL DEFAULT false,
    category         TEXT NOT NULL DEFAULT '',
    cursor           TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE fasp_debug_callbacks (
    id               BIGSERIAL PRIMARY KEY,
    fasp_provider_id BIGINT NOT NULL REFERENCES fasp_providers(id) ON DELETE CASCADE,
    ip               TEXT NOT NULL DEFAULT '',
    request_body     TEXT NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE fasp_follow_recommendations (
    id                     BIGSERIAL PRIMARY KEY,
    requesting_account_id  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    recommended_account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_fasp_follow_recommendations_on_requesting_account_id
    ON fasp_follow_recommendations(requesting_account_id);
CREATE INDEX index_fasp_follow_recommendations_on_recommended_account_id
    ON fasp_follow_recommendations(recommended_account_id);

-- ── instance_moderation_notes ─────────────────────────────────────────────────
CREATE TABLE instance_moderation_notes (
    id         BIGSERIAL PRIMARY KEY,
    content    TEXT,
    domain     TEXT NOT NULL DEFAULT '',
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_instance_moderation_notes_on_domain ON instance_moderation_notes(domain);

-- ── account_migrations ────────────────────────────────────────────────────────
CREATE TABLE account_migrations (
    id                BIGSERIAL PRIMARY KEY,
    account_id        BIGINT REFERENCES accounts(id) ON DELETE CASCADE,
    acct              TEXT NOT NULL DEFAULT '',
    followers_count   BIGINT NOT NULL DEFAULT 0,
    target_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_account_migrations_on_account_id ON account_migrations(account_id);
CREATE INDEX index_account_migrations_on_target_account_id
    ON account_migrations(target_account_id) WHERE target_account_id IS NOT NULL;

-- ── account_deletion_requests ─────────────────────────────────────────────────
CREATE TABLE account_deletion_requests (
    id         BIGSERIAL PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_account_deletion_requests_on_account_id
    ON account_deletion_requests(account_id);

-- ── relationship_severance_events ─────────────────────────────────────────────
CREATE TABLE relationship_severance_events (
    id                   BIGSERIAL PRIMARY KEY,
    type                 INTEGER NOT NULL DEFAULT 0,
    purged               BOOLEAN NOT NULL DEFAULT false,
    target_name          TEXT NOT NULL DEFAULT '',
    relationships_count  INTEGER,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_relationship_severance_events_on_type_and_target_name
    ON relationship_severance_events(type, target_name);

CREATE TABLE account_relationship_severance_events (
    id                               BIGSERIAL PRIMARY KEY,
    account_id                       BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    relationship_severance_event_id  BIGINT NOT NULL REFERENCES relationship_severance_events(id) ON DELETE CASCADE,
    followers_count                  INTEGER NOT NULL DEFAULT 0,
    following_count                  INTEGER NOT NULL DEFAULT 0,
    created_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, relationship_severance_event_id)
);
CREATE INDEX index_account_relationship_severance_events_on_account_id
    ON account_relationship_severance_events(account_id);

CREATE TABLE severed_relationships (
    id                               BIGSERIAL PRIMARY KEY,
    relationship_severance_event_id  BIGINT NOT NULL REFERENCES relationship_severance_events(id) ON DELETE CASCADE,
    local_account_id                 BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_account_id                BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    direction                        INTEGER NOT NULL DEFAULT 0,
    show_reblogs                     BOOLEAN,
    notify                           BOOLEAN,
    languages                        TEXT[],
    created_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (relationship_severance_event_id, local_account_id, direction, remote_account_id)
);
CREATE INDEX index_severed_relationships_on_local_account_and_event
    ON severed_relationships(local_account_id, relationship_severance_event_id);
CREATE INDEX index_severed_relationships_on_remote_account_id
    ON severed_relationships(remote_account_id);

-- ── account_statuses_cleanup_policies ────────────────────────────────────────
CREATE TABLE account_statuses_cleanup_policies (
    id                 BIGSERIAL PRIMARY KEY,
    account_id         BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    enabled            BOOLEAN NOT NULL DEFAULT true,
    min_status_age     INTEGER NOT NULL DEFAULT 1209600,
    keep_direct        BOOLEAN NOT NULL DEFAULT true,
    keep_pinned        BOOLEAN NOT NULL DEFAULT true,
    keep_polls         BOOLEAN NOT NULL DEFAULT false,
    keep_media         BOOLEAN NOT NULL DEFAULT false,
    keep_self_fav      BOOLEAN NOT NULL DEFAULT true,
    keep_self_bookmark BOOLEAN NOT NULL DEFAULT true,
    min_favs           INTEGER,
    min_reblogs        INTEGER,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_account_statuses_cleanup_policies_on_account_id
    ON account_statuses_cleanup_policies(account_id);

-- ── appeals ───────────────────────────────────────────────────────────────────
CREATE TABLE appeals (
    id                     BIGSERIAL PRIMARY KEY,
    account_id             BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    account_warning_id     BIGINT NOT NULL UNIQUE REFERENCES account_warnings(id) ON DELETE CASCADE,
    text                   TEXT NOT NULL DEFAULT '',
    approved_at            TIMESTAMPTZ,
    approved_by_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    rejected_at            TIMESTAMPTZ,
    rejected_by_account_id BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_appeals_on_account_id ON appeals(account_id);
CREATE INDEX index_appeals_on_approved_by_account_id
    ON appeals(approved_by_account_id) WHERE approved_by_account_id IS NOT NULL;
CREATE INDEX index_appeals_on_rejected_by_account_id
    ON appeals(rejected_by_account_id) WHERE rejected_by_account_id IS NOT NULL;

-- ── generated_annual_reports ──────────────────────────────────────────────────
CREATE TABLE generated_annual_reports (
    id             BIGSERIAL PRIMARY KEY,
    account_id     BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    year           INT NOT NULL,
    data           JSONB NOT NULL DEFAULT '{}',
    schema_version INT NOT NULL DEFAULT 1,
    share_key      TEXT,
    viewed_at      TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, year)
);

CREATE TABLE annual_report_statuses_per_account_counts (
    id             BIGSERIAL PRIMARY KEY,
    year           INTEGER NOT NULL,
    account_id     BIGINT NOT NULL,
    statuses_count BIGINT NOT NULL,
    UNIQUE (year, account_id)
);

-- ── username_blocks ───────────────────────────────────────────────────────────
CREATE TABLE username_blocks (
    id                  BIGSERIAL PRIMARY KEY,
    username            TEXT NOT NULL,
    exact               BOOLEAN NOT NULL DEFAULT false,
    normalized_username TEXT NOT NULL DEFAULT '',
    allow_with_approval BOOLEAN NOT NULL DEFAULT false,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_username_blocks_on_normalized_username
    ON username_blocks(normalized_username);
CREATE UNIQUE INDEX index_username_blocks_on_username_lower_btree
    ON username_blocks(lower(username));

-- ── email_subscriptions (added in migration 003) ─────────────────────────────
CREATE TABLE email_subscriptions (
    id                  BIGSERIAL PRIMARY KEY,
    account_id          BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    email               TEXT NOT NULL,
    locale              TEXT NOT NULL,
    confirmation_token  TEXT,
    confirmed_at        TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_email_subscriptions_on_account_id_and_email
    ON email_subscriptions(account_id, email);
CREATE UNIQUE INDEX index_email_subscriptions_on_confirmation_token
    ON email_subscriptions(confirmation_token) WHERE confirmation_token IS NOT NULL;

-- ── keypairs (added in migration 003) ────────────────────────────────────────
CREATE TABLE keypairs (
    id          BIGSERIAL PRIMARY KEY,
    account_id  BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    type        INTEGER NOT NULL,
    uri         TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    private_key TEXT,
    revoked     BOOLEAN NOT NULL DEFAULT false,
    expires_at  TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_keypairs_on_account_id ON keypairs(account_id);
CREATE UNIQUE INDEX index_keypairs_on_uri ON keypairs(uri);

-- ── tagged_objects (added in migration 003) ───────────────────────────────────
CREATE TABLE tagged_objects (
    id          BIGSERIAL PRIMARY KEY,
    status_id   BIGINT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    ap_type     TEXT NOT NULL,
    object_type TEXT,
    object_id   BIGINT,
    uri         TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_tagged_objects_on_object ON tagged_objects(object_type, object_id);
CREATE UNIQUE INDEX idx_on_status_id_object_type_object_id_tagged
    ON tagged_objects(status_id, object_type, object_id)
    WHERE object_type IS NOT NULL AND object_id IS NOT NULL;
CREATE UNIQUE INDEX index_tagged_objects_on_status_id_and_uri
    ON tagged_objects(status_id, uri) WHERE uri IS NOT NULL;

-- ── collections + collection_items + collection_reports (added in migration 003)
CREATE TABLE collections (
    id                       BIGSERIAL PRIMARY KEY,
    account_id               BIGINT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name                     TEXT NOT NULL,
    description              TEXT,
    description_html         TEXT,
    discoverable             BOOLEAN NOT NULL,
    local                    BOOLEAN NOT NULL,
    sensitive                BOOLEAN NOT NULL,
    item_count               INTEGER NOT NULL DEFAULT 0,
    original_number_of_items INTEGER,
    language                 TEXT,
    tag_id                   BIGINT REFERENCES tags(id) ON DELETE SET NULL,
    uri                      TEXT,
    url                      TEXT,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_collections_on_account_id ON collections(account_id);
CREATE INDEX index_collections_on_tag_id     ON collections(tag_id);
CREATE UNIQUE INDEX index_collections_on_uri ON collections(uri) WHERE uri IS NOT NULL;

CREATE TABLE collection_items (
    id                        BIGSERIAL PRIMARY KEY,
    collection_id             BIGINT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    account_id                BIGINT REFERENCES accounts(id) ON DELETE SET NULL,
    uri                       TEXT,
    activity_uri              TEXT,
    object_uri                TEXT,
    approval_uri              TEXT,
    approval_last_verified_at TIMESTAMPTZ,
    position                  INTEGER NOT NULL DEFAULT 1,
    state                     INTEGER NOT NULL DEFAULT 0,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX index_collection_items_on_account_id_and_collection_id
    ON collection_items(account_id, collection_id);
CREATE UNIQUE INDEX index_collection_items_on_approval_uri
    ON collection_items(approval_uri) WHERE approval_uri IS NOT NULL;
CREATE INDEX index_collection_items_on_collection_id ON collection_items(collection_id);
CREATE INDEX index_collection_items_on_state
    ON collection_items(state) WHERE state = ANY(ARRAY[2, 3]);
CREATE UNIQUE INDEX index_collection_items_on_uri
    ON collection_items(uri) WHERE uri IS NOT NULL;

CREATE TABLE collection_reports (
    id            BIGSERIAL PRIMARY KEY,
    collection_id BIGINT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    report_id     BIGINT NOT NULL REFERENCES reports(id) ON DELETE CASCADE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX index_collection_reports_on_collection_id ON collection_reports(collection_id);
CREATE INDEX index_collection_reports_on_report_id     ON collection_reports(report_id);

-- ── user_ips (VIEW) ───────────────────────────────────────────────────────────
CREATE VIEW user_ips AS
SELECT user_id, ip, MAX(used_at) AS used_at
FROM (
    SELECT u.id AS user_id, u.sign_up_ip AS ip, u.created_at AS used_at
    FROM users u WHERE u.sign_up_ip IS NOT NULL
    UNION ALL
    SELECT sa.user_id, sa.ip, sa.updated_at
    FROM session_activations sa WHERE sa.ip IS NOT NULL
    UNION ALL
    SELECT la.user_id, la.ip, la.created_at
    FROM login_activities la WHERE la.ip IS NOT NULL AND la.success = true
) t
GROUP BY user_id, ip;

-- ── account_summaries (materialized view, added in migration 004) ─────────────
CREATE MATERIALIZED VIEW account_summaries AS
SELECT
    accounts.id AS account_id,
    mode() WITHIN GROUP (ORDER BY t0.language)  AS language,
    mode() WITHIN GROUP (ORDER BY t0.sensitive) AS sensitive
FROM accounts
CROSS JOIN LATERAL (
    SELECT statuses.language, statuses.sensitive
    FROM statuses
    WHERE statuses.account_id = accounts.id
      AND statuses.deleted_at IS NULL
      AND statuses.reblog_of_id IS NULL
    ORDER BY statuses.id DESC
    LIMIT 20
) t0
WHERE accounts.suspended_at       IS NULL
  AND accounts.silenced_at         IS NULL
  AND accounts.moved_to_account_id IS NULL
  AND accounts.discoverable        = true
  AND accounts.locked              = false
GROUP BY accounts.id;

CREATE UNIQUE INDEX index_account_summaries_on_account_id ON account_summaries(account_id);
CREATE INDEX idx_on_account_id_language_sensitive_250461e1eb
    ON account_summaries(account_id, language, sensitive);

-- ── global_follow_recommendations (materialized view, added in migration 004) ─
CREATE MATERIALIZED VIEW global_follow_recommendations AS
SELECT account_id,
       sum(rank)         AS rank,
       array_agg(reason) AS reason
FROM (
    SELECT
        account_summaries.account_id,
        (count(follows.id)::numeric / (1.0 + count(follows.id)::numeric)) AS rank,
        'most_followed'::text AS reason
    FROM follows
    JOIN account_summaries ON account_summaries.account_id = follows.target_account_id
    JOIN users             ON users.account_id             = follows.account_id
    WHERE users.current_sign_in_at >= now() - INTERVAL 'P30D'
      AND account_summaries.sensitive = false
      AND NOT EXISTS (
          SELECT 1 FROM follow_recommendation_suppressions frs
          WHERE frs.account_id = follows.target_account_id
      )
    GROUP BY account_summaries.account_id
    HAVING count(follows.id) >= 5

    UNION ALL

    SELECT
        account_summaries.account_id,
        (sum(status_stats.reblogs_count + status_stats.favourites_count)
             / (1.0 + sum(status_stats.reblogs_count + status_stats.favourites_count))) AS rank,
        'most_interactions'::text AS reason
    FROM status_stats
    JOIN statuses          ON statuses.id              = status_stats.status_id
    JOIN account_summaries ON account_summaries.account_id = statuses.account_id
    WHERE statuses.id >= (date_part('epoch', now() - INTERVAL 'P30D') * 1000)::bigint << 16
      AND account_summaries.sensitive = false
      AND NOT EXISTS (
          SELECT 1 FROM follow_recommendation_suppressions frs
          WHERE frs.account_id = statuses.account_id
      )
    GROUP BY account_summaries.account_id
    HAVING sum(status_stats.reblogs_count + status_stats.favourites_count) >= 5
) t0
GROUP BY account_id
ORDER BY sum(rank) DESC;

CREATE UNIQUE INDEX index_global_follow_recommendations_on_account_id
    ON global_follow_recommendations(account_id);
