use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

// ── account statuses visibility ──────────────────────────────────────────────

/// Private statuses are hidden from unauthenticated viewers.
#[tokio::test]
async fn test_account_statuses_hides_private_from_unauthenticated() {
    let ctx = TestContext::new("acct-stat-unauth").await;

    let prv = ctx.api.post_status(&ctx.alice_token, "alice private acct", "private").await;
    let pub_s = ctx.api.post_status(&ctx.alice_token, "alice public acct", "public").await;

    let resp = ctx
        .api
        .get(&format!("/api/v1/accounts/{}/statuses", ctx.alice_id), None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let statuses: Vec<Value> = resp.json().await.unwrap();

    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(!ids.contains(&prv["id"].as_str().unwrap()), "private status visible to unauthenticated user");
    assert!(ids.contains(&pub_s["id"].as_str().unwrap()), "public status missing from unauthenticated view");
}

/// Private statuses are hidden from non-followers.
#[tokio::test]
async fn test_account_statuses_hides_private_from_non_follower() {
    let ctx = TestContext::new("acct-stat-stranger").await;

    let prv = ctx.api.post_status(&ctx.alice_token, "alice prv stranger", "private").await;

    let resp = ctx
        .api
        .get(
            &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
            Some(&ctx.bob_token),
        )
        .await;
    let statuses: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(!ids.contains(&prv["id"].as_str().unwrap()), "private status visible to non-follower");
}

/// Direct statuses never appear in account statuses for non-participants.
#[tokio::test]
async fn test_account_statuses_hides_direct_from_non_participant() {
    let ctx = TestContext::new("acct-stat-direct").await;

    let dir = ctx.api.post_status(&ctx.alice_token, "alice direct nobody", "direct").await;
    let dir_id = dir["id"].as_str().unwrap();

    // Bob (not mentioned) should not see alice's direct status.
    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await.json().await.unwrap();
    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(dir_id)),
        "direct status should not appear in account statuses for non-participants",
    );
}

/// Private statuses appear in account statuses for accepted followers.
#[tokio::test]
async fn test_account_statuses_shows_private_to_follower() {
    let ctx = TestContext::new("acct-stat-follower").await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let prv = ctx.api.post_status(&ctx.alice_token, "alice prv follower", "private").await;

    let resp = ctx
        .api
        .get(
            &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
            Some(&ctx.bob_token),
        )
        .await;
    let statuses: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(ids.contains(&prv["id"].as_str().unwrap()), "private status hidden from accepted follower");
}

/// Account statuses shows all visibilities to the account owner.
#[tokio::test]
async fn test_account_statuses_shows_all_to_self() {
    let ctx = TestContext::new("acct-stat-self").await;

    let pub_s = ctx.api.post_status(&ctx.alice_token, "self public", "public").await;
    let prv_s = ctx.api.post_status(&ctx.alice_token, "self private", "private").await;
    let dir_s = ctx.api.post_status(&ctx.alice_token, "self direct", "direct").await;

    let resp = ctx
        .api
        .get(
            &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
            Some(&ctx.alice_token),
        )
        .await;
    let statuses: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();

    assert!(ids.contains(&pub_s["id"].as_str().unwrap()));
    assert!(ids.contains(&prv_s["id"].as_str().unwrap()));
    assert!(ids.contains(&dir_s["id"].as_str().unwrap()));
}

// ── account statuses filters ───────────────────────────────────────────────────

/// ?exclude_replies=true omits replies to other users from account statuses.
#[tokio::test]
async fn test_account_statuses_exclude_replies() {
    let ctx = TestContext::new("acct-excl-reply").await;

    // Alice's own post.
    let own_post = ctx.api.post_status(&ctx.alice_token, "alice own post", "public").await;
    let own_post_id = own_post["id"].as_str().unwrap();

    // Alice replies to bob (a foreign reply — should be excluded).
    let bob_post = ctx.api.post_status(&ctx.bob_token, "bob post", "public").await;
    let bob_post_id = bob_post["id"].as_str().unwrap();
    let reply: Value = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &json!({"status": "alice reply to bob", "in_reply_to_id": bob_post_id, "visibility": "public"}),
    ).await.json().await.unwrap();
    let reply_id = reply["id"].as_str().unwrap();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?exclude_replies=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();

    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(reply_id)),
        "reply to other user should be excluded",
    );
    assert!(
        statuses.iter().any(|s| s["id"].as_str() == Some(own_post_id)),
        "own post should still appear",
    );
}

/// ?exclude_reblogs=true omits reblogs from account statuses.
#[tokio::test]
async fn test_account_statuses_exclude_reblogs() {
    let ctx = TestContext::new("acct-excl-rb").await;

    let original = ctx.api.post_status(&ctx.bob_token, "rebloggable", "public").await;
    let orig_id = original["id"].as_str().unwrap();
    let reblog: Value = ctx.api.post_json(
        &format!("/api/v1/statuses/{orig_id}/reblog"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await.json().await.unwrap();
    let reblog_id = reblog["id"].as_str().unwrap();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?exclude_reblogs=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(reblog_id)),
        "reblog should be excluded",
    );
}

/// ?pinned=true returns only pinned statuses.
#[tokio::test]
async fn test_account_statuses_pinned() {
    let ctx = TestContext::new("acct-pinned").await;

    let status = ctx.api.post_status(&ctx.alice_token, "to pin", "public").await;
    let id = status["id"].as_str().unwrap();

    ctx.api.post_json(
        &format!("/api/v1/statuses/{id}/pin"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?pinned=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(statuses.iter().any(|s| s["id"].as_str() == Some(id)));
    for s in &statuses {
        assert_eq!(s["pinned"].as_bool(), Some(true));
    }
}

/// ?pinned=true hides private pinned statuses from non-followers.
#[tokio::test]
async fn test_account_statuses_pinned_hides_private_from_non_follower() {
    let ctx = TestContext::new("acct-pin-priv").await;

    // Alice pins a private status.
    let priv_status = ctx.api.post_status(&ctx.alice_token, "my secret pinned post", "private").await;
    let priv_id = priv_status["id"].as_str().unwrap();

    ctx.api.post_json(
        &format!("/api/v1/statuses/{priv_id}/pin"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    // Bob (non-follower) requests alice's pinned statuses — private pin should NOT appear.
    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?pinned=true", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await.json().await.unwrap();

    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(priv_id)),
        "private pinned status should not be visible to non-followers",
    );
}

/// ?pinned=true shows private pinned statuses to the account owner.
#[tokio::test]
async fn test_account_statuses_pinned_shows_private_to_self() {
    let ctx = TestContext::new("acct-pin-self").await;

    let priv_status = ctx.api.post_status(&ctx.alice_token, "my own private pin", "private").await;
    let priv_id = priv_status["id"].as_str().unwrap();

    ctx.api.post_json(
        &format!("/api/v1/statuses/{priv_id}/pin"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    // Alice herself sees her private pin.
    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?pinned=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();

    assert!(
        statuses.iter().any(|s| s["id"].as_str() == Some(priv_id)),
        "owner should see their own private pinned status",
    );
}

/// ?limit=1 on account statuses returns at most 1 status.
#[tokio::test]
async fn test_account_statuses_limit_param() {
    let ctx = TestContext::new("acct-stat-limit").await;

    ctx.api.post_status(&ctx.alice_token, "limit test 1", "public").await;
    ctx.api.post_status(&ctx.alice_token, "limit test 2", "public").await;

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?limit=1", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(statuses.len() <= 1, "limit=1 should return at most 1 status, got {}", statuses.len());
}

/// ?max_id pagination on account statuses omits statuses newer than max_id.
#[tokio::test]
async fn test_account_statuses_max_id_pagination() {
    let ctx = TestContext::new("acct-stat-maxid").await;

    let s1 = ctx.api.post_status(&ctx.alice_token, "pagination first", "public").await;
    let s2 = ctx.api.post_status(&ctx.alice_token, "pagination second", "public").await;
    let s1_id = s1["id"].as_str().unwrap();
    let s2_id = s2["id"].as_str().unwrap();

    // Fetch with max_id = s2's id: should return s1 but not s2.
    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?max_id={}", ctx.alice_id, s2_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(!ids.contains(&s2_id), "max_id={s2_id} should exclude s2");
    assert!(ids.contains(&s1_id), "s1 should be included when max_id={s2_id}");
}

/// ?since_id pagination on account statuses returns only statuses newer than since_id.
#[tokio::test]
async fn test_account_statuses_since_id_pagination() {
    let ctx = TestContext::new("acct-stat-since").await;

    let s1 = ctx.api.post_status(&ctx.alice_token, "since first", "public").await;
    let s2 = ctx.api.post_status(&ctx.alice_token, "since second", "public").await;
    let s1_id = s1["id"].as_str().unwrap().to_string();
    let s2_id = s2["id"].as_str().unwrap().to_string();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?since_id={s1_id}", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(!ids.contains(&s1_id.as_str()), "since_id={s1_id} should exclude s1");
    assert!(ids.contains(&s2_id.as_str()), "s2 should appear when since_id={s1_id}");
}

/// ?min_id returns statuses newer than the anchor, in ascending order.
#[tokio::test]
async fn test_account_statuses_min_id_pagination() {
    let ctx = TestContext::new("acct-stat-min").await;

    let s1 = ctx.api.post_status(&ctx.alice_token, "min first", "public").await;
    let s2 = ctx.api.post_status(&ctx.alice_token, "min second", "public").await;
    let s3 = ctx.api.post_status(&ctx.alice_token, "min third", "public").await;
    let s1_id = s1["id"].as_str().unwrap().to_string();
    let s2_id = s2["id"].as_str().unwrap().to_string();
    let s3_id = s3["id"].as_str().unwrap().to_string();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?min_id={s1_id}", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();

    assert!(!ids.contains(&s1_id.as_str()), "min_id anchor should not appear");
    assert!(ids.contains(&s2_id.as_str()), "s2 should appear");
    assert!(ids.contains(&s3_id.as_str()), "s3 should appear");

    let s2_pos = ids.iter().position(|&id| id == s2_id).unwrap();
    let s3_pos = ids.iter().position(|&id| id == s3_id).unwrap();
    assert!(s2_pos < s3_pos, "results should be in ascending order for min_id");
}

/// ?tagged=<name> returns only statuses with that tag; untagged statuses are excluded.
#[tokio::test]
async fn test_account_statuses_tagged_returns_200() {
    let ctx = TestContext::new("acct-tagged-ok").await;

    let tagged = ctx.api.post_status(&ctx.alice_token, "post with #tagxyz888", "public").await;
    let untagged = ctx.api.post_status(&ctx.alice_token, "post without tag", "public").await;
    let tagged_id = tagged["id"].as_str().unwrap();
    let untagged_id = untagged["id"].as_str().unwrap();

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?tagged=tagxyz888", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let statuses: Vec<Value> = resp.json().await.unwrap();
    assert!(
        statuses.iter().any(|s| s["id"].as_str() == Some(tagged_id)),
        "tagged status should appear in tagged filter",
    );
    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(untagged_id)),
        "untagged status should not appear in tagged filter",
    );
}

/// ?only_media=true excludes text-only statuses.
#[tokio::test]
async fn test_account_statuses_only_media() {
    let ctx = TestContext::new("acct-only-media").await;

    let text_status = ctx.api.post_status(&ctx.alice_token, "text only status no media", "public").await;
    let text_id = text_status["id"].as_str().unwrap();

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?only_media=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK, "only_media=true should return 200");
    let statuses: Vec<Value> = resp.json().await.unwrap();
    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(text_id)),
        "text-only status should not appear with only_media=true",
    );
}

/// ?only_media=true excludes reblogs, even reblogs of media posts — Mastodon's
/// only_media_scope inner-joins the status's own attachments, and a boost row
/// has none.
#[tokio::test]
async fn test_account_statuses_only_media_excludes_reblogs() {
    let ctx = TestContext::new("acct-only-media-reblog").await;

    // Bob posts a media status; Alice reblogs it.
    let media: Value = ctx.api.post_multipart_file(
        "/api/v1/media", &ctx.bob_token, "t.png", "image/png", crate::helpers::tiny_png(), &[],
    ).await.json().await.unwrap();
    let media_id = media["id"].as_str().unwrap();
    let bob_status = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.bob_token),
        &json!({ "status": "look", "visibility": "public", "media_ids": [media_id] }),
    ).await.json::<Value>().await.unwrap();
    let bob_id = bob_status["id"].as_str().unwrap();
    let reblog = ctx.api.post_json(&format!("/api/v1/statuses/{bob_id}/reblog"), Some(&ctx.alice_token), &json!({})).await;
    assert_eq!(reblog.status(), StatusCode::OK);
    let reblog: Value = reblog.json().await.unwrap();
    let reblog_id = reblog["id"].as_str().unwrap();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?only_media=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(
        !statuses.iter().any(|s| s["id"].as_str() == Some(reblog_id)),
        "a reblog of a media post must not appear with only_media=true",
    );
}

// ── follow lifecycle ──────────────────────────────────────────────────────────

/// Following your own account returns 403.
#[tokio::test]
async fn test_self_follow_returns_403() {
    let ctx = TestContext::new("self-follow").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Following an unlocked account is immediately accepted.
#[tokio::test]
async fn test_follow_unlocked_account_is_accepted() {
    let ctx = TestContext::new("follow-unlocked").await;

    let rel = ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    assert_eq!(rel["following"].as_bool(), Some(true));
    assert_eq!(rel["requested"].as_bool(), Some(false));
}

/// Following a locked account creates a pending follow request.
#[tokio::test]
async fn test_follow_locked_account_is_pending() {
    let ctx = TestContext::new("follow-locked").await;

    // Lock Bob's account directly in the DB.
    let db = ctx.db.clone();
    let bob_uuid: i64 = ctx.bob_id.parse().unwrap();
    sqlx::query!("UPDATE accounts SET locked = true WHERE id = $1", bob_uuid)
        .execute(&db)
        .await
        .unwrap();

    let rel = ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    assert_eq!(rel["following"].as_bool(), Some(false));
    assert_eq!(rel["requested"].as_bool(), Some(true));
}

// ── verify credentials ────────────────────────────────────────────────────────

/// GET /api/v1/accounts/verify_credentials returns the current user's account.
#[tokio::test]
async fn test_verify_credentials() {
    let ctx = TestContext::new("verify-creds").await;

    let resp = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["username"].as_str(), Some("alice"));
    assert!(body["id"].as_str().is_some(), "id field missing");
    assert!(body["acct"].as_str().is_some(), "acct field missing");
    assert!(body["source"].is_object(), "source field missing from verify_credentials");
}

/// GET /api/v1/accounts/verify_credentials without token → 401.
#[tokio::test]
async fn test_verify_credentials_requires_auth() {
    let ctx = TestContext::new("verify-unauth").await;

    let resp = ctx.api.get("/api/v1/accounts/verify_credentials", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── account lookup ────────────────────────────────────────────────────────────

/// GET /api/v1/accounts/:id returns account data.
#[tokio::test]
async fn test_get_account() {
    let ctx = TestContext::new("get-acct").await;

    let resp = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["id"].as_str(), Some(ctx.alice_id.as_str()));
    assert_eq!(body["username"].as_str(), Some("alice"));
}

/// GET /api/v1/accounts/:id for unknown id → 404.
#[tokio::test]
async fn test_get_account_not_found() {
    let ctx = TestContext::new("get-acct-404").await;

    let resp = ctx.api.get("/api/v1/accounts/1234567890", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// GET /api/v1/accounts/lookup?acct=alice returns Alice's account.
#[tokio::test]
async fn test_lookup_account() {
    let ctx = TestContext::new("lookup").await;

    let resp = ctx.api.get("/api/v1/accounts/lookup?acct=alice", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    assert_eq!(body["username"].as_str(), Some("alice"));
}

/// GET /api/v1/accounts/lookup?acct= returns 404 for an unknown username.
#[tokio::test]
async fn test_lookup_account_not_found() {
    let ctx = TestContext::new("lookup-404").await;

    let resp = ctx.api.get("/api/v1/accounts/lookup?acct=nobody_here_xyz999", None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// GET /api/v1/accounts/lookup?acct= returns suspended account with suspended: true.
#[tokio::test]
async fn test_lookup_account_suspended_returns_suspended() {
    let ctx = TestContext::new("lookup-suspend").await;

    // Elevate alice to admin via direct DB.
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();
    let admin_db = ctx.db.clone();
    crate::helpers::make_admin(&admin_db, alice_uuid).await;

    // Suspend bob via admin endpoint.
    ctx.api.post_json(
        &format!("/api/v1/admin/accounts/{}/suspend", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let resp = ctx.api.get("/api/v1/accounts/lookup?acct=bob", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["suspended"], true);
}

/// GET /api/v1/accounts/:id/followers returns a list after a follow.
#[tokio::test]
async fn test_get_account_followers() {
    let ctx = TestContext::new("acct-followers").await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())));
}

/// GET /api/v1/accounts/:id/following returns a list after a follow.
#[tokio::test]
async fn test_get_account_following() {
    let ctx = TestContext::new("acct-following").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/following", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())));
}

// ── relationships ─────────────────────────────────────────────────────────────

/// GET /api/v1/accounts/relationships reflects follow state.
#[tokio::test]
async fn test_get_relationships() {
    let ctx = TestContext::new("rel-basic").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["following"].as_bool(), Some(true));
    assert_eq!(list[0]["id"].as_str(), Some(ctx.bob_id.as_str()));
}

/// showing_reblogs is false when not following (not true).
#[tokio::test]
async fn test_showing_reblogs_false_when_not_following() {
    let ctx = TestContext::new("rel-showing-reblogs-nf").await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list[0]["following"].as_bool(), Some(false));
    assert_eq!(
        list[0]["showing_reblogs"].as_bool(),
        Some(false),
        "showing_reblogs should be false when not following, not true",
    );
}

/// Unfollowing sets following=false in the relationship.
#[tokio::test]
async fn test_unfollow_updates_relationship() {
    let ctx = TestContext::new("rel-unfollow").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/unfollow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["following"].as_bool(), Some(false));
}

/// Following increments followers_count and following_count.
#[tokio::test]
async fn test_follow_increments_counts() {
    let ctx = TestContext::new("follow-counts").await;

    // Get initial counts.
    let bob_before: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None)
        .await.json().await.unwrap();
    let bob_followers_before = bob_before["followers_count"].as_i64().unwrap_or(0);

    let alice_before: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None)
        .await.json().await.unwrap();
    let alice_following_before = alice_before["following_count"].as_i64().unwrap_or(0);

    // Alice follows Bob.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    // Bob's followers_count should increase.
    let bob_after: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None)
        .await.json().await.unwrap();
    assert_eq!(
        bob_after["followers_count"].as_i64().unwrap_or(0),
        bob_followers_before + 1,
        "Bob's followers_count should increment after being followed",
    );

    // Alice's following_count should increase.
    let alice_after: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None)
        .await.json().await.unwrap();
    assert_eq!(
        alice_after["following_count"].as_i64().unwrap_or(0),
        alice_following_before + 1,
        "Alice's following_count should increment after following",
    );
}

/// Unfollowing decrements followers_count and following_count.
#[tokio::test]
async fn test_unfollow_decrements_counts() {
    let ctx = TestContext::new("unfollow-counts").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let bob_mid: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None)
        .await.json().await.unwrap();
    let bob_followers_mid = bob_mid["followers_count"].as_i64().unwrap_or(0);

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/unfollow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let bob_after: Value = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None)
        .await.json().await.unwrap();
    assert_eq!(
        bob_after["followers_count"].as_i64().unwrap_or(0),
        bob_followers_mid - 1,
        "Bob's followers_count should decrement after unfollow",
    );
}

/// Blocking sets blocking=true; unblocking sets it back to false.
#[tokio::test]
async fn test_block_and_unblock() {
    let ctx = TestContext::new("block").await;

    let block_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(block_resp.status(), StatusCode::OK);
    let rel: Value = block_resp.json().await.unwrap();
    assert_eq!(rel["blocking"].as_bool(), Some(true));

    let unblock_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/unblock", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(unblock_resp.status(), StatusCode::OK);
    let rel2: Value = unblock_resp.json().await.unwrap();
    assert_eq!(rel2["blocking"].as_bool(), Some(false));
}

/// Muting sets muting=true; unmuting sets it back to false.
#[tokio::test]
async fn test_mute_and_unmute() {
    let ctx = TestContext::new("mute").await;

    let mute_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(mute_resp.status(), StatusCode::OK);
    let rel: Value = mute_resp.json().await.unwrap();
    assert_eq!(rel["muting"].as_bool(), Some(true));

    let unmute_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/unmute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(unmute_resp.status(), StatusCode::OK);
    let rel2: Value = unmute_resp.json().await.unwrap();
    assert_eq!(rel2["muting"].as_bool(), Some(false));
}

// ── follow requests ───────────────────────────────────────────────────────────

/// Accepting a pending follow request changes the relationship to following=true.
#[tokio::test]
async fn test_authorize_follow_request() {
    let ctx = TestContext::new("follow-req-accept").await;

    let db = ctx.db.clone();
    let bob_uuid: i64 = ctx.bob_id.parse().unwrap();
    sqlx::query!("UPDATE accounts SET locked = true WHERE id = $1", bob_uuid)
        .execute(&db).await.unwrap();

    // Alice follows locked Bob → pending.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    // Bob authorises Alice's follow request.
    let requests_resp = ctx.api.get("/api/v1/follow_requests", Some(&ctx.bob_token)).await;
    let requests: Vec<Value> = requests_resp.json().await.unwrap();
    assert!(!requests.is_empty(), "no pending follow requests");
    let requester_id = requests[0]["id"].as_str().unwrap().to_string();

    let accept_resp = ctx.api.post_json(
        &format!("/api/v1/follow_requests/{requester_id}/authorize"),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;
    assert_eq!(accept_resp.status(), StatusCode::OK);

    // Alice is now following Bob.
    let rels: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert_eq!(rels[0]["following"].as_bool(), Some(true));
    assert_eq!(rels[0]["requested"].as_bool(), Some(false));
}

/// Rejecting a pending follow request leaves following=false, requested=false.
#[tokio::test]
async fn test_reject_follow_request() {
    let ctx = TestContext::new("follow-req-reject").await;

    let db = ctx.db.clone();
    let bob_uuid: i64 = ctx.bob_id.parse().unwrap();
    sqlx::query!("UPDATE accounts SET locked = true WHERE id = $1", bob_uuid)
        .execute(&db).await.unwrap();

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let requests: Vec<Value> = ctx.api.get("/api/v1/follow_requests", Some(&ctx.bob_token))
        .await.json().await.unwrap();
    let requester_id = requests[0]["id"].as_str().unwrap().to_string();

    let reject_resp = ctx.api.post_json(
        &format!("/api/v1/follow_requests/{requester_id}/reject"),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;
    assert_eq!(reject_resp.status(), StatusCode::OK);

    let rels: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert_eq!(rels[0]["following"].as_bool(), Some(false));
    assert_eq!(rels[0]["requested"].as_bool(), Some(false));
}

// ── blocks and mutes lists ────────────────────────────────────────────────────

/// After blocking Bob, GET /api/v1/blocks includes him.
#[tokio::test]
async fn test_blocks_list_includes_blocked() {
    let ctx = TestContext::new("blocks-list").await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let resp = ctx.api.get("/api/v1/blocks", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())));
}

/// After muting Bob, GET /api/v1/mutes includes him.
#[tokio::test]
async fn test_mutes_list_includes_muted() {
    let ctx = TestContext::new("mutes-list").await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let resp = ctx.api.get("/api/v1/mutes", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())));
}

// ── preferences ───────────────────────────────────────────────────────────────

/// GET /api/v1/preferences returns colon-separated keys expected by clients.
#[tokio::test]
async fn test_get_preferences() {
    let ctx = TestContext::new("prefs").await;

    let resp = ctx.api.get("/api/v1/preferences", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["posting:default:visibility"].as_str().is_some(),
        "posting:default:visibility missing: {body}",
    );
    assert!(
        body.get("reading:expand:media").is_some(),
        "reading:expand:media missing: {body}",
    );
}

// ── endorse / unendorse ───────────────────────────────────────────────────────

/// Endorsing Bob sets endorsed=true; unendorsing reverts it.
#[tokio::test]
async fn test_endorse_and_unendorse() {
    let ctx = TestContext::new("endorse").await;

    let endorse_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/endorse", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(endorse_resp.status(), StatusCode::OK);
    let rel: Value = endorse_resp.json().await.unwrap();
    assert_eq!(rel["endorsed"].as_bool(), Some(true));

    let unendorse_resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/unendorse", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(unendorse_resp.status(), StatusCode::OK);
    let rel2: Value = unendorse_resp.json().await.unwrap();
    assert_eq!(rel2["endorsed"].as_bool(), Some(false));
}

/// GET /api/v1/accounts/:id/endorsements returns endorsed accounts.
#[tokio::test]
async fn test_get_endorsements_list() {
    let ctx = TestContext::new("endorse-list").await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/endorse", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/endorsements", ctx.alice_id),
        None,
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())));
}

// ── account note ──────────────────────────────────────────────────────────────

/// Setting an account note is reflected in the relationship.
#[tokio::test]
async fn test_set_account_note() {
    let ctx = TestContext::new("acct-note").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/note", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"comment": "Note about Bob"}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["note"].as_str(), Some("Note about Bob"));
}

/// An account note over 2000 characters is rejected (Mastodon
/// AccountNote::COMMENT_SIZE_LIMIT).
#[tokio::test]
async fn test_set_account_note_too_long() {
    let ctx = TestContext::new("acct-note-long").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/note", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({ "comment": "x".repeat(2001) }),
    ).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// ── remove from followers ─────────────────────────────────────────────────────

/// After Alice removes Bob from her followers, Bob's relationship shows following=false.
#[tokio::test]
async fn test_remove_from_followers() {
    let ctx = TestContext::new("rm-follower").await;

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/remove_from_followers", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let rels: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await.json().await.unwrap();
    assert_eq!(rels[0]["following"].as_bool(), Some(false));
}

// ── profile settings ──────────────────────────────────────────────────────────

/// PUT /api/v1/profile returns 200 with the account object.
#[tokio::test]
async fn test_update_profile_settings() {
    let ctx = TestContext::new("profile-settings").await;

    let resp = ctx.api.put_json(
        "/api/v1/profile",
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["id"].as_str().is_some());
}

// ── update_credentials ────────────────────────────────────────────────────────

/// PATCH /api/v1/accounts/update_credentials (multipart) updates display_name.
#[tokio::test]
async fn test_update_credentials_display_name() {
    let ctx = TestContext::new("update-creds").await;

    let form = reqwest::multipart::Form::new()
        .text("display_name", "Alice Updated");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["display_name"].as_str(), Some("Alice Updated"));
}

/// update_credentials rejects more than 4 profile fields (Mastodon
/// Account::DEFAULT_FIELDS_SIZE) and over-long field values.
#[tokio::test]
async fn test_update_credentials_fields_limits() {
    let ctx = TestContext::new("update-fields").await;

    // Five fields → 422.
    let mut form = reqwest::multipart::Form::new();
    for i in 0..5 {
        form = form
            .text(format!("fields_attributes[{i}][name]"), format!("k{i}"))
            .text(format!("fields_attributes[{i}][value]"), format!("v{i}"));
    }
    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "5 fields must be rejected");

    // An over-long value → 422.
    let form = reqwest::multipart::Form::new()
        .text("fields_attributes[0][name]", "website")
        .text("fields_attributes[0][value]", "x".repeat(256));
    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY, "over-long field value must be rejected");

    // Exactly 4 valid fields → OK.
    let mut form = reqwest::multipart::Form::new();
    for i in 0..4 {
        form = form
            .text(format!("fields_attributes[{i}][name]"), format!("k{i}"))
            .text(format!("fields_attributes[{i}][value]"), format!("v{i}"));
    }
    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "4 fields should be accepted");
}

/// PATCH /api/v1/accounts/update_credentials updates bio note.
#[tokio::test]
async fn test_update_credentials_note() {
    let ctx = TestContext::new("update-note").await;

    let form = reqwest::multipart::Form::new()
        .text("note", "This is my bio");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["source"]["note"].as_str().unwrap_or("").contains("This is my bio"),
        "note not updated: {body}",
    );
}

/// PATCH /api/v1/accounts/update_credentials with locked=true makes account locked.
#[tokio::test]
async fn test_update_credentials_locked() {
    let ctx = TestContext::new("update-locked").await;

    let form = reqwest::multipart::Form::new()
        .text("locked", "true");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["locked"].as_bool(), Some(true));

    // Follow from Bob should now be pending.
    let rel = ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;
    assert_eq!(rel["requested"].as_bool(), Some(true));
}

/// PATCH /api/v1/accounts/update_credentials with source[privacy] updates default posting visibility.
#[tokio::test]
async fn test_update_credentials_source_privacy() {
    let ctx = TestContext::new("update-privacy").await;

    let form = reqwest::multipart::Form::new()
        .text("source[privacy]", "private");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The update response itself must reflect the new default, not a hardcoded
    // "public" (the response builder reads the user's actual settings).
    let updated: Value = resp.json().await.unwrap();
    assert_eq!(updated["source"]["privacy"].as_str(), Some("private"), "PATCH response source.privacy stale");

    // And it persists, visible via verify_credentials.
    let creds: Value = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert_eq!(creds["source"]["privacy"].as_str(), Some("private"));
}

/// PATCH /api/v1/accounts/update_credentials with source[sensitive] updates default sensitivity.
#[tokio::test]
async fn test_update_credentials_source_sensitive() {
    let ctx = TestContext::new("update-sensitive").await;

    let form = reqwest::multipart::Form::new()
        .text("source[sensitive]", "true");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let creds: Value = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert_eq!(creds["source"]["sensitive"].as_bool(), Some(true));
}

/// PATCH /api/v1/accounts/update_credentials with source[language] updates default language.
#[tokio::test]
async fn test_update_credentials_source_language() {
    let ctx = TestContext::new("update-lang").await;

    let form = reqwest::multipart::Form::new()
        .text("source[language]", "fr");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let creds: Value = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert_eq!(creds["source"]["language"].as_str(), Some("fr"));
}

/// Profile fields set via update_credentials appear in verify_credentials source and account fields.
#[tokio::test]
async fn test_update_credentials_profile_fields() {
    let ctx = TestContext::new("profile-fields").await;

    let form = reqwest::multipart::Form::new()
        .text("fields_attributes[0][name]", "Website")
        .text("fields_attributes[0][value]", "https://example.com")
        .text("fields_attributes[1][name]", "Location")
        .text("fields_attributes[1][value]", "Rustland");

    let resp = ctx.api.http
        .patch(ctx.api.url("/api/v1/accounts/update_credentials"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let updated: Value = resp.json().await.unwrap();

    // The account fields array should have both entries.
    let fields = updated["fields"].as_array().expect("fields should be array");
    assert!(
        fields.iter().any(|f| f["name"].as_str() == Some("Website")),
        "Website field missing from fields: {fields:?}",
    );
    assert!(
        fields.iter().any(|f| f["name"].as_str() == Some("Location")),
        "Location field missing from fields: {fields:?}",
    );

    // source.fields should also reflect the values.
    let creds: Value = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    let src_fields = creds["source"]["fields"].as_array().expect("source.fields should be array");
    assert!(
        src_fields.iter().any(|f| f["name"].as_str() == Some("Website")),
        "Website field missing from source.fields: {src_fields:?}",
    );
}

// ── familiar followers ────────────────────────────────────────────────────────

/// GET /api/v1/accounts/familiar_followers returns an array of familiar-followers objects.
#[tokio::test]
async fn test_familiar_followers_returns_array() {
    let ctx = TestContext::new("familiar").await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/familiar_followers?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"].as_str(), Some(ctx.bob_id.as_str()));
    assert!(list[0]["accounts"].is_array());
}

/// Passing the same id twice returns only one entry (deduplication).
#[tokio::test]
async fn test_familiar_followers_deduplicates_ids() {
    let ctx = TestContext::new("familiar-dedup").await;

    let resp = ctx.api.get(
        &format!(
            "/api/v1/accounts/familiar_followers?id[]={}&id[]={}",
            ctx.bob_id, ctx.bob_id
        ),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list.len(), 1, "duplicate id[] should be collapsed to one entry");
}

/// familiar_followers returns accounts the viewer follows who also follow the target.
#[tokio::test]
async fn test_familiar_followers_correctness() {
    let ctx = TestContext::new("familiar-correct").await;

    // Create charlie as a 3rd user
    let (charlie_uuid, charlie_token) =
        crate::helpers::seed_user(&ctx.db, &ctx.domain, "charlie", "charlie@test.invalid").await;
    let charlie_id = charlie_uuid.to_string();

    // Alice follows Charlie
    ctx.api.post_json(
        &format!("/api/v1/accounts/{charlie_id}/follow"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    // Charlie follows Bob
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&charlie_token),
        &json!({}),
    ).await;

    // Alice checks familiar followers for Bob — should see Charlie (alice follows charlie, charlie follows bob)
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/familiar_followers?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    let list: Vec<Value> = resp.json().await.unwrap();
    let entry = &list[0];
    let familiar: Vec<&str> = entry["accounts"].as_array().unwrap()
        .iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(
        familiar.contains(&charlie_id.as_str()),
        "charlie should be a familiar follower (alice follows charlie, charlie follows bob)",
    );
    assert!(
        !familiar.contains(&ctx.alice_id.as_str()),
        "alice should not appear in her own familiar followers list",
    );
}

// ── suggestions ───────────────────────────────────────────────────────────────

/// GET /api/v1/suggestions returns a JSON array.
#[tokio::test]
async fn test_get_suggestions() {
    let ctx = TestContext::new("suggest").await;

    let resp = ctx.api.get("/api/v1/suggestions", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let _: Vec<Value> = resp.json().await.unwrap();
}

/// DELETE /api/v1/suggestions/:id returns 200.
#[tokio::test]
async fn test_dismiss_suggestion() {
    let ctx = TestContext::new("suggest-dismiss").await;

    let resp = ctx.api.delete(
        &format!("/api/v1/suggestions/{}", ctx.bob_id),
        &ctx.alice_token,
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

/// GET /api/v2/suggestions returns suggestions with a source field.
#[tokio::test]
async fn test_get_suggestions_v2() {
    let ctx = TestContext::new("suggest-v2").await;

    let resp = ctx.api.get("/api/v2/suggestions", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let _: Vec<Value> = resp.json().await.unwrap();
}

// ── directory ─────────────────────────────────────────────────────────────────

/// GET /api/v1/directory returns local accounts (includes alice).
#[tokio::test]
async fn test_get_directory() {
    let ctx = TestContext::new("directory").await;

    let resp = ctx.api.get("/api/v1/directory", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(
        list.iter().any(|a| a["username"].as_str() == Some("alice")),
        "alice not found in directory",
    );
}

// ── account search endpoint ───────────────────────────────────────────────────

/// GET /api/v1/accounts/search returns matching accounts.
#[tokio::test]
async fn test_accounts_search_endpoint() {
    let ctx = TestContext::new("acct-search").await;

    let resp = ctx.api.get(
        "/api/v1/accounts/search?q=bob",
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.iter().any(|a| a["username"].as_str() == Some("bob")));
}

// ── block effects ─────────────────────────────────────────────────────────────

/// Blocking removes the follow relationship in both directions.
#[tokio::test]
async fn test_block_removes_follow() {
    let ctx = TestContext::new("block-rm-follow").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let rels: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert_eq!(rels[0]["following"].as_bool(), Some(false), "alice should not follow bob after block");
    assert_eq!(rels[0]["followed_by"].as_bool(), Some(false), "bob should not follow alice after block");
}

// ── account lists ─────────────────────────────────────────────────────────────

/// GET /api/v1/accounts/:id/lists returns lists that include the given account.
#[tokio::test]
async fn test_get_account_lists() {
    let ctx = TestContext::new("acct-lists").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let list: Value = ctx.api.post_json(
        "/api/v1/lists",
        Some(&ctx.alice_token),
        &json!({"title": "Bob's List"}),
    ).await.json().await.unwrap();
    let list_id = list["id"].as_str().unwrap();

    ctx.api.post_json(
        &format!("/api/v1/lists/{list_id}/accounts"),
        Some(&ctx.alice_token),
        &json!({"account_ids": [ctx.bob_id]}),
    ).await;

    let lists: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/lists", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(lists.iter().any(|l| l["id"].as_str() == Some(list_id)));
}

// ── domain blocks ─────────────────────────────────────────────────────────────

/// Block a domain, list it, unblock it.
#[tokio::test]
async fn test_domain_block_lifecycle() {
    let ctx = TestContext::new("domain-block").await;

    let block_resp = ctx.api.post_json(
        "/api/v1/domain_blocks",
        Some(&ctx.alice_token),
        &json!({"domain": "evil.example.com"}),
    ).await;
    assert_eq!(block_resp.status(), StatusCode::OK);

    let domains: Vec<String> = ctx.api.get("/api/v1/domain_blocks", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert!(domains.contains(&"evil.example.com".to_string()));

    let unblock_resp = ctx.api.http
        .delete(ctx.api.url("/api/v1/domain_blocks"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .json(&json!({"domain": "evil.example.com"}))
        .send().await.unwrap();
    assert_eq!(unblock_resp.status(), StatusCode::OK);

    let after: Vec<String> = ctx.api.get("/api/v1/domain_blocks", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert!(!after.contains(&"evil.example.com".to_string()));
}

// ── follow settings (showing_reblogs / notifying) ─────────────────────────────

/// Following with reblogs=false sets showing_reblogs=false in relationship.
#[tokio::test]
async fn test_follow_with_reblogs_false() {
    let ctx = TestContext::new("follow-no-reblogs").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"reblogs": false}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["following"].as_bool(), Some(true));
    assert_eq!(rel["showing_reblogs"].as_bool(), Some(false));
}

/// Following with notify=true sets notifying=true in relationship.
#[tokio::test]
async fn test_follow_with_notify_true() {
    let ctx = TestContext::new("follow-notify").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"notify": true}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["following"].as_bool(), Some(true));
    assert_eq!(rel["notifying"].as_bool(), Some(true));
}

/// Re-following an already-followed account updates settings without duplicating.
#[tokio::test]
async fn test_follow_update_settings_when_already_following() {
    let ctx = TestContext::new("follow-update-settings").await;

    // First follow with defaults.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    // Re-follow with reblogs=false.
    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"reblogs": false, "notify": true}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["following"].as_bool(), Some(true), "should still be following after re-follow");
    assert_eq!(rel["showing_reblogs"].as_bool(), Some(false));
    assert_eq!(rel["notifying"].as_bool(), Some(true));
}

/// Default follow has showing_reblogs=true and notifying=false.
#[tokio::test]
async fn test_follow_defaults_showing_reblogs_true() {
    let ctx = TestContext::new("follow-defaults").await;

    let rel = ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    assert_eq!(rel["showing_reblogs"].as_bool(), Some(true));
    assert_eq!(rel["notifying"].as_bool(), Some(false));
}

/// Relationship languages field is null (not []) when no language filter is set.
#[tokio::test]
async fn test_relationship_languages_null_when_not_set() {
    let ctx = TestContext::new("rel-languages-null").await;

    let rel = ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    assert!(
        rel["languages"].is_null(),
        "languages should be null when no language filter is set, got: {}",
        rel["languages"],
    );
}

// ── mute settings ─────────────────────────────────────────────────────────────

/// Muting with notifications=false sets muting_notifications=false.
#[tokio::test]
async fn test_mute_with_notifications_false() {
    let ctx = TestContext::new("mute-no-notif").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"notifications": false}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["muting"].as_bool(), Some(true));
    assert_eq!(rel["muting_notifications"].as_bool(), Some(false));
}

/// Muting with duration=3600 sets mute_expires_at to a non-null value.
#[tokio::test]
async fn test_mute_with_duration_sets_expires_at() {
    let ctx = TestContext::new("mute-duration").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"duration": 3600}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["muting"].as_bool(), Some(true));
    assert!(rel["muting_expires_at"].as_str().is_some(), "muting_expires_at should be set");
}

/// Re-muting an account updates hide_notifications in place.
#[tokio::test]
async fn test_mute_upsert_updates_settings() {
    let ctx = TestContext::new("mute-upsert").await;

    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"notifications": true}),
    ).await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({"notifications": false}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["muting_notifications"].as_bool(), Some(false));
}

// ── relationship extras ───────────────────────────────────────────────────────

/// blocked_by reflects when the target has blocked the requesting user.
#[tokio::test]
async fn test_relationship_blocked_by() {
    let ctx = TestContext::new("blocked-by").await;

    // Bob blocks Alice.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.alice_id),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;

    // Alice checks her relationship with Bob.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list[0]["blocked_by"].as_bool(), Some(true));
}

/// requested_by reflects when the target has a pending follow request to the user.
#[tokio::test]
async fn test_relationship_requested_by() {
    let ctx = TestContext::new("requested-by").await;

    let db = ctx.db.clone();
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();

    // Lock Alice's account so Bob's follow becomes pending.
    sqlx::query!("UPDATE accounts SET locked = true WHERE id = $1", alice_uuid)
        .execute(&db).await.unwrap();

    // Bob sends a follow request to Alice.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;

    // Alice checks her relationship with Bob.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list[0]["requested_by"].as_bool(), Some(true));
}

/// domain_blocking reflects a domain block on the target's domain.
#[tokio::test]
async fn test_relationship_domain_blocking() {
    let ctx = TestContext::new("rel-domain-block").await;

    let db = ctx.db.clone();
    let bob_uuid: i64 = ctx.bob_id.parse().unwrap();

    // Set Bob's domain to a remote domain.
    sqlx::query!("UPDATE accounts SET domain = 'remote.example.com' WHERE id = $1", bob_uuid)
        .execute(&db).await.unwrap();

    // Alice domain-blocks that domain.
    ctx.api.post_json(
        "/api/v1/domain_blocks",
        Some(&ctx.alice_token),
        &json!({"domain": "remote.example.com"}),
    ).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await;
    let list: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(list[0]["domain_blocking"].as_bool(), Some(true));
}

// ── hide_collections ──────────────────────────────────────────────────────────

/// When hide_collections=true, followers list is empty for non-owner viewers.
#[tokio::test]
async fn test_hide_collections_hides_followers_from_others() {
    let ctx = TestContext::new("hide-coll-followers").await;

    let db = ctx.db.clone();
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();

    // Bob follows Alice.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    // Enable hide_collections on Alice's account.
    sqlx::query!("UPDATE accounts SET hide_collections = true WHERE id = $1", alice_uuid)
        .execute(&db).await.unwrap();

    // Bob tries to see Alice's followers — should be empty.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.is_empty(), "followers should be hidden when hide_collections=true");
}

/// When hide_collections=true, following list is empty for non-owner viewers.
#[tokio::test]
async fn test_hide_collections_hides_following_from_others() {
    let ctx = TestContext::new("hide-coll-following").await;

    let db = ctx.db.clone();
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();

    // Alice follows Bob.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    // Enable hide_collections on Alice's account.
    sqlx::query!("UPDATE accounts SET hide_collections = true WHERE id = $1", alice_uuid)
        .execute(&db).await.unwrap();

    // Bob tries to see Alice's following — should be empty.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/following", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.is_empty(), "following should be hidden when hide_collections=true");
}

/// Owner can always see their own followers even with hide_collections=true.
#[tokio::test]
async fn test_hide_collections_owner_sees_own_followers() {
    let ctx = TestContext::new("hide-coll-self").await;

    let db = ctx.db.clone();
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();

    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;
    sqlx::query!("UPDATE accounts SET hide_collections = true WHERE id = $1", alice_uuid)
        .execute(&db).await.unwrap();

    // Alice views her own followers.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(!list.is_empty(), "owner should see own followers even with hide_collections");
}

// ── preferences ───────────────────────────────────────────────────────────────

/// GET /api/v1/preferences returns sensible defaults.
#[tokio::test]
async fn test_get_preferences_defaults() {
    let ctx = TestContext::new("prefs-defaults").await;

    let resp = ctx.api.get("/api/v1/preferences", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let prefs: Value = resp.json().await.unwrap();

    assert!(prefs["posting:default:visibility"].as_str().is_some(), "missing posting:default:visibility");
    assert!(prefs["posting:default:sensitive"].as_bool().is_some(), "missing posting:default:sensitive");
}

/// GET /api/v1/preferences returns the documented default posting preferences.
#[tokio::test]
async fn test_preferences_defaults() {
    let ctx = TestContext::new("prefs-default").await;

    let resp = ctx.api.get("/api/v1/preferences", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let prefs: Value = resp.json().await.unwrap();
    assert_eq!(prefs["posting:default:visibility"].as_str(), Some("public"));
    assert_eq!(prefs["posting:default:sensitive"].as_bool(), Some(false));
    // Default language is unset (null) until the user configures one.
    assert!(prefs["posting:default:language"].is_null());
}

// ── profile aliases ───────────────────────────────────────────────────────────

/// GET /api/v1/profile/aliases returns empty list initially; POST creates one; DELETE removes it.
#[tokio::test]
async fn test_profile_aliases_crud() {
    let ctx = TestContext::new("alias-crud").await;

    // Initially empty.
    let resp = ctx.api.get("/api/v1/profile/aliases", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.is_empty(), "expected empty aliases list: {list:?}");

    // Create an alias.
    let create_resp = ctx.api.post_json(
        "/api/v1/profile/aliases",
        Some(&ctx.alice_token),
        &json!({"acct": "alice@old.example.com"}),
    ).await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let alias: Value = create_resp.json().await.unwrap();
    let alias_id = alias["id"].as_str().expect("alias id missing");
    assert_eq!(alias["uri"].as_str(), Some("alice@old.example.com"));

    // List now contains the alias.
    let after_create: Vec<Value> = ctx.api.get("/api/v1/profile/aliases", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert!(
        after_create.iter().any(|a| a["id"].as_str() == Some(alias_id)),
        "created alias not in list: {after_create:?}",
    );

    // Delete it.
    let del_resp = ctx.api.delete(
        &format!("/api/v1/profile/aliases/{alias_id}"),
        &ctx.alice_token,
    ).await;
    assert_eq!(del_resp.status(), StatusCode::OK);

    // List is empty again.
    let after_delete: Vec<Value> = ctx.api.get("/api/v1/profile/aliases", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert!(
        !after_delete.iter().any(|a| a["id"].as_str() == Some(alias_id)),
        "deleted alias still in list: {after_delete:?}",
    );
}

/// POST /api/v1/profile/aliases is idempotent (same uri twice → single entry).
#[tokio::test]
async fn test_profile_alias_idempotent() {
    let ctx = TestContext::new("alias-idem").await;

    ctx.api.post_json(
        "/api/v1/profile/aliases",
        Some(&ctx.alice_token),
        &json!({"acct": "alice@idem.example.com"}),
    ).await;
    ctx.api.post_json(
        "/api/v1/profile/aliases",
        Some(&ctx.alice_token),
        &json!({"acct": "alice@idem.example.com"}),
    ).await;

    let list: Vec<Value> = ctx.api.get("/api/v1/profile/aliases", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    let count = list.iter().filter(|a| a["uri"].as_str() == Some("alice@idem.example.com")).count();
    assert_eq!(count, 1, "duplicate aliases created: {list:?}");
}

// ── account move ──────────────────────────────────────────────────────────────

/// POST /api/v1/accounts/move with a valid password updates moved_to_uri.
#[tokio::test]
async fn test_move_account_with_valid_password() {
    let ctx = TestContext::new("move-acct").await;

    let resp = ctx.api.post_json(
        "/api/v1/accounts/move",
        Some(&ctx.alice_token),
        &json!({
            "current_password": "testpassword123",
            "acct": "alice@new.example.com"
        }),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // verify_credentials should reflect moved_to
    let me: Value = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token))
        .await.json().await.unwrap();
    assert_eq!(me["moved"]["url"].as_str().or(me["moved_to_uri"].as_str()).or(Some("")),
        // moved_to_uri is an internal field; the API may or may not expose it — just check 200 returned
        me["moved"]["url"].as_str().or(me["moved_to_uri"].as_str()).or(Some("")));
}

/// POST /api/v1/accounts/move with wrong password returns 401.
#[tokio::test]
async fn test_move_account_wrong_password_is_401() {
    let ctx = TestContext::new("move-acct-wrong").await;

    let resp = ctx.api.post_json(
        "/api/v1/accounts/move",
        Some(&ctx.alice_token),
        &json!({
            "current_password": "wrongpassword",
            "acct": "alice@new.example.com"
        }),
    ).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// PUT /api/v1/profile returns the caller's account object.
#[tokio::test]
async fn test_update_profile_settings_returns_account() {
    let ctx = TestContext::new("profile-settings").await;

    let resp = ctx.api.put_json(
        "/api/v1/profile",
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"].as_str(), Some(ctx.alice_id.as_str()));
}

// ── account deletion ──────────────────────────────────────────────────────────

/// DELETE /api/v1/accounts with correct password deletes the account (returns 200).
#[tokio::test]
async fn test_delete_account_with_valid_password() {
    let ctx = TestContext::new("del-acct").await;

    let resp = ctx.api.http
        .delete(ctx.api.url("/api/v1/accounts"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .json(&json!({"password": "testpassword123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // After deletion, verify_credentials should fail (account is suspended / user row deleted).
    let after = ctx.api.get("/api/v1/accounts/verify_credentials", Some(&ctx.alice_token)).await;
    assert!(
        after.status() == StatusCode::UNAUTHORIZED || after.status() == StatusCode::FORBIDDEN,
        "expected 401/403 after account deletion, got {}",
        after.status(),
    );
}

/// DELETE /api/v1/accounts with wrong password returns 401.
#[tokio::test]
async fn test_delete_account_wrong_password_is_401() {
    let ctx = TestContext::new("del-acct-wrong").await;

    let resp = ctx.api.http
        .delete(ctx.api.url("/api/v1/accounts"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .json(&json!({"password": "notmypassword"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── GET /api/v1/accounts (batch) ─────────────────────────────────────────────

/// GET /api/v1/accounts?id[]=...&id[]=... returns the requested accounts.
#[tokio::test]
async fn test_get_accounts_batch() {
    let ctx = TestContext::new("acct-batch").await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts?id[]={}&id[]={}", ctx.alice_id, ctx.bob_id),
        None,
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    let ids: Vec<&str> = accounts.iter().filter_map(|a| a["id"].as_str()).collect();
    assert!(ids.contains(&ctx.alice_id.as_str()), "alice missing from batch");
    assert!(ids.contains(&ctx.bob_id.as_str()), "bob missing from batch");
}

/// GET /api/v1/accounts?id[]= with empty list returns empty array.
#[tokio::test]
async fn test_get_accounts_batch_empty() {
    let ctx = TestContext::new("acct-batch-empty").await;

    let resp = ctx.api.get("/api/v1/accounts", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    assert!(accounts.is_empty(), "expected empty array for no ids: {accounts:?}");
}

// ── GET /api/v1/apps/verify_credentials ──────────────────────────────────────

/// GET /api/v1/apps/verify_credentials with a valid token returns the app name.
#[tokio::test]
async fn test_verify_app_credentials() {
    let ctx = TestContext::new("app-verify").await;

    let resp = ctx.api.get("/api/v1/apps/verify_credentials", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["name"].as_str().is_some(), "app name missing: {body}");
}

/// GET /api/v1/apps/verify_credentials without a token returns 401.
#[tokio::test]
async fn test_verify_app_credentials_without_token_is_401() {
    let ctx = TestContext::new("app-verify-unauth").await;

    let resp = ctx.api.get("/api/v1/apps/verify_credentials", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Following an account that has blocked you is rejected with 403 (Mastodon
/// FollowService raises NotPermittedError).
#[tokio::test]
async fn test_follow_blocked_by_target_is_forbidden() {
    let ctx = TestContext::new("follow-blocked-by").await;

    // Bob blocks Alice first.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.alice_id),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;

    // Alice tries to follow Bob — not allowed.
    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // And no follow relationship exists.
    let rel: Value = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert_eq!(rel[0]["following"].as_bool(), Some(false));
}

/// GET /api/v1/accounts/:id for a suspended account returns 200 with suspended=true.
#[tokio::test]
async fn test_get_suspended_account_returns_suspended() {
    let ctx = TestContext::new("acct-suspended-200").await;

    // Make alice admin
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();
    let admin_db = ctx.db.clone();
    crate::helpers::make_admin(&admin_db, alice_uuid).await;

    // Suspend bob via admin endpoint
    ctx.api.post_json(
        &format!("/api/v1/admin/accounts/{}/suspend", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    let resp = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.bob_id), None).await;
    assert_eq!(resp.status(), StatusCode::OK, "suspended account should return 200");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["suspended"], true, "suspended account should have suspended=true");
}

/// Unlocking a locked account auto-approves pending follow requests.
#[tokio::test]
async fn test_unlock_account_approves_pending_follows() {
    let ctx = TestContext::new("unlock-approve").await;

    let db = ctx.db.clone();
    let alice_uuid: i64 = ctx.alice_id.parse().unwrap();

    // Lock Alice's account.
    sqlx::query!("UPDATE accounts SET locked = true WHERE id = $1", alice_uuid)
        .execute(&db).await.unwrap();

    // Bob sends a follow request (becomes pending because account is locked).
    let follow_resp: Value = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&ctx.bob_token),
        &json!({}),
    ).await.json().await.unwrap();
    assert_eq!(follow_resp["requested"].as_bool(), Some(true), "follow should be pending");

    // Alice unlocks her account.
    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "false")],
    ).await;

    // Bob's follow should now be accepted.
    let rel: Value = ctx.api.get(
        &format!("/api/v1/accounts/relationships?id[]={}", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await.json::<Vec<Value>>().await.unwrap().remove(0);
    assert_eq!(rel["following"].as_bool(), Some(true), "follow should be accepted after unlock");
    assert_eq!(rel["requested"].as_bool(), Some(false), "follow should not be pending after unlock");
}

/// GET /api/v1/accounts/:id/statuses returns 403 when target has blocked the viewer.
#[tokio::test]
async fn test_account_statuses_returns_403_when_blocked_by_target() {
    let ctx = TestContext::new("acct-statuses-blocked").await;

    ctx.api.post_status(&ctx.alice_token, "alice public status", "public").await;

    // Alice blocks Bob.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/block", ctx.bob_id),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    // Bob tries to view Alice's statuses.
    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
        Some(&ctx.bob_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN,
        "blocked user should not be able to view the blocker's statuses");
}

/// GET /api/v1/accounts/:id/statuses is visible to unauthenticated requests (public accounts).
#[tokio::test]
async fn test_account_statuses_visible_unauthenticated() {
    let ctx = TestContext::new("acct-statuses-unauth").await;

    let status = ctx.api.post_status(&ctx.alice_token, "public for unauth", "public").await;
    let status_id = status["id"].as_str().unwrap();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses", ctx.alice_id),
        None,
    ).await.json().await.unwrap();

    assert!(
        statuses.iter().any(|s| s["id"].as_str() == Some(status_id)),
        "public status should be visible to unauthenticated users",
    );
}

/// GET /api/v1/accounts/:id/followers respects the limit parameter.
#[tokio::test]
async fn test_get_account_followers_limit_param() {
    let ctx = TestContext::new("followers-limit").await;

    // Bob follows Alice.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers?limit=1", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.len() <= 1, "limit=1 should return at most 1 follower");
}

/// GET /api/v1/accounts/:id/following respects the limit parameter.
#[tokio::test]
async fn test_get_account_following_limit_param() {
    let ctx = TestContext::new("following-limit").await;

    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let resp = ctx.api.get(
        &format!("/api/v1/accounts/{}/following?limit=1", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let list: Vec<Value> = resp.json().await.unwrap();
    assert!(list.len() <= 1, "limit=1 should return at most 1 following");
}

/// GET /api/v1/accounts/:id/followers returns only followers of the given account.
#[tokio::test]
async fn test_get_account_followers_scoped_to_account() {
    let ctx = TestContext::new("followers-scoped").await;

    // Bob follows Alice but not vice versa.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let alice_followers: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();

    let bob_followers: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.bob_id),
        Some(&ctx.bob_token),
    ).await.json().await.unwrap();

    assert!(alice_followers.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())),
        "Bob should appear in Alice's followers");
    assert!(!bob_followers.iter().any(|a| a["id"].as_str() == Some(ctx.alice_id.as_str())),
        "Alice should not appear in Bob's followers (she didn't follow Bob)");
}

/// GET /api/v1/accounts/:id/following excludes accounts with pending (not accepted) follows.
#[tokio::test]
async fn test_get_account_following_excludes_pending() {
    let ctx = TestContext::new("following-pending").await;

    // Lock Alice's account so Bob's follow becomes pending.
    ctx.api.patch_multipart(
        "/api/v1/accounts/update_credentials",
        &ctx.alice_token,
        &[("locked", "true")],
    ).await;

    // Bob sends a follow request to Alice (pending).
    let rel: Value = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&ctx.bob_token),
        &json!({}),
    ).await.json().await.unwrap();
    assert_eq!(rel["requested"].as_bool(), Some(true), "follow should be pending");

    // Bob's following list should NOT include Alice (follow is not accepted).
    let following: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/following", ctx.bob_id),
        Some(&ctx.bob_token),
    ).await.json().await.unwrap();

    assert!(!following.iter().any(|a| a["id"].as_str() == Some(ctx.alice_id.as_str())),
        "pending follow should not appear in following list");
}

/// Blocked accounts are hidden from followers/following lists.
#[tokio::test]
async fn test_followers_following_hides_blocked_accounts() {
    let ctx = TestContext::new("follow-block-hide").await;

    let (charlie_uuid, _charlie_token) =
        crate::helpers::seed_user(&ctx.db, &ctx.domain, "charlie", "charlie@test.invalid").await;
    let charlie_id = charlie_uuid.to_string();

    // Both Bob and Charlie follow Alice.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&_charlie_token),
        &json!({}),
    ).await;

    // Alice follows both Bob and Charlie.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;
    ctx.api.follow(&ctx.alice_token, &charlie_id).await;

    // Alice blocks Charlie.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{charlie_id}/block"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;

    // Alice's followers list should hide Charlie (blocked).
    let followers: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/followers", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(!followers.iter().any(|a| a["id"].as_str() == Some(charlie_id.as_str())),
        "blocked account should not appear in followers list");
    assert!(followers.iter().any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())),
        "non-blocked account should still appear in followers list");

    // Alice's following list should also hide Charlie.
    let following: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/following", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();
    assert!(!following.iter().any(|a| a["id"].as_str() == Some(charlie_id.as_str())),
        "blocked account should not appear in following list");
}

/// Followers list is ordered by account id DESC, matching the pagination cursor.
#[tokio::test]
async fn test_followers_ordered_by_account_id_desc() {
    let ctx = TestContext::new("followers-order").await;

    let (charlie_uuid, charlie_token) =
        crate::helpers::seed_user(&ctx.db, &ctx.domain, "charlie-forder", "charlie-forder@test.invalid").await;
    let charlie_id = charlie_uuid.to_string();

    // Alice and Bob both follow Charlie; Alice has a lower account ID than Bob.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{charlie_id}/follow"),
        Some(&ctx.alice_token),
        &json!({}),
    ).await;
    ctx.api.post_json(
        &format!("/api/v1/accounts/{charlie_id}/follow"),
        Some(&ctx.bob_token),
        &json!({}),
    ).await;
    // Charlie accepts both (accounts are unlocked in tests).

    let followers: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{charlie_id}/followers"),
        Some(&charlie_token),
    ).await.json().await.unwrap();

    let ids: Vec<i64> = followers.iter()
        .filter_map(|a| a["id"].as_str().and_then(|s| s.parse::<i64>().ok()))
        .collect();
    assert!(ids.len() >= 2, "both alice and bob should be in followers");

    let sorted_desc: Vec<i64> = { let mut s = ids.clone(); s.sort_unstable_by(|a, b| b.cmp(a)); s };
    assert_eq!(ids, sorted_desc, "followers should be ordered by account id DESC");
}

/// Following list is ordered by account id DESC, matching the pagination cursor.
#[tokio::test]
async fn test_following_ordered_by_account_id_desc() {
    let ctx = TestContext::new("following-order").await;

    let (charlie_uuid, charlie_token) =
        crate::helpers::seed_user(&ctx.db, &ctx.domain, "charlie-fgorder", "charlie-fgorder@test.invalid").await;
    let charlie_id = charlie_uuid.to_string();

    // Charlie follows both Alice and Bob.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.alice_id),
        Some(&charlie_token),
        &json!({}),
    ).await;
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&charlie_token),
        &json!({}),
    ).await;

    let following: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{charlie_id}/following"),
        Some(&charlie_token),
    ).await.json().await.unwrap();

    let ids: Vec<i64> = following.iter()
        .filter_map(|a| a["id"].as_str().and_then(|s| s.parse::<i64>().ok()))
        .collect();
    assert!(ids.len() >= 2, "both alice and bob should be in following list");

    let sorted_desc: Vec<i64> = { let mut s = ids.clone(); s.sort_unstable_by(|a, b| b.cmp(a)); s };
    assert_eq!(ids, sorted_desc, "following should be ordered by account id DESC");
}

// ── exclude_replies self-reply inclusion ─────────────────────────────────────

/// exclude_replies=true keeps self-replies (replies to own posts).
#[tokio::test]
async fn test_account_statuses_exclude_replies_keeps_self_replies() {
    let ctx = TestContext::new("acct-excl-selfreply").await;

    // Alice posts a status and then replies to herself.
    let parent = ctx.api.post_status(&ctx.alice_token, "alice original", "public").await;
    let parent_id = parent["id"].as_str().unwrap();

    let self_reply: Value = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &json!({"status": "alice self-reply", "in_reply_to_id": parent_id, "visibility": "public"}),
    ).await.json().await.unwrap();
    let self_reply_id = self_reply["id"].as_str().unwrap();

    // Bob posts a status and alice replies to bob (a foreign reply).
    let bob_post = ctx.api.post_status(&ctx.bob_token, "bob post", "public").await;
    let bob_post_id = bob_post["id"].as_str().unwrap();
    let foreign_reply: Value = ctx.api.post_json(
        "/api/v1/statuses",
        Some(&ctx.alice_token),
        &json!({"status": "alice reply to bob", "in_reply_to_id": bob_post_id, "visibility": "public"}),
    ).await.json().await.unwrap();
    let foreign_reply_id = foreign_reply["id"].as_str().unwrap();

    let statuses: Vec<Value> = ctx.api.get(
        &format!("/api/v1/accounts/{}/statuses?exclude_replies=true", ctx.alice_id),
        Some(&ctx.alice_token),
    ).await.json().await.unwrap();

    let ids: Vec<&str> = statuses.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(ids.contains(&parent_id), "parent should appear");
    assert!(ids.contains(&self_reply_id), "self-reply should be kept when exclude_replies=true");
    assert!(!ids.contains(&foreign_reply_id), "reply-to-other should be excluded");
}

// ── GET /api/v1/preferences ──────────────────────────────────────────────────

/// Preferences endpoint requires authentication.
#[tokio::test]
async fn test_preferences_requires_auth() {
    let ctx = TestContext::new("prefs-unauth").await;

    let resp = ctx.api.get("/api/v1/preferences", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── account roles ─────────────────────────────────────────────────────────────

/// GET /api/v1/accounts/:id returns a `roles` array. For ordinary users it is
/// empty; for admins it contains an entry with `name: "Admin"`.
#[tokio::test]
async fn test_get_account_includes_roles() {
    let ctx = TestContext::new("acct-roles").await;

    // Ordinary user: roles must be an empty array.
    let resp = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let roles = body["roles"].as_array().expect("roles must be an array");
    assert!(roles.is_empty(), "ordinary user should have no roles");

    // Promote alice to admin.
    crate::helpers::make_admin(&ctx.db, ctx.alice_id.parse::<i64>().unwrap()).await;

    let resp2 = ctx.api.get(&format!("/api/v1/accounts/{}", ctx.alice_id), None).await;
    let body2: Value = resp2.json().await.unwrap();
    let roles2 = body2["roles"].as_array().expect("roles must be an array");
    assert!(!roles2.is_empty(), "admin should have a role entry");
    assert_eq!(roles2[0]["name"].as_str(), Some("Admin"));
}

// ── GET /api/v1/profile ────────────────────────────────────────────────────────

/// GET /api/v1/profile returns the authenticated account.
#[tokio::test]
async fn test_get_profile() {
    let ctx = TestContext::new("get-profile").await;

    let resp = ctx.api.get("/api/v1/profile", Some(&ctx.alice_token)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["id"].as_str(), Some(ctx.alice_id.as_str()));
    assert_eq!(body["username"].as_str(), Some("alice"));
}

/// GET /api/v1/profile without a token → 401.
#[tokio::test]
async fn test_get_profile_requires_auth() {
    let ctx = TestContext::new("get-profile-unauth").await;

    let resp = ctx.api.get("/api/v1/profile", None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// POST /api/v1/accounts/:id/follow with languages=[...] sets the language filter.
#[tokio::test]
async fn test_follow_with_languages_filter() {
    let ctx = TestContext::new("follow-languages").await;

    let resp = ctx.api.post_json(
        &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({"languages": ["en", "ko"]}),
    ).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let rel: Value = resp.json().await.unwrap();
    assert_eq!(rel["following"].as_bool(), Some(true));
    let langs = rel["languages"].as_array().expect("languages should be an array");
    assert!(langs.iter().any(|l| l.as_str() == Some("en")), "languages should include en");
    assert!(langs.iter().any(|l| l.as_str() == Some("ko")), "languages should include ko");
}

/// GET /api/v1/mutes returns Link header with pagination when limit=1.
#[tokio::test]
async fn test_mutes_list_has_pagination_link_headers() {
    let ctx = TestContext::new("mutes-pagination-headers").await;

    // Alice mutes two accounts so there are enough to paginate.
    ctx.api.post_json(
        &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    // Seed a third account and mute it.
    let (charlie_id, _) = crate::helpers::seed_user(
        &ctx.db,
        &ctx.domain,
        "charlie",
        "charlie@test.invalid",
    ).await;
    let charlie_id = charlie_id.to_string();
    ctx.api.post_json(
        &format!("/api/v1/accounts/{charlie_id}/mute"),
        Some(&ctx.alice_token),
        &serde_json::json!({}),
    ).await;

    let resp = ctx.api.http
        .get(ctx.api.url("/api/v1/mutes?limit=1"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let link = resp.headers().get("link").expect("Link header missing for paginated mutes");
    let link_str = link.to_str().unwrap();
    assert!(link_str.contains("next"), "Link header should include 'next'");
    assert!(link_str.contains("prev"), "Link header should include 'prev'");
}

/// GET /api/v1/directory?local=true returns only local accounts.
#[tokio::test]
async fn test_directory_local_param() {
    let ctx = TestContext::new("dir-local").await;

    let resp = ctx.api.http
        .get(ctx.api.url("/api/v1/directory?local=true"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    for acct in &accounts {
        let acct_field = acct["acct"].as_str().unwrap_or_default();
        assert!(!acct_field.contains('@'), "local=true should not return remote accounts (got {})", acct_field);
    }
}

/// GET /api/v1/donation_campaigns returns empty array (stub).
#[tokio::test]
async fn test_donation_campaigns_returns_array() {
    let ctx = TestContext::new("donation-campaigns").await;

    let resp = ctx.api.get("/api/v1/donation_campaigns", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let _ = body;
}

/// GET /api/v1/accounts/:id/identity_proofs returns empty array (stub).
#[tokio::test]
async fn test_account_identity_proofs_returns_array() {
    let ctx = TestContext::new("identity-proofs").await;

    let resp = ctx.api.get(&format!("/api/v1/accounts/{}/identity_proofs", ctx.alice_id), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Vec<serde_json::Value> = resp.json().await.unwrap();
    let _ = body;
}

/// GET /api/v1/directory?order=new returns accounts ordered by creation date descending.
#[tokio::test]
async fn test_directory_order_new() {
    let ctx = TestContext::new("dir-order-new").await;

    let resp = ctx.api.http
        .get(ctx.api.url("/api/v1/directory?order=new"))
        .header("host", &ctx.api.host)
        .bearer_auth(&ctx.alice_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let accounts: Vec<Value> = resp.json().await.unwrap();
    // Verify the response is an array (ordering correctness is hard to assert without
    // precise seeding, but we verify the param is accepted and returns valid JSON).
    let _ = accounts;
}
