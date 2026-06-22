//! The per-instance "instance actor".
//!
//! A single Application actor served at `https://{domain}/actor`, used to sign
//! outbound authorized-fetch GET requests (and to be dereferenced by remote
//! servers verifying those signatures). Its keypair is generated lazily on
//! first use and persisted in the `instance_actors` table.

use crate::state::AppState;

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
        "SELECT private_key, public_key FROM instance_actors WHERE domain = $1",
        domain,
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok((row.private_key, row.public_key));
    }

    let (private_pem, public_pem) = crate::crypto::generate_rsa_keypair()
        .map_err(|e| anyhow::anyhow!("instance actor keygen: {e}"))?;

    // ON CONFLICT guards against a race between two concurrent first-uses;
    // RETURNING gives us whichever row won.
    let row = sqlx::query!(
        r#"INSERT INTO instance_actors (domain, private_key, public_key)
           VALUES ($1, $2, $3)
           ON CONFLICT (domain) DO UPDATE SET domain = EXCLUDED.domain
           RETURNING private_key, public_key"#,
        domain,
        private_pem,
        public_pem,
    )
    .fetch_one(&state.db)
    .await?;

    Ok((row.private_key, row.public_key))
}

/// Return just the instance actor's public key PEM (generating if needed).
pub async fn public_key(state: &AppState) -> anyhow::Result<String> {
    Ok(get_or_create(state).await?.1)
}
