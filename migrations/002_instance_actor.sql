-- Per-instance "instance actor" keypair, used to sign authorized-fetch GET
-- requests when dereferencing objects/actors from secure-mode servers.
-- The actor is served at https://{domain}/actor with keyId
-- https://{domain}/actor#main-key.
CREATE TABLE instance_actors (
    domain      TEXT PRIMARY KEY,
    private_key TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
