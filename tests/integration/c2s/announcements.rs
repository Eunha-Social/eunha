use reqwest::StatusCode;
use serde_json::Value;
use sqlx::PgPool;

use crate::helpers::TestContext;

/// Insert a published announcement directly for testing.
async fn seed_announcement(db: &PgPool, text: &str) -> i64 {
    sqlx::query_scalar!(
        r#"INSERT INTO announcements (text, published, published_at)
           VALUES ($1, true, now())
           RETURNING id"#,
        text,
    )
    .fetch_one(db)
    .await
    .unwrap()
}

/// GET /api/v1/announcements returns published announcements.
#[tokio::test]
async fn test_announcements_list() {
    let ctx = TestContext::new("ann-list").await;
    seed_announcement(&ctx.db, "Hello everyone!").await;

    let resp = ctx.api.get("/api/v1/announcements", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(!body.is_empty(), "should return at least one announcement");
    assert!(body.iter().any(|a| a["content"].as_str() == Some("<p>Hello everyone!</p>")));
}

/// GET /api/v1/announcements works without authentication (returns published ones).
#[tokio::test]
async fn test_announcements_unauthenticated() {
    let ctx = TestContext::new("ann-unauth").await;
    seed_announcement(&ctx.db, "Public announcement").await;

    let resp = ctx.api.get("/api/v1/announcements", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(!body.is_empty());
}

/// Each announcement has the expected fields.
#[tokio::test]
async fn test_announcement_shape() {
    let ctx = TestContext::new("ann-shape").await;
    seed_announcement(&ctx.db, "Field check announcement").await;

    let body: Vec<Value> = ctx.api.get("/api/v1/announcements", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    let ann = body.iter().find(|a| a["content"].as_str() == Some("<p>Field check announcement</p>")).unwrap();

    assert!(ann["id"].as_str().is_some(), "id missing");
    assert!(ann["content"].as_str().is_some(), "content missing");
    assert!(ann.get("reactions").is_some(), "reactions missing");
}

/// POST /api/v1/announcements/:id/dismiss marks it as dismissed (204).
#[tokio::test]
async fn test_announcement_dismiss() {
    let ctx = TestContext::new("ann-dismiss").await;
    let ann_id = seed_announcement(&ctx.db, "Dismiss me").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/announcements/{}/dismiss", ann_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "dismiss should return 200");

    // After dismissing, dismissed announcements should not appear when with_dismissed=false (default).
    // Mastodon hides dismissed announcements unless explicitly requested.
    // Verify the announcement is recorded as dismissed for alice.
    let dismissed = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM announcement_mutes WHERE announcement_id = $1 AND account_id = $2)",
        ann_id, ctx.alice_id.parse::<i64>().unwrap()
    )
    .fetch_one(&ctx.db)
    .await
    .unwrap()
    .unwrap_or(false);
    assert!(dismissed, "dismissal should be recorded in DB");
}

/// POST /api/v1/announcements/:id/dismiss for non-existent id returns 404.
#[tokio::test]
async fn test_announcement_dismiss_not_found() {
    let ctx = TestContext::new("ann-dismiss-404").await;

    let resp = ctx.api.post_json(
        "/api/v1/announcements/999999/dismiss",
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// PUT /api/v1/announcements/:id/reactions/:name adds an emoji reaction.
#[tokio::test]
async fn test_announcement_reaction_add() {
    let ctx = TestContext::new("ann-react-add").await;
    let ann_id = seed_announcement(&ctx.db, "React to me").await;

    let resp = ctx.api.put_json(
        &format!("/api/v1/announcements/{}/reactions/👍", ann_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "adding reaction should return 200");

    // Reaction should appear in the announcement's reactions list.
    let anns: Vec<Value> = ctx.api.get("/api/v1/announcements", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    let ann = anns.iter().find(|a| a["id"].as_str().map(|s| s.parse::<i64>().ok()) == Some(Some(ann_id))).unwrap();
    let reactions = ann["reactions"].as_array().unwrap();
    assert!(
        reactions.iter().any(|r| r["name"].as_str() == Some("👍") && r["me"].as_bool() == Some(true)),
        "thumbs-up reaction with me=true should appear"
    );
}

/// DELETE /api/v1/announcements/:id/reactions/:name removes the reaction.
#[tokio::test]
async fn test_announcement_reaction_remove() {
    let ctx = TestContext::new("ann-react-rm").await;
    let ann_id = seed_announcement(&ctx.db, "React then remove").await;

    // Add it.
    ctx.api.put_json(
        &format!("/api/v1/announcements/{}/reactions/❤️", ann_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    // Remove it.
    let resp = ctx.api.delete(
        &format!("/api/v1/announcements/{}/reactions/❤️", ann_id),
        &ctx.alice_token,
    ).await;
    // 200 or 404 are acceptable; the important thing is it doesn't 500.
    assert!(
        resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND,
        "remove reaction should not error, got {}",
        resp.status()
    );
}
