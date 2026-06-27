CREATE TABLE activity_delivery_jobs (
    id              BIGSERIAL PRIMARY KEY,
    activity        JSONB NOT NULL,
    inbox_url       TEXT NOT NULL,
    key_id          TEXT NOT NULL,
    private_key_pem TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
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
    ON activity_delivery_jobs (run_at, id)
    WHERE delivered_at IS NULL AND failed_at IS NULL;

CREATE INDEX activity_delivery_jobs_inbox_pending
    ON activity_delivery_jobs (inbox_url)
    WHERE delivered_at IS NULL AND failed_at IS NULL;
