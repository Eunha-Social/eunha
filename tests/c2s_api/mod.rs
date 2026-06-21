//! Mastodon C2S API compatibility tests.
//!
//! These tests run against a live HTTP server. By default they spin up eunha
//! on a random port, but you can point them at any Mastodon-compatible server:
//!
//!   C2S_BASE_URL=https://mastodon.social \
//!   C2S_HOST=mastodon.social \
//!   C2S_ALICE_TOKEN=<token> \
//!   C2S_ALICE_ID=<account-id> \
//!   C2S_BOB_TOKEN=<token> \
//!   C2S_BOB_ID=<account-id> \
//!   cargo test --test c2s_api
//!
//! When those env vars are absent the test harness bootstraps eunha
//! automatically using DATABASE_URL.

mod helpers;

mod accounts;
mod admin;
mod announcements;
mod annual_reports;
mod apps;
mod blocks;
mod bookmarks;
mod collections;
mod conversations;
mod domain_blocks;
mod favourites;
mod featured_tags;
mod filters;
mod follow_requests;
mod instance;
mod invites;
mod lists;
mod markers;
mod media;
mod mutes;
mod notifications;
mod oembed;
mod polls;
mod push;
mod reports;
mod scope;
mod search;
mod statuses;
mod streaming;
mod tags;
mod timelines;
mod trends;
