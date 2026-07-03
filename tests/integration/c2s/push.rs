use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

fn fake_sub_payload(endpoint: &str) -> serde_json::Value {
    json!({
        "subscription": {
            "endpoint": endpoint,
            "keys": {
                "p256dh": "BNcRdreALRFXTkOOUHK1EtK2wtZ5MRe5dvXNkbmkjfGAaLfMIRyWTa8dFbGFnO2hFmPbq3bWI4_4lCLi0bJkLY=",
                "auth": "tBHItJI5svbpez7KI4CCXg=="
            }
        },
        "data": {
            "alerts": {
                "follow": true,
                "favourite": false,
                "reblog": true,
                "mention": true,
                "poll": false,
                "status": false
            },
            "policy": "all"
        }
    })
}

/// Full push subscription lifecycle: create → get → update → delete.
#[tokio::test]
async fn test_push_subscription_lifecycle() {
    let ctx = TestContext::new("push-lifecycle").await;

    // Create subscription.
    let create_resp = ctx
        .api
        .post_json(
            "/api/v1/push/subscription",
            Some(&ctx.alice_token),
            &fake_sub_payload("https://push.example.com/test-endpoint"),
        )
        .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let sub: Value = create_resp.json().await.unwrap();
    assert!(sub["id"].as_str().is_some(), "id missing");
    assert_eq!(
        sub["endpoint"].as_str(),
        Some("https://push.example.com/test-endpoint")
    );
    assert_eq!(sub["alerts"]["follow"].as_bool(), Some(true));
    assert_eq!(sub["alerts"]["favourite"].as_bool(), Some(false));
    assert!(sub["server_key"].as_str().is_some(), "server_key missing");

    // GET returns the same subscription.
    let get_resp = ctx
        .api
        .get("/api/v1/push/subscription", Some(&ctx.alice_token))
        .await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let got: Value = get_resp.json().await.unwrap();
    assert_eq!(got["id"].as_str(), sub["id"].as_str());
    assert_eq!(
        got["endpoint"].as_str(),
        Some("https://push.example.com/test-endpoint")
    );

    // PUT updates alert settings.
    let update_resp = ctx
        .api
        .put_json(
            "/api/v1/push/subscription",
            Some(&ctx.alice_token),
            &json!({
                "data": {
                    "alerts": {"follow": false, "favourite": true},
                    "policy": "followed"
                }
            }),
        )
        .await;
    assert_eq!(update_resp.status(), StatusCode::OK);
    let updated: Value = update_resp.json().await.unwrap();
    assert_eq!(updated["alerts"]["follow"].as_bool(), Some(false));
    assert_eq!(updated["alerts"]["favourite"].as_bool(), Some(true));
    assert_eq!(updated["policy"].as_str(), Some("followed"));

    // DELETE removes the subscription.
    let del_resp = ctx
        .api
        .delete("/api/v1/push/subscription", &ctx.alice_token)
        .await;
    assert_eq!(del_resp.status(), StatusCode::OK);

    // GET now returns 404.
    let after_del = ctx
        .api
        .get("/api/v1/push/subscription", Some(&ctx.alice_token))
        .await;
    assert_eq!(after_del.status(), StatusCode::NOT_FOUND);
}

/// POST /api/v1/push/subscription is idempotent: second POST for the same token replaces the first.
#[tokio::test]
async fn test_push_subscription_idempotent() {
    let ctx = TestContext::new("push-idem").await;

    ctx.api
        .post_json(
            "/api/v1/push/subscription",
            Some(&ctx.alice_token),
            &fake_sub_payload("https://push.example.com/first"),
        )
        .await;

    let second = ctx
        .api
        .post_json(
            "/api/v1/push/subscription",
            Some(&ctx.alice_token),
            &fake_sub_payload("https://push.example.com/second"),
        )
        .await;
    assert_eq!(second.status(), StatusCode::OK);

    // GET should return the second endpoint.
    let got: Value = ctx
        .api
        .get("/api/v1/push/subscription", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        got["endpoint"].as_str(),
        Some("https://push.example.com/second")
    );
}

/// GET /api/v1/push/subscription returns 404 when no subscription exists.
#[tokio::test]
async fn test_push_subscription_get_when_none() {
    let ctx = TestContext::new("push-get-none").await;

    let resp = ctx
        .api
        .get("/api/v1/push/subscription", Some(&ctx.alice_token))
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
