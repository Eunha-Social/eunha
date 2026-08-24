use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;

use crate::helpers::TestContext;

// ── helpers ───────────────────────────────────────────────────────────────

type WsSink = futures::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;
type WsStream = futures::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn ws_connect(ctx: &TestContext, stream: &str, token: Option<&str>) -> (WsSink, WsStream) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let ws_base = ctx.api.base_url.replace("http://", "ws://");
    let mut query = format!("stream={}", stream);
    if let Some(tok) = token {
        query.push_str("&access_token=");
        query.push_str(tok);
    }
    let url = format!("{}/api/v1/streaming?{}", ws_base, query);

    // Build the request from the URL (which adds the WS handshake headers),
    // then override the Host header so eunha's multi-tenant routing works.
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("host", ctx.domain.parse().unwrap());

    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("WebSocket connection failed");
    ws.split()
}

/// Read the next JSON event from the stream, skipping pings. Times out after 3 s.
async fn next_event(rx: &mut WsStream) -> Option<serde_json::Value> {
    loop {
        match timeout(Duration::from_secs(3), rx.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                return serde_json::from_str(&text).ok();
            }
            Ok(Some(Ok(Message::Ping(_)))) | Ok(Some(Ok(Message::Pong(_)))) => continue,
            _ => return None,
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────

/// A public status posted by alice appears on the `public` stream.
#[tokio::test]
async fn test_streaming_public_receives_new_status() {
    let ctx = TestContext::new("streaming-pub").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "public", None).await;

    // Give the connection a moment to be established server-side.
    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "Hello streaming world", "public")
        .await;

    let event = next_event(&mut rx).await.expect("no event received");
    assert_eq!(event["event"], "update");
    let streams: Vec<&str> = event["stream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(streams.contains(&"public"));

    let payload: serde_json::Value =
        serde_json::from_str(event["payload"].as_str().unwrap()).unwrap();
    assert!(payload["content"]
        .as_str()
        .unwrap()
        .contains("Hello streaming world"));

    let _ = tx.close().await;
}

/// A non-public status does NOT appear on the `public` stream.
#[tokio::test]
async fn test_streaming_public_excludes_unlisted() {
    let ctx = TestContext::new("streaming-pub-excl").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "public", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "This is unlisted", "unlisted")
        .await;

    // Nothing should arrive.
    let event = next_event(&mut rx).await;
    assert!(event.is_none(), "unlisted status leaked onto public stream");

    let _ = tx.close().await;
}

/// `public:local` stream only delivers events from the same instance.
#[tokio::test]
async fn test_streaming_public_local_receives_status() {
    let ctx = TestContext::new("streaming-local").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "public:local", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "Local hello", "public")
        .await;

    let event = next_event(&mut rx).await.expect("no event on public:local");
    assert_eq!(event["event"], "update");
    let streams: Vec<&str> = event["stream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(streams.contains(&"public:local"));

    let _ = tx.close().await;
}

/// `user` stream delivers alice's own status to alice.
#[tokio::test]
async fn test_streaming_user_own_status() {
    let ctx = TestContext::new("streaming-user-own").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "user", Some(&ctx.alice_token)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "User stream test", "public")
        .await;

    let event = next_event(&mut rx).await.expect("no event on user stream");
    assert_eq!(event["event"], "update");
    let streams: Vec<&str> = event["stream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(streams.contains(&"user"));

    // Payload must include viewer-context fields.
    let payload: serde_json::Value =
        serde_json::from_str(event["payload"].as_str().unwrap()).unwrap();
    assert!(
        payload.get("favourited").is_some(),
        "missing favourited in viewer context"
    );
    assert!(
        payload.get("reblogged").is_some(),
        "missing reblogged in viewer context"
    );
    assert!(
        payload.get("bookmarked").is_some(),
        "missing bookmarked in viewer context"
    );

    let _ = tx.close().await;
}

/// `user` stream is auth-gated — unauthenticated connection must not receive events.
#[tokio::test]
async fn test_streaming_user_requires_auth() {
    let ctx = TestContext::new("streaming-user-auth").await;

    // Connect without token; the server should accept the upgrade but drop the
    // subscription because `requires_auth("user")` is true and account_id is None.
    let (mut tx, mut rx) = ws_connect(&ctx, "user", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "Should not appear", "public")
        .await;

    let event = next_event(&mut rx).await;
    assert!(
        event.is_none(),
        "user stream delivered event to unauthenticated connection"
    );

    let _ = tx.close().await;
}

/// Editing a status fires a `status.update` event on the `public` stream.
#[tokio::test]
async fn test_streaming_status_update_event() {
    let ctx = TestContext::new("streaming-edit").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "public", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Post original status.
    let status = ctx
        .api
        .post_status(&ctx.alice_token, "Original text", "public")
        .await;
    let status_id = status["id"].as_str().unwrap();

    // Consume the initial `update` event.
    let first = next_event(&mut rx).await.expect("no initial update event");
    assert_eq!(first["event"], "update");

    // Edit the status.
    let resp = ctx
        .api
        .put_json(
            &format!("/api/v1/statuses/{}", status_id),
            Some(&ctx.alice_token),
            &serde_json::json!({"status": "Edited text", "visibility": "public"}),
        )
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    let event = next_event(&mut rx).await.expect("no status.update event");
    assert_eq!(event["event"], "status.update");

    let _ = tx.close().await;
}

/// Deleting a status fires a `delete` event on the `public` stream.
#[tokio::test]
async fn test_streaming_delete_event() {
    let ctx = TestContext::new("streaming-del").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "public", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "Will be deleted", "public")
        .await;
    let status_id = status["id"].as_str().unwrap();

    // Consume initial update.
    let _upd = next_event(&mut rx).await.expect("no initial update");

    // Delete the status.
    let resp = ctx
        .api
        .delete(&format!("/api/v1/statuses/{}", status_id), &ctx.alice_token)
        .await;
    assert_eq!(resp.status().as_u16(), 200);

    let event = next_event(&mut rx).await.expect("no delete event");
    assert_eq!(event["event"], "delete");
    // The payload for delete is the status id as a plain string (not JSON-encoded object).
    assert_eq!(event["payload"].as_str().unwrap(), status_id);

    let _ = tx.close().await;
}

/// `hashtag:TAG` stream delivers statuses that include the tag.
#[tokio::test]
async fn test_streaming_hashtag() {
    let ctx = TestContext::new("streaming-tag").await;

    let (mut tx, mut rx) = ws_connect(&ctx, "hashtag:rusteunha", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Post with the hashtag.
    ctx.api
        .post_status(&ctx.alice_token, "Hello #rusteunha world", "public")
        .await;

    let event = next_event(&mut rx).await.expect("no hashtag event");
    assert_eq!(event["event"], "update");
    let streams: Vec<&str> = event["stream"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(streams.contains(&"hashtag"));

    // Post without the hashtag — nothing should arrive.
    ctx.api
        .post_status(&ctx.alice_token, "No tag here", "public")
        .await;
    let extra = next_event(&mut rx).await;
    assert!(
        extra.is_none(),
        "status without matching hashtag delivered to hashtag stream"
    );

    let _ = tx.close().await;
}

/// `user:notification` delivers notifications but not regular status updates.
#[tokio::test]
async fn test_streaming_user_notification_stream() {
    let ctx = TestContext::new("streaming-notif").await;

    // Alice subscribes to user:notification.
    let (mut tx, mut rx) = ws_connect(&ctx, "user:notification", Some(&ctx.alice_token)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Bob posts a status — should NOT appear on alice's user:notification stream.
    ctx.api
        .post_status(&ctx.bob_token, "Bob says hi", "public")
        .await;
    let no_event = next_event(&mut rx).await;
    assert!(
        no_event.is_none(),
        "status update leaked to user:notification stream"
    );

    // Bob follows alice — alice gets a notification.
    ctx.api.follow(&ctx.bob_token, &ctx.alice_id).await;

    let event = next_event(&mut rx).await.expect("no notification event");
    assert_eq!(event["event"], "notification");
    let payload: serde_json::Value =
        serde_json::from_str(event["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["type"], "follow");

    let _ = tx.close().await;
}

/// Subscribe / unsubscribe multiplexing over a single connection.
#[tokio::test]
async fn test_streaming_subscribe_unsubscribe() {
    let ctx = TestContext::new("streaming-mux").await;

    // Connect without any initial stream.
    let (mut tx, mut rx) = ws_connect(&ctx, "", None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Dynamically subscribe to public:local.
    tx.send(Message::Text(
        serde_json::json!({"type": "subscribe", "stream": "public:local"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "Mux test", "public")
        .await;

    let event = next_event(&mut rx)
        .await
        .expect("no event after dynamic subscribe");
    assert_eq!(event["event"], "update");

    // Unsubscribe.
    tx.send(Message::Text(
        serde_json::json!({"type": "unsubscribe", "stream": "public:local"})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    ctx.api
        .post_status(&ctx.alice_token, "After unsubscribe", "public")
        .await;
    let gone = next_event(&mut rx).await;
    assert!(gone.is_none(), "event delivered after unsubscribe");

    let _ = tx.close().await;
}
