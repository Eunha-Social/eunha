use reqwest::StatusCode;

use crate::helpers::TestContext;

/// GET /api/v1/domain_blocks is empty initially.
#[tokio::test]
async fn test_domain_blocks_empty_initially() {
    let ctx = TestContext::new("dblk-empty").await;

    let resp = ctx.api.get("/api/v1/domain_blocks", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Vec<String> = resp.json().await.unwrap();
    assert!(body.is_empty());
}

/// GET /api/v1/domain_blocks requires authentication.
#[tokio::test]
async fn test_domain_blocks_requires_auth() {
    let ctx = TestContext::new("dblk-unauth").await;

    let resp = ctx.api.get("/api/v1/domain_blocks", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// POST /api/v1/domain_blocks adds a domain; GET returns it.
#[tokio::test]
async fn test_domain_blocks_add_and_list() {
    let ctx = TestContext::new("dblk-add").await;

    let post_resp = ctx
        .api
        .post_json(
            "/api/v1/domain_blocks",
            Some(&ctx.alice_token),
            &serde_json::json!({"domain": "evil.example"}),
        )
        .await;
    assert_eq!(post_resp.status(), StatusCode::OK);

    let resp = ctx.api.get("/api/v1/domain_blocks", Some(&ctx.alice_token)).await;
    let body: Vec<String> = resp.json().await.unwrap();
    assert!(body.contains(&"evil.example".to_string()), "blocked domain not listed");
}

/// POST is idempotent — blocking an already-blocked domain returns 200.
#[tokio::test]
async fn test_domain_blocks_idempotent() {
    let ctx = TestContext::new("dblk-idem").await;

    for _ in 0..2 {
        let resp = ctx
            .api
            .post_json(
                "/api/v1/domain_blocks",
                Some(&ctx.alice_token),
                &serde_json::json!({"domain": "spam.example"}),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let list: Vec<String> = ctx
        .api
        .get("/api/v1/domain_blocks", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.iter().filter(|d| d.as_str() == "spam.example").count(), 1);
}

/// DELETE /api/v1/domain_blocks removes the domain from the list.
#[tokio::test]
async fn test_domain_blocks_delete() {
    let ctx = TestContext::new("dblk-del").await;

    ctx.api
        .post_json(
            "/api/v1/domain_blocks",
            Some(&ctx.alice_token),
            &serde_json::json!({"domain": "gone.example"}),
        )
        .await;

    let del_resp = ctx
        .api
        .delete_json(
            "/api/v1/domain_blocks",
            &ctx.alice_token,
            &serde_json::json!({"domain": "gone.example"}),
        )
        .await;
    assert_eq!(del_resp.status(), StatusCode::OK);

    let list: Vec<String> = ctx
        .api
        .get("/api/v1/domain_blocks", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    assert!(!list.contains(&"gone.example".to_string()), "domain still blocked after delete");
}

/// DELETE of a non-blocked domain returns 200 (idempotent).
#[tokio::test]
async fn test_domain_blocks_delete_nonexistent_ok() {
    let ctx = TestContext::new("dblk-del-nx").await;

    let resp = ctx
        .api
        .delete_json(
            "/api/v1/domain_blocks",
            &ctx.alice_token,
            &serde_json::json!({"domain": "notblocked.example"}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
}
