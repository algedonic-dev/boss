//! `boss-ledger-replay-check` — 1:1 reconstruction integrity check.
//!
//! Replays every `financial_facts` row through the active posting-rule
//! registry inside an aborted transaction and diffs the rebuilt
//! `gl_journal_entries`/`gl_journal_lines` against the live projection.
//! Any divergence is either non-determinism in a posting rule or a
//! live-side mutation that bypassed the rule pipeline. Both are bugs.
//!
//! Wire as a daily systemd timer alongside `boss-audit-integrity-check`,
//! and as a CI gate on every PR that touches `gl_rule_versions`.
//!
//! Exit codes:
//! - 0 — projection matches replay byte-for-byte
//! - 1 — divergence found (details on stderr)
//! - 2 — operational error (DB unreachable, etc.)

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_ledger::config::LedgerApiConfig;
use boss_ledger::replay_check::{self, Divergence};
use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "boss-ledger-replay-check",
    about = "Verify gl_journal_entries reconstructs 1:1 from financial_facts",
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

    info!("running ledger 1:1 reconstruction integrity check");

    let report = match replay_check::replay_check(&pool).await {
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
    for (i, d) in report.divergences.iter().take(cli.max_print).enumerate() {
        eprintln!("--- divergence {} ---", i + 1);
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
    if report.divergences.len() > cli.max_print {
        eprintln!(
            "... {} more divergences truncated (raise --max-print to see them)",
            report.divergences.len() - cli.max_print
        );
    }

    std::process::exit(1);
}
