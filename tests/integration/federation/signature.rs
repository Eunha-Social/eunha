//! Inbound HTTP Signature enforcement on the shared inbox.

use reqwest::StatusCode;
use serde_json::json;

use crate::helpers::TestContext;

/// An activity with no HTTP Signature is rejected with 401.
#[tokio::test]
async fn test_unsigned_activity_rejected() {
    let ctx = TestContext::new("sig-unsigned").await;

    let follow = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://remote.invalid/activities/1",
        "type": "Follow",
        "actor": "https://remote.invalid/users/eve",
        "object": format!("https://{}/users/alice", ctx.domain),
    });

    let resp = ctx.api.post_json("/inbox", None, &follow).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "unsigned activity must be rejected");
}

/// A signature whose keyId host differs from the activity's actor host is
/// rejected (cross-domain forgery guard), even though the signature itself is
/// cryptographically valid.
#[tokio::test]
async fn test_signature_actor_host_mismatch_rejected() {
    let ctx = TestContext::new("sig-mismatch").await;

    // A real keypair for a key on attacker.invalid …
    let (priv_pem, pub_pem) = eunha::crypto::generate_rsa_keypair().unwrap();
    let attacker_uri = "https://attacker.invalid/users/eve";
    sqlx::query!(
        r#"INSERT INTO accounts (id, username, domain, display_name, note, url, uri, public_key, inbox_url, outbox_url, created_at, updated_at)
           VALUES ($1, 'eve', 'attacker.invalid', 'eve', '', $2::text, $2::text, $3, $2::text||'/inbox', $2::text||'/outbox', now(), now())"#,
        eunha::snowflake::next_id(),
        attacker_uri,
        pub_pem,
    )
    .execute(&ctx.db)
    .await
    .unwrap();

    // … used to sign an activity that claims to be from victim.invalid.
    let activity = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://victim.invalid/activities/1",
        "type": "Follow",
        "actor": "https://victim.invalid/users/victim",
        "object": format!("https://{}/users/alice", ctx.domain),
    });
    let resp = ctx
        .api
        .post_signed("/inbox", &activity, &format!("{attacker_uri}#main-key"), &priv_pem)
        .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "actor/key host mismatch must be rejected");
}

/// An unverifiable `Delete` is accepted-and-ignored (202) rather than rejected,
/// to avoid backscatter when the actor or its key is already gone.
#[tokio::test]
async fn test_unsigned_delete_accepted_without_processing() {
    let ctx = TestContext::new("sig-delete").await;

    let delete = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": "https://remote.invalid/activities/del-1",
        "type": "Delete",
        "actor": "https://remote.invalid/users/ghost",
        "object": "https://remote.invalid/users/ghost",
    });

    let resp = ctx.api.post_json("/inbox", None, &delete).await;
    assert_eq!(resp.status(), StatusCode::ACCEPTED, "unverified Delete should be accepted without processing");
}
