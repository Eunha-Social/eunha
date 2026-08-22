//! Local accounts' signing keys.
//!
//! Mastodon 4.7.0 moved them out of `accounts.private_key` / `public_key` and
//! into the `keypairs` table, one row per key with a `local_fragment` naming
//! the key inside the actor document (`#main-key`), and the private half
//! encrypted at rest. The old columns stay readable — upstream's
//! `Keypair.from_legacy_account` still falls back to them — so a database can
//! be in either state, and eunha reads both.
//!
//! Writing the new form needs the encryption keys Mastodon requires
//! (`ACTIVE_RECORD_ENCRYPTION_PRIMARY_KEY` and `_KEY_DERIVATION_SALT`), because
//! a private key eunha wrote in the clear is one a Mastodon pointed at the same
//! database could not read. Without them configured, eunha keeps using the
//! legacy columns, which both implementations still understand.

use anyhow::{anyhow, Context, Result};

use crate::state::AppState;

/// The fragment Mastodon gives an account's main signing key.
pub const MAIN_KEY_FRAGMENT: &str = "#main-key";

/// `keypairs.type`, from the model's enum: `rsa: 0`.
const TYPE_RSA: i32 = 0;

/// A local account's signing key, whichever place it lives in.
pub struct SigningKey {
    pub private_key: String,
    pub public_key: String,
}

/// Read a local account's usable main key, preferring `keypairs`.
///
/// A revoked or expired keypair is skipped, matching the model's `usable`
/// scope, and the legacy columns answer for accounts whose key never moved.
pub async fn signing_key(state: &AppState, account_id: i64) -> Result<SigningKey> {
    let stored = sqlx::query!(
        r#"SELECT private_key, public_key
           FROM keypairs
           WHERE account_id = $1
             AND local_fragment = $2
             AND NOT revoked
             AND (expires_at IS NULL OR expires_at > now())
           ORDER BY id DESC
           LIMIT 1"#,
        account_id,
        MAIN_KEY_FRAGMENT,
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(row) = stored {
        let sealed = row
            .private_key
            .filter(|key| !key.is_empty())
            .ok_or_else(|| anyhow!("keypair for account {account_id} has no private key"))?;
        let encryptor = state.encryptor.as_ref().ok_or_else(|| {
            anyhow!(
                "account {account_id} keeps its signing key in `keypairs`, which is encrypted, \
                 but no ActiveRecord encryption keys are configured"
            )
        })?;
        return Ok(SigningKey {
            private_key: encryptor
                .decrypt(&sealed)
                .with_context(|| format!("decrypting the signing key for account {account_id}"))?,
            public_key: row.public_key,
        });
    }

    let legacy = sqlx::query!(
        "SELECT private_key, public_key FROM accounts WHERE id = $1",
        account_id,
    )
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| anyhow!("account {account_id} does not exist"))?;

    let private_key = legacy
        .private_key
        .filter(|key| !key.is_empty())
        .ok_or_else(|| anyhow!("no signing key for account {account_id}"))?;

    Ok(SigningKey {
        private_key,
        public_key: legacy.public_key,
    })
}

/// Whether a local account can sign, without decrypting anything.
///
/// The write paths only need to know that a key exists before they build an
/// activity for the delivery queue, which loads the key itself.
pub async fn has_signing_key(state: &AppState, account_id: i64) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM keypairs
             WHERE account_id = $1
               AND local_fragment = $2
               AND private_key IS NOT NULL AND private_key <> ''
               AND NOT revoked
               AND (expires_at IS NULL OR expires_at > now())
             UNION ALL
             SELECT 1 FROM accounts
             WHERE id = $1 AND private_key IS NOT NULL AND private_key <> ''
           ) AS "exists!""#,
        account_id,
        MAIN_KEY_FRAGMENT,
    )
    .fetch_one(&state.db)
    .await?;
    Ok(exists)
}

/// The public half of a local account's main key, for its actor document.
pub async fn public_key(state: &AppState, account_id: i64) -> Result<Option<String>> {
    let stored = sqlx::query_scalar!(
        r#"SELECT public_key
           FROM keypairs
           WHERE account_id = $1
             AND local_fragment = $2
             AND NOT revoked
             AND (expires_at IS NULL OR expires_at > now())
           ORDER BY id DESC
           LIMIT 1"#,
        account_id,
        MAIN_KEY_FRAGMENT,
    )
    .fetch_optional(&state.db)
    .await?;

    if let Some(key) = stored.filter(|key| !key.is_empty()) {
        return Ok(Some(key));
    }

    Ok(
        sqlx::query_scalar!("SELECT public_key FROM accounts WHERE id = $1", account_id,)
            .fetch_optional(&state.db)
            .await?
            .filter(|key| !key.is_empty()),
    )
}

/// Record a newly generated key for a local account, and return the key that
/// ends up in force.
///
/// Goes into `keypairs` when the private half can be encrypted, and into the
/// legacy columns otherwise, so that whichever form the rest of the database is
/// in stays readable by both implementations.
///
/// An account that already has a key keeps it: two processes starting at once
/// both generate one, and the actor document can only advertise a single public
/// half, so the first key written is the one everyone must agree on.
pub async fn store_local(
    state: &AppState,
    account_id: i64,
    private_key: &str,
    public_key: &str,
) -> Result<SigningKey> {
    let Some(encryptor) = state.encryptor.as_ref() else {
        let row = sqlx::query!(
            r#"UPDATE accounts
               SET private_key = COALESCE(NULLIF(private_key, ''), $2),
                   public_key = CASE WHEN public_key = '' THEN $3 ELSE public_key END,
                   updated_at = now()
               WHERE id = $1
               RETURNING private_key, public_key"#,
            account_id,
            private_key,
            public_key,
        )
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| anyhow!("account {account_id} does not exist"))?;

        return Ok(SigningKey {
            private_key: row
                .private_key
                .filter(|key| !key.is_empty())
                .ok_or_else(|| anyhow!("no signing key for account {account_id}"))?,
            public_key: row.public_key,
        });
    };

    let sealed = encryptor.encrypt(private_key)?;
    sqlx::query!(
        r#"INSERT INTO keypairs
             (account_id, type, local_fragment, public_key, private_key, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, now(), now())
           ON CONFLICT (account_id, local_fragment) DO NOTHING"#,
        account_id,
        TYPE_RSA,
        MAIN_KEY_FRAGMENT,
        public_key,
        sealed,
    )
    .execute(&state.db)
    .await?;

    // The secret now lives in `keypairs`; the legacy columns must not keep a
    // second copy of it, in the clear, for something else to pick up.
    sqlx::query!(
        "UPDATE accounts SET private_key = NULL, public_key = '', updated_at = now() WHERE id = $1",
        account_id,
    )
    .execute(&state.db)
    .await?;

    signing_key(state, account_id).await
}

/// Mastodon's `20260702144128_migrate_local_account_keypairs`.
///
/// Moves every local account's key out of `accounts` and into `keypairs`, then
/// records the migration as run so that a Mastodon booted on this database does
/// not try to move keys that have already moved. Does nothing without
/// encryption keys configured: recording it while the keys were still sitting
/// in the old columns would be a lie, and moving them in the clear would leave
/// rows Mastodon refuses to read.
pub async fn migrate_local_keypairs(state: &AppState) -> Result<usize> {
    let Some(encryptor) = state.encryptor.as_ref() else {
        // Loud only if the move has already happened elsewhere, since then the
        // keys eunha needs are encrypted and it cannot sign for anyone.
        let moved: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM keypairs WHERE local_fragment IS NOT NULL) AS "exists!""#,
        )
        .fetch_one(&state.db)
        .await?;
        if moved {
            tracing::error!(
                "local signing keys are stored encrypted in `keypairs`, but no ActiveRecord \
                 encryption keys are configured; this instance cannot sign anything it sends"
            );
        }
        return Ok(0);
    };

    let pending = sqlx::query!(
        r#"SELECT id, private_key, public_key
           FROM accounts
           WHERE domain IS NULL
             AND private_key IS NOT NULL
             AND private_key <> ''"#,
    )
    .fetch_all(&state.db)
    .await?;

    let mut moved = 0;
    for account in pending {
        let Some(private_key) = account.private_key.filter(|key| !key.is_empty()) else {
            continue;
        };
        let sealed = encryptor
            .encrypt(&private_key)
            .with_context(|| format!("encrypting the signing key for account {}", account.id))?;

        // An account that already has a keypair keeps it: the row in `keypairs`
        // is the newer of the two, and the legacy column is what is stale.
        let mut tx = state.db.begin().await?;
        sqlx::query!(
            r#"INSERT INTO keypairs
                 (account_id, type, local_fragment, public_key, private_key, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, now(), now())
               ON CONFLICT (account_id, local_fragment) DO NOTHING"#,
            account.id,
            TYPE_RSA,
            MAIN_KEY_FRAGMENT,
            account.public_key,
            sealed,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            "UPDATE accounts SET public_key = '', private_key = NULL, updated_at = now() WHERE id = $1",
            account.id,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        moved += 1;
    }

    if moved > 0 {
        tracing::info!(moved, "moved local signing keys into `keypairs`");
    }

    // Only claim the migration once nothing is left in the old columns.
    sqlx::query!(
        r#"INSERT INTO public.schema_migrations (version)
           SELECT '20260702144128'
           WHERE NOT EXISTS (
             SELECT 1 FROM accounts
             WHERE domain IS NULL AND private_key IS NOT NULL AND private_key <> ''
           )
           ON CONFLICT (version) DO NOTHING"#,
    )
    .execute(&state.db)
    .await?;

    Ok(moved)
}
