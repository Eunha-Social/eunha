use eunha::{build_app, config, state};
use sqlx::{postgres::PgPoolOptions, Executor as _};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            "eunha=debug,tower_http=info,sqlx=warn".into()
        }))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::Config::from_env()?;

    // Resolve unqualified names against the eunha schema first, then public.
    // All app queries are schema-qualified, so this only affects where sqlx
    // creates its unqualified `_sqlx_migrations` bookkeeping table.
    let db = PgPoolOptions::new()
        .max_connections(20)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                conn.execute("SET search_path TO eunha, public").await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await?;

    // Pre-create `eunha` so `_sqlx_migrations` lands there (first on the search
    // path) instead of `public`, keeping `public` a pure Mastodon-schema mirror.
    db.execute("CREATE SCHEMA IF NOT EXISTS eunha").await?;
    sqlx::migrate!("./migrations").run(&db).await?;

    let state = state::AppState::new(db, config.clone()).await?;
    eunha::background::spawn(state.clone());
    let app = build_app(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_address).await?;
    tracing::info!("listening on {}", config.bind_address);
    axum::serve(listener, app).await?;

    Ok(())
}
