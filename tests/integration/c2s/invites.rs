use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

/// Create an invite, list it, then delete it.
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

    let after: Vec<Value> = ctx
        .api
        .get("/api/v1/invites", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    assert!(!after
        .iter()
        .any(|i| i["id"].as_str() == Some(invite_id.as_str())));
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
