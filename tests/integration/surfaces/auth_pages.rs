use reqwest::StatusCode;

use crate::helpers::TestContext;

/// The server-rendered auth pages link the shared SPA-matching stylesheet.
#[tokio::test]
async fn test_auth_pages_use_shared_stylesheet() {
    let ctx = TestContext::new("auth-pages-css").await;

    for path in ["/auth/signup", "/account/login"] {
        let resp = ctx.api.get(path, None).await;
        assert_eq!(resp.status(), StatusCode::OK, "{path} should render");
        let body = resp.text().await.unwrap();
        assert!(
            body.contains("/auth.css"),
            "{path} should link the shared auth stylesheet",
        );
        assert!(
            !body.contains("background:#0f0f0f"),
            "{path} should no longer carry the old hard-coded dark styles",
        );
    }
}
