//! Reads the upstream Mastodon repository: releases, migration lists, and the
//! `db/schema.rb` that defines the schema eunha must reproduce.
//!
//! Eunha promises a database a real Mastodon can be pointed at, so upstream's
//! own schema definition is the only authority worth checking against. Rather
//! than vendoring a copy that silently rots, this module fetches it from GitHub
//! for whichever tag is being asked about.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
}

/// A foreign key, keyed the way both sides can agree on: the constraint's
/// auto-generated Rails name is not derivable from `schema.rb`, so identity is
/// (table, column, target table) and the interesting part is `on_delete`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub unique: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: BTreeMap<String, Column>,
    pub indexes: BTreeMap<String, Index>,
}

/// The full picture of a schema, from either upstream or a live database.
#[derive(Debug, Clone, Default)]
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

/// Fetch and parse `db/schema.rb` at `tag`.
pub async fn schema(tag: &str) -> Result<Schema> {
    let url = format!("{RAW}/{tag}/db/schema.rb");
    let body = client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .text()
        .await?;
    Ok(parse_schema_rb(&body))
}

static DEFINE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"define\(version:\s*([0-9_]+)\)").expect("valid regex"));
static CREATE_TABLE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*create_table "(\w+)"(.*)$"#).expect("valid regex"));
static COLUMN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*t\.(\w+) "(\w+)"(.*)$"#).expect("valid regex"));
static INDEX_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"name: "?([\w.]+)"?"#).expect("valid regex"));
static PRIMARY_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"primary_key: "(\w+)""#).expect("valid regex"));
static FOREIGN_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*add_foreign_key "(\w+)", "(\w+)"(.*)$"#).expect("valid regex"));
static FK_COLUMN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"column: "(\w+)""#).expect("valid regex"));
static ON_DELETE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"on_delete: :(\w+)").expect("valid regex"));

/// Translate a Rails column type into the type Postgres reports for it.
fn pg_type(rails_type: &str) -> Option<&'static str> {
    Some(match rails_type {
        "string" => "character varying",
        "text" => "text",
        "integer" => "integer",
        "bigint" => "bigint",
        "boolean" => "boolean",
        // Rails' `precision: nil` and its default precision of 6 are the same
        // type to Postgres, whose default timestamp precision is already 6.
        "datetime" | "timestamp" => "timestamp without time zone",
        "date" => "date",
        "float" => "double precision",
        "decimal" => "numeric",
        "jsonb" => "jsonb",
        "json" => "json",
        "binary" => "bytea",
        "uuid" => "uuid",
        "inet" => "inet",
        "interval" => "interval",
        "daterange" => "daterange",
        _ => return None,
    })
}

/// Parse the subset of `schema.rb` that describes physical structure.
///
/// Scenic's `create_view` blocks at the end of the file are skipped: views are
/// derived objects, and a live database is compared on base tables only.
pub fn parse_schema_rb(source: &str) -> Schema {
    let mut schema = Schema::default();
    if let Some(caps) = DEFINE.captures(source) {
        schema.version = caps[1].replace('_', "");
    }

    let mut current: Option<String> = None;
    for line in source.lines() {
        if let Some(caps) = CREATE_TABLE.captures(line) {
            let name = caps[1].to_string();
            let options = &caps[2];
            let mut table = Table::default();

            // Rails gives every table an `id` bigint unless told otherwise:
            // `primary_key: "x"` renames it, a composite `primary_key: [...]`
            // is spelled out in the columns below, and `id: false` drops it.
            if options.contains("primary_key: [") {
                // Nothing implicit to add.
            } else if let Some(pk) = PRIMARY_KEY.captures(options) {
                table.columns.insert(
                    pk[1].to_string(),
                    Column {
                        name: pk[1].to_string(),
                        sql_type: "bigint".to_string(),
                        nullable: false,
                    },
                );
            } else if !options.contains("id: false") {
                let sql_type = if options.contains("id: :uuid") {
                    "uuid"
                } else if options.contains("id: :integer") {
                    "integer"
                } else {
                    "bigint"
                };
                table.columns.insert(
                    "id".to_string(),
                    Column {
                        name: "id".to_string(),
                        sql_type: sql_type.to_string(),
                        nullable: false,
                    },
                );
            }

            schema.tables.insert(name.clone(), table);
            current = Some(name);
            continue;
        }

        if let Some(caps) = FOREIGN_KEY.captures(line) {
            let table = caps[1].to_string();
            let target = caps[2].to_string();
            let rest = &caps[3];
            let column = FK_COLUMN
                .captures(rest)
                .map(|c| c[1].to_string())
                // Rails' default: the target table name, singularised, `_id`.
                .unwrap_or_else(|| format!("{}_id", singularize(&target)));
            let on_delete = ON_DELETE
                .captures(rest)
                .map(|c| c[1].to_string())
                .unwrap_or_else(|| "none".to_string());
            schema.foreign_keys.insert(ForeignKey {
                table,
                column,
                target,
                on_delete,
            });
            continue;
        }

        let Some(table_name) = current.clone() else {
            continue;
        };
        if line.trim() == "end" {
            current = None;
            continue;
        }

        let Some(caps) = COLUMN.captures(line) else {
            // `t.index "lower((username)::text)", name: "..."` — an expression
            // index, whose name is all this comparison needs.
            if line.trim_start().starts_with("t.index ") {
                if let Some(name) = INDEX_NAME.captures(line) {
                    if let Some(table) = schema.tables.get_mut(&table_name) {
                        let name = name[1].to_string();
                        table.indexes.insert(
                            name.clone(),
                            Index {
                                name,
                                unique: line.contains("unique: true"),
                            },
                        );
                    }
                }
            }
            continue;
        };

        let kind = caps[1].to_string();
        let name = caps[2].to_string();
        let rest = caps[3].to_string();
        let Some(table) = schema.tables.get_mut(&table_name) else {
            continue;
        };

        match kind.as_str() {
            "index" => {
                if let Some(index_name) = INDEX_NAME.captures(&rest) {
                    let name = index_name[1].to_string();
                    table.indexes.insert(
                        name.clone(),
                        Index {
                            name,
                            unique: rest.contains("unique: true"),
                        },
                    );
                }
            }
            "check_constraint" => {}
            _ => {
                let Some(base) = pg_type(&kind) else { continue };
                let sql_type = if rest.contains("array: true") {
                    format!("{base}[]")
                } else {
                    base.to_string()
                };
                table.columns.insert(
                    name.clone(),
                    Column {
                        name,
                        sql_type,
                        nullable: !rest.contains("null: false"),
                    },
                );
            }
        }
    }

    // `t.timestamps` expands to created_at/updated_at, but Mastodon's dumped
    // schema.rb always writes them out explicitly, so no expansion is needed.
    schema
}

/// Rails' inflection for the table names Mastodon actually uses.
fn singularize(table: &str) -> String {
    for (plural, singular) in [
        ("statuses", "status"),
        ("aliases", "alias"),
        ("classes", "class"),
        ("boxes", "box"),
    ] {
        if let Some(prefix) = table.strip_suffix(plural) {
            return format!("{prefix}{singular}");
        }
    }
    if let Some(prefix) = table.strip_suffix("ies") {
        return format!("{prefix}y");
    }
    table.strip_suffix('s').unwrap_or(table).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tables_columns_and_indexes() {
        let schema = parse_schema_rb(
            r#"
ActiveRecord::Schema[8.1].define(version: 2026_08_12_154114) do
  create_table "accounts", id: :bigint, default: -> { "timestamp_id('accounts'::text)" }, force: :cascade do |t|
    t.string "also_known_as", array: true
    t.string "username", default: "", null: false
    t.datetime "requested_deletion_at"
    t.index ["domain", "id"], name: "index_accounts_on_domain_and_id"
    t.index "lower((username)::text)", name: "index_accounts_on_username_lower", unique: true
  end

  create_table "account_summaries", primary_key: "account_id", force: :cascade do |t|
    t.boolean "sensitive", default: false, null: false
  end

  create_table "statuses_tags", primary_key: ["tag_id", "status_id"], force: :cascade do |t|
    t.bigint "status_id", null: false
    t.bigint "tag_id", null: false
  end

  add_foreign_key "account_aliases", "accounts", on_delete: :cascade
  add_foreign_key "account_migrations", "accounts", column: "target_account_id", on_delete: :nullify
end
"#,
        );

        assert_eq!(schema.version, "20260812154114");

        let accounts = &schema.tables["accounts"];
        assert_eq!(accounts.columns["id"].sql_type, "bigint");
        assert_eq!(
            accounts.columns["also_known_as"].sql_type,
            "character varying[]"
        );
        assert!(!accounts.columns["username"].nullable);
        assert!(accounts.columns["requested_deletion_at"].nullable);
        assert!(!accounts.indexes["index_accounts_on_domain_and_id"].unique);
        assert!(accounts.indexes["index_accounts_on_username_lower"].unique);

        // `primary_key:` renames the implicit id rather than adding to it.
        let summaries = &schema.tables["account_summaries"];
        assert!(summaries.columns.contains_key("account_id"));
        assert!(!summaries.columns.contains_key("id"));

        // A composite primary key means there is no implicit `id` at all.
        let join_table = &schema.tables["statuses_tags"];
        assert!(!join_table.columns.contains_key("id"));
        assert_eq!(join_table.columns.len(), 2);

        assert!(schema.foreign_keys.contains(&ForeignKey {
            table: "account_aliases".into(),
            column: "account_id".into(),
            target: "accounts".into(),
            on_delete: "cascade".into(),
        }));
        assert!(schema.foreign_keys.contains(&ForeignKey {
            table: "account_migrations".into(),
            column: "target_account_id".into(),
            target: "accounts".into(),
            on_delete: "nullify".into(),
        }));
    }

    #[test]
    fn singularizes_mastodon_table_names() {
        assert_eq!(singularize("accounts"), "account");
        assert_eq!(singularize("statuses"), "status");
        assert_eq!(singularize("account_aliases"), "account_alias");
        assert_eq!(singularize("policies"), "policy");
    }
}
