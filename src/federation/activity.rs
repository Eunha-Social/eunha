//! ActivityPub activity construction.
//!
//! Thin builders over [`feder_vocab`]'s typed activity structs. Each builder
//! takes string URIs (as the rest of eunha stores them), constructs the typed
//! vocabulary value, and serializes it to a [`serde_json::Value`] ready for
//! delivery. Mastodon/Misskey-specific JSON-LD (FEP-044f quote terms) lives
//! here rather than in the shared vocabulary crate.

use anyhow::Context as _;
use feder_vocab as vocab;
use serde_json::Value;
use vocab::{Reference, Iri};

/// The special collection addressing every actor (public posts).
pub const AS_PUBLIC: &str = vocab::ACTIVITYSTREAMS_PUBLIC;

fn iri(s: &str) -> anyhow::Result<Iri> {
    s.parse::<Iri>()
        .map_err(|e| anyhow::anyhow!("invalid ActivityPub IRI {s:?}: {e}"))
}

fn iris(values: &[&str]) -> anyhow::Result<Vec<Iri>> {
    values.iter().map(|s| iri(s)).collect()
}

fn actor_ref(s: &str) -> anyhow::Result<Reference<vocab::Actor>> {
    Ok(Reference::id(iri(s)?))
}

fn to_value<T: serde::Serialize>(value: &T) -> anyhow::Result<Value> {
    serde_json::to_value(value).context("serialize activity")
}

// ── Follow ──────────────────────────────────────────────────────────────────

/// Build a `Follow` activity.
pub fn follow(id: &str, actor: &str, object: &str) -> anyhow::Result<Value> {
    to_value(&vocab::Follow::new(iri(id)?, actor_ref(actor)?, actor_ref(object)?))
}

/// Build the embedded `Follow` object referenced by Accept/Reject/Undo
/// (no `@context`, as it is nested inside another activity).
fn embedded_follow(
    follow_id: &str,
    follow_actor: &str,
    follow_object: &str,
) -> anyhow::Result<vocab::Follow> {
    let mut f = vocab::Follow::new(
        iri(follow_id)?,
        actor_ref(follow_actor)?,
        actor_ref(follow_object)?,
    );
    f.context = None;
    Ok(f)
}

// ── Accept / Reject ───────────────────────────────────────────────────────────

/// Build an `Accept(Follow)` activity sent in response to a received follow.
pub fn accept_follow(
    id: &str,
    actor: &str,
    follow_id: &str,
    follow_actor: &str,
    follow_object: &str,
) -> anyhow::Result<Value> {
    let follow = embedded_follow(follow_id, follow_actor, follow_object)?;
    to_value(&vocab::Accept::new(
        iri(id)?,
        actor_ref(actor)?,
        Reference::object(follow),
    ))
}

/// Build a `Reject(Follow)` activity.
pub fn reject_follow(
    id: &str,
    actor: &str,
    follow_id: &str,
    follow_actor: &str,
    follow_object: &str,
) -> anyhow::Result<Value> {
    let follow = embedded_follow(follow_id, follow_actor, follow_object)?;
    to_value(&vocab::Reject::new(
        iri(id)?,
        actor_ref(actor)?,
        Reference::object(follow),
    ))
}

// ── Undo ──────────────────────────────────────────────────────────────────────

/// Build an `Undo(Follow)` activity used when unfollowing.
pub fn undo_follow(
    id: &str,
    actor: &str,
    follow_id: &str,
    follow_actor: &str,
    follow_object: &str,
) -> anyhow::Result<Value> {
    let follow = embedded_follow(follow_id, follow_actor, follow_object)?;
    to_value(&vocab::Undo::new(iri(id)?, actor_ref(actor)?, follow))
}

/// Build an `Undo(Like)` activity (unfavourite).
pub fn undo_like(id: &str, actor: &str, like_id: &str, like_object: &str) -> anyhow::Result<Value> {
    let mut like = vocab::Like::new(iri(like_id)?, actor_ref(actor)?, iri(like_object)?);
    like.context = None;
    to_value(&vocab::Undo::new(iri(id)?, actor_ref(actor)?, like))
}

/// Build an `Undo(Announce)` activity (unboost).
pub fn undo_announce(
    id: &str,
    actor: &str,
    announce_id: &str,
    announce_object: &str,
) -> anyhow::Result<Value> {
    let mut announce =
        vocab::Announce::new(iri(announce_id)?, actor_ref(actor)?, iri(announce_object)?);
    announce.context = None;
    to_value(&vocab::Undo::new(iri(id)?, actor_ref(actor)?, announce))
}

/// Build an `Undo(Block)` activity.
pub fn undo_block(
    id: &str,
    actor: &str,
    block_id: &str,
    block_object: &str,
) -> anyhow::Result<Value> {
    let mut block = vocab::Block::new(iri(block_id)?, actor_ref(actor)?, iri(block_object)?);
    block.context = None;
    to_value(&vocab::Undo::new(iri(id)?, actor_ref(actor)?, block))
}

// ── Delete ────────────────────────────────────────────────────────────────────

/// Build a `Delete` activity for a local object being removed.
pub fn delete(id: &str, actor: &str, object: &str) -> anyhow::Result<Value> {
    to_value(&vocab::Delete::new(
        iri(id)?,
        actor_ref(actor)?,
        vocab::Tombstone::new(iri(object)?),
    ))
}

// ── Move ──────────────────────────────────────────────────────────────────────

/// Build a `Move` activity for account migration. `object` is the old (moving)
/// actor; `target` is the new account's actor URI.
pub fn move_actor(id: &str, actor: &str, object: &str, target: &str) -> Value {
    serde_json::json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": id,
        "type": "Move",
        "actor": actor,
        "object": object,
        "target": target,
    })
}

// ── Like ──────────────────────────────────────────────────────────────────────

/// Build a `Like` activity (favourite).
pub fn like(id: &str, actor: &str, object: &str) -> anyhow::Result<Value> {
    to_value(&vocab::Like::new(iri(id)?, actor_ref(actor)?, iri(object)?))
}

// ── Announce ──────────────────────────────────────────────────────────────────

/// Build an `Announce` activity (boost/reblog).
pub fn announce(
    id: &str,
    actor: &str,
    object: &str,
    to: &[&str],
    cc: &[&str],
    published: &str,
) -> anyhow::Result<Value> {
    let mut announce = vocab::Announce::new(iri(id)?, actor_ref(actor)?, iri(object)?);
    announce.published = Some(published.to_string());
    announce.to = iris(to)?;
    announce.cc = iris(cc)?;
    to_value(&announce)
}

// ── Block ─────────────────────────────────────────────────────────────────────

/// Build a `Block` activity.
pub fn block(id: &str, actor: &str, object: &str) -> anyhow::Result<Value> {
    to_value(&vocab::Block::new(iri(id)?, actor_ref(actor)?, iri(object)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn follow_shape() {
        assert_eq!(
            follow("https://a.test/f/1", "https://a.test/u/alice", "https://b.test/u/bob").unwrap(),
            json!({
                "@context": "https://www.w3.org/ns/activitystreams",
                "id": "https://a.test/f/1",
                "type": "Follow",
                "actor": "https://a.test/u/alice",
                "object": "https://b.test/u/bob",
            })
        );
    }

    #[test]
    fn accept_follow_embeds_follow_without_context() {
        let v = accept_follow(
            "https://a.test/acc/1",
            "https://a.test/u/alice",
            "https://b.test/f/9",
            "https://b.test/u/bob",
            "https://a.test/u/alice",
        )
        .unwrap();
        assert_eq!(v["type"], "Accept");
        assert_eq!(v["object"]["type"], "Follow");
        assert!(v["object"].get("@context").is_none());
        assert_eq!(v["object"]["id"], "https://b.test/f/9");
    }

    #[test]
    fn undo_like_embeds_like_without_context() {
        let v = undo_like(
            "https://a.test/l/1#undo",
            "https://a.test/u/alice",
            "https://a.test/l/1",
            "https://b.test/notes/9",
        )
        .unwrap();
        assert_eq!(v["type"], "Undo");
        assert_eq!(v["object"]["type"], "Like");
        assert!(v["object"].get("@context").is_none());
        assert_eq!(v["object"]["actor"], "https://a.test/u/alice");
        assert_eq!(v["object"]["object"], "https://b.test/notes/9");
    }

    #[test]
    fn delete_uses_tombstone() {
        let v = delete(
            "https://a.test/d/1",
            "https://a.test/u/alice",
            "https://a.test/notes/1",
        )
        .unwrap();
        assert_eq!(v["type"], "Delete");
        assert_eq!(
            v["object"],
            json!({ "type": "Tombstone", "id": "https://a.test/notes/1" })
        );
    }

    #[test]
    fn announce_has_audience() {
        let v = announce(
            "https://a.test/b/1",
            "https://a.test/u/alice",
            "https://b.test/notes/9",
            &[AS_PUBLIC],
            &["https://a.test/u/alice/followers"],
            "2026-06-21T00:00:00+00:00",
        )
        .unwrap();
        assert_eq!(v["type"], "Announce");
        assert_eq!(v["to"], json!([AS_PUBLIC]));
        assert_eq!(v["cc"], json!(["https://a.test/u/alice/followers"]));
        assert_eq!(v["published"], "2026-06-21T00:00:00+00:00");
        assert_eq!(v["object"], "https://b.test/notes/9");
    }

}
