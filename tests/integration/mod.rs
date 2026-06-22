//! eunha integration test suite.
//!
//! These tests run against a live HTTP server. By default they spin up eunha
//! on a random port, but the C2S tests can be pointed at any
//! Mastodon-compatible server:
//!
//!   C2S_BASE_URL=https://mastodon.social \
//!   C2S_HOST=mastodon.social \
//!   C2S_ALICE_TOKEN=<token> \
//!   C2S_ALICE_ID=<account-id> \
//!   C2S_BOB_TOKEN=<token> \
//!   C2S_BOB_ID=<account-id> \
//!   cargo test --test integration
//!
//! When those env vars are absent the harness bootstraps eunha automatically
//! using DATABASE_URL.
//!
//! Layout:
//!   helpers     — shared live-server bootstrap + HTTP client.
//!   c2s/        — Mastodon Client-to-Server REST API compatibility.
//!   federation/ — Server-to-Server ActivityPub (federation) behaviour.
//!   surfaces/   — other public API surfaces (oEmbed, streaming).

mod helpers;

mod c2s;
mod federation;
mod surfaces;
