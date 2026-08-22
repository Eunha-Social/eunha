//! Tracks upstream Mastodon releases and checks eunha's database against them.
//!
//! Eunha aims to be a drop-in replacement on top of an existing Mastodon
//! database, which means two ongoing obligations: noticing when upstream ships
//! a release, and proving that what eunha's migrations build is still the same
//! schema upstream builds. This tool covers both.
//!
//!   eunha-schema status            — is there a newer Mastodon release?
//!   eunha-schema plan --to v4.8.0  — what would adopting it involve?
//!   eunha-schema check             — does a live database match the target?

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use eunha::{schema_check, upstream, version};

#[derive(Parser, Debug)]
#[command(
    name = "eunha-schema",
    about = "Track Mastodon releases and verify database schema compatibility"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Compare the tracked Mastodon release against the newest one upstream.
    Status,
    /// List the upstream migrations between what eunha covers and a target release.
    Plan {
        /// Target release tag, e.g. `v4.8.0`. Defaults to the newest stable release.
        #[arg(long)]
        to: Option<String>,
        /// Print an INSERT for public.schema_migrations covering the new versions.
        #[arg(long)]
        sql: bool,
    },
    /// Diff a live database against upstream's schema for the tracked release.
    Check {
        /// Database to inspect. Defaults to $DATABASE_URL.
        #[arg(long)]
        database_url: Option<String>,
        /// Check against a different release than the tracked one.
        #[arg(long)]
        against: Option<String>,
        /// Read db/schema.rb from a local Mastodon checkout instead of GitHub.
        #[arg(long, value_name = "PATH")]
        schema_rb: Option<std::path::PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    match Args::parse().command {
        Command::Status => status().await,
        Command::Plan { to, sql } => plan(to, sql).await,
        Command::Check {
            database_url,
            against,
            schema_rb,
        } => check(database_url, against, schema_rb).await,
    }
}

/// The tag for the release this build targets.
fn tracked_tag() -> String {
    format!("v{}", version::MASTODON)
}

async fn status() -> Result<()> {
    println!("eunha            {}", version::EUNHA_FULL);
    println!(
        "tracking         Mastodon {} (schema {})",
        version::MASTODON,
        version::MASTODON_SCHEMA
    );

    let latest = upstream::latest_release().await?;
    println!(
        "latest upstream  Mastodon {} (released {})",
        latest.version, latest.published_at
    );

    if latest.version == version::MASTODON {
        println!("\nUp to date.");
        return Ok(());
    }

    // A newer tag is not always a newer *branch*: upstream backports fixes to
    // older series, so 4.6.7 can be published after 4.7.0. Only report a real
    // upgrade if the tag differs from the one being tracked.
    let migrations = upstream::migrations(&latest.tag).await?;
    let covered = schema_check::covered_versions(migrations_dir().as_path())?;
    let outstanding = migrations
        .iter()
        .filter(|m| !covered.contains(&m.version))
        .count();

    println!(
        "\nMastodon {} is available, with {outstanding} migration(s) eunha does not cover yet.",
        latest.version
    );
    println!(
        "Run `eunha-schema plan --to {}` for the details.",
        latest.tag
    );
    Ok(())
}

async fn plan(to: Option<String>, sql: bool) -> Result<()> {
    let target = match to {
        Some(tag) if tag.starts_with('v') => tag,
        Some(version) => format!("v{version}"),
        None => upstream::latest_release().await?.tag,
    };

    let migrations = upstream::migrations(&target).await?;
    let covered = schema_check::covered_versions(migrations_dir().as_path())?;
    let outstanding: Vec<_> = migrations
        .iter()
        .filter(|m| !covered.contains(&m.version))
        .collect();

    let adopting = target != tracked_tag();
    if adopting {
        println!(
            "Adopting Mastodon {target} from {} ({} of {} migrations outstanding)\n",
            tracked_tag(),
            outstanding.len(),
            migrations.len()
        );
    } else {
        println!(
            "Already tracking Mastodon {target} ({} of {} migrations outstanding)\n",
            outstanding.len(),
            migrations.len()
        );
    }

    if outstanding.is_empty() {
        println!("migrations/ already covers every migration in {target}.");
    } else {
        for migration in &outstanding {
            let kind = if migration.post_deploy {
                "post"
            } else {
                "    "
            };
            println!("  {kind}  {}  {}", migration.version, migration.name);
        }
        println!("\nSource: https://github.com/mastodon/mastodon/tree/{target}/db");
    }

    if adopting {
        let schema = upstream::schema(&target).await?;
        println!("\nSchema version at {target}: {}", schema.version);
        println!(
            "Once migrations/ covers those, set mastodon.toml to version = \"{}\",",
            target.trim_start_matches('v')
        );
        println!(
            "schema_version = \"{}\", and give Cargo.toml a version ending in",
            schema.version
        );
        println!(
            "`+mastodon.{}`. Then `mise run schema:check`.",
            target.trim_start_matches('v')
        );
    } else if !outstanding.is_empty() {
        println!(
            "\nThese are outstanding by choice; see the migration that covers this\n\
             release for why. `mise run schema:check` confirms the schema itself matches."
        );
    }

    if sql && !outstanding.is_empty() {
        println!("\n-- Record the migrations this covers, as Mastodon would.");
        println!("INSERT INTO public.schema_migrations (version) VALUES");
        let rows: Vec<String> = outstanding
            .iter()
            .map(|m| format!("    ('{}')", m.version))
            .collect();
        println!("{}\nON CONFLICT (version) DO NOTHING;", rows.join(",\n"));
    }

    Ok(())
}

async fn check(
    database_url: Option<String>,
    against: Option<String>,
    schema_rb: Option<std::path::PathBuf>,
) -> Result<()> {
    let database_url = database_url
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database given: pass --database-url or set DATABASE_URL")?;
    let target = match against {
        Some(tag) if tag.starts_with('v') => tag,
        Some(version) => format!("v{version}"),
        None => tracked_tag(),
    };

    let (expected, source) = match &schema_rb {
        Some(path) => {
            let body = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            (upstream::parse_schema_rb(&body), path.display().to_string())
        }
        None => (
            upstream::schema(&target).await?,
            format!("mastodon/mastodon@{target}"),
        ),
    };
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .with_context(|| format!("connecting to {database_url}"))?;
    let live = schema_check::introspect(&pool).await?;

    let findings = schema_check::diff(&live, &expected);
    if findings.is_empty() {
        println!(
            "Schema matches {source} ({} tables, {} foreign keys).",
            expected.tables.len(),
            expected.foreign_keys.len()
        );
        return Ok(());
    }

    println!("{} difference(s) from {source}:\n", findings.len());
    for finding in &findings {
        println!("  {finding}");
    }
    // A drifted schema is a failure, so CI and `mise run schema:check` notice.
    std::process::exit(1);
}

/// eunha's own migrations, relative to the crate rather than the cwd.
fn migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}
