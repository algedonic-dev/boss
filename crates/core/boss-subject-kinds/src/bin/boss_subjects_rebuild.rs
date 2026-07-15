//! `boss-subjects-rebuild` — reproject the `subjects` identity table.
//!
//! The projection half of the R1 dual contract
//! (docs/design/subject-identity-and-relationships.md, Q1): every
//! identity row must be reproducible from `audit_log` alone. Sources:
//!
//! - the TOML-registered identity-bearing events
//!   (`seeds/subject_identity_sources.toml`) — one upsert per event;
//! - `jobs.job.created` — its `{subject_kind, subject_id}` pair
//!   proves the subject existed (identity-first), which is what
//!   homes kinds with no domain table (campaign today).
//!
//! `--backfill-from-projections` additionally upserts from the live
//! domain tables — the FIRST-deploy path, covering rows whose create
//! events predate the epoch baseline (seed-loaded projections) or
//! seed-only kinds (locations). Honestly labeled: that mode reads
//! projections, the default mode reads only the log.
//!
//! Exit codes: 0 ok, 1 operational error.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-subjects-rebuild",
    about = "Reproject the subjects identity table from audit_log",
    version
)]
struct Cli {
    /// Postgres connection string. Falls back to the config file's
    /// `postgres_url`, then `DATABASE_URL`.
    #[arg(long)]
    database_url: Option<String>,

    /// Optional config file (TOML with `postgres_url`).
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Also upsert identities from the live domain tables — the
    /// first-deploy backfill for rows whose create events predate the
    /// epoch baseline, and for seed-only kinds (locations).
    #[arg(long, default_value_t = false)]
    backfill_from_projections: bool,
}

#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    postgres_url: Option<String>,
}

/// (table, kind, id column, label column) — the projection-side
/// backfill map. Deliberately mirrors the domain tables the audit
/// catalogued; only used with `--backfill-from-projections`.
const PROJECTION_SOURCES: &[(&str, &str, &str, Option<&str>)] = &[
    ("accounts", "account", "id", Some("name")),
    ("employees", "employee", "id", Some("name")),
    ("vendors", "vendor", "id", Some("name")),
    ("assets", "asset", "asset_id", None),
    ("locations", "location", "id", Some("name")),
    ("products", "product", "sku", Some("name")),
    ("invoices", "invoice", "id", None),
    ("purchase_orders", "purchase_order", "id::text", None),
    ("vendor_invoices", "vendor-invoice", "id", None),
    ("shipments", "shipment", "id", None),
    ("marketing_assets", "marketing-asset", "id", Some("title")),
    ("messages", "message", "id", None),
];

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .compact()
        .init();

    let cli = Cli::parse();
    let cfg: ConfigFile = match &cli.config {
        Some(path) => toml::from_str(
            &std::fs::read_to_string(path)
                .with_context(|| format!("reading config {}", path.display()))?,
        )
        .with_context(|| format!("parsing config {}", path.display()))?,
        None => ConfigFile::default(),
    };
    let db_url = resolve_database_url(
        cli.database_url,
        cfg.postgres_url,
        &["DATABASE_URL"],
        "boss-subjects-rebuild",
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .context("connecting to Postgres")?;

    let mut total = boss_subject_kinds::subjects::rebuild_subjects(&pool)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    info!(upserts = total, "log-sourced identity rebuild complete");

    if cli.backfill_from_projections {
        for (table, kind, id_col, label_col) in PROJECTION_SOURCES {
            let label_expr = label_col.unwrap_or("NULL");
            let sql = format!(
                "INSERT INTO subjects (kind, id, label) \
                 SELECT $1, {id_col}, {label_expr} FROM {table} \
                 ON CONFLICT (kind, id) DO UPDATE \
                    SET label = COALESCE(EXCLUDED.label, subjects.label)"
            );
            let res = sqlx::query(&sql)
                .bind(kind)
                .execute(&pool)
                .await
                .with_context(|| format!("backfill from {table}"))?;
            info!(table, kind, rows = res.rows_affected(), "backfilled");
            total += res.rows_affected();
        }
    }

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM subjects")
        .fetch_one(&pool)
        .await?;
    info!(
        upserts = total,
        subjects = count,
        "subjects rebuild complete"
    );
    Ok(())
}
