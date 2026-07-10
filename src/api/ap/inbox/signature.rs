//! Authentication of inbound ActivityPub activities: HTTP Signature
//! verification (with body-digest and clock-skew enforcement) and the FEP-8b32
//! Object Integrity Proof fallback, plus the public-key fetch/refresh cache.

use serde_json::Value;

use crate::state::AppState;

use super::same_host;

/// The lower-cased list of header names a `Signature` header claims to cover
/// (its `headers="…"` parameter). Empty when the parameter is absent.
fn signed_headers_list(sig_val: &str) -> Vec<String> {
    for part in sig_val.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("headers=") {
            return rest
                .trim_matches('"')
                .split_whitespace()
                .map(|s| s.to_ascii_lowercase())
                .collect();
        }
    }
    Vec::new()
}

/// True if an HTTP-date `Date` header is within `skew` of now (in either
/// direction). Rejects malformed dates.
fn date_within_skew(date_val: &str, skew: chrono::Duration) -> bool {
    let trimmed = date_val.trim();
    let parsed = chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT")
        .map(|n| n.and_utc())
        .or_else(|_| {
            chrono::DateTime::parse_from_rfc2822(trimmed).map(|d| d.with_timezone(&chrono::Utc))
        });
    match parsed {
        Ok(signed) => (chrono::Utc::now() - signed).abs() <= skew,
        Err(_) => false,
    }
}

/// Verify the HTTP Signature on an inbound activity. Returns `Ok(())` only when
/// the request is signed by a key belonging to the same host as the claimed
/// actor and the signature (and body digest) check out.
pub(super) async fn verify_inbound_signature(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    path: &str,
    body: &[u8],
    actor_uri: &str,
) -> Result<(), String> {
    let sig_val = headers
        .get("signature")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| "missing Signature header".to_string())?;

    let kid = crate::federation::signature::key_id_from_header(sig_val)
        .ok_or_else(|| "no keyId in Signature header".to_string())?;
    let key_actor = kid.split('#').next().unwrap_or(kid);

    // The signing key must belong to the same host as the activity's actor, so a
    // valid signature from one server cannot authorize an activity attributed to
    // an actor on another server.
    if !actor_uri.is_empty() && !same_host(key_actor, actor_uri) {
        return Err(format!(
            "signing key host does not match actor ({key_actor} vs {actor_uri})"
        ));
    }

    // feder's verify_request only checks the Digest if a Digest header happens to
    // be present, and never bounds the signature's age. Without the following an
    // attacker could sign only `date` and then swap the body (recomputing the
    // Digest header that the signature never covered), or replay a captured
    // request indefinitely. So, before doing any work:
    //   * require the signature to actually cover (request-target), host, date,
    //     and digest;
    //   * require a Digest header (feder then binds it to the body);
    //   * reject stale or future-dated requests (±1h skew, as Mastodon does).
    let covered = signed_headers_list(sig_val);
    for required in ["(request-target)", "host", "date", "digest"] {
        if !covered.iter().any(|h| h == required) {
            return Err(format!(
                "signature does not cover required header {required:?}"
            ));
        }
    }
    if headers.get("digest").is_none() {
        return Err("missing Digest header".to_string());
    }
    let date_val = headers
        .get("date")
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| "missing Date header".to_string())?;
    if !date_within_skew(date_val, chrono::Duration::hours(1)) {
        return Err(format!(
            "Date header outside acceptable clock skew: {date_val:?}"
        ));
    }

    let pem = fetch_public_key(state, key_actor)
        .await
        .map_err(|e| format!("could not fetch public key: {e}"))?;

    let hdr_vec = crate::federation::signature::headers_to_vec(headers);
    let hdr_refs: Vec<(&str, &str)> = hdr_vec
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    match crate::federation::signature::verify_request("post", path, &hdr_refs, body, &pem) {
        Ok(()) => Ok(()),
        Err(first_err) => {
            let refreshed_pem = refresh_public_key(state, key_actor)
                .await
                .map_err(|e| format!("could not refresh public key: {e}"))?;
            crate::federation::signature::verify_request(
                "post",
                path,
                &hdr_refs,
                body,
                &refreshed_pem,
            )
            .map_err(|e| format!("{first_err}; after key refresh: {e}"))
        }
    }
}

/// Authenticate an inbound activity via its FEP-8b32 Object Integrity Proof.
///
/// Used as a fallback when the HTTP Signature can't be verified. The proof's
/// `verificationMethod` must live on the actor's host and be declared by the
/// actor as one of its `assertionMethod` keys, which binds the signing key to
/// the claimed actor.
pub(super) async fn verify_object_integrity(
    state: &AppState,
    activity: &serde_json::Value,
    actor_uri: &str,
) -> Result<(), String> {
    let (proof, verification_method) =
        feder_runtime::integrity::extract_integrity_proof(activity)
            .ok_or_else(|| "no eddsa-jcs-2022 integrity proof".to_string())?;

    // The signing key must belong to the same host as the actor.
    if actor_uri.is_empty() || !same_host(&verification_method, actor_uri) {
        return Err(format!(
            "proof key host does not match actor ({verification_method} vs {actor_uri})"
        ));
    }

    // Fetch the actor document and confirm it declares this verification method
    // as an assertionMethod, then read the Ed25519 public key.
    let actor_doc = crate::federation::fetch::signed_get_json(state, actor_uri)
        .await
        .map_err(|e| format!("could not fetch actor for proof key: {e}"))?;
    let multibase = assertion_method_key(&actor_doc, &verification_method)
        .ok_or_else(|| format!("actor does not declare assertionMethod {verification_method}"))?;
    let public_key = feder_runtime::integrity::decode_ed25519_multikey(&multibase)
        .map_err(|e| format!("invalid assertionMethod key: {e}"))?;

    feder_runtime::integrity::verify_object_integrity_proof(activity, &proof, &public_key)
        .map_err(|e| e.to_string())
}

/// Find the `publicKeyMultibase` of the `assertionMethod` whose id equals
/// `verification_method` in a fetched actor document. Handles the field being a
/// single Multikey object or an array of them.
fn assertion_method_key(actor: &serde_json::Value, verification_method: &str) -> Option<String> {
    let methods = actor.get("assertionMethod")?;
    let entries: Vec<&serde_json::Value> = match methods {
        serde_json::Value::Array(a) => a.iter().collect(),
        obj @ serde_json::Value::Object(_) => vec![obj],
        _ => return None,
    };
    entries.into_iter().find_map(|m| {
        if m.get("id").and_then(|v| v.as_str()) == Some(verification_method) {
            m.get("publicKeyMultibase")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        } else {
            None
        }
    })
}

async fn fetch_public_key(state: &AppState, actor_url: &str) -> anyhow::Result<String> {
    if let Some(pem) = sqlx::query_scalar!(
        "SELECT public_key FROM accounts WHERE uri = $1 AND public_key != ''",
        actor_url,
    )
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(pem);
    }

    refresh_public_key(state, actor_url).await
}

async fn refresh_public_key(state: &AppState, actor_url: &str) -> anyhow::Result<String> {
    let actor = crate::federation::fetch::signed_get_json(state, actor_url).await?;
    let pem = public_key_from_actor(&actor)?;

    sqlx::query!(
        "UPDATE accounts SET public_key = $2, updated_at = now() WHERE uri = $1 AND domain IS NOT NULL",
        actor_url,
        pem,
    )
    .execute(&state.db)
    .await?;

    Ok(pem)
}

fn public_key_from_actor(actor: &Value) -> anyhow::Result<String> {
    Ok(actor
        .get("publicKey")
        .and_then(|k| k.get("publicKeyPem"))
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow::anyhow!("no publicKeyPem"))?
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_headers_list_parses_covered_set() {
        let sig = r#"keyId="https://a.test/users/x#main-key",algorithm="rsa-sha256",headers="(request-target) Host Date Digest",signature="abc""#;
        let covered = signed_headers_list(sig);
        assert!(covered.contains(&"(request-target)".to_string()));
        assert!(covered.contains(&"host".to_string()));
        assert!(covered.contains(&"date".to_string()));
        assert!(covered.contains(&"digest".to_string()));
        assert!(signed_headers_list("keyId=\"x\",signature=\"y\"").is_empty());
    }

    #[test]
    fn date_within_skew_accepts_recent_and_rejects_stale() {
        let now = chrono::Utc::now();
        let fmt = "%a, %d %b %Y %H:%M:%S GMT";
        let recent = now.format(fmt).to_string();
        let stale = (now - chrono::Duration::hours(2)).format(fmt).to_string();
        let future = (now + chrono::Duration::hours(2)).format(fmt).to_string();
        assert!(date_within_skew(&recent, chrono::Duration::hours(1)));
        assert!(!date_within_skew(&stale, chrono::Duration::hours(1)));
        assert!(!date_within_skew(&future, chrono::Duration::hours(1)));
        assert!(!date_within_skew("garbage", chrono::Duration::hours(1)));
    }
}
