use clap::{Parser, Subcommand};
use eunha::{build_app, config, migrate, state};
use sqlx::{postgres::PgPoolOptions, Executor as _};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "eunha", about = "A Mastodon-compatible ActivityPub server")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Apply pending database migrations and exit.
    ///
    /// Separate from serving on purpose: a migration takes as long as it takes,
    /// and some of them are destructive. Running them from a deploy script,
    /// before the new binary starts, means a failure is found with the old
    /// version still serving rather than with nothing serving at all.
    Migrate {
        /// Report what is pending without applying anything. Exits non-zero if
        /// the database is behind this binary.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "eunha=debug,tower_http=info,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Migrating needs a database and nothing else. Loading the full config
    // would make a deploy script's migrate step fail for want of an S3 bucket,
    // which has no bearing on whether the schema can be brought up to date.
    if let Some(Command::Migrate { check }) = args.command {
        let db = connect(&migration_database_url()?).await?;
        return match (check, migrate::pending(&db).await?) {
            (true, None) => {
                println!("Database is up to date.");
                Ok(())
            }
            (true, Some(pending)) => {
                println!("{pending}");
                std::process::exit(1);
            }
            (false, _) => {
                migrate::run(&db).await?;
                println!("Migrations applied.");
                Ok(())
            }
        };
    }

    let config = config::Config::from_env()?;

    // Resolve unqualified names against the eunha schema first, then public.
    // All app queries are schema-qualified, so this only affects where sqlx
    // creates its unqualified `_sqlx_migrations` bookkeeping table.
    let db = connect(&config.database_url).await?;

    // Serving refuses to start against a schema this binary does not know,
    // rather than running queries against a shape that has moved underneath
    // them. `eunha migrate` is the fix, and the message says so.
    if let Some(pending) = migrate::pending(&db).await? {
        anyhow::bail!("{pending}. Run `eunha migrate` before starting the server.");
    }

    let state = state::AppState::new(db, config.clone()).await?;

    // Mastodon 4.7's post-deploy migration, which cannot be SQL: the keys it
    // moves have to be encrypted with this instance's configured secrets.
    if let Err(e) = eunha::federation::keypair::migrate_local_keypairs(&state).await {
        tracing::error!(error = %e, "could not move local signing keys into `keypairs`");
    }
    eunha::background::spawn(state.clone());
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_address).await?;
    tracing::info!("listening on {}", config.bind_address);
    axum::serve(listener, app).await?;

    Ok(())
}

/// Open a pool whose connections resolve unqualified names against the eunha
/// schema first, then public. Every app query is schema-qualified, so this only
/// decides where sqlx keeps its own `_sqlx_migrations` ledger — out of `public`,
/// which stays a pure mirror of Mastodon's schema.
async fn connect(database_url: &str) -> anyhow::Result<sqlx::PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(20)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("SET search_path TO eunha, public").await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await?)
}

/// The database to migrate: `DATABASE_URL` if set (including from `.env`),
/// otherwise whatever the server would have used.
fn migration_database_url() -> anyhow::Result<String> {
    dotenvy::dotenv().ok();
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    Ok(config::Config::from_env()?.database_url)
}
