//! Builders for the FEP-style consent handshake (feder's reusable pattern).
//!
//! A requester sends a `*Request`; the target replies with an `Accept` carrying
//! a `result` authorization URI, or a `Reject`. The same pattern backs quote
//! posts (`QuoteRequest`/`QuoteAuthorization`) and account features
//! (`FeatureRequest`/`FeatureAuthorization`). These helpers serialize feder's
//! typed consent vocabulary into delivery-ready JSON.

use anyhow::Context as _;
use feder_vocab as vocab;
use serde_json::Value;
use vocab::{AuthorizationType, Iri, Reference, RequestType};

fn iri(s: &str) -> anyhow::Result<Iri> {
    s.parse::<Iri>()
        .map_err(|e| anyhow::anyhow!("invalid ActivityPub IRI {s:?}: {e}"))
}

fn to_value<T: serde::Serialize>(v: &T) -> anyhow::Result<Value> {
    serde_json::to_value(v).context("serialize consent activity")
}

/// Build a `FeatureRequest` (collection owner asks a remote account for consent
/// to be featured). `id` is the request activity URI, `account` the account to
/// feature, `collection` the collection doing the featuring.
pub fn feature_request(
    id: &str,
    actor: &str,
    account: &str,
    collection: &str,
) -> anyhow::Result<Value> {
    let mut req = vocab::ConsentRequest::new(
        RequestType::FeatureRequest,
        iri(id)?,
        iri(account)?,
        iri(collection)?,
    );
    req.actor = Some(Reference::id(iri(actor)?));
    to_value(&req)
}

/// Build a `QuoteRequest` (we ask a remote author for consent to quote their
/// post). `id` is the request activity URI, `quoted_status` the post we want to
/// quote, `quoting_status` our quote post.
pub fn quote_request(
    id: &str,
    actor: &str,
    quoted_status: &str,
    quoting_status: &str,
) -> anyhow::Result<Value> {
    let mut req = vocab::ConsentRequest::new(
        RequestType::QuoteRequest,
        iri(id)?,
        iri(quoted_status)?,
        iri(quoting_status)?,
    );
    req.actor = Some(Reference::id(iri(actor)?));
    to_value(&req)
}

/// Build an `Accept` granting a request, pointing `result` at an authorization
/// stamp. `to` is the original requester.
pub fn accept(
    id: &str,
    actor: &str,
    to: &str,
    request_uri: &str,
    authorization_uri: &str,
) -> anyhow::Result<Value> {
    let mut a = vocab::ConsentAccept::new(
        iri(id)?,
        Reference::id(iri(actor)?),
        iri(request_uri)?,
        iri(authorization_uri)?,
    );
    a.to = Some(iri(to)?);
    to_value(&a)
}

/// Build a `Reject` declining a request.
pub fn reject(id: &str, actor: &str, to: &str, request_uri: &str) -> anyhow::Result<Value> {
    let mut r = vocab::ConsentReject::new(iri(id)?, Reference::id(iri(actor)?), iri(request_uri)?);
    r.to = Some(iri(to)?);
    to_value(&r)
}

/// Build a `FeatureAuthorization` stamp object (served at `id`).
pub fn feature_authorization(id: &str, collection: &str, account: &str) -> anyhow::Result<Value> {
    let auth = vocab::Authorization::new(
        AuthorizationType::FeatureAuthorization,
        iri(id)?,
        iri(collection)?,
        iri(account)?,
    );
    to_value(&auth)
}

/// Build a `QuoteAuthorization` stamp object (served at `id`).
pub fn quote_authorization(
    id: &str,
    quoted_account: &str,
    quoting_status: &str,
    quoted_status: &str,
) -> anyhow::Result<Value> {
    let mut auth = vocab::Authorization::new(
        AuthorizationType::QuoteAuthorization,
        iri(id)?,
        iri(quoting_status)?,
        iri(quoted_status)?,
    );
    auth.attributed_to = Some(Reference::id(iri(quoted_account)?));
    to_value(&auth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn feature_request_shape() {
        let v = feature_request(
            "https://a.test/users/alice/feature_requests/1",
            "https://a.test/users/alice",
            "https://b.test/users/bob",
            "https://a.test/collections/1",
        )
        .unwrap();
        assert_eq!(v["type"], "FeatureRequest");
        assert_eq!(v["actor"], "https://a.test/users/alice");
        assert_eq!(v["object"], "https://b.test/users/bob");
        assert_eq!(v["instrument"], "https://a.test/collections/1");
    }

    #[test]
    fn accept_carries_result_and_to() {
        let v = accept(
            "https://b.test/users/bob#accepts/feature_requests/1",
            "https://b.test/users/bob",
            "https://a.test/users/alice",
            "https://a.test/users/alice/feature_requests/1",
            "https://b.test/users/bob/feature_authorizations/1",
        )
        .unwrap();
        assert_eq!(v["type"], "Accept");
        assert_eq!(v["to"], "https://a.test/users/alice");
        assert_eq!(v["object"], "https://a.test/users/alice/feature_requests/1");
        assert_eq!(
            v["result"],
            "https://b.test/users/bob/feature_authorizations/1"
        );
    }

    #[test]
    fn authorizations_have_correct_markers() {
        let f = feature_authorization(
            "https://b.test/users/bob/feature_authorizations/1",
            "https://a.test/collections/1",
            "https://b.test/users/bob",
        )
        .unwrap();
        assert_eq!(f["type"], "FeatureAuthorization");
        assert_eq!(f["interactingObject"], "https://a.test/collections/1");
        assert_eq!(f["interactionTarget"], "https://b.test/users/bob");
        assert!(f.get("attributedTo").is_none());

        let q = quote_authorization(
            "https://b.test/notes/9/approvals/1",
            "https://b.test/users/bob",
            "https://a.test/notes/1",
            "https://b.test/notes/9",
        )
        .unwrap();
        assert_eq!(q["type"], "QuoteAuthorization");
        assert_eq!(q["attributedTo"], "https://b.test/users/bob");
        assert_eq!(
            q,
            json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "type": "QuoteAuthorization",
                "id": "https://b.test/notes/9/approvals/1",
                "attributedTo": "https://b.test/users/bob",
                "interactingObject": "https://a.test/notes/1",
                "interactionTarget": "https://b.test/notes/9",
            })
        );
    }
}
