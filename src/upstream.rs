//! Reads the upstream Mastodon repository: releases, migration lists, and the
//! `db/schema.rb` that defines the schema eunha must reproduce.
//!
//! Eunha promises a database a real Mastodon can be pointed at, so upstream's
//! own schema definition is the only authority worth checking against. Rather
//! than vendoring a copy that silently rots, this module fetches it from GitHub
//! for whichever tag is being asked about.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const API: &str = "https://api.github.com/repos/mastodon/mastodon";
const RAW: &str = "https://raw.githubusercontent.com/mastodon/mastodon";

/// An upstream release.
#[derive(Debug, Clone)]
pub struct Release {
    /// Tag as it appears upstream, e.g. `v4.7.0`.
    pub tag: String,
    /// Tag without the leading `v`, matching `mastodon.toml`.
    pub version: String,
    pub published_at: String,
    pub prerelease: bool,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    published_at: Option<String>,
    prerelease: bool,
    draft: bool,
}

impl From<RawRelease> for Release {
    fn from(raw: RawRelease) -> Self {
        let version = raw.tag_name.trim_start_matches('v').to_string();
        Release {
            tag: raw.tag_name,
            version,
            published_at: raw
                .published_at
                .map(|t| t[..10.min(t.len())].to_string())
                .unwrap_or_default(),
            prerelease: raw.prerelease,
        }
    }
}

/// A Rails migration file in `db/migrate` or `db/post_migrate`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Migration {
    /// The 14-digit timestamp that lands in `public.schema_migrations`.
    pub version: String,
    /// Human-readable remainder of the filename, e.g. `add_local_fragment_to_keypair`.
    pub name: String,
    /// `db/post_migrate` migrations run after the new code is deployed.
    pub post_deploy: bool,
}

impl Migration {
    pub fn path(&self) -> String {
        let dir = if self.post_deploy {
            "db/post_migrate"
        } else {
            "db/migrate"
        };
        format!("{dir}/{}_{}.rb", self.version, self.name)
    }
}

/// A column as upstream declares it, normalised to how Postgres reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
    /// The column's default exactly as Postgres reports it. Both sides of a
    /// comparison are Postgres, so there is nothing to normalise.
    pub default: Option<String>,
}

/// A foreign key, keyed the way both sides can agree on: the constraint's
/// auto-generated Rails name is not derivable from `schema.rb`, so identity is
/// (table, column, target table) and the interesting part is `on_delete`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ForeignKey {
    pub table: String,
    pub column: String,
    pub target: String,
    /// `cascade`, `nullify`, or `none` — Postgres' `NO ACTION`.
    pub on_delete: String,
}

/// An index, compared by name and uniqueness.
///
/// The indexed expression is deliberately not compared: `schema.rb` writes
/// column lists while Postgres writes a normalised expression, and reconciling
/// the two produces more noise than signal. Name and uniqueness catch what
/// upstream actually churns between releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    /// `pg_get_indexdef`, with the table name elided: the columns or expression
    /// the index covers, its method, and any partial predicate.
    pub definition: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Table {
    pub columns: BTreeMap<String, Column>,
    pub indexes: BTreeMap<String, Index>,
}

/// The full picture of a schema, from either upstream or a live database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schema {
    /// `define(version:)` from `schema.rb`, or `max(version)` from a live
    /// `public.schema_migrations`.
    pub version: String,
    pub tables: BTreeMap<String, Table>,
    pub foreign_keys: BTreeSet<ForeignKey>,
}

fn client() -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    // Unauthenticated GitHub allows 60 requests an hour, which a few `check`
    // runs can exhaust; honour a token when one is around.
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {token}").parse()?,
            );
        }
    }
    Ok(reqwest::Client::builder()
        .user_agent(crate::version::USER_AGENT)
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let response = client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    let body = response.text().await?;
    anyhow::ensure!(
        status.is_success(),
        "GET {url} failed with {status}: {}",
        body.chars().take(200).collect::<String>()
    );
    serde_json::from_str(&body).with_context(|| format!("parsing response from {url}"))
}

/// The newest stable release upstream.
pub async fn latest_release() -> Result<Release> {
    let raw: RawRelease = get_json(&format!("{API}/releases/latest")).await?;
    Ok(raw.into())
}

/// Recent releases, newest first, excluding drafts.
pub async fn releases() -> Result<Vec<Release>> {
    let raw: Vec<RawRelease> = get_json(&format!("{API}/releases?per_page=100")).await?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.draft)
        .map(Release::from)
        .collect())
}

#[derive(Deserialize)]
struct TreeResponse {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
}

/// Every migration shipped in `tag`, ordered by version.
pub async fn migrations(tag: &str) -> Result<Vec<Migration>> {
    let mut all = Vec::new();
    for (dir, post_deploy) in [("db/migrate", false), ("db/post_migrate", true)] {
        let url = format!("{API}/git/trees/{tag}:{}", dir.replace('/', "%2F"));
        let tree: TreeResponse = get_json(&url).await?;
        anyhow::ensure!(!tree.truncated, "{url} returned a truncated tree");
        for entry in tree.tree {
            let Some(stem) = entry.path.strip_suffix(".rb") else {
                continue;
            };
            let Some((version, name)) = stem.split_once('_') else {
                continue;
            };
            if version.len() != 14 || !version.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            all.push(Migration {
                version: version.to_string(),
                name: name.to_string(),
                post_deploy,
            });
        }
    }
    all.sort();
    Ok(all)
}

/// Fetch upstream's `db/schema.rb` at `tag`, verbatim.
///
/// # Errors
/// Returns an error if the file cannot be fetched.
pub async fn schema_rb(tag: &str) -> Result<String> {
    let url = format!("{RAW}/{tag}/db/schema.rb");
    Ok(client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .text()
        .await?)
}

/// The `define(version:)` of upstream's `db/schema.rb` at `tag` — the newest
/// migration that release contains.
///
/// # Errors
/// Returns an error if the file cannot be fetched or states no version.
pub async fn schema_rb_version(tag: &str) -> Result<String> {
    let body = schema_rb(tag).await?;
    let at = body
        .find("define(version:")
        .context("schema.rb states no version")?;
    let rest = &body[at + "define(version:".len()..];
    let end = rest.find(')').context("malformed define(version:)")?;
    Ok(rest[..end].trim().replace('_', ""))
}

/// The reference schema eunha is checked against: what Mastodon's own
/// ActiveRecord builds from its `db/schema.rb`, introspected once and recorded
/// here.
///
/// Recorded rather than derived, because deriving it means either parsing Ruby
/// — a parser of ours standing between us and the truth — or running Rails,
/// which no test should need. `scripts/build_mastodon_schema.sh` regenerates
/// both this and the `.sql` beside it when a release is adopted.
///
/// # Panics
/// Panics if the vendored file is not valid JSON, which would mean the build
/// script wrote something broken.
#[must_use]
pub fn reference_schema() -> Schema {
    serde_json::from_str(include_str!("../mastodon/schema.json"))
        .expect("mastodon/schema.json is not a valid recorded schema")
}
