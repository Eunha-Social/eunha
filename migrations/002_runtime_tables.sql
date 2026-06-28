CREATE SCHEMA IF NOT EXISTS eunha;

CREATE TABLE eunha.activity_delivery_jobs (
    id               BIGSERIAL PRIMARY KEY,
    activity         JSONB NOT NULL,
    inbox_url        TEXT NOT NULL,
    key_id           TEXT NOT NULL,
    -- Signing account; its private key is loaded from `accounts` at send time
    -- rather than copied into every job row.
    actor_account_id BIGINT,
    attempts         INTEGER NOT NULL DEFAULT 0,
    max_attempts    INTEGER NOT NULL DEFAULT 12,
    run_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at       TIMESTAMPTZ,
    locked_by       TEXT,
    last_error      TEXT,
    delivered_at    TIMESTAMPTZ,
    failed_at       TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX activity_delivery_jobs_pending
    ON eunha.activity_delivery_jobs (run_at, id)
    WHERE delivered_at IS NULL AND failed_at IS NULL;

CREATE INDEX activity_delivery_jobs_inbox_pending
    ON eunha.activity_delivery_jobs (inbox_url)
    WHERE delivered_at IS NULL AND failed_at IS NULL;

CREATE TABLE eunha.pending_signups (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username           TEXT NOT NULL,
    email              TEXT NOT NULL,
    email_normalized   TEXT NOT NULL UNIQUE,
    password_hash      TEXT NOT NULL,
    invite_id          BIGINT,
    reason             TEXT,
    locale             TEXT NOT NULL DEFAULT 'en',
    app_id             BIGINT,
    confirmation_token TEXT NOT NULL UNIQUE,
    expires_at         TIMESTAMPTZ NOT NULL DEFAULT now() + interval '24 hours',
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE eunha.instance_user_sessions (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    BIGINT NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
    token      TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX instance_user_sessions_token_idx
    ON eunha.instance_user_sessions(token);
