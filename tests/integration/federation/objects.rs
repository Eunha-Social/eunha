//! Outbound dereferencing surfaces: individual status Notes, the followers /
//! following collections, and the featured (pinned) collection.

use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

/// Post a status as alice and return its id.
async fn post_status(ctx: &TestContext, body: &Value) -> String {
    let resp = ctx
        .api
        .post_json("/api/v1/statuses", Some(&ctx.alice_token), body)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let status: Value = resp.json().await.unwrap();
    status["id"].as_str().expect("status id").to_string()
}

/// GET /users/{u}/statuses/{id} serves a Note; /activity serves the Create.
#[tokio::test]
async fn test_status_served_as_ap_note() {
    let ctx = TestContext::new("ap-note").await;
    let id = post_status(
        &ctx,
        &json!({ "status": "hello #world", "visibility": "public" }),
    )
    .await;

    let actor_url = format!("https://{}/users/alice", ctx.domain);
    let note_uri = format!("{actor_url}/statuses/{id}");

    let resp = ctx
        .api
        .get(&format!("/users/alice/statuses/{id}"), None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let note: Value = resp.json().await.unwrap();
    assert_eq!(note["type"].as_str(), Some("Note"));
    assert_eq!(note["id"].as_str(), Some(note_uri.as_str()));
    assert_eq!(note["attributedTo"].as_str(), Some(actor_url.as_str()));
    assert!(note["content"].as_str().unwrap_or("").contains("world"));
    assert!(
        note["to"].as_array().map_or(false, |a| a
            .iter()
            .any(|v| v.as_str() == Some("https://www.w3.org/ns/activitystreams#Public"))),
        "public note should address the Public collection: {note}"
    );
    // The hashtag is carried in the tag array for remote indexing.
    let has_hashtag = note["tag"].as_array().map_or(false, |a| {
        a.iter()
            .any(|t| t["type"].as_str() == Some("Hashtag") && t["name"].as_str() == Some("#world"))
    });
    assert!(has_hashtag, "expected a Hashtag tag: {note}");

    // The /activity wrapper is a Create around the same Note.
    let resp = ctx
        .api
        .get(&format!("/users/alice/statuses/{id}/activity"), None)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let create: Value = resp.json().await.unwrap();
    assert_eq!(create["type"].as_str(), Some("Create"));
    assert_eq!(
        create["id"].as_str(),
        Some(format!("{note_uri}/activity").as_str())
    );
    assert_eq!(create["object"]["id"].as_str(), Some(note_uri.as_str()));
}

/// Private statuses are not served over unauthenticated AP GET.
#[tokio::test]
async fn test_private_status_not_dereferenceable() {
    let ctx = TestContext::new("ap-note-priv").await;
    let id = post_status(
        &ctx,
        &json!({ "status": "secret", "visibility": "private" }),
    )
    .await;

    let resp = ctx
        .api
        .get(&format!("/users/alice/statuses/{id}"), None)
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// followers / following collections reflect a local follow.
#[tokio::test]
async fn test_followers_following_collections() {
    let ctx = TestContext::new("ap-rel").await;

    // alice follows bob.
    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/accounts/{}/follow", ctx.bob_id),
            Some(&ctx.alice_token),
            &json!({}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // bob's followers collection counts alice.
    let followers: Value = ctx
        .api
        .get("/users/bob/followers", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(followers["type"].as_str(), Some("OrderedCollection"));
    assert_eq!(followers["totalItems"].as_i64(), Some(1));

    let page: Value = ctx
        .api
        .get("/users/bob/followers?page=true", None)
        .await
        .json()
        .await
        .unwrap();
    let alice_uri = format!("https://{}/users/alice", ctx.domain);
    assert!(
        page["orderedItems"].as_array().map_or(false, |a| a
            .iter()
            .any(|v| v.as_str() == Some(alice_uri.as_str()))),
        "alice should appear in bob's followers page: {page}"
    );

    // alice's following collection counts bob.
    let following: Value = ctx
        .api
        .get("/users/alice/following", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(following["totalItems"].as_i64(), Some(1));
}

/// The featured collection lists pinned statuses by URI.
#[tokio::test]
async fn test_featured_collection_lists_pins() {
    let ctx = TestContext::new("ap-featured").await;
    let id = post_status(&ctx, &json!({ "status": "pin me", "visibility": "public" })).await;

    let resp = ctx
        .api
        .post_json(
            &format!("/api/v1/statuses/{id}/pin"),
            Some(&ctx.alice_token),
            &json!({}),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let featured: Value = ctx
        .api
        .get("/users/alice/collections/featured", None)
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(featured["type"].as_str(), Some("OrderedCollection"));
    let note_uri = format!("https://{}/users/alice/statuses/{id}", ctx.domain);
    assert!(
        featured["orderedItems"].as_array().map_or(false, |a| a
            .iter()
            .any(|v| v.as_str() == Some(note_uri.as_str()))),
        "pinned status should appear in featured collection: {featured}"
    );
}
