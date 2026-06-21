//! HTTP Signature (draft-cavage-http-signatures) for ActivityPub federation.
//!
//! This module is intentionally runtime-agnostic: it works with plain byte
//! slices and key–value header lists, so it can be used with any HTTP framework.
//! It also provides an adapter to convert axum's `HeaderMap` into the flat
//! slice format expected by [`verify_request`].

use anyhow::Context as _;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest as _, Sha256};

// ── Public types ─────────────────────────────────────────────────────────────

/// Headers produced by [`sign_request`] that must be attached to the outgoing request.
#[derive(Debug, Clone)]
pub struct SignedHeaders {
    /// Value for the `Signature` header.
    pub signature: String,
    /// Value for the `Date` header.
    pub date: String,
    /// Value for the `Digest` header.
    pub digest: String,
}

// ── Signing ───────────────────────────────────────────────────────────────────

/// Sign an outgoing HTTP POST request body using RSA-SHA256.
///
/// Returns [`SignedHeaders`] that the caller must attach to the request.
///
/// # Arguments
/// * `method`          – HTTP method in any case (will be lowercased)
/// * `url`             – Full URL of the target inbox
/// * `body`            – Serialized activity body (JSON bytes)
/// * `key_id`          – `keyId` URI, typically `https://actor/url#main-key`
/// * `private_key_pem` – PKCS#8 PEM-encoded RSA private key
pub fn sign_request(
    method: &str,
    url: &str,
    body: &[u8],
    key_id: &str,
    private_key_pem: &str,
) -> anyhow::Result<SignedHeaders> {
    let digest = format!("SHA-256={}", BASE64.encode(Sha256::digest(body)));
    let date = chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();

    let parsed = url::Url::parse(url).context("invalid URL")?;
    let host = parsed.host_str().unwrap_or("").to_string();
    let path = parsed.path().to_string();

    let signing_string = format!(
        "(request-target): {} {}\nhost: {}\ndate: {}\ndigest: {}",
        method.to_lowercase(),
        path,
        host,
        date,
        digest,
    );

    let sig_b64 = rsa_sign(private_key_pem, signing_string.as_bytes())?;
    let signature = format!(
        r#"keyId="{}",algorithm="rsa-sha256",headers="(request-target) host date digest",signature="{}""#,
        key_id, sig_b64
    );

    Ok(SignedHeaders {
        signature,
        date,
        digest,
    })
}

// ── Verification ──────────────────────────────────────────────────────────────

/// Verify the HTTP Signature on an incoming POST request.
///
/// # Arguments
/// * `method`         – HTTP method in any case
/// * `path`           – Request path (e.g., `/inbox`)
/// * `headers`        – All request headers as `(lowercase-name, value)` pairs
/// * `body`           – Raw request body bytes
/// * `public_key_pem` – SPKI PEM-encoded RSA public key of the signing actor
pub fn verify_request(
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    public_key_pem: &str,
) -> anyhow::Result<()> {
    let get = |name: &str| -> &str {
        headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
            .unwrap_or("")
    };

    let sig_header_val = get("signature");
    anyhow::ensure!(!sig_header_val.is_empty(), "missing Signature header");

    let params = parse_params(sig_header_val)?;
    let headers_list = params.get("headers").map(String::as_str).unwrap_or("date");
    let sig_b64 = params.get("signature").context("missing signature field")?;

    let digest_val = get("digest");
    if !digest_val.is_empty() {
        let expected = format!("SHA-256={}", BASE64.encode(Sha256::digest(body)));
        anyhow::ensure!(digest_val == expected, "body digest mismatch");
    }

    let signing_string: String = headers_list
        .split_whitespace()
        .map(|h| match h {
            "(request-target)" => {
                format!("(request-target): {} {}", method.to_lowercase(), path)
            }
            other => format!("{other}: {}", get(other)),
        })
        .collect::<Vec<_>>()
        .join("\n");

    rsa_verify(public_key_pem, signing_string.as_bytes(), sig_b64)
}

/// Extract the `keyId` URI from a raw `Signature` header value.
pub fn key_id_from_header(sig_header: &str) -> Option<&str> {
    for part in sig_header.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("keyId=") {
            return Some(rest.trim_matches('"'));
        }
    }
    None
}

/// Convert an axum `HeaderMap` to a `Vec<(String, String)>` for use with
/// [`verify_request`].
pub fn headers_to_vec(map: &axum::http::HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|val| (k.as_str().to_lowercase(), val.to_string()))
        })
        .collect()
}

// ── RSA helpers ───────────────────────────────────────────────────────────────

fn rsa_sign(private_key_pem: &str, message: &[u8]) -> anyhow::Result<String> {
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding as _, Signer as _};

    let private_key = parse_private_key(private_key_pem).context("parse RSA private key")?;
    let signing_key = SigningKey::<Sha256>::new(private_key);
    let sig: rsa::pkcs1v15::Signature = signing_key.sign(message);
    Ok(BASE64.encode(sig.to_bytes()))
}

/// Parse an RSA private key from either PKCS#8 (`BEGIN PRIVATE KEY`) or
/// PKCS#1 (`BEGIN RSA PRIVATE KEY`) PEM format.
fn parse_private_key(pem: &str) -> anyhow::Result<rsa::RsaPrivateKey> {
    use rsa::pkcs8::DecodePrivateKey as _;

    if let Ok(key) = rsa::RsaPrivateKey::from_pkcs8_pem(pem) {
        return Ok(key);
    }

    use rsa::pkcs1::DecodeRsaPrivateKey as _;
    rsa::RsaPrivateKey::from_pkcs1_pem(pem).context("not valid PKCS#8 or PKCS#1 PEM")
}

fn rsa_verify(public_key_pem: &str, message: &[u8], sig_b64: &str) -> anyhow::Result<()> {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::DecodePublicKey as _;
    use rsa::signature::Verifier as _;

    let sig_bytes = BASE64.decode(sig_b64).context("decode base64 signature")?;
    let public_key = rsa::RsaPublicKey::from_public_key_pem(public_key_pem)
        .context("parse RSA public key")?;
    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let sig = Signature::try_from(sig_bytes.as_slice()).context("parse signature bytes")?;
    verifying_key
        .verify(message, &sig)
        .context("signature verification failed")
}

fn parse_params(header: &str) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for part in header.split(',') {
        let part = part.trim();
        if let Some(pos) = part.find('=') {
            let key = part[..pos].trim().to_string();
            let val = part[pos + 1..].trim().trim_matches('"').to_string();
            map.insert(key, val);
        }
    }
    Ok(map)
}
