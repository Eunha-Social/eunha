use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

/// Create an invite, list it, then delete (expire) it.
#[tokio::test]
async fn test_invite_lifecycle() {
    let ctx = TestContext::new("invite").await;

    let invite: Value = ctx
        .api
        .post_json("/api/v1/invites", Some(&ctx.alice_token), &json!({}))
        .await
        .json()
        .await
        .unwrap();
    let invite_id = invite["id"].as_str().unwrap().to_string();
    assert!(invite["code"].as_str().is_some());
    assert!(invite["url"].as_str().is_some());

    let invites: Vec<Value> = ctx
        .api
        .get("/api/v1/invites", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    assert!(invites
        .iter()
        .any(|i| i["id"].as_str() == Some(invite_id.as_str())));

    let del_resp = ctx
        .api
        .delete(&format!("/api/v1/invites/{invite_id}"), &ctx.alice_token)
        .await;
    assert_eq!(del_resp.status(), StatusCode::OK);

    // Deleting an invite expires it (Mastodon Expireable#expire!) rather than
    // removing the row, so it still appears in the list but now has expires_at set.
    let after: Vec<Value> = ctx
        .api
        .get("/api/v1/invites", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    let expired = after
        .iter()
        .find(|i| i["id"].as_str() == Some(invite_id.as_str()))
        .expect("expired invite should still be listed");
    assert!(expired["expires_at"].as_str().is_some());
}

/// Invite with max_uses and expires_in round-trips those fields.
#[tokio::test]
async fn test_invite_with_options() {
    let ctx = TestContext::new("invite-opts").await;

    let invite: Value = ctx
        .api
        .post_json(
            "/api/v1/invites",
            Some(&ctx.alice_token),
            &json!({"max_uses": 5, "expires_in": 3600}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(invite["max_uses"].as_i64(), Some(5));
    assert!(invite["expires_at"].as_str().is_some());
}

/// Invite with autofollow and comment round-trips those fields on create + list.
#[tokio::test]
async fn test_invite_autofollow_and_comment() {
    let ctx = TestContext::new("invite-autofollow").await;

    let invite: Value = ctx
        .api
        .post_json(
            "/api/v1/invites",
            Some(&ctx.alice_token),
            &json!({"autofollow": true, "comment": "come join us"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let invite_id = invite["id"].as_str().unwrap().to_string();
    assert_eq!(invite["autofollow"].as_bool(), Some(true));
    assert_eq!(invite["comment"].as_str(), Some("come join us"));

    let invites: Vec<Value> = ctx
        .api
        .get("/api/v1/invites", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    let listed = invites
        .iter()
        .find(|i| i["id"].as_str() == Some(invite_id.as_str()))
        .expect("invite should be listed");
    assert_eq!(listed["autofollow"].as_bool(), Some(true));
    assert_eq!(listed["comment"].as_str(), Some("come join us"));
}

/// A comment longer than Mastodon's 420-character limit is rejected.
#[tokio::test]
async fn test_invite_comment_too_long() {
    let ctx = TestContext::new("invite-longcomment").await;

    let resp = ctx
        .api
        .post_json(
            "/api/v1/invites",
            Some(&ctx.alice_token),
            &json!({"comment": "x".repeat(421)}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
