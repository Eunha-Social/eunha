use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

/// Helper: post a status with a poll and return the status JSON.
async fn post_poll_status(ctx: &TestContext) -> Value {
    ctx.api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.alice_token),
            &json!({
                "status": "Which do you prefer?",
                "visibility": "public",
                "poll": {
                    "options": ["Cats", "Dogs"],
                    "expires_in": 86400,
                    "multiple": false
                }
            }),
        )
        .await
        .json()
        .await
        .unwrap()
}

/// GET /api/v1/polls/:id returns the poll data.
#[tokio::test]
async fn test_poll_get() {
    let ctx = TestContext::new("poll-get").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().expect("poll.id missing");

    let resp = ctx
        .api
        .get(
            &format!("/api/v1/polls/{}", poll_id),
            Some(&ctx.alice_token),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let poll: Value = resp.json().await.unwrap();
    assert_eq!(poll["id"].as_str(), Some(poll_id));
    let options = poll["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options[0]["title"].as_str(), Some("Cats"));
    assert_eq!(options[1]["title"].as_str(), Some("Dogs"));
}

/// GET /api/v1/polls/:id for nonexistent id returns 404.
#[tokio::test]
async fn test_poll_get_not_found() {
    let ctx = TestContext::new("poll-get-404").await;

    let resp = ctx
        .api
        .get("/api/v1/polls/999999999", Some(&ctx.alice_token))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// POST /api/v1/polls/:id/votes casts a vote and reflects it in the poll.
#[tokio::test]
async fn test_poll_vote() {
    let ctx = TestContext::new("poll-vote").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().unwrap();

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [0] }),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK, "vote should succeed");
    let poll: Value = resp.json().await.unwrap();
    assert_eq!(poll["votes_count"].as_i64(), Some(1));
    let options = poll["options"].as_array().unwrap();
    assert_eq!(options[0]["votes_count"].as_i64(), Some(1));
    assert_eq!(options[1]["votes_count"].as_i64(), Some(0));
}

/// Voting on a poll you already voted in returns 422.
#[tokio::test]
async fn test_poll_vote_duplicate() {
    let ctx = TestContext::new("poll-vote-dup").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().unwrap();

    ctx.api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [0] }),
        )
        .await;

    let second = ctx
        .api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [1] }),
        )
        .await;
    assert_eq!(
        second.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "duplicate vote should be 422"
    );
}

/// Voting with an out-of-range choice index returns 422.
#[tokio::test]
async fn test_poll_vote_invalid_choice() {
    let ctx = TestContext::new("poll-vote-invalid").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().unwrap();

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [99] }),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid choice should be 422"
    );
}

/// The poll author cannot vote on their own poll.
#[tokio::test]
async fn test_poll_owner_cannot_vote() {
    let ctx = TestContext::new("poll-owner-vote").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().unwrap();

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.alice_token),
            &json!({ "choices": [0] }),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "owner voting on own poll should be 422"
    );
}

/// A multiple-choice poll accepts multiple selections.
#[tokio::test]
async fn test_poll_multiple_choice_vote() {
    let ctx = TestContext::new("poll-multi").await;

    let status: Value = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.alice_token),
            &json!({
                "status": "Pick all that apply",
                "visibility": "public",
                "poll": {
                    "options": ["Red", "Green", "Blue"],
                    "expires_in": 86400,
                    "multiple": true
                }
            }),
        )
        .await
        .json()
        .await
        .unwrap();
    let poll_id = status["poll"]["id"].as_str().unwrap();

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [0, 2] }),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "multi-choice vote should succeed"
    );
    let poll: Value = resp.json().await.unwrap();
    assert_eq!(poll["votes_count"].as_i64(), Some(2));
}

/// Creating a status with a poll requires at least 2 options.
#[tokio::test]
async fn test_poll_create_requires_two_options() {
    let ctx = TestContext::new("poll-min-opts").await;

    let resp = ctx
        .api
        .post_json(
            "/api/v1/statuses",
            Some(&ctx.alice_token),
            &json!({
                "status": "Bad poll",
                "poll": {
                    "options": ["Only one"],
                    "expires_in": 86400
                }
            }),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "single-option poll should be 422"
    );
}

/// Poll `voted` field reflects whether the authenticated user has voted.
#[tokio::test]
async fn test_poll_voted_field() {
    let ctx = TestContext::new("poll-voted").await;
    let status: Value = post_poll_status(&ctx).await;
    let poll_id = status["poll"]["id"].as_str().unwrap();

    // Before voting: voted should be false.
    let before: Value = ctx
        .api
        .get(&format!("/api/v1/polls/{}", poll_id), Some(&ctx.bob_token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(before["voted"].as_bool(), Some(false));

    // Cast vote.
    ctx.api
        .post_json(
            &format!("/api/v1/polls/{}/votes", poll_id),
            Some(&ctx.bob_token),
            &json!({ "choices": [1] }),
        )
        .await;

    // After voting: voted should be true and own_votes should list choice.
    let after: Value = ctx
        .api
        .get(&format!("/api/v1/polls/{}", poll_id), Some(&ctx.bob_token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(after["voted"].as_bool(), Some(true));
    let own = after["own_votes"].as_array().unwrap();
    assert!(own.iter().any(|v| v.as_i64() == Some(1)));
}
