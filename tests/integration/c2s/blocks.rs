use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// Authenticated user can list their blocked accounts.
#[tokio::test]
async fn test_blocks_returns_blocked_accounts() {
    let ctx = TestContext::new("blocks-list").await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/block", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    let resp = ctx.api.get("/api/v1/blocks", Some(&ctx.bob_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = body.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(ids.contains(&ctx.alice_id.as_str()), "alice not in blocks list");
}

/// Block list is empty when nothing has been blocked.
#[tokio::test]
async fn test_blocks_empty_when_none() {
    let ctx = TestContext::new("blocks-empty").await;

    let resp = ctx.api.get("/api/v1/blocks", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

/// Unauthenticated request returns 401.
#[tokio::test]
async fn test_blocks_requires_auth() {
    let ctx = TestContext::new("blocks-unauth").await;

    let resp = ctx.api.get("/api/v1/blocks", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// GET /api/v1/blocks with limit=1 returns at most 1 account.
#[tokio::test]
async fn test_blocks_limit_param() {
    let ctx = TestContext::new("blocks-limit").await;

    // Alice blocks bob (so alice has at least 1 block).
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    let resp = ctx.api.get("/api/v1/blocks?limit=1", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.len() <= 1, "limit=1 should return at most 1 block");
}

/// Blocking an account then unblocking it removes it from the list.
#[tokio::test]
async fn test_blocks_unblock_removes_from_list() {
    let ctx = TestContext::new("blocks-unblock").await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/block", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/unblock", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    let resp = ctx.api.get("/api/v1/blocks", Some(&ctx.bob_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = body.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(!ids.contains(&ctx.alice_id.as_str()), "alice still in blocks list after unblock");
}
