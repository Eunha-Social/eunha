//! Outbound dereferencing of remote ActivityPub objects with HTTP Signatures.
//!
//! GETs are signed with the instance actor's key so that servers running in
//! authorized-fetch (secure) mode will serve the object. Servers that don't
//! require signatures simply ignore them.

use serde_json::Value;

use crate::state::AppState;

const AP_ACCEPT: &str = "application/activity+json, application/ld+json";

/// Fetch a remote ActivityPub object as JSON, signing the GET with the instance
/// actor's key.
pub async fn signed_get_json(state: &AppState, url: &str) -> anyhow::Result<Value> {
    let (private_key, _) = crate::federation::instance_actor::get_or_create(state).await?;
    let key_id = crate::federation::instance_actor::key_id(&state.instance.domain);

    let signed = feder_runtime::signature::sign_get(url, &key_id, &private_key)?;

    let resp = state
        .http
        .get(url)
        .header("Accept", AP_ACCEPT)
        .header("Date", signed.date)
        .header("Signature", signed.signature)
        .send()
        .await?
        .error_for_status()?;

    Ok(resp.json().await?)
}
