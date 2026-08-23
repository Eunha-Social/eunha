//! Do eunha's API entities carry the fields Mastodon's carry?
//!
//! Eunha aims for behavioural parity, and the most mechanical part of that is
//! shape: a client written against Mastodon reads fields by name, so a missing
//! one breaks it and an unexpected one is a divergence nobody decided on.
//!
//! `mastodon/entities.json` records what each of Mastodon 4.7.0's REST
//! serializers emits, split into fields it always includes and fields it
//! includes conditionally. These tests fetch real responses from a running
//! eunha and compare the two.
//!
//! A missing field is a finding. An extra field is a finding. A conditional
//! field being absent is not — Mastodon omits those too, depending on who is
//! asking.

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use crate::helpers::TestContext;

/// What Mastodon's serializer for one entity emits.
struct Definition {
    always: BTreeSet<String>,
    conditional: BTreeSet<String>,
}

fn definitions() -> HashMap<String, Definition> {
    #[derive(serde::Deserialize)]
    struct Raw {
        always: Vec<String>,
        conditional: Vec<String>,
    }

    let raw: HashMap<String, Value> =
        serde_json::from_str(include_str!("../../../mastodon/entities.json"))
            .expect("mastodon/entities.json is not readable");
    raw.into_iter()
        // `_comment` and anything else underscored documents the file rather
        // than describing an entity.
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| {
            let v: Raw = serde_json::from_value(v)
                .unwrap_or_else(|e| panic!("entity `{k}` is not readable: {e}"));
            (
                k,
                Definition {
                    always: v.always.into_iter().collect(),
                    conditional: v.conditional.into_iter().collect(),
                },
            )
        })
        .collect()
}

/// Compare one response object against the serializer it corresponds to.
fn compare(entity: &str, actual: &Value, findings: &mut Vec<String>) {
    let definitions = definitions();
    let Some(definition) = definitions.get(entity) else {
        findings.push(format!("no Mastodon serializer recorded for `{entity}`"));
        return;
    };
    let Some(object) = actual.as_object() else {
        findings.push(format!("`{entity}` response is not an object"));
        return;
    };

    let present: BTreeSet<String> = object.keys().cloned().collect();

    for field in definition.always.difference(&present) {
        findings.push(format!("{entity}: missing `{field}`"));
    }
    for field in &present {
        if !definition.always.contains(field) && !definition.conditional.contains(field) {
            findings.push(format!("{entity}: unexpected `{field}`"));
        }
    }
}

fn report(findings: Vec<String>) {
    assert!(
        findings.is_empty(),
        "{} entity field difference(s) from Mastodon {}:\n{}",
        findings.len(),
        eunha::version::MASTODON,
        findings
            .iter()
            .map(|f| format!("  {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// `Account`, as returned for another account and for one's own credentials.
/// `verify_credentials` returns an Account with `source` added, so the same
/// field set applies.
#[tokio::test]
async fn test_account_entities_match() {
    let ctx = TestContext::new("parity-account").await;
    let mut findings = Vec::new();

    let account: Value = ctx
        .api
        .get(
            &format!("/api/v1/accounts/{}", ctx.bob_id),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    compare("account", &account, &mut findings);

    let credentials: Value = ctx
        .api
        .get(
            "/api/v1/accounts/verify_credentials",
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    compare("credential_account", &credentials, &mut findings);

    report(findings);
}

/// `Status`, with its nested `MediaAttachment` and `Poll`.
#[tokio::test]
async fn test_status_entities_match() {
    let ctx = TestContext::new("parity-status").await;
    let mut findings = Vec::new();

    let status = ctx
        .api
        .post_status(&ctx.alice_token, "a status to inspect", "public")
        .await;
    compare("status", &status, &mut findings);

    // The account nested inside a status is the same entity.
    compare("account", &status["account"], &mut findings);

    report(findings);
}

/// `Relationship`, which clients poll constantly and read field by field.
#[tokio::test]
async fn test_relationship_entity_matches() {
    let ctx = TestContext::new("parity-relationship").await;
    let mut findings = Vec::new();

    let relationships: Value = ctx
        .api
        .get(
            &format!("/api/v1/accounts/relationships?id[]={}", ctx.bob_id),
            Some(&ctx.alice_token),
        )
        .await
        .json()
        .await
        .unwrap();
    compare("relationship", &relationships[0], &mut findings);

    report(findings);
}

/// The instance description, which every client fetches before anything else.
#[tokio::test]
async fn test_instance_entity_matches() {
    let ctx = TestContext::new("parity-instance").await;
    let mut findings = Vec::new();

    let instance: Value = ctx
        .api
        .get("/api/v2/instance", None)
        .await
        .json()
        .await
        .unwrap();
    compare("instance", &instance, &mut findings);

    report(findings);
}
