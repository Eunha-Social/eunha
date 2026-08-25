use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::helpers::TestContext;

/// Search by username finds the matching account.
#[tokio::test]
async fn test_search_accounts() {
    let ctx = TestContext::new("search-acct").await;

    let resp = ctx
        .api
        .get(
            "/api/v2/search?q=alice&type=accounts",
            Some(&ctx.alice_token),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    let accounts = body["accounts"].as_array().unwrap();
    assert!(accounts
        .iter()
        .any(|a| a["username"].as_str() == Some("alice")));
}

/// Search for a status by its text returns the matching status.
#[tokio::test]
async fn test_search_statuses() {
    let ctx = TestContext::new("search-status").await;

    ctx.api
        .post_status(&ctx.alice_token, "uniqueterm12345", "public")
        .await;

    let resp = ctx
        .api
        .get(
            "/api/v2/search?q=uniqueterm12345&type=statuses",
            Some(&ctx.alice_token),
        )
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();

    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses.iter().any(|s| s["content"]
            .as_str()
            .unwrap_or("")
            .contains("uniqueterm12345")
            || s["text"].as_str().unwrap_or("").contains("uniqueterm12345")),
        "search did not find status with uniqueterm12345"
    );
}

/// Search without a type param returns accounts, statuses, and hashtags.
#[tokio::test]
async fn test_search_all_types() {
    let ctx = TestContext::new("search-all").await;

    ctx.api
        .post_status(&ctx.alice_token, "searching #alltype999 here", "public")
        .await;

    let body: Value = ctx
        .api
        .get("/api/v2/search?q=alltype999", Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();

    assert!(
        body["accounts"].is_array(),
        "accounts missing from search result"
    );
    assert!(
        body["statuses"].is_array(),
        "statuses missing from search result"
    );
    assert!(
        body["hashtags"].is_array(),
        "hashtags missing from search result"
    );
}

/// Search with limit=1 returns at most one result per category.
#[tokio::test]
async fn test_search_limit_param() {
    let ctx = TestContext::new("search-limit").await;

    ctx.api
        .post_status(&ctx.alice_token, "limitterm888", "public")
        .await;
    ctx.api
        .post_status(&ctx.alice_token, "limitterm888 second", "public")
        .await;

    let body: Value = ctx
        .api
        .get(
            "/api/v2/search?q=limitterm888&type=statuses&limit=1",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();

    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses.len() <= 1,
        "limit=1 should return at most 1 status, got {}",
        statuses.len()
    );
}

/// GET /api/v2/search with offset parameter is accepted (returns 200).
#[tokio::test]
async fn test_search_offset_param_accepted() {
    let ctx = TestContext::new("search-offset").await;

    ctx.api
        .post_status(&ctx.alice_token, "offsetterm777 first", "public")
        .await;

    let resp = ctx
        .api
        .get(
            "/api/v2/search?q=offsetterm777&type=statuses&offset=1",
            Some(&ctx.alice_token),
        )
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "offset param should be accepted"
    );
    let body: Value = resp.json().await.unwrap();
    assert!(body["statuses"].is_array(), "statuses field missing");
}

/// GET /api/v2/search?following=true only returns accounts the viewer follows.
#[tokio::test]
async fn test_search_following_filter() {
    let ctx = TestContext::new("search-following").await;

    // Without following Bob, searching with following=true should return no results.
    let no_follow: Value = ctx
        .api
        .get(
            "/api/v2/search?q=bob&type=accounts&following=true",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        no_follow["accounts"].as_array().unwrap().is_empty(),
        "following=true should return empty when not following bob",
    );

    // Now follow Bob.
    ctx.api.follow(&ctx.alice_token, &ctx.bob_id).await;

    let after_follow: Value = ctx
        .api
        .get(
            "/api/v2/search?q=bob&type=accounts&following=true",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        after_follow["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"].as_str() == Some(ctx.bob_id.as_str())),
        "following=true should include bob after following",
    );
}

/// GET /api/v2/search?type=hashtags finds tags created by posting.
#[tokio::test]
async fn test_search_hashtags() {
    let ctx = TestContext::new("search-hash").await;

    ctx.api
        .post_status(&ctx.alice_token, "I enjoy #searchhash999", "public")
        .await;

    let body: Value = ctx
        .api
        .get(
            "/api/v2/search?q=searchhash999&type=hashtags",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    let hashtags = body["hashtags"].as_array().unwrap();
    assert!(
        hashtags
            .iter()
            .any(|t| t["name"].as_str() == Some("searchhash999")),
        "hashtag not found in search results",
    );
}

/// Search does not return statuses from accounts blocked by or blocking the viewer.
#[tokio::test]
async fn test_search_excludes_blocked_accounts() {
    let ctx = TestContext::new("search-block").await;

    // Bob posts a searchable status.
    ctx.api
        .post_status(&ctx.bob_token, "blocksearchterm42 hello", "public")
        .await;

    // Verify it appears before the block.
    let before: Value = ctx
        .api
        .get(
            "/api/v2/search?q=blocksearchterm42&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        before["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| { s["account"]["id"].as_str() == Some(ctx.bob_id.as_str()) }),
        "bob's status should appear in search before block",
    );

    // Alice blocks Bob.
    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/block", ctx.bob_id),
            Some(&ctx.alice_token),
            &json!({}),
        )
        .await;

    let after: Value = ctx
        .api
        .get(
            "/api/v2/search?q=blocksearchterm42&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        after["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| { s["account"]["id"].as_str() != Some(ctx.bob_id.as_str()) }),
        "blocked account's statuses should not appear in search results",
    );
}

/// Search does not return statuses from muted accounts.
#[tokio::test]
async fn test_search_excludes_muted_accounts() {
    let ctx = TestContext::new("search-mute").await;

    ctx.api
        .post_status(&ctx.bob_token, "mutesearchterm55 hello", "public")
        .await;

    // Verify it appears before the mute.
    let before: Value = ctx
        .api
        .get(
            "/api/v2/search?q=mutesearchterm55&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        before["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| { s["account"]["id"].as_str() == Some(ctx.bob_id.as_str()) }),
        "bob's status should appear in search before mute",
    );

    ctx.api
        .post_json(
            &format!("/api/v1/accounts/{}/mute", ctx.bob_id),
            Some(&ctx.alice_token),
            &json!({}),
        )
        .await;

    let after: Value = ctx
        .api
        .get(
            "/api/v2/search?q=mutesearchterm55&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        after["statuses"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| { s["account"]["id"].as_str() != Some(ctx.bob_id.as_str()) }),
        "muted account's statuses should not appear in search results",
    );
}

/// Viewer's own private and direct statuses appear in their own search results.
#[tokio::test]
async fn test_search_own_private_statuses() {
    let ctx = TestContext::new("search-private").await;

    ctx.api
        .post_status(&ctx.alice_token, "privateuniqueterm11111", "private")
        .await;
    ctx.api
        .post_status(&ctx.alice_token, "directuniqueterm22222", "direct")
        .await;

    // Alice can find her own private status.
    let body: Value = ctx
        .api
        .get(
            "/api/v2/search?q=privateuniqueterm11111&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses.iter().any(|s| s["content"]
            .as_str()
            .unwrap_or("")
            .contains("privateuniqueterm11111")),
        "alice should find her own private status in search results",
    );

    // Alice can find her own direct status.
    let body: Value = ctx
        .api
        .get(
            "/api/v2/search?q=directuniqueterm22222&type=statuses",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses.iter().any(|s| s["content"]
            .as_str()
            .unwrap_or("")
            .contains("directuniqueterm22222")),
        "alice should find her own direct status in search results",
    );

    // Bob cannot find alice's private status.
    let body: Value = ctx
        .api
        .get(
            "/api/v2/search?q=privateuniqueterm11111&type=statuses",
            Some(&ctx.bob_token),
        )
        .await
        .json()
        .await
        .unwrap();
    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses.iter().all(|s| !s["content"]
            .as_str()
            .unwrap_or("")
            .contains("privateuniqueterm11111")),
        "bob should not find alice's private status in search results",
    );
}

/// GET /api/v2/search?account_id= filters statuses to the specified account.
#[tokio::test]
async fn test_search_account_id_filter() {
    let ctx = TestContext::new("search-acct-id").await;

    ctx.api
        .post_status(&ctx.alice_token, "alicesearch uniqueword9876", "public")
        .await;
    ctx.api
        .post_status(&ctx.bob_token, "bobsearch uniqueword9876", "public")
        .await;

    // Search filtered to alice's account should only return alice's status.
    let body: Value = ctx
        .api
        .get(
            &format!(
                "/api/v2/search?q=uniqueword9876&type=statuses&account_id={}",
                ctx.alice_id
            ),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    let statuses = body["statuses"].as_array().unwrap();
    assert!(
        statuses
            .iter()
            .all(|s| s["account"]["id"].as_str() == Some(ctx.alice_id.as_str())),
        "search with account_id should only return that account's statuses, got: {statuses:?}",
    );
    assert!(
        !statuses.is_empty(),
        "alice's status should appear in filtered search",
    );
}

/// Anonymous search cannot paginate (Mastodon returns 401 when offset/min_id/
/// max_id are supplied without a token).
#[tokio::test]
async fn test_search_anonymous_pagination_unauthorized() {
    let ctx = TestContext::new("search-anon-page").await;

    let resp = ctx.api.get("/api/v2/search?q=alice&offset=20", None).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anon pagination must be 401"
    );

    // Without pagination, anonymous search still works.
    let resp = ctx.api.get("/api/v2/search?q=alice", None).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "anon search without pagination should be 200"
    );
}

/// Anonymous search cannot resolve remote resources (Mastodon returns 401 when
/// resolve=true without a token).
#[tokio::test]
async fn test_search_anonymous_resolve_unauthorized() {
    let ctx = TestContext::new("search-anon-resolve").await;

    let resp = ctx
        .api
        .get("/api/v2/search?q=alice&resolve=true", None)
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "anon resolve must be 401"
    );
}

/// A URL is resolved rather than searched for, and a status's own `url`
/// resolves back to it — `ResolveURLService#process_local_url`, which routes a
/// URL on our own domain by its shape instead of fetching it.
#[tokio::test]
async fn test_search_resolves_a_local_status_url() {
    let ctx = TestContext::new("search-url-status").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "findable by its address", "public")
        .await;
    let url = status["url"].as_str().expect("status has a url");

    let body: Value = ctx
        .api
        .get(
            &format!("/api/v2/search?q={url}&resolve=true"),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();

    let statuses = body["statuses"].as_array().unwrap();
    assert_eq!(statuses.len(), 1, "a URL resolves to exactly one thing");
    assert_eq!(statuses[0]["id"], status["id"]);
    assert!(
        body["accounts"].as_array().unwrap().is_empty(),
        "a URL query runs no other search",
    );
}

/// A local status resolves from its ActivityPub URI as well as its web URL:
/// both are routes we serve. The URI is built here rather than read off the
/// status, because `convert::local_domain` is a process-wide `OnceLock` and a
/// test process runs many instances — the serialized `uri` carries whichever
/// domain was initialised first, while `url` comes from the row.
#[tokio::test]
async fn test_search_resolves_a_local_status_uri() {
    let ctx = TestContext::new("search-uri-status").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "findable by its uri", "public")
        .await;
    let id = status["id"].as_str().unwrap();

    // Both actor-URI schemes: `username_ap_id`, and the numeric one whose
    // sub-resources hang off `/ap/users/{account_id}`.
    for uri in [
        format!("https://{}/users/alice/statuses/{id}", ctx.domain),
        format!(
            "https://{}/ap/users/{}/statuses/{id}",
            ctx.domain, ctx.alice_id
        ),
    ] {
        let body: Value = ctx
            .api
            .get(
                &format!("/api/v2/search?q={uri}&resolve=true"),
                Some(&ctx.alice_token),
            )
            .await
            .json()
            .await
            .unwrap();

        assert_eq!(body["statuses"][0]["id"], status["id"], "{uri}");
    }
}

/// A profile URL resolves to the account.
#[tokio::test]
async fn test_search_resolves_a_local_account_url() {
    let ctx = TestContext::new("search-url-acct").await;

    let url = format!("https://{}/@alice", ctx.domain);
    let body: Value = ctx
        .api
        .get(
            &format!("/api/v2/search?q={url}&resolve=true"),
            Some(&ctx.bob_token),
        )
        .await
        .json()
        .await
        .unwrap();

    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0]["id"], ctx.alice_id);
}

/// A local URL that names no status — a profile's sub-page, or an id that is
/// not one — resolves to nothing rather than to whatever the digits happen to
/// match.
#[tokio::test]
async fn test_search_url_that_names_nothing_resolves_to_nothing() {
    let ctx = TestContext::new("search-url-nothing").await;

    for path in ["@alice/following", "@alice/media", "tags/art"] {
        let url = format!("https://{}/{path}", ctx.domain);
        let body: Value = ctx
            .api
            .get(
                &format!("/api/v2/search?q={url}&resolve=true"),
                Some(&ctx.alice_token),
            )
            .await
            .json()
            .await
            .unwrap();
        assert!(
            body["statuses"].as_array().unwrap().is_empty()
                && body["accounts"].as_array().unwrap().is_empty(),
            "{url} should resolve to nothing",
        );
    }
}

/// `SearchService#url_query?` is `@resolve && url?`: without `resolve` a URL is
/// an ordinary query, and full-text search does not match a status by its
/// address.
#[tokio::test]
async fn test_search_url_without_resolve_is_an_ordinary_query() {
    let ctx = TestContext::new("search-url-noresolve").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "not findable by address", "public")
        .await;
    let url = status["url"].as_str().unwrap();

    let body: Value = ctx
        .api
        .get(&format!("/api/v2/search?q={url}"), Some(&ctx.alice_token))
        .await
        .json()
        .await
        .unwrap();

    assert!(
        body["statuses"].as_array().unwrap().is_empty(),
        "an unresolved URL query should not resolve the URL",
    );
}

/// Resolving a URL does not hand over a status its viewer may not see:
/// `ResolveURLService` authorizes what it resolved against the caller.
#[tokio::test]
async fn test_search_url_of_a_private_status_is_authorized() {
    let ctx = TestContext::new("search-url-private").await;

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "for followers only", "private")
        .await;
    let url = status["url"].as_str().unwrap();

    let body: Value = ctx
        .api
        .get(
            &format!("/api/v2/search?q={url}&resolve=true"),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["statuses"][0]["id"], status["id"],
        "the author can resolve her own private status",
    );

    let body: Value = ctx
        .api
        .get(
            &format!("/api/v2/search?q={url}&resolve=true"),
            Some(&ctx.bob_token),
        )
        .await
        .json()
        .await
        .unwrap();
    assert!(
        body["statuses"].as_array().unwrap().is_empty(),
        "a stranger who knows the address still may not read it",
    );
}
