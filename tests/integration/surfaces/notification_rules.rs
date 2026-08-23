//! When Mastodon does not notify, and whether eunha agrees.
//!
//! `NotifyService::DropCondition#drop?` is a list of reasons to say nothing.
//! The ones tested here are the unconditional ones — not the account's
//! notification policy, which is a preference, but the reasons that hold
//! regardless: a block, a mute, a domain block. Getting one wrong means a
//! notification from someone the recipient has explicitly shut out.

use crate::helpers::TestContext;

/// Seed a remote account and return its id and actor uri.
async fn remote_actor(ctx: &TestContext, username: &str, domain: &str) -> (i64, String) {
    let uri = format!("https://{domain}/users/{username}");
    let id = eunha::snowflake::next_id();
    sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, $2, $3, $2, '', $4::text, $4::text, 'remote-key',
                   $4::text||'/inbox', $4::text||'/outbox', now(), now())"#,
        id,
        username,
        domain,
        uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();
    (id, uri)
}

async fn notification_count(ctx: &TestContext, account_id: i64) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM notifications WHERE account_id = $1"#,
        account_id
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap()
}

/// An account on a domain the recipient has blocked cannot notify them.
///
/// `domain_blocking?` is `@recipient.domain_blocking?(@sender.domain) &&
/// not_following?`: blocking a domain is a statement about wanting nothing from
/// it, and a mention arriving as a notification is the thing being asked to
/// stop. Following someone there is the exception, since that is a deliberate
/// choice to keep hearing from them.
#[tokio::test]
async fn test_a_domain_block_stops_notifications() {
    let ctx = TestContext::new("notify-domain-block").await;
    let alice_id: i64 = ctx.alice_id.parse().unwrap();

    let domain = "notify-blocked.invalid";
    let (_sender_id, actor_uri) = remote_actor(&ctx, "stranger", domain).await;

    sqlx::query!(
        r#"INSERT INTO account_domain_blocks (account_id, domain, created_at, updated_at)
           VALUES ($1, $2, now(), now())"#,
        alice_id,
        domain,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    let before = notification_count(&ctx, alice_id).await;

    let alice_uri = format!("https://{}/users/alice", ctx.domain);
    let create = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("https://{domain}/activities/mention-1"),
        "type": "Create",
        "actor": actor_uri,
        "to": [alice_uri],
        "object": {
            "id": format!("https://{domain}/notes/mention-1"),
            "type": "Note",
            "attributedTo": actor_uri,
            "content": "<p>hello there</p>",
            "to": [alice_uri],
            "tag": [{"type": "Mention", "href": alice_uri, "name": "@alice"}],
            "published": "2026-01-01T00:00:00Z",
        },
    });
    sqlx::query!(
        r#"INSERT INTO eunha.inbox_jobs (activity, activity_type, actor_uri, created_at, updated_at)
           VALUES ($1, 'Create', $2, now(), now())"#,
        create,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();
    eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .unwrap();

    assert_eq!(
        notification_count(&ctx, alice_id).await,
        before,
        "an account on a blocked domain must not reach the recipient's notifications"
    );
}

/// Following someone on a blocked domain still notifies.
///
/// The domain block is `&& not_following?`. Blocking a domain is a statement
/// about the domain; following one account there is a deliberate exception to
/// it, and silently swallowing that account's mentions would make the follow
/// useless without saying so.
#[tokio::test]
async fn test_following_through_a_domain_block_still_notifies() {
    let ctx = TestContext::new("notify-domain-block-followed").await;
    let alice_id: i64 = ctx.alice_id.parse().unwrap();

    let domain = "notify-followed.invalid";
    let (sender_id, actor_uri) = remote_actor(&ctx, "friend", domain).await;

    sqlx::query!(
        r#"INSERT INTO account_domain_blocks (account_id, domain, created_at, updated_at)
           VALUES ($1, $2, now(), now())"#,
        alice_id,
        domain,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    // Alice follows this one account despite blocking its domain.
    sqlx::query!(
        r#"INSERT INTO follows (id, account_id, target_account_id, created_at, updated_at)
           VALUES ($1, $2, $3, now(), now())"#,
        eunha::snowflake::next_id(),
        alice_id,
        sender_id,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    let before = notification_count(&ctx, alice_id).await;

    let alice_uri = format!("https://{}/users/alice", ctx.domain);
    let create = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("https://{domain}/activities/mention-2"),
        "type": "Create",
        "actor": actor_uri,
        "to": [alice_uri],
        "object": {
            "id": format!("https://{domain}/notes/mention-2"),
            "type": "Note",
            "attributedTo": actor_uri,
            "content": "<p>still here</p>",
            "to": [alice_uri],
            "tag": [{"type": "Mention", "href": alice_uri, "name": "@alice"}],
            "published": "2026-01-01T00:00:00Z",
        },
    });
    sqlx::query!(
        r#"INSERT INTO eunha.inbox_jobs (activity, activity_type, actor_uri, created_at, updated_at)
           VALUES ($1, 'Create', $2, now(), now())"#,
        create,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();
    eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .unwrap();

    assert_eq!(
        notification_count(&ctx, alice_id).await,
        before + 1,
        "an account followed through a domain block should still notify"
    );
}
