//! `boss-views-api` — the View registry + the endpoint that runs one.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "boss-views-api", about = "Views API", version)]
struct Cli {
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    postgres_url: String,
    #[arg(long, default_value_t = boss_ports::prod("views"))]
    http_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();
    let cli = Cli::parse();

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&cli.postgres_url)
        .await
        .context("connecting to Postgres")?;

    let app = boss_views::http::router(boss_views::http::ViewsApiState {
        repo: Arc::new(boss_views::PgViewsRepo::new(pool.clone())),
        resolver: Arc::new(boss_views::PgViewResolver::new(pool)),
    });
    let addr = format!("127.0.0.1:{}", cli.http_port);
    tracing::info!(addr = %addr, "boss-views-api listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
