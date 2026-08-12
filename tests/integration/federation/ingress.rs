//! The durable ingress queue that carries inbound activities off the request
//! path.
//!
//! `TestContext` puts the inbox in sync mode so the rest of the suite can
//! assert on an activity right after POSTing it, which means the queued path
//! needs its own coverage. These tests drive the worker directly: enqueue a row
//! the way the handler would, drain one batch, and check both the activity's
//! effect and the job's bookkeeping.

use serde_json::json;

use crate::helpers::TestContext;

/// Insert a remote account so handlers resolve its actor without a network
/// fetch (the test domains are unreachable by design).
async fn seed_remote_actor(ctx: &TestContext, username: &str, domain: &str) -> String {
    let uri = format!("https://{domain}/users/{username}");
    sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, $2, $3, $2, '', $4::text, $4::text, 'remote-key',
                   $4::text||'/inbox', $4::text||'/outbox', now(), now())"#,
        eunha::snowflake::next_id(),
        username,
        domain,
        uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();
    uri
}

/// A queued activity is claimed, dispatched to its handler, and the job row is
/// removed once it succeeds.
#[tokio::test]
async fn test_queued_activity_is_processed_and_job_removed() {
    let ctx = TestContext::new("ingress-follow").await;
    let actor_uri = seed_remote_actor(&ctx, "eve", "ingress-remote.invalid").await;

    let follow = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://ingress-remote.invalid/activities/1",
        "type": "Follow",
        "actor": actor_uri,
        "object": format!("https://{}/users/alice", ctx.domain),
    });

    sqlx::query!(
        r#"INSERT INTO eunha.inbox_jobs (activity, activity_type, actor_uri, created_at, updated_at)
           VALUES ($1, 'Follow', $2, now(), now())"#,
        follow,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    let processed = eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .expect("draining the ingress queue must not error");
    assert_eq!(processed, 1, "the queued Follow must be claimed");

    let alice_id: i64 = ctx.alice_id.parse().unwrap();
    let follows: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM follows f
           JOIN accounts a ON a.id = f.account_id
           WHERE a.uri = $1 AND f.target_account_id = $2"#,
        actor_uri,
        alice_id,
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    assert_eq!(follows, 1, "the queued Follow must take effect");

    let remaining: i64 = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM eunha.inbox_jobs"#)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(remaining, 0, "a succeeded job must be deleted");
}

/// Draining an empty queue is a no-op, and a job scheduled for the future is
/// left alone rather than claimed early.
#[tokio::test]
async fn test_queue_respects_run_at() {
    let ctx = TestContext::new("ingress-runat").await;
    let actor_uri = seed_remote_actor(&ctx, "mallory", "ingress-later.invalid").await;

    assert_eq!(
        eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
            .await
            .unwrap(),
        0,
        "an empty queue must drain to nothing"
    );

    let follow = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://ingress-later.invalid/activities/1",
        "type": "Follow",
        "actor": actor_uri,
        "object": format!("https://{}/users/alice", ctx.domain),
    });
    sqlx::query!(
        r#"INSERT INTO eunha.inbox_jobs
             (activity, activity_type, actor_uri, run_at, created_at, updated_at)
           VALUES ($1, 'Follow', $2, now() + interval '1 hour', now(), now())"#,
        follow,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    assert_eq!(
        eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
            .await
            .unwrap(),
        0,
        "a backed-off job must not be claimed before its run_at"
    );

    let remaining: i64 = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM eunha.inbox_jobs"#)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(remaining, 1, "the future job must still be queued");
}
