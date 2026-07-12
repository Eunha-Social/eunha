use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// The eunha invite-tree endpoint requires authentication.
#[tokio::test]
async fn test_invite_tree_requires_auth() {
    let ctx = TestContext::new("invite-tree-auth").await;
    let resp = ctx.api.get("/api/eunha/v1/invite_tree", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Invitees are nested under the account whose invite they used; uninvited
/// members are roots.
#[tokio::test]
async fn test_invite_tree_nests_invitees() {
    let ctx = TestContext::new("invite-tree-nest").await;
    let alice_account_id: i64 = ctx.alice_id.parse().unwrap();
    let bob_account_id: i64 = ctx.bob_id.parse().unwrap();

    // Make bob look like he signed up through one of alice's invites.
    let alice_user_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE account_id = $1")
        .bind(alice_account_id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    let invite_id: i64 = sqlx::query_scalar(
        "INSERT INTO invites (code, user_id, uses, created_at, updated_at)
         VALUES ($1, $2, 1, now(), now()) RETURNING id",
    )
    .bind("invitetree1")
    .bind(alice_user_id)
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    sqlx::query("UPDATE users SET invite_id = $1 WHERE account_id = $2")
        .bind(invite_id)
        .bind(bob_account_id)
        .execute(&ctx.db)
        .await
        .unwrap();

    let resp = ctx
        .api
        .get("/api/eunha/v1/invite_tree", Some(&ctx.alice_token))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    assert!(body["total"].as_u64().unwrap() >= 2);
    let roots = body["roots"].as_array().unwrap();

    // alice is a registration root; bob is nested under her, not a root.
    let alice = roots
        .iter()
        .find(|n| n["id"].as_str() == Some(ctx.alice_id.as_str()))
        .expect("alice should be a root");
    assert!(
        !roots
            .iter()
            .any(|n| n["id"].as_str() == Some(ctx.bob_id.as_str())),
        "bob should not be a root",
    );
    let children = alice["children"].as_array().unwrap();
    assert!(
        children
            .iter()
            .any(|n| n["id"].as_str() == Some(ctx.bob_id.as_str())),
        "bob should be nested under alice",
    );
}
