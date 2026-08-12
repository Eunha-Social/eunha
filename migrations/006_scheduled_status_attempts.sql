-- Retry bookkeeping for the scheduled status publisher.
--
-- `public.scheduled_statuses` is part of the Mastodon schema mirror and must
-- keep its upstream shape, so the attempt counter lives beside it here, keyed
-- by the scheduled status id.
--
-- Previously a failed publish deleted the schedule outright "to avoid retrying
-- indefinitely", which turned any transient database error into silent loss of
-- a user's post. Now a publish that wrote nothing is retried with backoff, and
-- once the attempts are exhausted the row is marked `failed_at` and simply
-- stops being picked up — the schedule itself is never destroyed, so the post
-- is still visible to its author instead of vanishing.

CREATE TABLE eunha.scheduled_status_attempts (
    scheduled_status_id BIGINT PRIMARY KEY,
    attempts            INTEGER NOT NULL DEFAULT 0,
    run_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    failed_at           TIMESTAMPTZ,
    last_error          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX scheduled_status_attempts_retryable
    ON eunha.scheduled_status_attempts (run_at)
    WHERE failed_at IS NULL;
