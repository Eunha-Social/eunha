//! What eunha's counters count, against what Mastodon's count.
//!
//! A count is read by everyone who sees the status or account, so counting the
//! wrong things is not only wrong but can say something the viewer should not
//! learn. Mastodon's rules are in `Status#increment_counter_caches` and
//! `Account::Counters`; the ones checked here are the conditional ones, since
//! an unconditional counter is hard to get wrong.

use crate::helpers::TestContext;

async fn replies_count(ctx: &TestContext, id: &str, token: &str) -> i64 {
    let status: serde_json::Value = ctx
        .api
        .get(&format!("/api/v1/statuses/{id}"), Some(token))
        .await
        .json()
        .await
        .unwrap();
    status["replies_count"].as_i64().unwrap_or(-1)
}

/// Only replies everyone can see are counted.
///
/// Mastodon increments `replies_count` `if in_reply_to_id.present? &&
/// distributable?`, and `distributable?` is public or unlisted. A
/// followers-only or direct reply therefore leaves the count alone — otherwise
/// the number visible to everyone would announce that someone had replied
/// privately, which is exactly what the visibility was chosen to avoid.
#[tokio::test]
async fn test_only_distributable_replies_are_counted() {
    let ctx = TestContext::new("counters-replies").await;

    let parent = ctx
        .api
        .post_status(&ctx.alice_token, "a public post", "public")
        .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    assert_eq!(replies_count(&ctx, &parent_id, &ctx.alice_token).await, 0);

    // Public and unlisted replies count.
    for (visibility, expected) in [("public", 1), ("unlisted", 2)] {
        let response = ctx
            .api
            .post_json(
                "/api/v1/statuses",
                Some(&ctx.bob_token),
                &serde_json::json!({
                    "status": format!("a {visibility} reply"),
                    "in_reply_to_id": parent_id,
                    "visibility": visibility,
                }),
            )
            .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            replies_count(&ctx, &parent_id, &ctx.alice_token).await,
            expected,
            "a {visibility} reply should be counted"
        );
    }

    // Followers-only and direct replies do not.
    for visibility in ["private", "direct"] {
        let response = ctx
            .api
            .post_json(
                "/api/v1/statuses",
                Some(&ctx.bob_token),
                &serde_json::json!({
                    "status": format!("a {visibility} reply"),
                    "in_reply_to_id": parent_id,
                    "visibility": visibility,
                }),
            )
            .await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            replies_count(&ctx, &parent_id, &ctx.alice_token).await,
            2,
            "a {visibility} reply must not change a count everyone can read"
        );
    }
}

/// Deleting a reply that was never counted must not decrement the count.
///
/// The mirror of the rule above: if a private reply did not add to the count,
/// removing it must not subtract, or the count drifts down every time someone
/// deletes one.
#[tokio::test]
async fn test_deleting_an_uncounted_reply_leaves_the_count_alone() {
    let ctx = TestContext::new("counters-replies-delete").await;

    let parent = ctx
        .api
        .post_status(&ctx.alice_token, "a public post", "public")
        .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();

    let public_reply = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.bob_token),
            &serde_json::json!({
                "status": "a public reply",
                "in_reply_to_id": parent_id,
                "visibility": "public",
            }),
        )
        .await;
    assert_eq!(public_reply.status().as_u16(), 200);
    let _: serde_json::Value = public_reply.json().await.unwrap();

    let private_reply: serde_json::Value = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.bob_token),
            &serde_json::json!({
                "status": "a private reply",
                "in_reply_to_id": parent_id,
                "visibility": "private",
            }),
        )
        .await
        .json()
        .await
        .unwrap();

    assert_eq!(replies_count(&ctx, &parent_id, &ctx.alice_token).await, 1);

    let deleted = ctx
        .api
        .delete(
            &format!("/api/v1/statuses/{}", private_reply["id"].as_str().unwrap()),
            &ctx.bob_token,
        )
        .await;
    assert_eq!(deleted.status().as_u16(), 200);

    assert_eq!(
        replies_count(&ctx, &parent_id, &ctx.alice_token).await,
        1,
        "deleting a reply that was never counted must not decrement"
    );
}

/// A reply that arrives over ActivityPub counts like one posted here.
///
/// Mastodon's counter callbacks live on the `Status` model, so they run
/// whenever a status is created — whether a local client posted it or it
/// arrived in the inbox. A local post replied to from across the network should
/// show those replies, and eunha counted only the ones posted through its own
/// API, so a widely federated post read as having none.
#[tokio::test]
async fn test_a_federated_reply_is_counted() {
    let ctx = TestContext::new("counters-federated-reply").await;

    let parent = ctx
        .api
        .post_status(&ctx.alice_token, "a post the network can see", "public")
        .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let parent_uri: String = sqlx::query_scalar!(
        "SELECT uri FROM statuses WHERE id = $1",
        parent_id.parse::<i64>().unwrap()
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap()
    .unwrap();

    let domain = "counters-remote.invalid";
    let actor_uri = format!("https://{domain}/users/mallory");
    sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, 'mallory', $2, 'mallory', '', $3::text, $3::text, 'remote-key',
                   $3::text||'/inbox', $3::text||'/outbox', now(), now())"#,
        eunha::snowflake::next_id(),
        domain,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    let create = serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("https://{domain}/activities/reply-1"),
        "type": "Create",
        "actor": actor_uri,
        "to": ["https://www.w3.org/ns/activitystreams#Public"],
        "object": {
            "id": format!("https://{domain}/notes/reply-1"),
            "type": "Note",
            "attributedTo": actor_uri,
            "content": "<p>a reply from elsewhere</p>",
            "inReplyTo": parent_uri,
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
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

    let processed = eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .expect("draining must not error");
    assert_eq!(processed, 1, "the queued Create must be claimed");

    let stored: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM statuses WHERE in_reply_to_id = $1"#,
        parent_id.parse::<i64>().unwrap()
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    assert_eq!(stored, 1, "the reply itself must have been stored");

    assert_eq!(
        replies_count(&ctx, &parent_id, &ctx.alice_token).await,
        1,
        "a public reply from another instance must be counted"
    );
}

/// A federated reply that is deleted stops being counted.
///
/// The mirror of counting it on arrival: without this the count only ever rises,
/// and a thread whose replies were all withdrawn still claims to have them.
#[tokio::test]
async fn test_a_deleted_federated_reply_is_uncounted() {
    let ctx = TestContext::new("counters-federated-delete").await;

    let parent = ctx
        .api
        .post_status(&ctx.alice_token, "a post the network can see", "public")
        .await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let parent_uri: String = sqlx::query_scalar!(
        "SELECT uri FROM statuses WHERE id = $1",
        parent_id.parse::<i64>().unwrap()
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap()
    .unwrap();

    let domain = "counters-del-remote.invalid";
    let actor_uri = format!("https://{domain}/users/mallory");
    sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, 'mallory', $2, 'mallory', '', $3::text, $3::text, 'remote-key',
                   $3::text||'/inbox', $3::text||'/outbox', now(), now())"#,
        eunha::snowflake::next_id(),
        domain,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    let note_uri = format!("https://{domain}/notes/reply-1");
    let enqueue = |activity: serde_json::Value, kind: &'static str| {
        let db = ctx.db.clone();
        let actor = actor_uri.clone();
        async move {
            sqlx::query!(
                r#"INSERT INTO eunha.inbox_jobs (activity, activity_type, actor_uri, created_at, updated_at)
                   VALUES ($1, $2, $3, now(), now())"#,
                activity,
                kind,
                actor,
            )
            .execute(&db)
            .await
            .unwrap();
        }
    };

    enqueue(
        serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/activities/reply-1"),
            "type": "Create",
            "actor": actor_uri,
            "to": ["https://www.w3.org/ns/activitystreams#Public"],
            "object": {
                "id": note_uri,
                "type": "Note",
                "attributedTo": actor_uri,
                "content": "<p>a reply from elsewhere</p>",
                "inReplyTo": parent_uri,
                "to": ["https://www.w3.org/ns/activitystreams#Public"],
                "published": "2026-01-01T00:00:00Z",
            },
        }),
        "Create",
    )
    .await;
    eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .unwrap();
    assert_eq!(replies_count(&ctx, &parent_id, &ctx.alice_token).await, 1);

    enqueue(
        serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/activities/delete-1"),
            "type": "Delete",
            "actor": actor_uri,
            "object": {"id": note_uri, "type": "Tombstone"},
        }),
        "Delete",
    )
    .await;
    eunha::api::ap::inbox::drain_inbox_queue(&ctx.state)
        .await
        .unwrap();

    assert_eq!(
        replies_count(&ctx, &parent_id, &ctx.alice_token).await,
        0,
        "a withdrawn reply must stop being counted"
    );
}

async fn statuses_count(ctx: &TestContext, account_id: &str) -> i64 {
    let account: serde_json::Value = ctx
        .api
        .get(
            &format!("/api/v1/accounts/{account_id}"),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    account["statuses_count"].as_i64().unwrap_or(-1)
}

/// A direct message does not raise the public post count.
///
/// `Status#increment_counter_caches` opens with `return if direct_visibility?`,
/// so none of the counters move for a DM. The count is on a profile anyone can
/// read, and a number that climbs whenever an account sends a private message
/// reports that it sent one.
#[tokio::test]
async fn test_a_direct_message_is_not_counted() {
    let ctx = TestContext::new("counters-direct").await;

    let before = statuses_count(&ctx, &ctx.alice_id).await;

    let public = ctx
        .api
        .post_status(&ctx.alice_token, "everyone can see this", "public")
        .await;
    assert_eq!(public["visibility"].as_str(), Some("public"));
    assert_eq!(
        statuses_count(&ctx, &ctx.alice_id).await,
        before + 1,
        "a public post counts"
    );

    let response = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.alice_token),
            &serde_json::json!({"status": "just between us", "visibility": "direct"}),
        )
        .await;
    assert_eq!(response.status().as_u16(), 200);

    assert_eq!(
        statuses_count(&ctx, &ctx.alice_id).await,
        before + 1,
        "a direct message must not change a count anyone can read"
    );
}

/// Deleting a direct message does not lower the count either.
#[tokio::test]
async fn test_deleting_a_direct_message_leaves_the_count_alone() {
    let ctx = TestContext::new("counters-direct-delete").await;

    ctx.api
        .post_status(&ctx.alice_token, "everyone can see this", "public")
        .await;
    let before = statuses_count(&ctx, &ctx.alice_id).await;

    let dm: serde_json::Value = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.alice_token),
            &serde_json::json!({"status": "just between us", "visibility": "direct"}),
        )
        .await
        .json()
        .await
        .unwrap();

    let deleted = ctx
        .api
        .delete(
            &format!("/api/v1/statuses/{}", dm["id"].as_str().unwrap()),
            &ctx.alice_token,
        )
        .await;
    assert_eq!(deleted.status().as_u16(), 200);

    assert_eq!(
        statuses_count(&ctx, &ctx.alice_id).await,
        before,
        "deleting a message that was never counted must not decrement"
    );
}

/// A remote account's post count and last-post time follow what arrives.
///
/// `increment_counter_caches` runs on the Status model, so it fires for a
/// status that arrived in the inbox just as for one posted here. Without it a
/// remote profile shows a post count frozen at whatever it was when the account
/// was first seen, however much that account goes on to post.
#[tokio::test]
async fn test_a_federated_status_counts_for_its_author() {
    let ctx = TestContext::new("counters-remote-statuses").await;

    let domain = "counters-author.invalid";
    let actor_uri = format!("https://{domain}/users/mallory");
    let account_id = eunha::snowflake::next_id();
    sqlx::query!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, 'mallory', $2, 'mallory', '', $3::text, $3::text, 'remote-key',
                   $3::text||'/inbox', $3::text||'/outbox', now(), now())"#,
        account_id,
        domain,
        actor_uri,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    // An inbox drops a status from an account nobody here follows, so give it a
    // local follower to make the Create relevant.
    sqlx::query!(
        r#"INSERT INTO follows (id, account_id, target_account_id, created_at, updated_at)
           VALUES ($1, $2, $3, now(), now())"#,
        eunha::snowflake::next_id(),
        ctx.alice_id.parse::<i64>().unwrap(),
        account_id,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    for (n, visibility_to) in [
        (1, "https://www.w3.org/ns/activitystreams#Public"),
        (2, "https://www.w3.org/ns/activitystreams#Public"),
    ] {
        let create = serde_json::json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": format!("https://{domain}/activities/{n}"),
            "type": "Create",
            "actor": actor_uri,
            "to": [visibility_to],
            "object": {
                "id": format!("https://{domain}/notes/{n}"),
                "type": "Note",
                "attributedTo": actor_uri,
                "content": format!("<p>post {n}</p>"),
                "to": [visibility_to],
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
    }

    let ingested: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "c!" FROM statuses WHERE account_id = $1"#,
        account_id
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    assert_eq!(
        ingested, 2,
        "both posts must have been stored to be counted"
    );

    assert_eq!(
        statuses_count(&ctx, &account_id.to_string()).await,
        2,
        "both federated posts should count for their author"
    );

    let last: Option<chrono::NaiveDateTime> = sqlx::query_scalar!(
        "SELECT last_status_at FROM account_stats WHERE account_id = $1",
        account_id
    )
    .fetch_optional(&ctx.db)
    .await
    .unwrap()
    .flatten();
    assert!(last.is_some(), "last_status_at should follow what arrives");
}
