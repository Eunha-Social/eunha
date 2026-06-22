use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// When alice locks her account, bob's follow creates a pending request.
/// GET /api/v1/follow_requests returns that request.
#[tokio::test]
async fn test_follow_requests_lists_pending_followers() {
    let ctx = TestContext::new("freq-list").await;

    // Lock alice's account so follows require approval.
    ctx.api
        .patch_multipart(
            "/api/v1/accounts/update_credentials",
            &ctx.alice_token,
            &[("locked", "true")],
        )
        .await;

    // Bob follows alice — should become pending.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx
        .api
        .get("/api/v1/follow_requests", Some(&ctx.alice_token))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = body.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(ids.contains(&ctx.bob_id.as_str()), "bob not in alice's follow requests");
}

/// GET /api/v1/follow_requests returns empty when there are no pending requests.
#[tokio::test]
async fn test_follow_requests_empty_when_none() {
    let ctx = TestContext::new("freq-empty").await;

    let resp = ctx
        .api
        .get("/api/v1/follow_requests", Some(&ctx.alice_token))
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

/// GET /api/v1/follow_requests requires authentication.
#[tokio::test]
async fn test_follow_requests_requires_auth() {
    let ctx = TestContext::new("freq-unauth").await;

    let resp = ctx.api.get("/api/v1/follow_requests", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authorizing a follow request sets followed_by to true in the relationship.
#[tokio::test]
async fn test_follow_requests_authorize() {
    let ctx = TestContext::new("freq-auth").await;

    ctx.api
        .patch_multipart(
            "/api/v1/accounts/update_credentials",
            &ctx.alice_token,
            &[("locked", "true")],
        )
        .await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/follow_requests/{}/authorize", ctx.bob_id),
            Some(&ctx.alice_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["followed_by"], true, "followed_by should be true after authorization");

    // The request should no longer appear in the list.
    let list_resp = ctx
        .api
        .get("/api/v1/follow_requests", Some(&ctx.alice_token))
        .await;
    let list: Vec<Value> = list_resp.json().await.unwrap();
    assert!(list.is_empty(), "follow request still listed after authorization");
}

/// Rejecting a follow request sets followed_by to false and removes the request.
#[tokio::test]
async fn test_follow_requests_reject() {
    let ctx = TestContext::new("freq-rej").await;

    ctx.api
        .patch_multipart(
            "/api/v1/accounts/update_credentials",
            &ctx.alice_token,
            &[("locked", "true")],
        )
        .await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/follow_requests/{}/reject", ctx.bob_id),
            Some(&ctx.alice_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["followed_by"], false, "followed_by should be false after rejection");

    // The request should no longer appear in the list.
    let list_resp = ctx
        .api
        .get("/api/v1/follow_requests", Some(&ctx.alice_token))
        .await;
    let list: Vec<Value> = list_resp.json().await.unwrap();
    assert!(list.is_empty(), "follow request still listed after rejection");
}

/// Authorizing a follow request creates a "follow" notification for the requester.
#[tokio::test]
async fn test_authorize_follow_request_creates_notification() {
    let ctx = TestContext::new("freq-notif").await;

    // Lock alice's account.
    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "true")],
    ).await;

    // Bob sends a follow request.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    // Alice accepts it.
    ctx.api.post_json(
        &format!("/api/v1/follow_requests/{}/authorize", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    // Bob should have a "follow" notification from Alice.
    let notifs: Vec<Value> = ctx.api.get("/api/v1/notifications", Some(&ctx.bob_token))
        .await.json().await.unwrap();

    let follow_notif = notifs.iter().find(|n| {
        n["type"].as_str() == Some("follow")
        && n["account"]["id"].as_str() == Some(ctx.alice_id.as_str())
    });
    assert!(follow_notif.is_some(), "Bob should receive a follow notification when his request is accepted");
}

/// Following a locked account creates a follow_request notification for the target.
#[tokio::test]
async fn test_follow_locked_account_creates_follow_request_notification() {
    let ctx = TestContext::new("freq-notif-type").await;

    // Lock alice's account.
    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "true")],
    ).await;

    // Bob follows alice — should create a follow_request notification for alice.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let notifs: Vec<Value> = ctx.api.get("/api/v1/notifications", Some(&ctx.alice_token))
        .await.json().await.unwrap();

    let req_notif = notifs.iter().find(|n| {
        n["type"].as_str() == Some("follow_request")
        && n["account"]["id"].as_str() == Some(ctx.bob_id.as_str())
    });
    assert!(req_notif.is_some(), "locked account should receive a follow_request notification");
}

/// Follow request limit pagination returns at most `limit` items.
#[tokio::test]
async fn test_follow_requests_limit_param() {
    let ctx = TestContext::new("freq-limit").await;

    // Lock charlie's account (we create a separate user via API).
    // Use alice and lock her account so we can generate requests.
    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "true")],
    ).await;

    // Bob follows alice (pending).
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx.api.get("/api/v1/follow_requests?limit=1", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.len() <= 1, "limit=1 should return at most 1 request");
}

/// Double-accepting a follow request must not inflate follower/following counts.
#[tokio::test]
async fn test_authorize_follow_request_idempotent_counts() {
    let ctx = TestContext::new("freq-idem-counts").await;

    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "true")],
    ).await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    // First accept.
    ctx.api.post_json(
        &format!("/api/v1/follow_requests/{}/authorize", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    // Second accept (no pending row exists — must be a no-op).
    ctx.api.post_json(
        &format!("/api/v1/follow_requests/{}/authorize", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    let alice: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None)
        .await.json().await.unwrap();
    let bob: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None)
        .await.json().await.unwrap();

    assert_eq!(alice["followers_count"].as_i64(), Some(1), "alice should have exactly 1 follower");
    assert_eq!(bob["following_count"].as_i64(), Some(1), "bob should be following exactly 1 account");
}
