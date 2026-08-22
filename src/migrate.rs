//! Applying and checking eunha's own migrations.
//!
//! Migrations are embedded in the binary, but running them is a separate act
//! from serving: a migration that rewrites a large table takes as long as it
//! takes, and doing that during startup means the instance is down for the
//! duration and a deploy's health check may kill the process midway through.
//! Some of them are destructive besides — 4.7's account merge deletes rows —
//! and that is not something to do as a side effect of a restart.
//!
//! So `eunha migrate` applies them and exits, and startup only *checks*: an
//! instance whose schema is behind its binary refuses to serve rather than
//! quietly running queries against a shape that no longer matches.

use anyhow::{Context, Result};
use sqlx::{migrate::Migrator, PgPool};

/// The migrations compiled into this binary.
pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Apply every migration that has not been applied yet.
pub async fn run(db: &PgPool) -> Result<()> {
    // sqlx keeps its ledger in whichever schema comes first on the search path,
    // which the pool sets to `eunha` so that `public` stays a pure mirror of
    // Mastodon's schema.
    sqlx::query("CREATE SCHEMA IF NOT EXISTS eunha")
        .execute(db)
        .await
        .context("creating the eunha schema")?;

    MIGRATOR.run(db).await.context("running migrations")?;
    Ok(())
}

/// What a database is missing relative to the binary asking.
#[derive(Debug)]
pub struct Pending {
    pub versions: Vec<i64>,
}

impl std::fmt::Display for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list = self
            .versions
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "{} migration(s) not applied: {list}",
            self.versions.len()
        )
    }
}

/// Which of this binary's migrations the database has not applied.
///
/// A database *ahead* of the binary is not reported: that is a rollback, where
/// the newer schema is generally still readable by the older code, and refusing
/// to start would turn a rollback into an outage.
pub async fn pending(db: &PgPool) -> Result<Option<Pending>> {
    // No ledger at all means nothing has ever been applied.
    let ledger_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM pg_class c
           JOIN pg_namespace n ON n.oid = c.relnamespace
           WHERE c.relname = '_sqlx_migrations' AND n.nspname = 'eunha'
         )",
    )
    .fetch_one(db)
    .await
    .context("looking for the migration ledger")?;

    if !ledger_exists {
        return Ok(Some(Pending {
            versions: MIGRATOR.iter().map(|m| m.version).collect(),
        }));
    }

    let applied: std::collections::HashSet<i64> =
        sqlx::query_scalar("SELECT version FROM eunha._sqlx_migrations WHERE success")
            .fetch_all(db)
            .await
            .context("reading the migration ledger")?
            .into_iter()
            .collect();

    let versions: Vec<i64> = MIGRATOR
        .iter()
        .map(|m| m.version)
        .filter(|version| !applied.contains(version))
        .collect();

    Ok((!versions.is_empty()).then_some(Pending { versions }))
}
