//! OAuth scope enforcement tests.
//!
//! Each test creates a token with the *wrong* scope for the endpoint and
//! verifies a 403 response, then repeats with the correct scope and verifies
//! success.  The parent-scope rules are also exercised:
//!   "read"  covers all "read:*"
//!   "write" covers all "write:*"
//!   "follow" covers write:follows / write:blocks / write:mutes

use reqwest::StatusCode;

use crate::helpers::{seed_token_with_scopes, TestContext};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Assert that `method(path, token)` returns 403.
macro_rules! assert_forbidden {
    ($ctx:expr, GET $path:expr, $token:expr) => {{
        let r = $ctx.api.get($path, Some($token)).await;
        assert_eq!(
            r.status(),
            StatusCode::FORBIDDEN,
            "GET {} with wrong scope should be 403",
            $path
        );
    }};
    ($ctx:expr, POST $path:expr, $token:expr) => {{
        let r = $ctx
            .api
            .post_json($path, Some($token), &serde_json::json!({}))
            .await;
        assert_eq!(
            r.status(),
            StatusCode::FORBIDDEN,
            "POST {} with wrong scope should be 403",
            $path
        );
    }};
    ($ctx:expr, DELETE $path:expr, $token:expr) => {{
        let r = $ctx.api.delete($path, $token).await;
        assert_eq!(
            r.status(),
            StatusCode::FORBIDDEN,
            "DELETE {} with wrong scope should be 403",
            $path
        );
    }};
}

// ── GET endpoints that require read:* ────────────────────────────────────────

/// GET /api/v1/blocks requires read:blocks; a write-only token is rejected.
#[tokio::test]
async fn test_scope_get_blocks_requires_read_blocks() {
    let ctx = TestContext::new("scope-blocks").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/blocks", &write_token);

    // Parent scope "read" covers "read:blocks"
    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read").await;
    let r = ctx.api.get("/api/v1/blocks", Some(&read_token)).await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/mutes requires read:mutes.
#[tokio::test]
async fn test_scope_get_mutes_requires_read_mutes() {
    let ctx = TestContext::new("scope-mutes").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/mutes", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read").await;
    let r = ctx.api.get("/api/v1/mutes", Some(&read_token)).await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/favourites requires read:favourites.
#[tokio::test]
async fn test_scope_get_favourites_requires_read_favourites() {
    let ctx = TestContext::new("scope-favs").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/favourites", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read:favourites").await;
    let r = ctx.api.get("/api/v1/favourites", Some(&read_token)).await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/bookmarks requires read:bookmarks.
#[tokio::test]
async fn test_scope_get_bookmarks_requires_read_bookmarks() {
    let ctx = TestContext::new("scope-bmarks").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/bookmarks", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read:bookmarks").await;
    let r = ctx.api.get("/api/v1/bookmarks", Some(&read_token)).await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/follow_requests requires read:follows.
#[tokio::test]
async fn test_scope_get_follow_requests_requires_read_follows() {
    let ctx = TestContext::new("scope-freq").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/follow_requests", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read:follows").await;
    let r = ctx
        .api
        .get("/api/v1/follow_requests", Some(&read_token))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/notifications requires read:notifications.
#[tokio::test]
async fn test_scope_get_notifications_requires_read_notifications() {
    let ctx = TestContext::new("scope-notifs").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/notifications", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read:notifications").await;
    let r = ctx
        .api
        .get("/api/v1/notifications", Some(&read_token))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
}

/// GET /api/v1/accounts/verify_credentials requires read:accounts.
#[tokio::test]
async fn test_scope_verify_credentials_requires_read_accounts() {
    let ctx = TestContext::new("scope-vcreds").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    assert_forbidden!(ctx, GET "/api/v1/accounts/verify_credentials", &write_token);

    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read:accounts").await;
    let r = ctx
        .api
        .get("/api/v1/accounts/verify_credentials", Some(&read_token))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
}

// ── POST endpoints that require write:* ──────────────────────────────────────

/// POST /api/v1/statuses requires write:statuses; a read-only token is rejected.
#[tokio::test]
async fn test_scope_post_status_requires_write_statuses() {
    let ctx = TestContext::new("scope-post").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read").await;

    let r = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&read_token),
            &serde_json::json!({"status": "scope test", "visibility": "public"}),
        )
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // Parent scope "write" covers "write:statuses"
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;
    let r2 = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&write_token),
            &serde_json::json!({"status": "scope test ok", "visibility": "public"}),
        )
        .await;
    assert_eq!(r2.status(), StatusCode::OK);
}

/// POST /api/v1/accounts/:id/follow requires write:follows OR follow scope.
#[tokio::test]
async fn test_scope_follow_requires_write_follows_or_follow() {
    let ctx = TestContext::new("scope-follow").await;
    let alice_id_uuid = ctx.alice_id.parse::<i64>().unwrap();
    let read_token = seed_token_with_scopes(&ctx.db, alice_id_uuid, "read").await;

    let r = ctx
        .api
        .post_json(
            &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
            Some(&read_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // "follow" scope covers write:follows
    let follow_token = seed_token_with_scopes(&ctx.db, alice_id_uuid, "follow").await;
    let r2 = ctx
        .api
        .post_json(
            &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
            Some(&follow_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(r2.status(), StatusCode::OK);
}

/// POST /api/v1/accounts/:id/block requires write:blocks OR follow scope.
#[tokio::test]
async fn test_scope_block_requires_write_blocks_or_follow() {
    let ctx = TestContext::new("scope-block").await;
    let alice_id_uuid = ctx.alice_id.parse::<i64>().unwrap();
    let read_token = seed_token_with_scopes(&ctx.db, alice_id_uuid, "read").await;

    let r = ctx
        .api
        .post_json(
            &format!("/api/v1/accounts/{}/block", ctx.bob_id),
            Some(&read_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    let follow_token = seed_token_with_scopes(&ctx.db, alice_id_uuid, "follow").await;
    let r2 = ctx
        .api
        .post_json(
            &format!("/api/v1/accounts/{}/block", ctx.bob_id),
            Some(&follow_token),
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(r2.status(), StatusCode::OK);
}

// ── Parent-scope coverage ─────────────────────────────────────────────────────

/// A token with only "read" (parent) can access any read:* endpoint.
#[tokio::test]
async fn test_scope_read_parent_covers_all_read_children() {
    let ctx = TestContext::new("scope-read-parent").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let read_token = seed_token_with_scopes(&ctx.db, alice_id, "read").await;

    for path in &[
        "/api/v1/blocks",
        "/api/v1/mutes",
        "/api/v1/favourites",
        "/api/v1/bookmarks",
        "/api/v1/follow_requests",
        "/api/v1/notifications",
        "/api/v1/accounts/verify_credentials",
    ] {
        let r = ctx.api.get(path, Some(&read_token)).await;
        assert_eq!(
            r.status(),
            StatusCode::OK,
            "read parent should cover {path}"
        );
    }
}

/// A token with only "write" (parent) can access any write:* endpoint.
#[tokio::test]
async fn test_scope_write_parent_covers_write_statuses() {
    let ctx = TestContext::new("scope-write-parent").await;
    let alice_id = ctx.alice_id.parse::<i64>().unwrap();
    let write_token = seed_token_with_scopes(&ctx.db, alice_id, "write").await;

    let r = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&write_token),
            &serde_json::json!({"status": "write parent test", "visibility": "public"}),
        )
        .await;
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "write parent should cover write:statuses"
    );
}
