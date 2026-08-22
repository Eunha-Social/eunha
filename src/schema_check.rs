//! Compares a live eunha database against upstream Mastodon's schema.
//!
//! "Drop-in replacement on top of your existing Mastodon database" only holds
//! if the two schemas are the same object, so this reads the database back out
//! of Postgres and diffs it against upstream's `db/schema.rb`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;

use crate::upstream::{Column, ForeignKey, Index, Schema, Table};

/// Migration ledgers, which `schema.rb` never describes: Rails' two, which
/// eunha maintains for compatibility, and sqlx's own, which it insists on
/// keeping in `public`. Never a drift finding.
const BOOKKEEPING: [&str; 3] = [
    "schema_migrations",
    "ar_internal_metadata",
    "_sqlx_migrations",
];

/// One way in which a live database departs from upstream's schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    SchemaVersion {
        live: String,
        expected: String,
    },
    MissingTable(String),
    ExtraTable(String),
    MissingColumn {
        table: String,
        column: Column,
    },
    ExtraColumn {
        table: String,
        column: String,
    },
    TypeMismatch {
        table: String,
        column: String,
        live: String,
        expected: String,
    },
    NullabilityMismatch {
        table: String,
        column: String,
        expected_nullable: bool,
    },
    MissingIndex {
        table: String,
        index: String,
        unique: bool,
    },
    ExtraIndex {
        table: String,
        index: String,
    },
    UniquenessMismatch {
        table: String,
        index: String,
        expected_unique: bool,
    },
    IndexDefinitionMismatch {
        table: String,
        index: String,
        live: String,
        expected: String,
    },
    DefaultMismatch {
        table: String,
        column: String,
        live: Option<String>,
        expected: Option<String>,
    },
    MissingForeignKey(ForeignKey),
    ExtraForeignKey(ForeignKey),
    OnDeleteMismatch {
        key: ForeignKey,
        live: String,
    },
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::SchemaVersion { live, expected } => write!(
                f,
                "schema_migrations is at {} but this build targets {expected}",
                if live.is_empty() { "(empty)" } else { live }
            ),
            Finding::MissingTable(t) => write!(f, "missing table {t}"),
            Finding::ExtraTable(t) => write!(f, "unexpected table {t}"),
            Finding::MissingColumn { table, column } => write!(
                f,
                "missing column {table}.{} ({}{})",
                column.name,
                column.sql_type,
                if column.nullable { "" } else { " not null" }
            ),
            Finding::ExtraColumn { table, column } => {
                write!(f, "unexpected column {table}.{column}")
            }
            Finding::TypeMismatch {
                table,
                column,
                live,
                expected,
            } => {
                write!(f, "{table}.{column} is {live}, expected {expected}")
            }
            Finding::NullabilityMismatch {
                table,
                column,
                expected_nullable,
            } => write!(
                f,
                "{table}.{column} should be {}",
                if *expected_nullable {
                    "nullable"
                } else {
                    "NOT NULL"
                }
            ),
            Finding::MissingIndex {
                table,
                index,
                unique,
            } => write!(
                f,
                "missing {}index {index} on {table}",
                if *unique { "unique " } else { "" }
            ),
            Finding::ExtraIndex { table, index } => {
                write!(f, "unexpected index {index} on {table}")
            }
            Finding::IndexDefinitionMismatch {
                table,
                index,
                live,
                expected,
            } => write!(
                f,
                "index {index} on {table} covers\n      {live}\n    but should cover\n      {expected}"
            ),
            Finding::DefaultMismatch {
                table,
                column,
                live,
                expected,
            } => write!(
                f,
                "{table}.{column} defaults to {} but should default to {}",
                live.as_deref().unwrap_or("nothing"),
                expected.as_deref().unwrap_or("nothing")
            ),
            Finding::UniquenessMismatch {
                table,
                index,
                expected_unique,
            } => write!(
                f,
                "index {index} on {table} should be {}",
                if *expected_unique {
                    "UNIQUE"
                } else {
                    "non-unique"
                }
            ),
            Finding::MissingForeignKey(k) => write!(
                f,
                "missing foreign key {}.{} -> {} (on delete {})",
                k.table, k.column, k.target, k.on_delete
            ),
            Finding::ExtraForeignKey(k) => write!(
                f,
                "unexpected foreign key {}.{} -> {}",
                k.table, k.column, k.target
            ),
            Finding::OnDeleteMismatch { key, live } => write!(
                f,
                "foreign key {}.{} -> {} is on delete {live}, expected {}",
                key.table, key.column, key.target, key.on_delete
            ),
        }
    }
}

/// Read the `public` schema back out of a live database.
pub async fn introspect(pool: &PgPool) -> Result<Schema> {
    let mut schema = Schema::default();

    let version: Option<String> =
        sqlx::query_scalar("SELECT max(version) FROM public.schema_migrations")
            .fetch_optional(pool)
            .await
            .context("reading public.schema_migrations")?
            .flatten();
    schema.version = version.unwrap_or_default();

    let columns: Vec<(String, String, String, String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT c.table_name::text, c.column_name::text, c.is_nullable::text,
               c.data_type::text, c.udt_name::text, c.column_default::text
        FROM information_schema.columns c
        JOIN information_schema.tables t
          ON t.table_schema = c.table_schema AND t.table_name = c.table_name
        WHERE c.table_schema = 'public' AND t.table_type = 'BASE TABLE'
        "#,
    )
    .fetch_all(pool)
    .await
    .context("reading columns")?;

    for (table, column, nullable, data_type, udt, default) in columns {
        schema
            .tables
            .entry(table)
            .or_insert_with(Table::default)
            .columns
            .insert(
                column.clone(),
                Column {
                    name: column,
                    sql_type: sql_type(&data_type, &udt),
                    nullable: nullable == "YES",
                    default,
                },
            );
    }

    // Primary-key and constraint-backed indexes are implied by the column and
    // constraint definitions, and upstream's schema.rb does not list them.
    let indexes: Vec<(String, String, bool, String)> = sqlx::query_as(
        r#"
        SELECT t.relname::text, i.relname::text, x.indisunique,
               pg_get_indexdef(i.oid)::text
        FROM pg_index x
        JOIN pg_class i ON i.oid = x.indexrelid
        JOIN pg_class t ON t.oid = x.indrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        WHERE n.nspname = 'public'
          AND t.relkind IN ('r', 'p')
          AND NOT x.indisprimary
          AND NOT EXISTS (
            SELECT 1 FROM pg_constraint c
            WHERE c.conindid = i.oid AND c.contype IN ('p', 'u')
          )
        "#,
    )
    .fetch_all(pool)
    .await
    .context("reading indexes")?;

    for (table, index, unique, definition) in indexes {
        // `pg_get_indexdef` names the table the index is on, which differs
        // between the reference database and the one under test; what matters
        // is the rest — the columns or expression, the method, any predicate.
        let definition = definition.replace(&format!(" ON public.{table} "), " ON <table> ");
        schema
            .tables
            .entry(table)
            .or_insert_with(Table::default)
            .indexes
            .insert(
                index.clone(),
                Index {
                    name: index,
                    unique,
                    definition,
                },
            );
    }

    let foreign_keys: Vec<(String, String, String, String)> = sqlx::query_as(
        r#"
        SELECT t.relname::text, a.attname::text, ft.relname::text, c.confdeltype::text
        FROM pg_constraint c
        JOIN pg_class t ON t.oid = c.conrelid
        JOIN pg_class ft ON ft.oid = c.confrelid
        JOIN pg_namespace n ON n.oid = t.relnamespace
        JOIN LATERAL unnest(c.conkey) AS k(attnum) ON true
        JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum
        WHERE c.contype = 'f' AND n.nspname = 'public'
        "#,
    )
    .fetch_all(pool)
    .await
    .context("reading foreign keys")?;

    for (table, column, target, confdeltype) in foreign_keys {
        schema.foreign_keys.insert(ForeignKey {
            table,
            column,
            target,
            on_delete: on_delete(&confdeltype).to_string(),
        });
    }

    Ok(schema)
}

/// Render a column's type the way `schema.rb` parsing does, so the two are
/// comparable: array columns become `element[]`.
fn sql_type(data_type: &str, udt: &str) -> String {
    if data_type != "ARRAY" {
        return data_type.to_string();
    }
    let element = udt.strip_prefix('_').unwrap_or(udt);
    let element = match element {
        "varchar" => "character varying",
        "int8" => "bigint",
        "int4" => "integer",
        "int2" => "smallint",
        "float8" => "double precision",
        "bool" => "boolean",
        "timestamp" => "timestamp without time zone",
        other => other,
    };
    format!("{element}[]")
}

fn on_delete(confdeltype: &str) -> &'static str {
    match confdeltype {
        "c" => "cascade",
        "n" => "nullify",
        "r" => "restrict",
        "d" => "default",
        _ => "none",
    }
}

/// Diff a live schema against upstream's, most structural findings first.
pub fn diff(live: &Schema, upstream: &Schema) -> Vec<Finding> {
    let mut findings = Vec::new();

    if !upstream.version.is_empty() && live.version != upstream.version {
        findings.push(Finding::SchemaVersion {
            live: live.version.clone(),
            expected: upstream.version.clone(),
        });
    }

    for name in upstream.tables.keys() {
        if !live.tables.contains_key(name) {
            findings.push(Finding::MissingTable(name.clone()));
        }
    }
    for name in live.tables.keys() {
        if !upstream.tables.contains_key(name) && !BOOKKEEPING.contains(&name.as_str()) {
            findings.push(Finding::ExtraTable(name.clone()));
        }
    }

    for (name, expected) in &upstream.tables {
        let Some(actual) = live.tables.get(name) else {
            continue;
        };

        for (column_name, expected_column) in &expected.columns {
            match actual.columns.get(column_name) {
                None => findings.push(Finding::MissingColumn {
                    table: name.clone(),
                    column: expected_column.clone(),
                }),
                Some(actual_column) => {
                    if actual_column.sql_type != expected_column.sql_type {
                        findings.push(Finding::TypeMismatch {
                            table: name.clone(),
                            column: column_name.clone(),
                            live: actual_column.sql_type.clone(),
                            expected: expected_column.sql_type.clone(),
                        });
                    }
                    if actual_column.nullable != expected_column.nullable {
                        findings.push(Finding::NullabilityMismatch {
                            table: name.clone(),
                            column: column_name.clone(),
                            expected_nullable: expected_column.nullable,
                        });
                    }
                    if actual_column.default != expected_column.default {
                        findings.push(Finding::DefaultMismatch {
                            table: name.clone(),
                            column: column_name.clone(),
                            live: actual_column.default.clone(),
                            expected: expected_column.default.clone(),
                        });
                    }
                }
            }
        }
        for column_name in actual.columns.keys() {
            if !expected.columns.contains_key(column_name) {
                findings.push(Finding::ExtraColumn {
                    table: name.clone(),
                    column: column_name.clone(),
                });
            }
        }

        for (index_name, expected_index) in &expected.indexes {
            match actual.indexes.get(index_name) {
                None => findings.push(Finding::MissingIndex {
                    table: name.clone(),
                    index: index_name.clone(),
                    unique: expected_index.unique,
                }),
                Some(actual_index) if actual_index.unique != expected_index.unique => findings
                    .push(Finding::UniquenessMismatch {
                        table: name.clone(),
                        index: index_name.clone(),
                        expected_unique: expected_index.unique,
                    }),
                Some(actual_index) if actual_index.definition != expected_index.definition => {
                    findings.push(Finding::IndexDefinitionMismatch {
                        table: name.clone(),
                        index: index_name.clone(),
                        live: actual_index.definition.clone(),
                        expected: expected_index.definition.clone(),
                    })
                }
                Some(_) => {}
            }
        }
        for index_name in actual.indexes.keys() {
            if !expected.indexes.contains_key(index_name) {
                findings.push(Finding::ExtraIndex {
                    table: name.clone(),
                    index: index_name.clone(),
                });
            }
        }
    }

    // Foreign keys are identified by (table, column, target); `on_delete` is
    // the part that drifts, so report that separately from a missing key.
    let by_identity = |keys: &BTreeSet<ForeignKey>| -> BTreeMap<(String, String, String), String> {
        keys.iter()
            .map(|k| {
                (
                    (k.table.clone(), k.column.clone(), k.target.clone()),
                    k.on_delete.clone(),
                )
            })
            .collect()
    };
    let live_keys = by_identity(&live.foreign_keys);
    let upstream_keys = by_identity(&upstream.foreign_keys);

    for (identity, expected_on_delete) in &upstream_keys {
        let key = ForeignKey {
            table: identity.0.clone(),
            column: identity.1.clone(),
            target: identity.2.clone(),
            on_delete: expected_on_delete.clone(),
        };
        match live_keys.get(identity) {
            None => findings.push(Finding::MissingForeignKey(key)),
            Some(live_on_delete) if live_on_delete != expected_on_delete => {
                findings.push(Finding::OnDeleteMismatch {
                    key,
                    live: live_on_delete.clone(),
                })
            }
            Some(_) => {}
        }
    }
    for (identity, on_delete) in &live_keys {
        if !upstream_keys.contains_key(identity) && !BOOKKEEPING.contains(&identity.0.as_str()) {
            findings.push(Finding::ExtraForeignKey(ForeignKey {
                table: identity.0.clone(),
                column: identity.1.clone(),
                target: identity.2.clone(),
                on_delete: on_delete.clone(),
            }));
        }
    }

    findings
}

static SCHEMA_MIGRATION_VERSION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"'(\d{14})'").expect("valid regex"));

/// The Mastodon migration versions eunha's own `migrations/` claim to cover,
/// read out of the `public.schema_migrations` seeds they insert.
pub fn covered_versions(migrations_dir: &std::path::Path) -> Result<BTreeSet<String>> {
    let mut covered = BTreeSet::new();
    let entries = std::fs::read_dir(migrations_dir)
        .with_context(|| format!("reading {}", migrations_dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if !body.contains("schema_migrations") {
            continue;
        }
        for caps in SCHEMA_MIGRATION_VERSION.captures_iter(&body) {
            covered.insert(caps[1].to_string());
        }
    }
    Ok(covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, sql_type: &str, nullable: bool) -> Column {
        Column {
            name: name.to_string(),
            sql_type: sql_type.to_string(),
            nullable,
            default: None,
        }
    }

    fn index(name: &str, unique: bool, definition: &str) -> Index {
        Index {
            name: name.to_string(),
            unique,
            definition: definition.to_string(),
        }
    }

    fn table(columns: Vec<Column>, indexes: Vec<Index>) -> Table {
        Table {
            columns: columns.into_iter().map(|c| (c.name.clone(), c)).collect(),
            indexes: indexes.into_iter().map(|i| (i.name.clone(), i)).collect(),
        }
    }

    fn schema(tables: Vec<(&str, Table)>, foreign_keys: Vec<ForeignKey>) -> Schema {
        Schema {
            version: "20260812154114".to_string(),
            tables: tables
                .into_iter()
                .map(|(name, t)| (name.to_string(), t))
                .collect(),
            foreign_keys: foreign_keys.into_iter().collect(),
        }
    }

    fn reference() -> Schema {
        schema(
            vec![(
                "accounts",
                table(
                    vec![
                        column("id", "bigint", false),
                        column("username", "character varying", false),
                    ],
                    vec![index(
                        "index_accounts_on_username",
                        true,
                        "CREATE UNIQUE INDEX index_accounts_on_username ON <table> USING btree (username)",
                    )],
                ),
            )],
            vec![ForeignKey {
                table: "statuses".into(),
                column: "account_id".into(),
                target: "accounts".into(),
                on_delete: "cascade".into(),
            }],
        )
    }

    #[test]
    fn identical_schemas_have_no_findings() {
        let s = reference();
        assert!(diff(&s, &s).is_empty());
    }

    /// The differences the checker exists to catch, including the two that a
    /// `schema.rb` parser could not see: a wrong default, and an index over the
    /// wrong columns.
    #[test]
    fn reports_every_kind_of_drift() {
        let expected = reference();
        let mut live = reference();

        let accounts = live.tables.get_mut("accounts").unwrap();
        accounts.columns.get_mut("username").unwrap().nullable = true;
        accounts.columns.get_mut("username").unwrap().default =
            Some("'anon'::character varying".into());
        accounts.columns.get_mut("id").unwrap().sql_type = "integer".into();
        accounts.indexes.get_mut("index_accounts_on_username").unwrap().definition =
            "CREATE UNIQUE INDEX index_accounts_on_username ON <table> USING btree (lower(username))"
                .into();
        accounts
            .columns
            .insert("leftover".to_string(), column("leftover", "text", true));
        live.tables
            .insert("gone_missing".to_string(), Table::default());
        live.foreign_keys.clear();

        let findings = diff(&live, &expected);

        assert!(
            findings.iter().any(|f| matches!(
                f,
                Finding::DefaultMismatch { column, .. } if column == "username"
            )),
            "a wrong default must be reported: {findings:?}"
        );
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::IndexDefinitionMismatch { index, .. } if index == "index_accounts_on_username"
        )), "an index over the wrong columns must be reported");
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::NullabilityMismatch { column, .. } if column == "username"
        )));
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::TypeMismatch { column, .. } if column == "id"
        )));
        assert!(findings.iter().any(|f| matches!(
            f,
            Finding::ExtraColumn { column, .. } if column == "leftover"
        )));
        assert!(findings
            .iter()
            .any(|f| matches!(f, Finding::ExtraTable(name) if name == "gone_missing")));
        assert!(findings
            .iter()
            .any(|f| matches!(f, Finding::MissingForeignKey(k) if k.table == "statuses")));
    }

    /// Migration ledgers are not part of Mastodon's schema and are never drift.
    #[test]
    fn bookkeeping_tables_are_not_findings() {
        let expected = reference();
        let mut live = reference();
        for ledger in [
            "schema_migrations",
            "ar_internal_metadata",
            "_sqlx_migrations",
        ] {
            live.tables.insert(ledger.to_string(), Table::default());
        }
        assert!(diff(&live, &expected).is_empty());
    }

    #[test]
    fn array_columns_round_trip_through_postgres_names() {
        assert_eq!(sql_type("ARRAY", "_varchar"), "character varying[]");
        assert_eq!(sql_type("ARRAY", "_int8"), "bigint[]");
        assert_eq!(
            sql_type("character varying", "varchar"),
            "character varying"
        );
    }
}
