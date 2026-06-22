use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// Authenticated user can list their muted accounts.
#[tokio::test]
async fn test_mutes_returns_muted_accounts() {
    let ctx = TestContext::new("mutes-list").await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/mute", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    let resp = ctx.api.get("/api/v1/mutes", Some(&ctx.bob_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = body.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(ids.contains(&ctx.alice_id.as_str()), "alice not in mutes list");
}

/// Mute list is empty when nothing has been muted.
#[tokio::test]
async fn test_mutes_empty_when_none() {
    let ctx = TestContext::new("mutes-empty").await;

    let resp = ctx.api.get("/api/v1/mutes", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

/// Unauthenticated request returns 401.
#[tokio::test]
async fn test_mutes_requires_auth() {
    let ctx = TestContext::new("mutes-unauth").await;

    let resp = ctx.api.get("/api/v1/mutes", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// GET /api/v1/mutes with limit=1 returns at most 1 account.
#[tokio::test]
async fn test_mutes_limit_param() {
    let ctx = TestContext::new("mutes-limit").await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    let resp = ctx.api.get("/api/v1/mutes?limit=1", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.len() <= 1, "limit=1 should return at most 1 muted account");
}

/// Muting then unmuting removes the account from the list.
#[tokio::test]
async fn test_mutes_unmute_removes_from_list() {
    let ctx = TestContext::new("mutes-unmute").await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/mute", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/unmute", ctx.alice_id),
            Some(&ctx.bob_token),
            &serde_json::json!({}),
        )
        .await;

    let resp = ctx.api.get("/api/v1/mutes", Some(&ctx.bob_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = body.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(!ids.contains(&ctx.alice_id.as_str()), "alice still in mutes list after unmute");
}
