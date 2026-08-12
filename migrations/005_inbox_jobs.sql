-- Durable queue for inbound ActivityPub activities (the "ingress" path).
--
-- The HTTP handler verifies the signature inline — that decides the response
-- code, so it cannot be deferred — then enqueues the activity and returns 202,
-- matching Mastodon's ActivityPub::ProcessingWorker on its `ingress` queue.
-- Handler work (DB writes, remote actor/status fetches, fan-out) then runs off
-- the request path, so a slow handler no longer holds a sending server's
-- connection open, and a burst from a shared inbox queues instead of pinning
-- request-handler tasks.
--
-- `actor_uri` is recorded for debugging and is already signature-verified by
-- the time a row exists; the worker re-reads everything else it needs from
-- `activity`.

CREATE TABLE eunha.inbox_jobs (
    id            BIGSERIAL PRIMARY KEY,
    activity      JSONB NOT NULL,
    activity_type TEXT NOT NULL,
    actor_uri     TEXT NOT NULL,
    attempts      INTEGER NOT NULL DEFAULT 0,
    -- Mastodon's ingress queue uses retry: 8.
    max_attempts  INTEGER NOT NULL DEFAULT 8,
    run_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at     TIMESTAMPTZ,
    locked_by     TEXT,
    last_error    TEXT,
    failed_at     TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX inbox_jobs_pending
    ON eunha.inbox_jobs (run_at, id)
    WHERE failed_at IS NULL;
