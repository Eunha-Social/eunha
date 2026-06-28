use redis::{aio::ConnectionManager, AsyncCommands};
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};

const FEED_MAX_ITEMS: isize = 800;
const FEED_TTL_SECS: u64 = 7 * 24 * 3600; // 1 week

// When true, fanout/populate/backfill run inline (no tokio::spawn).
// Set by integration tests to eliminate timing races.
static SYNC_FANOUT: AtomicBool = AtomicBool::new(false);

pub fn enable_sync_fanout() {
    SYNC_FANOUT.store(true, Ordering::Relaxed);
}

pub fn sync_fanout() -> bool {
    SYNC_FANOUT.load(Ordering::Relaxed)
}

fn feed_key(account_id: i64) -> String {
    format!("feed:home:{}", account_id)
}

fn populated_key(account_id: i64) -> String {
    format!("feed:home:{}:populated", account_id)
}

pub async fn is_feed_populated(
    redis: &mut ConnectionManager,
    account_id: i64,
) -> bool {
    redis
        .exists::<_, bool>(populated_key(account_id))
        .await
        .unwrap_or(false)
}

pub async fn feed_push(
    redis: &mut ConnectionManager,
    account_id: i64,
    status_id: i64,
) {
    let key = feed_key(account_id);
    let result: redis::RedisResult<()> = redis::pipe()
        .zadd(&key, status_id, status_id as f64)
        .zremrangebyrank(&key, 0, -(FEED_MAX_ITEMS + 1))
        .ignore()
        .query_async(redis)
        .await;
    if let Err(e) = result {
        tracing::warn!("feed_push error for account {}: {}", account_id, e);
    }
}

pub async fn feed_remove(
    redis: &mut ConnectionManager,
    account_id: i64,
    status_id: i64,
) {
    let result: redis::RedisResult<()> = redis
        .zrem(feed_key(account_id), status_id)
        .await;
    if let Err(e) = result {
        tracing::warn!("feed_remove error for account {}: {}", account_id, e);
    }
}

/// Fetch status IDs from the Redis feed, honouring Mastodon-style pagination.
/// Returns None if the feed has never been populated (cold start signal).
pub async fn feed_get(
    redis: &mut ConnectionManager,
    account_id: i64,
    max_id: Option<i64>,
    since_id: Option<i64>,
    min_id: Option<i64>,
    limit: isize,
) -> Option<Vec<i64>> {
    if !is_feed_populated(redis, account_id).await {
        return None;
    }

    let key = feed_key(account_id);

    let ids: Vec<i64> = if let Some(min_id) = min_id {
        let min_score = format!("({min_id}");
        redis::cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg(&min_score)
            .arg("+inf")
            .arg("LIMIT")
            .arg(0i64)
            .arg(limit)
            .query_async(redis)
            .await
            .unwrap_or_default()
    } else {
        let max_score = max_id
            .map(|id| format!("({}", id))
            .unwrap_or_else(|| "+inf".to_string());
        let min_score = since_id
            .map(|id| format!("({}", id))
            .unwrap_or_else(|| "-inf".to_string());
        redis::cmd("ZREVRANGEBYSCORE")
            .arg(&key)
            .arg(&max_score)
            .arg(&min_score)
            .arg("LIMIT")
            .arg(0i64)
            .arg(limit)
            .query_async(redis)
            .await
            .unwrap_or_default()
    };

    Some(ids)
}

/// Populate the Redis feed from DB (called on first timeline load).
pub async fn feed_populate(
    redis: &mut ConnectionManager,
    account_id: i64,
    db: &PgPool,
) {
    let _: redis::RedisResult<()> = redis
        .set_ex(populated_key(account_id), 1i64, FEED_TTL_SECS)
        .await;

    let status_ids: Vec<i64> = sqlx::query_scalar!(
        r#"WITH candidate_ids AS (
               SELECT s.id FROM statuses s
               WHERE s.account_id IN (
                   SELECT target_account_id FROM follows
                   WHERE account_id = $1
                   UNION ALL SELECT $1
               )
               AND s.deleted_at IS NULL
               UNION
               SELECT st.status_id FROM statuses_tags st
               JOIN tag_follows tf ON tf.tag_id = st.tag_id
               JOIN statuses s ON s.id = st.status_id
               WHERE tf.account_id = $1
               AND s.visibility = 0
               AND s.deleted_at IS NULL
           )
           SELECT id FROM candidate_ids ORDER BY id DESC LIMIT $2"#,
        account_id,
        FEED_MAX_ITEMS as i64,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .flatten()
    .collect();

    if !status_ids.is_empty() {
        let key = feed_key(account_id);
        let mut pipe = redis::pipe();
        for &id in &status_ids {
            pipe.zadd(&key, id, id as f64);
        }
        pipe.expire(&key, FEED_TTL_SECS as i64);
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

/// Fan-out a newly posted status to all followers' initialized feeds,
/// plus accounts following any of the status's hashtags.
pub async fn fanout_new_status(
    redis: &mut ConnectionManager,
    db: &PgPool,
    author_id: i64,
    status_id: i64,
    tag_ids: &[i64],
) {
    let follower_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM follows WHERE target_account_id = $1",
        author_id,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let hashtag_recipients: Vec<i64> = if !tag_ids.is_empty() {
        sqlx::query_scalar!(
            r#"SELECT DISTINCT tf.account_id FROM tag_follows tf
               WHERE tf.tag_id = ANY($1::bigint[])
               AND tf.account_id != $2
               AND NOT EXISTS (
                   SELECT 1 FROM follows
                   WHERE account_id = tf.account_id
                   AND target_account_id = $2
               )"#,
            tag_ids as &[i64],
            author_id,
        )
        .fetch_all(db)
        .await
        .unwrap_or_default()
    } else {
        vec![]
    };

    let recipients: Vec<i64> = std::iter::once(author_id)
        .chain(follower_ids)
        .chain(hashtag_recipients)
        .collect();

    if recipients.is_empty() {
        return;
    }

    let pop_keys: Vec<String> = recipients
        .iter()
        .map(|&id| populated_key(id))
        .collect();
    let initialized: Vec<Option<i64>> = match redis.mget(&pop_keys).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("fanout mget error: {}", e);
            return;
        }
    };

    let score = status_id as f64;
    let mut pipe = redis::pipe();
    let mut any = false;
    for (&id, init) in recipients.iter().zip(initialized.iter()) {
        if init.is_some() {
            let key = feed_key(id);
            pipe.zadd(&key, status_id, score);
            pipe.zremrangebyrank(&key, 0, -(FEED_MAX_ITEMS + 1));
            any = true;
        }
    }
    if any {
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

/// Remove a deleted status from all followers' initialized feeds.
pub async fn fanout_remove_status(
    redis: &mut ConnectionManager,
    db: &PgPool,
    author_id: i64,
    status_id: i64,
) {
    let follower_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT account_id FROM follows WHERE target_account_id = $1",
        author_id,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let recipients: Vec<i64> = std::iter::once(author_id).chain(follower_ids).collect();
    let pop_keys: Vec<String> = recipients
        .iter()
        .map(|&id| populated_key(id))
        .collect();
    let initialized: Vec<Option<i64>> = match redis.mget(&pop_keys).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("fanout_remove mget error: {}", e);
            return;
        }
    };

    let mut pipe = redis::pipe();
    let mut any = false;
    for (&id, init) in recipients.iter().zip(initialized.iter()) {
        if init.is_some() {
            pipe.zrem(feed_key(id), status_id);
            any = true;
        }
    }
    if any {
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

// ── List feed ─────────────────────────────────────────────────────────────

fn list_feed_key(list_id: i64) -> String {
    format!("feed:list:{}", list_id)
}

fn list_populated_key(list_id: i64) -> String {
    format!("feed:list:{}:populated", list_id)
}

pub async fn is_list_feed_populated(
    redis: &mut ConnectionManager,
    list_id: i64,
) -> bool {
    redis
        .exists::<_, bool>(list_populated_key(list_id))
        .await
        .unwrap_or(false)
}

/// Fetch status IDs from a list's Redis feed.
pub async fn list_feed_get(
    redis: &mut ConnectionManager,
    list_id: i64,
    max_id: Option<i64>,
    since_id: Option<i64>,
    min_id: Option<i64>,
    limit: isize,
) -> Option<Vec<i64>> {
    if !is_list_feed_populated(redis, list_id).await {
        return None;
    }
    let key = list_feed_key(list_id);
    let ids: Vec<i64> = if let Some(min_id) = min_id {
        let min_score = format!("({min_id}");
        redis::cmd("ZRANGEBYSCORE")
            .arg(&key)
            .arg(&min_score)
            .arg("+inf")
            .arg("LIMIT")
            .arg(0i64)
            .arg(limit)
            .query_async(redis)
            .await
            .unwrap_or_default()
    } else {
        let max_score = max_id
            .map(|id| format!("({}", id))
            .unwrap_or_else(|| "+inf".to_string());
        let min_score = since_id
            .map(|id| format!("({}", id))
            .unwrap_or_else(|| "-inf".to_string());
        redis::cmd("ZREVRANGEBYSCORE")
            .arg(&key)
            .arg(&max_score)
            .arg(&min_score)
            .arg("LIMIT")
            .arg(0i64)
            .arg(limit)
            .query_async(redis)
            .await
            .unwrap_or_default()
    };
    Some(ids)
}

/// Populate a list's Redis feed from DB (called on first list timeline access).
pub async fn list_feed_populate(
    redis: &mut ConnectionManager,
    list_id: i64,
    owner_id: i64,
    replies_policy: &str,
    db: &PgPool,
) {
    let _: redis::RedisResult<()> = redis
        .set_ex(list_populated_key(list_id), 1i64, FEED_TTL_SECS)
        .await;

    let status_ids: Vec<i64> = match replies_policy {
        "none" => sqlx::query_scalar!(
            r#"SELECT s.id FROM statuses s
               JOIN list_accounts la ON la.account_id = s.account_id
               WHERE la.list_id = $1
                 AND s.deleted_at IS NULL
                 AND s.visibility != 3
                 AND (s.in_reply_to_id IS NULL
                      OR s.in_reply_to_account_id = s.account_id
                      OR s.in_reply_to_account_id = $2)
               ORDER BY s.id DESC LIMIT $3"#,
            list_id, owner_id, FEED_MAX_ITEMS as i64,
        )
        .fetch_all(db).await.unwrap_or_default(),
        "list" => sqlx::query_scalar!(
            r#"SELECT s.id FROM statuses s
               JOIN list_accounts la ON la.account_id = s.account_id
               WHERE la.list_id = $1
                 AND s.deleted_at IS NULL
                 AND s.visibility != 3
                 AND (s.in_reply_to_id IS NULL
                      OR s.in_reply_to_account_id = $2
                      OR EXISTS (
                          SELECT 1 FROM statuses s2
                          JOIN list_accounts la2 ON la2.account_id = s2.account_id
                          WHERE s2.id = s.in_reply_to_id AND la2.list_id = $1
                      ))
               ORDER BY s.id DESC LIMIT $3"#,
            list_id, owner_id, FEED_MAX_ITEMS as i64,
        )
        .fetch_all(db).await.unwrap_or_default(),
        _ => sqlx::query_scalar!(
            r#"SELECT s.id FROM statuses s
               JOIN list_accounts la ON la.account_id = s.account_id
               WHERE la.list_id = $1
                 AND s.deleted_at IS NULL
                 AND s.visibility != 3
                 AND (s.in_reply_to_id IS NULL OR EXISTS (
                     SELECT 1 FROM statuses s2
                     WHERE s2.id = s.in_reply_to_id
                       AND (s2.account_id = $2 OR EXISTS (
                           SELECT 1 FROM follows f
                           WHERE f.account_id = $2 AND f.target_account_id = s2.account_id
                       ))
                 ))
               ORDER BY s.id DESC LIMIT $3"#,
            list_id, owner_id, FEED_MAX_ITEMS as i64,
        )
        .fetch_all(db).await.unwrap_or_default(),
    };

    if !status_ids.is_empty() {
        let key = list_feed_key(list_id);
        let mut pipe = redis::pipe();
        for &id in &status_ids {
            pipe.zadd(&key, id, id as f64);
        }
        pipe.expire(&key, FEED_TTL_SECS as i64);
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

/// Fan out a newly posted status to all initialized list feeds that contain the author.
pub async fn fanout_to_lists(
    redis: &mut ConnectionManager,
    db: &PgPool,
    author_id: i64,
    status_id: i64,
    in_reply_to_account_id: Option<i64>,
    visibility: &str,
) {
    if visibility == "direct" {
        return;
    }

    let lists = sqlx::query!(
        r#"SELECT l.id, l.account_id,
                  CASE l.replies_policy WHEN 0 THEN 'followed' WHEN 1 THEN 'list' WHEN 2 THEN 'none' ELSE 'list' END AS "replies_policy!"
           FROM lists l
           JOIN list_accounts la ON la.list_id = l.id
           WHERE la.account_id = $1"#,
        author_id,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if lists.is_empty() {
        return;
    }

    let pop_keys: Vec<String> = lists.iter().map(|l| list_populated_key(l.id)).collect();
    let initialized: Vec<Option<i64>> = match redis.mget(&pop_keys).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("fanout_to_lists mget error: {}", e);
            return;
        }
    };

    let score = status_id as f64;
    let mut pipe = redis::pipe();
    let mut any = false;

    for (list, init) in lists.iter().zip(initialized.iter()) {
        if init.is_none() {
            continue;
        }

        let passes = if let Some(reply_author) = in_reply_to_account_id {
            if reply_author == author_id || reply_author == list.account_id {
                true
            } else {
                match list.replies_policy.as_str() {
                    "none" => false,
                    "list" => sqlx::query_scalar!(
                        "SELECT 1 FROM list_accounts WHERE list_id = $1 AND account_id = $2",
                        list.id, reply_author,
                    )
                    .fetch_optional(db).await.unwrap_or(None).is_some(),
                    _ => sqlx::query_scalar!(
                        "SELECT 1 FROM follows WHERE account_id = $1 AND target_account_id = $2",
                        list.account_id, reply_author,
                    )
                    .fetch_optional(db).await.unwrap_or(None).is_some(),
                }
            }
        } else {
            true
        };

        if passes {
            let key = list_feed_key(list.id);
            pipe.zadd(&key, status_id, score);
            pipe.zremrangebyrank(&key, 0, -(FEED_MAX_ITEMS + 1));
            any = true;
        }
    }

    if any {
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

/// Remove a deleted status from all initialized list feeds that contain the author.
pub async fn fanout_remove_from_lists(
    redis: &mut ConnectionManager,
    db: &PgPool,
    author_id: i64,
    status_id: i64,
) {
    let list_ids: Vec<i64> = sqlx::query_scalar!(
        "SELECT l.id FROM lists l JOIN list_accounts la ON la.list_id = l.id WHERE la.account_id = $1",
        author_id,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if list_ids.is_empty() {
        return;
    }

    let pop_keys: Vec<String> = list_ids.iter().map(|&id| list_populated_key(id)).collect();
    let initialized: Vec<Option<i64>> = match redis.mget(&pop_keys).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("fanout_remove_from_lists mget error: {}", e);
            return;
        }
    };

    let mut pipe = redis::pipe();
    let mut any = false;
    for (&list_id, init) in list_ids.iter().zip(initialized.iter()) {
        if init.is_some() {
            pipe.zrem(list_feed_key(list_id), status_id);
            any = true;
        }
    }
    if any {
        let _: redis::RedisResult<()> = pipe.query_async(redis).await;
    }
}

/// Backfill a list feed with recent statuses from a newly-added member.
pub async fn backfill_list_member(
    redis: &mut ConnectionManager,
    db: &PgPool,
    list_id: i64,
    member_id: i64,
    owner_id: i64,
    replies_policy: &str,
) {
    if !is_list_feed_populated(redis, list_id).await {
        return;
    }

    let recent: Vec<i64> = match replies_policy {
        "none" => sqlx::query_scalar!(
            r#"SELECT id FROM statuses
               WHERE account_id = $1 AND deleted_at IS NULL AND visibility != 3
                 AND (in_reply_to_id IS NULL
                      OR in_reply_to_account_id = $1
                      OR in_reply_to_account_id = $2)
               ORDER BY id DESC LIMIT 20"#,
            member_id, owner_id,
        )
        .fetch_all(db).await.unwrap_or_default(),
        "list" => sqlx::query_scalar!(
            r#"SELECT s.id FROM statuses s
               WHERE s.account_id = $1 AND s.deleted_at IS NULL AND s.visibility != 3
                 AND (s.in_reply_to_id IS NULL
                      OR s.in_reply_to_account_id = $3
                      OR EXISTS (
                          SELECT 1 FROM statuses s2
                          JOIN list_accounts la ON la.account_id = s2.account_id
                          WHERE s2.id = s.in_reply_to_id AND la.list_id = $2
                      ))
               ORDER BY s.id DESC LIMIT 20"#,
            member_id, list_id, owner_id,
        )
        .fetch_all(db).await.unwrap_or_default(),
        _ => sqlx::query_scalar!(
            r#"SELECT s.id FROM statuses s
               WHERE s.account_id = $1 AND s.deleted_at IS NULL AND s.visibility != 3
                 AND (s.in_reply_to_id IS NULL OR EXISTS (
                     SELECT 1 FROM statuses s2
                     WHERE s2.id = s.in_reply_to_id
                       AND (s2.account_id = $2 OR EXISTS (
                           SELECT 1 FROM follows f
                           WHERE f.account_id = $2 AND f.target_account_id = s2.account_id
                       ))
                 ))
               ORDER BY s.id DESC LIMIT 20"#,
            member_id, owner_id,
        )
        .fetch_all(db).await.unwrap_or_default(),
    };

    if recent.is_empty() {
        return;
    }

    let key = list_feed_key(list_id);
    let mut pipe = redis::pipe();
    for &id in &recent {
        pipe.zadd(&key, id, id as f64);
    }
    pipe.zremrangebyrank(&key, 0, -(FEED_MAX_ITEMS + 1));
    let _: redis::RedisResult<()> = pipe.query_async(redis).await;
}

/// Delete a list's Redis feed keys (called when the list itself is deleted).
pub async fn delete_list_feed(redis: &mut ConnectionManager, list_id: i64) {
    let _: redis::RedisResult<()> = redis::pipe()
        .del(list_feed_key(list_id))
        .del(list_populated_key(list_id))
        .query_async(redis)
        .await;
}

/// Backfill the follower's feed with recent statuses from the newly-followed account.
pub async fn backfill_follow(
    redis: &mut ConnectionManager,
    db: &PgPool,
    follower_id: i64,
    followed_id: i64,
) {
    if !is_feed_populated(redis, follower_id).await {
        return;
    }

    let recent: Vec<i64> = sqlx::query_scalar!(
        "SELECT id FROM statuses WHERE account_id = $1 AND deleted_at IS NULL ORDER BY id DESC LIMIT 20",
        followed_id,
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();

    if recent.is_empty() {
        return;
    }

    let key = feed_key(follower_id);
    let mut pipe = redis::pipe();
    for &id in &recent {
        pipe.zadd(&key, id, id as f64);
    }
    pipe.zremrangebyrank(&key, 0, -(FEED_MAX_ITEMS + 1));
    let _: redis::RedisResult<()> = pipe.query_async(redis).await;
}
