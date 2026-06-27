//! The per-instance "instance actor".
//!
//! A single Application actor served at `https://{domain}/actor`, used to sign
//! outbound authorized-fetch GET requests (and to be dereferenced by remote
//! servers verifying those signatures). Mastodon stores this actor as a
//! reserved `accounts` row with id `-99`; Eunha follows that shape so restored
//! Mastodon databases can keep their existing instance actor keypair.

use crate::state::AppState;

pub const INSTANCE_ACTOR_ID: i64 = -99;

/// The instance actor's id / URL for a domain.
pub fn actor_url(domain: &str) -> String {
    format!("https://{domain}/actor")
}

/// The instance actor's `keyId`.
pub fn key_id(domain: &str) -> String {
    format!("https://{domain}/actor#main-key")
}

/// Return the instance actor's (private_pem, public_pem), generating and
/// persisting a keypair on first use.
pub async fn get_or_create(state: &AppState) -> anyhow::Result<(String, String)> {
    let domain = &state.instance.domain;

    if let Some(row) = sqlx::query!(
        "SELECT private_key, public_key FROM accounts WHERE id = $1",
        INSTANCE_ACTOR_ID,
    )
    .fetch_optional(&state.db)
    .await?
    {
        if let Some(private_key) = row.private_key.filter(|s| !s.is_empty()) {
            if !row.public_key.is_empty() {
                return Ok((private_key, row.public_key));
            }
        }
    }

    let (private_pem, public_pem) = crate::crypto::generate_rsa_keypair()
        .map_err(|e| anyhow::anyhow!("instance actor keygen: {e}"))?;

    let row = sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, private_key, public_key, actor_type, locked, created_at, updated_at)
           VALUES ($1, $2, $3, $4, 'Application', true, now(), now())
           ON CONFLICT (id) DO UPDATE
             SET username = EXCLUDED.username,
                 private_key = CASE
                     WHEN accounts.private_key IS NULL OR accounts.private_key = ''
                     THEN EXCLUDED.private_key
                     ELSE accounts.private_key
                 END,
                 public_key = CASE
                     WHEN accounts.public_key = ''
                     THEN EXCLUDED.public_key
                     ELSE accounts.public_key
                 END,
                 actor_type = 'Application',
                 locked = true,
                 updated_at = now()
           RETURNING private_key, public_key"#,
        INSTANCE_ACTOR_ID,
        domain,
        private_pem,
        public_pem,
    )
    .fetch_one(&state.db)
    .await?;

    Ok((
        row.private_key
            .ok_or_else(|| anyhow::anyhow!("instance actor missing private key"))?,
        row.public_key,
    ))
}

/// Return just the instance actor's public key PEM (generating if needed).
pub async fn public_key(state: &AppState) -> anyhow::Result<String> {
    Ok(get_or_create(state).await?.1)
}
