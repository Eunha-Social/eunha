use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// GET /api/oembed returns OEmbed for a public status.
#[tokio::test]
async fn test_oembed_public_status() {
    let ctx = TestContext::new("oembed-public").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "Hello oEmbed world", "public")
        .await;
    let status_id = status["id"].as_str().unwrap();

    let url = format!("https://{}/@alice/{}", ctx.domain, status_id);
    let resp = ctx
        .api
        .get(
            &format!("/api/oembed?url={}", urlencoding::encode(&url)),
            None,
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str(), Some("rich"));
    assert_eq!(body["version"].as_str(), Some("1.0"));
    assert!(body["html"].as_str().is_some());
    assert!(body["author_name"].as_str().is_some());
    assert!(body["provider_url"].as_str().is_some());
    assert_eq!(body["cache_age"].as_u64(), Some(86400));
}

/// GET /api/oembed returns 404 for a non-existent status.
#[tokio::test]
async fn test_oembed_not_found() {
    let ctx = TestContext::new("oembed-404").await;

    let url = format!("https://{}/@alice/999999999999", ctx.domain);
    let resp = ctx
        .api
        .get(
            &format!("/api/oembed?url={}", urlencoding::encode(&url)),
            None,
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// GET /api/oembed returns 404 for a private status.
#[tokio::test]
async fn test_oembed_private_status() {
    let ctx = TestContext::new("oembed-private").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "Private post", "private")
        .await;
    let status_id = status["id"].as_str().unwrap();

    let url = format!("https://{}/@alice/{}", ctx.domain, status_id);
    let resp = ctx
        .api
        .get(
            &format!("/api/oembed?url={}", urlencoding::encode(&url)),
            None,
        )
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
