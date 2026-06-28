-- Stop storing the signing private key in every delivery job row. Instead store
-- the signing account's id and resolve its key from `accounts` at send time, so
-- the secret lives in exactly one place.

ALTER TABLE eunha.activity_delivery_jobs
    ADD COLUMN actor_account_id BIGINT;

-- Pending jobs can't be signed once the key column is gone; drop them (they are
-- transient and will simply be re-federated by their authors as needed).
DELETE FROM eunha.activity_delivery_jobs
    WHERE delivered_at IS NULL AND failed_at IS NULL;

ALTER TABLE eunha.activity_delivery_jobs
    DROP COLUMN private_key_pem;
