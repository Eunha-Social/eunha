//! End-to-end FEP-044f quote consent handshake across two eunha instances.
//!
//! Two real servers (separate databases + domains) talk through their actual
//! HTTP inbox / authorization endpoints. The test plays the network, relaying
//! the activities one server emits to the other's inbox. Because the test
//! domains are unreachable (`*.c2s-test.invalid`), each side is cross-seeded
//! with the other's account/status so no outbound fetch is required.

use reqwest::StatusCode;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::helpers::TestContext;

/// Insert a remote account into `db`, returning its local id.
async fn seed_remote_account(db: &PgPool, username: &str, domain: &str) -> (i64, String) {
    let uri = format!("https://{domain}/users/{username}");
    let inbox = format!("{uri}/inbox");
    let id = sqlx::query_scalar!(
        r#"INSERT INTO accounts
             (id, username, domain, display_name, note, url, uri, public_key,
              inbox_url, outbox_url, shared_inbox_url, discoverable)
           VALUES ($1,$2,$3,$2,'',$4,$4,'remote-key',$5,$4||'/outbox',''::text,true)
           RETURNING id"#,
        eunha::snowflake::next_id(),
        username,
        domain,
        uri,
        inbox,
    )
    .fetch_one(db)
    .await
    .unwrap();
    (id, uri)
}

/// Insert a remote status into `db`, returning its local id.
async fn seed_remote_status(db: &PgPool, account_id: i64, uri: &str) -> i64 {
    sqlx::query_scalar!(
        r#"INSERT INTO statuses (id, account_id, text, visibility, uri, created_at)
           VALUES ($1, $2, 'remote post', 0, $3, now())
           RETURNING id"#,
        eunha::snowflake::next_id(),
        account_id,
        uri,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

#[tokio::test]
async fn test_quote_consent_handshake_between_instances() {
    // Instance B hosts the quoted author (bob); instance A hosts the quoter (alice).
    let a = TestContext::new("qfed-a").await;
    let b = TestContext::new("qfed-b").await;

    // ── B: bob publishes a public status to be quoted ──────────────────────────
    let s: Value = b
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&b.alice_token),
            &json!({"status": "quote me", "visibility": "public"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let s_uri = s["uri"].as_str().unwrap().to_string();
    // b.alice is the author on instance B; treat it as "bob" for clarity.
    let bob_uri = format!("https://{}/users/alice", b.domain);

    // ── A: cross-seed bob + his status, then alice quotes it ───────────────────
    let (bob_in_a, _) = seed_remote_account(&a.db, "alice", &b.domain).await;
    let s_in_a = seed_remote_status(&a.db, bob_in_a, &s_uri).await;

    let quote_post: Value = a
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&a.alice_token),
            &json!({"status": "great post!", "visibility": "public", "quoted_status_id": s_in_a.to_string()}),
        )
        .await
        .json()
        .await
        .unwrap();
    let quote_post_uri = quote_post["uri"].as_str().unwrap().to_string();
    let alice_uri = format!("https://{}/users/alice", a.domain);

    // A recorded the quote as pending with a QuoteRequest activity_uri.
    let (activity_uri, state): (Option<String>, i32) = sqlx::query!(
        r#"SELECT q.activity_uri, q.state
           FROM quotes q JOIN statuses s ON s.id = q.status_id
           WHERE s.uri = $1"#,
        quote_post_uri,
    )
    .fetch_one(&a.db)
    .await
    .map(|r| (r.activity_uri, r.state))
    .unwrap();
    assert_eq!(state, 0, "quote of a remote post should start pending");
    let activity_uri = activity_uri.expect("pending remote quote must carry a QuoteRequest activity_uri");

    // ── Relay the QuoteRequest to B's inbox ────────────────────────────────────
    // Cross-seed alice + her quote post on B so it needs no outbound fetch.
    let (alice_in_b, _) = seed_remote_account(&b.db, "alice-remote", &a.domain).await;
    // Override the seeded uri to alice's real actor uri so resolution matches.
    sqlx::query!("UPDATE accounts SET uri = $2, url = $2 WHERE id = $1", alice_in_b, alice_uri)
        .execute(&b.db)
        .await
        .unwrap();
    seed_remote_status(&b.db, alice_in_b, &quote_post_uri).await;

    // The quoted account needs a private key to get past the delivery guard in
    // handle_quote_request (the Accept it sends is best-effort / fire-and-forget).
    sqlx::query!(
        "UPDATE accounts SET private_key = 'test-key' WHERE username = 'alice' AND domain IS NULL",
    )
    .execute(&b.db)
    .await
    .unwrap();

    let quote_request = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": activity_uri,
        "type": "QuoteRequest",
        "actor": alice_uri,
        "object": s_uri,
        "instrument": quote_post_uri,
    });
    let resp = b.api.post_json("/inbox", None, &quote_request).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "B should accept the QuoteRequest");

    // B recorded an accepted quote with an approval (authorization) URI.
    let approval_uri: String = sqlx::query_scalar!(
        r#"SELECT q.approval_uri
           FROM quotes q JOIN statuses s ON s.id = q.quoted_status_id
           WHERE s.uri = $1 AND q.state = 1"#,
        s_uri,
    )
    .fetch_one(&b.db)
    .await
    .unwrap()
    .expect("B should stamp an approval_uri on accept");
    assert!(
        approval_uri.contains("/quote_authorizations/"),
        "approval should point at a QuoteAuthorization: {approval_uri}",
    );

    // The QuoteAuthorization stamp is fetchable on B and well-formed.
    let auth_path = approval_uri.strip_prefix(&format!("https://{}", b.domain)).unwrap();
    let auth: Value = b.api.get(auth_path, None).await.json().await.unwrap();
    assert_eq!(auth["type"].as_str(), Some("QuoteAuthorization"));
    assert_eq!(auth["interactionTarget"].as_str(), Some(s_uri.as_str()));
    assert_eq!(auth["interactingObject"].as_str(), Some(quote_post_uri.as_str()));
    assert_eq!(auth["attributedTo"].as_str(), Some(bob_uri.as_str()));

    // ── Relay B's Accept back to A's inbox ─────────────────────────────────────
    let accept = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{bob_uri}#accepts/quote_requests/1"),
        "type": "Accept",
        "actor": bob_uri,
        "to": alice_uri,
        "object": activity_uri,
        "result": approval_uri,
    });
    let resp = a.api.post_json("/inbox", None, &accept).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "A should accept the Accept");

    // A's quote is now accepted and carries the approval URI from B.
    let (final_state, final_approval): (i32, Option<String>) = sqlx::query!(
        r#"SELECT q.state, q.approval_uri
           FROM quotes q JOIN statuses s ON s.id = q.status_id
           WHERE s.uri = $1"#,
        quote_post_uri,
    )
    .fetch_one(&a.db)
    .await
    .map(|r| (r.state, r.approval_uri))
    .unwrap();
    assert_eq!(final_state, 1, "A's quote should be accepted after B's Accept");
    assert_eq!(final_approval.as_deref(), Some(approval_uri.as_str()));
}
