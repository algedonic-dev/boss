//! `boss-audit-integrity-check` — daily scan over `audit_log`.
//!
//! Layer 1 of the immutable-audit-log story
//! (`docs/architecture-decisions.md` §Correctness protocol & the
//! audit log). Schema-level triggers
//! reject UPDATE / DELETE / TRUNCATE; this binary is the second-line
//! check that surfaces evidence of someone bypassing the trigger
//! (DROP TRIGGER, ALTER TABLE DISABLE TRIGGER, restore-from-backup).
//!
//! Wired up via a systemd timer (`infra/boss-audit-integrity-check.timer`),
//! cron, or any scheduler. Exit codes:
//!
//! - `0` — the log holds. Warnings may still have been logged.
//! - `2` — evidence of tampering: a chain break, a `created_at`
//!   regression, or a dangling foreign ref.
//! - other — operational error (DB unreachable, bad config)
//!
//! `systemctl is-failed` flips on exit code 2, so the timer's
//! `OnFailure=` hook fires the alert.
//!
//! **An id gap on its own is not exit 2.** It used to be, and the
//! result was a nightly chore that stayed red for days over two gaps
//! that were aborted-transaction sequence burns — 56 ids, one of them
//! sitting exactly in the 2026-08-14 production-crash window. An alarm
//! that is permanently red trains people to ignore it, which is the
//! worst property an integrity alarm can have; it costs more than the
//! signal is worth. Deletion of a committed row is caught by the hash
//! chain, which was intact across both gaps. So a gap with an intact
//! chain is a WARNING that explains itself, and a gap with a broken
//! chain is an error — because the break is.

use std::path::PathBuf;

use anyhow::{Context, Result};
use boss_core::rebuild::resolve_database_url;
use boss_events::{GapReading, IntegrityReport, check_audit_log_integrity};
use chrono::{DateTime, Utc};
use clap::Parser;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// The current chain head — what an auditor compares against on
/// their next handoff. `audit_log` is empty before the first event.
async fn current_chain_head(
    pool: &PgPool,
) -> Result<Option<(i64, Vec<u8>, Vec<u8>, DateTime<Utc>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, row_hash, prev_hash, created_at \
         FROM audit_log ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
}

#[derive(Parser, Debug)]
#[command(
    name = "boss-audit-integrity-check",
    about = "Scan audit_log for id gaps and created_at regressions",
    version
)]
struct Cli {
    /// Postgres connection string. Falls back to `BOSS_AUDIT_DATABASE_URL`
    /// then `DATABASE_URL`.
    #[arg(long)]
    database_url: Option<String>,

    /// Optional config file (TOML with a single `database_url` key).
    /// Provided so the timer unit can point at the same on-disk
    /// secrets file the API services use.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Print the full report as JSON on stdout regardless of result.
    /// The systemd journal-friendly default is one summary line.
    #[arg(long)]
    json: bool,
}

#[derive(serde::Deserialize)]
struct ConfigFile {
    database_url: String,
}

/// Read the `--config` file's `database_url`, when one was passed.
///
/// Unlike the projection-rebuild twins (which swallow a bad config and
/// fall through to env vars), an explicitly-supplied `--config` that is
/// unreadable or malformed is a hard error here — the operator asked for
/// that file specifically.
fn config_database_url(cli: &Cli) -> Result<Option<String>> {
    let Some(path) = &cli.config else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config from {}", path.display()))?;
    let cfg: ConfigFile =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg.database_url))
}

/// Log the run's id gaps at the level the hash chain earns them.
///
/// A gap the chain vouches for is reported with its explanation
/// attached, every run. That is the point: an operator reading the
/// journal should not have to remember which gaps are known, and a
/// warning that says only "id gap" is what made the old alarm
/// unreadable.
fn log_gaps(report: &IntegrityReport) {
    match report.gap_reading() {
        GapReading::NoGaps => {}
        GapReading::SequenceBurn => {
            warn!(
                gaps = report.gaps.len(),
                missing_ids = report.missing_ids(),
                "id gaps with an INTACT hash chain: sequence values burned by \
                 transactions that reached the id-allocating trigger and then \
                 aborted. Every committed row is still present and verified — \
                 a deletion would have broken the chain at the deleted row's \
                 successor. Not an integrity failure."
            );
            for gap in &report.gaps {
                warn!(
                    prev_id = gap.prev_id,
                    id = gap.id,
                    missing = gap.missing_count(),
                    "id gap (burned sequence values)"
                );
            }
        }
        GapReading::PossibleDeletion => {
            for gap in &report.gaps {
                error!(
                    prev_id = gap.prev_id,
                    id = gap.id,
                    missing = gap.missing_count(),
                    "id gap — and the hash chain did NOT verify. These ids may be \
                     deleted rows, not burned sequence values."
                );
            }
        }
    }
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
    let config_url = config_database_url(&cli)?;
    let db_url = resolve_database_url(
        cli.database_url,
        config_url,
        &["BOSS_AUDIT_DATABASE_URL", "DATABASE_URL"],
        "pass --database-url, --config <file>, \
         or set BOSS_AUDIT_DATABASE_URL / DATABASE_URL",
    )?;

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&db_url)
        .await
        .with_context(|| "connecting to Postgres")?;

    let report = check_audit_log_integrity(&pool)
        .await
        .with_context(|| "running integrity scan")?;

    // The event-kind drift guard (registry 108; warn-not-abort per
    // the review): an emitted kind nothing declares is a vocabulary
    // hole — loud in the journal, never a failed run.
    match boss_events::integrity::unregistered_kinds(&pool).await {
        Ok(missing) if !missing.is_empty() => tracing::warn!(
            count = missing.len(),
            kinds = %missing.join(", "),
            "event kinds EMITTED but not declared in the event_kinds registry — \
             add rows (or a family pattern) in a new migration"
        ),
        Ok(_) => info!("event-kind registry covers every emitted kind"),
        Err(e) => {
            tracing::warn!(error = %e, "event-kind drift check failed (registry table missing?)")
        }
    }

    // Layer 3: the daily checkpoint — log the current chain head so
    // each run leaves a journal entry an auditor can compare against
    // on a future DB snapshot. Cheap, no external service. Empty log
    // (fresh deployment) just logs an empty checkpoint.
    //
    // The gap census rides on this line rather than in an allowlist
    // file. Known gaps do get re-reported every night, deliberately:
    // the census is a MEASUREMENT, and one that moves — a third gap, a
    // wider one — is the thing worth seeing. An allowlist would hide
    // exactly that, would be a second mutable record of an immutable
    // log, and would hand an operator who deleted a row the one edit
    // that silences the alarm. Two consecutive checkpoints answer
    // "same gaps as yesterday?" without any of that.
    match current_chain_head(&pool).await? {
        Some((id, row_hash, prev_hash, created_at)) => info!(
            chain_head_id = id,
            chain_head_row_hash = %hex::encode(&row_hash),
            chain_head_prev_hash = %hex::encode(&prev_hash),
            chain_head_created_at = %created_at,
            total_rows = report.total_rows,
            gap_count = report.gaps.len(),
            missing_ids = report.missing_ids(),
            gap_reading = report.gap_reading().as_str(),
            chain_intact = report.chain_intact(),
            checkpoint_at = %Utc::now(),
            "audit_log checkpoint"
        ),
        None => info!(checkpoint_at = %Utc::now(), "audit_log checkpoint (empty log)"),
    }

    if cli.json {
        let body = serde_json::json!({
            "total_rows": report.total_rows,
            "gap_count": report.gaps.len(),
            "missing_ids": report.missing_ids(),
            "gap_reading": report.gap_reading().as_str(),
            "chain_intact": report.chain_intact(),
            "exit_code": if report.has_errors() { 2 } else { 0 },
            "regression_count": report.regressions.len(),
            "chain_break_count": report.chain_breaks.len(),
            "gaps": report.gaps.iter().map(|g| serde_json::json!({
                "prev_id": g.prev_id,
                "id": g.id,
                "missing_count": g.missing_count(),
            })).collect::<Vec<_>>(),
            "regressions": report.regressions.iter().map(|r| serde_json::json!({
                "prev_id": r.prev_id,
                "prev_created_at": r.prev_created_at,
                "id": r.id,
                "created_at": r.created_at,
            })).collect::<Vec<_>>(),
            "chain_breaks": report.chain_breaks.iter().map(|b| serde_json::json!({
                "id": b.id,
                "stored_hash": hex::encode(&b.stored_hash),
                "computed_hash": hex::encode(&b.computed_hash),
            })).collect::<Vec<_>>(),
            "dangling_ref_count": report.dangling_refs.len(),
            "dangling_refs": report.dangling_refs.iter().map(|r| serde_json::json!({
                "id": r.id,
                "kind": r.kind,
                "field": r.field,
                "foreign_id": r.foreign_id,
                "expected_parent_kind": r.expected_parent_kind,
            })).collect::<Vec<_>>(),
            "sanctioned_trim_gap": report.sanctioned_trim_gap.as_ref().map(|g| serde_json::json!({
                "prev_id": g.prev_id,
                "id": g.id,
                "missing_count": g.missing_count(),
            })),
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
    }

    // Visible but not an anomaly: the demo's epoch-restart trim
    // leaves exactly one gap starting at the seed baseline row —
    // by design, every rollover.
    if let Some(g) = &report.sanctioned_trim_gap {
        info!(
            prev_id = g.prev_id,
            id = g.id,
            trimmed_rows = g.missing_count(),
            "epoch-trim gap at the seed baseline (sanctioned — restart_epoch working as designed)"
        );
    }

    if report.is_clean() {
        info!(total_rows = report.total_rows, "audit_log integrity clean");
        return Ok(());
    }

    // Gaps are logged before the verdict because they are the one
    // signal whose severity is decided by another signal.
    log_gaps(&report);

    if !report.has_errors() {
        info!(
            total_rows = report.total_rows,
            gaps = report.gaps.len(),
            missing_ids = report.missing_ids(),
            "audit_log integrity holds — id gaps only, and the chain verified across them"
        );
        return Ok(());
    }

    warn!(
        total_rows = report.total_rows,
        gaps = report.gaps.len(),
        regressions = report.regressions.len(),
        chain_breaks = report.chain_breaks.len(),
        dangling_refs = report.dangling_refs.len(),
        "audit_log integrity anomalies"
    );
    for r in &report.regressions {
        error!(
            prev_id = r.prev_id,
            prev_created_at = %r.prev_created_at,
            id = r.id,
            created_at = %r.created_at,
            "created_at regression"
        );
    }
    for b in &report.chain_breaks {
        error!(
            id = b.id,
            stored = hex::encode(&b.stored_hash),
            computed = hex::encode(&b.computed_hash),
            "chain break"
        );
    }
    for d in &report.dangling_refs {
        error!(
            id = d.id,
            kind = %d.kind,
            field = %d.field,
            foreign_id = %d.foreign_id,
            expected_parent_kind = %d.expected_parent_kind,
            "dangling foreign ref"
        );
    }
    std::process::exit(2);
}
