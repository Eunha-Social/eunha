-- Durable queue for server-side video/gifv/audio transcoding. Lives in the
-- `eunha` schema (public stays a pure Mastodon mirror). The uploaded source is
-- stored in object storage at `source_key`; the worker downloads it, transcodes,
-- updates the media row, then deletes the source and this job. Survives restarts:
-- a stale lock is re-claimed, so an interrupted transcode resumes.

CREATE TABLE eunha.media_processing_jobs (
    id           BIGSERIAL PRIMARY KEY,
    media_id     BIGINT NOT NULL,
    media_type   TEXT NOT NULL,
    source_key   TEXT NOT NULL,
    content_type TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    run_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    locked_at    TIMESTAMPTZ,
    locked_by    TEXT,
    last_error   TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX media_processing_jobs_pending
    ON eunha.media_processing_jobs (run_at, id);
