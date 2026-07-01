use reqwest::StatusCode;
use serde_json::Value;

use crate::helpers::TestContext;

/// Minimal 1×1 PNG image (valid, 67 bytes).
fn tiny_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
        0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR chunk length + type
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // width=1, height=1
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // bit depth, color, crc
        0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, // IDAT chunk
        0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc,
        0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, // IEND chunk
        0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

/// POST /api/v1/media uploads an image and returns a media attachment.
#[tokio::test]
async fn test_media_upload_image() {
    let ctx = TestContext::new("media-upload").await;

    let resp = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[],
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "upload should succeed");
    let media: Value = resp.json().await.unwrap();
    assert!(media["id"].as_str().is_some(), "id missing");
    assert_eq!(media["type"].as_str(), Some("image"));
    assert!(media["url"].as_str().is_some(), "url missing");
}

/// POST /api/v2/media also works and returns the same shape.
#[tokio::test]
async fn test_media_upload_v2() {
    let ctx = TestContext::new("media-upload-v2").await;

    let resp = ctx.api.post_multipart_file(
        "/api/v2/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[],
    ).await;
    assert!(
        resp.status() == StatusCode::OK || resp.status() == StatusCode::ACCEPTED,
        "v2 upload should return 200 or 202, got {}",
        resp.status()
    );
    let media: Value = resp.json().await.unwrap();
    assert!(media["id"].as_str().is_some(), "id missing");
    assert_eq!(media["type"].as_str(), Some("image"));
}

/// POST /api/v1/media with a description stores it.
#[tokio::test]
async fn test_media_upload_with_description() {
    let ctx = TestContext::new("media-desc").await;

    let resp = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[("description", "a tiny image")],
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let media: Value = resp.json().await.unwrap();
    assert_eq!(media["description"].as_str(), Some("a tiny image"));
}

/// POST /api/v1/media without a file returns 422.
#[tokio::test]
async fn test_media_upload_missing_file() {
    let ctx = TestContext::new("media-no-file").await;

    // Send an empty multipart (no file part).
    let form = reqwest::multipart::Form::new().text("description", "no file here");
    let resp = ctx.api.http
        .post(ctx.api.url("/api/v1/media"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "missing file should be 422");
}

/// GET /api/v1/media/:id returns the media attachment.
#[tokio::test]
async fn test_media_get() {
    let ctx = TestContext::new("media-get").await;

    let upload: Value = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[],
    ).await.json().await.unwrap();
    let id = upload["id"].as_str().unwrap();

    let resp = ctx.api.get(&format!("/api/v1/media/{}", id), Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let got: Value = resp.json().await.unwrap();
    assert_eq!(got["id"].as_str(), Some(id));
    assert_eq!(got["type"].as_str(), Some("image"));
}

/// GET /api/v1/media/:id for unknown id returns 404.
#[tokio::test]
async fn test_media_get_not_found() {
    let ctx = TestContext::new("media-get-404").await;

    let resp = ctx.api.get("/api/v1/media/999999999999", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// A status may attach at most 4 media (Mastodon `MEDIA_ATTACHMENTS_LIMIT`);
/// a fifth attachment is rejected while exactly four is accepted.
#[tokio::test]
async fn test_status_rejects_more_than_four_media() {
    let ctx = TestContext::new("media-limit").await;

    let mut ids = Vec::new();
    for _ in 0..5 {
        let media: Value = ctx.api.post_multipart_file(
            "/api/v1/media", &ctx.alice_token, "t.png", "image/png", tiny_png(), &[],
        ).await.json().await.unwrap();
        ids.push(media["id"].as_str().unwrap().to_string());
    }

    let resp = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &serde_json::json!({ "status": "five", "media_ids": ids }),
    ).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "5 media must be rejected");

    let resp = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &serde_json::json!({ "status": "four", "media_ids": ids[..4] }),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "exactly 4 media should be accepted");
}

/// PUT /api/v1/media/:id updates the description.
#[tokio::test]
async fn test_media_update_description() {
    let ctx = TestContext::new("media-update").await;

    let upload: Value = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[("description", "original")],
    ).await.json().await.unwrap();
    let id = upload["id"].as_str().unwrap();

    let resp = ctx.api.put_json(
        &format!("/api/v1/media/{}", id),
        Some(&ctx.alice_token),
        &serde_json::json!({ "description": "updated description" }),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let updated: Value = resp.json().await.unwrap();
    assert_eq!(updated["description"].as_str(), Some("updated description"));
}

/// PUT /api/v1/media/:id owned by another user returns 404.
#[tokio::test]
async fn test_media_update_not_owner() {
    let ctx = TestContext::new("media-update-owner").await;

    let upload: Value = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[],
    ).await.json().await.unwrap();
    let id = upload["id"].as_str().unwrap();

    let resp = ctx.api.put_json(
        &format!("/api/v1/media/{}", id),
        Some(&ctx.bob_token),
        &serde_json::json!({ "description": "should fail" }),
    ).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Uploading media and attaching it to a status works end-to-end.
#[tokio::test]
async fn test_media_attach_to_status() {
    let ctx = TestContext::new("media-attach").await;

    let upload: Value = ctx.api.post_multipart_file(
        "/api/v1/media",
        &ctx.alice_token,
        "test.png",
        "image/png",
        tiny_png(),
        &[("description", "attached image")],
    ).await.json().await.unwrap();
    let media_id = upload["id"].as_str().unwrap();

    let status: Value = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &serde_json::json!({
            "status": "look at this image",
            "media_ids": [media_id]
        }),
    ).await.json().await.unwrap();

    let attachments = status["media_attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1, "status should have one attachment");
    assert_eq!(attachments[0]["id"].as_str(), Some(media_id));
    assert_eq!(attachments[0]["description"].as_str(), Some("attached image"));
}
