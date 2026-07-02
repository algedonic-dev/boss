//! `boss-ledger-replay-check` — reconstruction integrity check.
//!
//! Default (entry-level): replays every `financial_facts` row through the
//! active posting-rule registry inside an aborted transaction and diffs the
//! rebuilt `gl_journal_entries`/`gl_journal_lines` against the live
//! projection.
//!
//! `--deep` (fact-level): additionally reprojects `financial_facts` from
//! `audit_log` and diffs the rebuilt facts against live BEFORE the entry
//! diff. This is the only mode that catches live-vs-rebuild *fact-payload*
//! drift — the entry-level check can't see it because posting rules read
//! only a few payload keys and ignore the rest. Any divergence is either
//! non-determinism in a rule or a live-side write that bypassed the shared
//! projection shape. Both are bugs.
//!
//! Wire as a daily systemd timer alongside `boss-audit-integrity-check`,
//! and as a CI gate on every PR that touches `gl_rule_versions` or a
//! fact-emitting write path. Prefer `--deep` once the projection is clean.
//!
//! Exit codes:
//! - 0 — projection matches replay byte-for-byte
//! - 1 — divergence found (details on stderr)
//! - 2 — operational error (DB unreachable, etc.)

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::replay_check::{self, Divergence, FactDivergence};
use clap::Parser;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-ledger-replay-check",
    about = "Verify gl_journal_entries (and, with --deep, financial_facts) reconstruct 1:1",
    version
)]
struct Cli {
    /// Config path. Shares `boss-ledger-api.toml` since the DB URL is
    /// the same — the verifier just reads the same pool.
    #[arg(short, long, default_value = "/etc/boss-ledger-api.toml")]
    config: PathBuf,

    /// Maximum number of divergences to print before truncating output.
    /// Defaults to 25; the report's totals always reflect the full
    /// divergence set even when output is truncated.
    #[arg(long, default_value_t = 25)]
    max_print: usize,

    /// Run the DEEP fact-level check: reproject `financial_facts` from
    /// `audit_log` and diff the rebuilt facts against live, in addition to
    /// the entry-level diff. Catches fact-payload drift the default mode
    /// cannot see. Slower (a full TRUNCATE-then-reproject inside the
    /// aborted tx). Live state is untouched either way.
    #[arg(long, default_value_t = false)]
    deep: bool,
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
    let cfg = LedgerApiConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.postgres_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    if cli.deep {
        run_deep(&pool, cli.max_print).await
    } else {
        run_shallow(&pool, cli.max_print).await
    }
}

/// Entry-level check (default): financial_facts → gl_journal_entries.
async fn run_shallow(pool: &PgPool, max_print: usize) -> Result<()> {
    info!("running ledger 1:1 reconstruction integrity check (entry-level)");

    let report = match replay_check::replay_check(pool).await {
        Ok(r) => r,
        Err(e) => {
            error!("replay-check failed: {e}");
            std::process::exit(2);
        }
    };

    info!(
        facts_replayed = report.facts_replayed,
        open_periods = report.open_periods,
        live_entries = report.live_entries,
        replay_entries = report.replay_entries,
        divergences = report.divergences.len(),
        "replay-check complete"
    );

    if report.is_ok() {
        info!("OK — projection matches replay 1:1");
        return Ok(());
    }

    eprintln!(
        "DIVERGENCE: {} entries differ between live and replay",
        report.divergences.len()
    );
    for (i, d) in report.divergences.iter().take(max_print).enumerate() {
        eprintln!("--- divergence {} ---", i + 1);
        print_entry_divergence(d);
    }
    if report.divergences.len() > max_print {
        eprintln!(
            "... {} more divergences truncated (raise --max-print to see them)",
            report.divergences.len() - max_print
        );
    }

    std::process::exit(1);
}

/// Fact-level check (`--deep`): audit_log → financial_facts → gl_journal_entries.
async fn run_deep(pool: &PgPool, max_print: usize) -> Result<()> {
    info!("running DEEP audit-log-rooted reconstruction integrity check (fact + entry level)");

    let report = match replay_check::replay_check_from_audit_log(pool).await {
        Ok(r) => r,
        Err(e) => {
            error!("deep replay-check failed: {e}");
            std::process::exit(2);
        }
    };

    info!(
        events_scanned = report.events_scanned,
        facts_in_live = report.facts_in_live,
        facts_in_replay = report.facts_in_replay,
        fact_divergences = report.fact_divergences.len(),
        open_periods = report.open_periods,
        live_entries = report.live_entries,
        replay_entries = report.replay_entries,
        entry_divergences = report.entry_divergences.len(),
        "deep replay-check complete"
    );

    if report.is_ok() {
        info!("OK — facts AND entries reconstruct 1:1 from audit_log");
        return Ok(());
    }

    if !report.fact_divergences.is_empty() {
        eprintln!(
            "FACT DIVERGENCE: {} facts differ between live and rebuild",
            report.fact_divergences.len()
        );
        for (i, d) in report.fact_divergences.iter().take(max_print).enumerate() {
            eprintln!("--- fact divergence {} ---", i + 1);
            print_fact_divergence(d);
        }
    }
    if !report.entry_divergences.is_empty() {
        eprintln!(
            "ENTRY DIVERGENCE: {} entries differ between live and replay",
            report.entry_divergences.len()
        );
        for (i, d) in report.entry_divergences.iter().take(max_print).enumerate() {
            eprintln!("--- entry divergence {} ---", i + 1);
            print_entry_divergence(d);
        }
    }

    std::process::exit(1);
}

/// Render one entry-level divergence to stderr. Shared by both modes.
fn print_entry_divergence(d: &Divergence) {
    match d {
        Divergence::OnlyInLive { key, live } => {
            eprintln!("only-in-live  fact={} rv={}", key.0, key.1);
            eprintln!("  posted_on={} memo={:?}", live.posted_on, live.memo);
            for l in &live.lines {
                eprintln!(
                    "    {} dr={} cr={} {} {:?}",
                    l.account_code, l.debit_cents, l.credit_cents, l.currency, l.memo
                );
            }
        }
        Divergence::OnlyInReplay { key, replay } => {
            eprintln!("only-in-replay fact={} rv={}", key.0, key.1);
            eprintln!("  posted_on={} memo={:?}", replay.posted_on, replay.memo);
            for l in &replay.lines {
                eprintln!(
                    "    {} dr={} cr={} {} {:?}",
                    l.account_code, l.debit_cents, l.credit_cents, l.currency, l.memo
                );
            }
        }
        Divergence::Mismatch { key, live, replay } => {
            eprintln!("mismatch       fact={} rv={}", key.0, key.1);
            eprintln!(
                "  live    posted_on={} memo={:?}",
                live.posted_on, live.memo
            );
            for l in &live.lines {
                eprintln!(
                    "    L {} dr={} cr={} {} {:?}",
                    l.account_code, l.debit_cents, l.credit_cents, l.currency, l.memo
                );
            }
            eprintln!(
                "  replay  posted_on={} memo={:?}",
                replay.posted_on, replay.memo
            );
            for l in &replay.lines {
                eprintln!(
                    "    R {} dr={} cr={} {} {:?}",
                    l.account_code, l.debit_cents, l.credit_cents, l.currency, l.memo
                );
            }
        }
    }
}

/// Render one fact-level divergence to stderr. The key is
/// `(kind, source_table, source_id)`; payloads are printed compactly so an
/// operator can see exactly which keys drifted.
fn print_fact_divergence(d: &FactDivergence) {
    match d {
        FactDivergence::OnlyInLive { key, live } => {
            eprintln!("only-in-live  kind={} src={}/{}", key.0, key.1, key.2);
            eprintln!(
                "  happened_on={} created_by={} payload={}",
                live.happened_on, live.created_by, live.payload
            );
        }
        FactDivergence::OnlyInReplay { key, replay } => {
            eprintln!("only-in-replay kind={} src={}/{}", key.0, key.1, key.2);
            eprintln!(
                "  happened_on={} created_by={} payload={}",
                replay.happened_on, replay.created_by, replay.payload
            );
        }
        FactDivergence::Mismatch { key, live, replay } => {
            eprintln!("mismatch       kind={} src={}/{}", key.0, key.1, key.2);
            eprintln!("  live   payload={}", live.payload);
            eprintln!("  replay payload={}", replay.payload);
        }
    }
}
