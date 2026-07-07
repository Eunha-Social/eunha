//! Denormalized account counter maintenance (`account_stats`).
//!
//! Mirrors Mastodon's `Account::Counters`: the follower/following totals are
//! kept as running counts that are bumped on follow create/remove, guarded so
//! decrements never fall below zero, and reconcilable from the source-of-truth
//! `follows` table when they drift (Mastodon's `Account#refresh_counts`, run by
//! `tootctl accounts refresh`).
//!
//! `follower` is the account doing the following; `target` is the account being
//! followed. A follow bumps `target.followers_count` and `follower.following_count`.

use sqlx::PgPool;

/// Record a new follow edge: increment the target's `followers_count` and the
/// follower's `following_count`, creating the `account_stats` row if absent.
/// Call after the `follows` row is inserted.
pub async fn on_follow_created(db: &PgPool, follower: i64, target: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT INTO account_stats (account_id, followers_count, created_at, updated_at)
         VALUES ($1, 1, now(), now())
         ON CONFLICT (account_id) DO UPDATE
           SET followers_count = account_stats.followers_count + 1, updated_at = now()",
        target,
    )
    .execute(db)
    .await?;
    sqlx::query!(
        "INSERT INTO account_stats (account_id, following_count, created_at, updated_at)
         VALUES ($1, 1, now(), now())
         ON CONFLICT (account_id) DO UPDATE
           SET following_count = account_stats.following_count + 1, updated_at = now()",
        follower,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Reverse a removed follow edge: decrement the target's `followers_count` and
/// the follower's `following_count`, floored at 0. Call only when a `follows`
/// row was actually deleted, so idempotent unfollows don't over-decrement.
pub async fn on_follow_removed(db: &PgPool, follower: i64, target: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE account_stats SET followers_count = GREATEST(followers_count - 1, 0), updated_at = now()
         WHERE account_id = $1",
        target,
    )
    .execute(db)
    .await?;
    sqlx::query!(
        "UPDATE account_stats SET following_count = GREATEST(following_count - 1, 0), updated_at = now()
         WHERE account_id = $1",
        follower,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Recompute a **local** account's follow counters from the `follows` table,
/// matching Mastodon's `Account#refresh_counts`. Reconciles rows that have
/// drifted from the source of truth (including any left negative by legacy code
/// paths).
///
/// Local-only by design: for remote actors the `follows` table holds only the
/// edges involving local accounts, while `account_stats` mirrors the remote
/// instance's true totals (seeded at import). Recounting a remote account from
/// our partial view would clobber those totals, so this no-ops for them.
pub async fn recount_follows(db: &PgPool, account_id: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE account_stats s
         SET followers_count = (SELECT COUNT(*) FROM follows WHERE target_account_id = s.account_id),
             following_count = (SELECT COUNT(*) FROM follows WHERE account_id = s.account_id),
             updated_at = now()
         FROM accounts a
         WHERE a.id = s.account_id AND a.domain IS NULL AND s.account_id = $1",
        account_id,
    )
    .execute(db)
    .await?;
    Ok(())
}
