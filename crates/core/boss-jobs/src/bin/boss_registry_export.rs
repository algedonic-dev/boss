//! `boss-registry-export` — write, or verify, a recovery export of the
//! four runtime registries (workflows, stations, pools, cadence rules)
//! straight from the deployment's database.
//!
//! Backlog c26aacaa, design 942dc0ac Q2 (David, 2026-11-08): the live
//! registry becomes the authority after bootstrap, conditional on a
//! generated, versioned recovery export, a tested offline restore and a
//! deliberate import. This binary is the export and its read-back; the
//! format and the comparison live in `boss_jobs::registry_export`.
//!
//! IT READS THE DATABASE, NOT THE JOBS API, on purpose: a recovery copy
//! that can only be taken while the API serves is not one an operator
//! can take on the day the API is the thing that is broken (CLAUDE.md
//! §Diagnosis, "an arm that needs the patient is not an arm"). It only
//! reads; nothing here writes a row.
//!
//!   boss-registry-export --out <file> [--source <label>]
//!       Write every row of every version as one JSON document and
//!       print its digest and row counts. Refuses to overwrite a file.
//!   boss-registry-export --verify <file>
//!       Check the file's own digest, read the registries now, and name
//!       every row on which they disagree. Exit 0 only when none does.
//!
//! Exit codes: 0 — written, or verified equal; 1 — a verify found a
//! difference; other — refused (a bad file, a database unreachable).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use boss_jobs::cadence::PgCadence;
use boss_jobs::pools::PgPools;
use boss_jobs::registry_export::{Registries, RegistryExport, diff, export};
use boss_jobs::{PgStations, PgWorkflows};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "boss-registry-export",
    about = "Write or verify a recovery export of the runtime registries",
    version
)]
struct Cli {
    /// Postgres URL for the deployment being exported.
    #[arg(long, env = "BOSS_POSTGRES_URL")]
    database_url: String,

    /// Write the export here. The file must not exist yet.
    #[arg(long, conflicts_with = "verify", required_unless_present = "verify")]
    out: Option<PathBuf>,

    /// A label the export carries naming what it copies.
    #[arg(long, default_value = "live registry")]
    source: String,

    /// Verify this export against the registries as they stand now.
    #[arg(long)]
    verify: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    // Read a file to verify BEFORE connecting: a file that fails its
    // own digest is refused without touching the database.
    let claimed = match &cli.verify {
        Some(path) => Some(read_export(path).await?),
        None => None,
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&cli.database_url)
        .await
        .context("connecting to Postgres")?;
    let workflows = PgWorkflows::new(pool.clone());
    let stations = PgStations::new(pool.clone());
    let pools = PgPools::new(pool.clone());
    let cadence = PgCadence::new(pool);
    let registries = Registries {
        workflows: &workflows,
        stations: &stations,
        pools: &pools,
        cadence: &cadence,
    };
    // A record stamp, not a business date: when this copy was read.
    let now = boss_clock_client::wall_now();
    let live = export(&registries, &cli.source, now)
        .await
        .context("reading the registries")?;

    if let Some(claimed) = claimed {
        let differences = diff(&claimed, &live)?;
        for d in &differences {
            println!("{d}");
        }
        if differences.is_empty() {
            println!(
                "registry-export: verified — {} matches the registries now ({})",
                claimed.digest,
                counts(&live)
            );
            return Ok(ExitCode::SUCCESS);
        }
        println!(
            "registry-export: {} row(s) differ — the export is {}, the registries now {}",
            differences.len(),
            claimed.digest,
            live.digest
        );
        return Ok(ExitCode::from(1));
    }

    let out = cli.out.context("--out or --verify is required")?;
    let text = serde_json::to_string_pretty(&live)?;
    write_new(&out, text.as_bytes()).await?;
    println!(
        "registry-export: wrote {} — {} ({})",
        out.display(),
        live.digest,
        counts(&live)
    );
    Ok(ExitCode::SUCCESS)
}

fn counts(e: &RegistryExport) -> String {
    format!(
        "{} workflow, {} station, {} pool, {} cadence row(s)",
        e.workflows.len(),
        e.stations.len(),
        e.pools.len(),
        e.cadence_rules.len()
    )
}

async fn read_export(path: &PathBuf) -> Result<RegistryExport> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let export: RegistryExport =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    export
        .checked()
        .with_context(|| format!("checking {}", path.display()))
}

/// Write `bytes` to a file that must not exist: an export is a record,
/// and overwriting one discards the copy it replaced.
async fn write_new(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .with_context(|| {
            format!(
                "creating {} (an export is never overwritten)",
                path.display()
            )
        })?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    Ok(())
}
