INSERT INTO accounts (
    id,
    username,
    private_key,
    public_key,
    actor_type,
    locked,
    created_at,
    updated_at
)
SELECT
    -99,
    domain,
    private_key,
    public_key,
    'Application',
    true,
    created_at,
    now()
FROM instance_actors
ON CONFLICT (id) DO UPDATE
SET username = EXCLUDED.username,
    private_key = EXCLUDED.private_key,
    public_key = EXCLUDED.public_key,
    actor_type = 'Application',
    locked = true,
    updated_at = now();

DROP TABLE instance_actors;
