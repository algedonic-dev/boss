//! Prediction generators — deterministic rules wrapped as models.
//! Each generator loads the signals its model needs via direct SQL
//! against the shared DB, computes a score per entity, and POSTs
//! predictions through the MlRepository port.
//!
//! These are "rules that masquerade as models"
//! (docs/architecture-decisions.md §ML platform): no training, no
//! serving runtime, deterministic Rust code. The wrappers expose them as the same
//! `MlPrediction` shape a future learned model would produce, so
//! the consumer surfaces (CTO MlModelsPanel, territory-rep
//! watchlist, sales workbench) don't change when we swap in a real
//! model later.
//!
//! Idempotency: prediction ids embed the date stamp so a same-day
//! re-run is a no-op (ON CONFLICT DO NOTHING), while a next-day run
//! writes fresh rows. Running these on a nightly cron gives a
//! rolling prediction history keyed by (entity, date).

#![cfg(feature = "postgres")]

use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use serde_json::json;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::port::{MlError, MlRepository};
use crate::types::CreatePredictionInput;

/// Identifier of the churn-risk model (seeded by `bootstrap.rs`).
pub const MODEL_CHURN: &str = "mdl-account-churn-risk-v1";
/// Identifier of the device-MTBF model.
pub const MODEL_MTBF: &str = "mdl-device-mtbf-v1";
/// Identifier of the opportunity-win-probability model.
pub const MODEL_WIN_PROB: &str = "mdl-opportunity-win-probability-v1";

/// Per-generator summary — counts + timing for logging / CLI output.
#[derive(Debug, Clone)]
pub struct GeneratorSummary {
    pub model_id: &'static str,
    pub entities_scored: usize,
    pub predictions_written: usize,
    pub predictions_skipped: usize,
}

/// Run all three generators. Logs each step; a failure in
/// one generator doesn't short-circuit the others.
pub async fn run_all(repo: Arc<dyn MlRepository>, pool: &PgPool) -> Vec<GeneratorSummary> {
    let today = Utc::now().date_naive();
    let mut out = Vec::new();
    match run_churn_risk(repo.as_ref(), pool, today).await {
        Ok(s) => {
            info!(model = %s.model_id, scored = s.entities_scored, written = s.predictions_written, "churn generator done");
            out.push(s);
        }
        Err(e) => warn!(error = %e, "churn generator failed"),
    }
    match run_device_mtbf(repo.as_ref(), pool, today).await {
        Ok(s) => {
            info!(model = %s.model_id, scored = s.entities_scored, written = s.predictions_written, "MTBF generator done");
            out.push(s);
        }
        Err(e) => warn!(error = %e, "MTBF generator failed"),
    }
    match run_opportunity_win_prob(repo.as_ref(), pool, today).await {
        Ok(s) => {
            info!(model = %s.model_id, scored = s.entities_scored, written = s.predictions_written, "win-prob generator done");
            out.push(s);
        }
        Err(e) => warn!(error = %e, "win-prob generator failed"),
    }
    out
}

// ---------------------------------------------------------------------------
// Churn risk
// ---------------------------------------------------------------------------

/// Composite churn-risk score per account, on the same 0..=100 scale
/// the on-demand `GET /api/people/accounts/risk-scores` endpoint uses.
/// Duplicates the bump rules from
/// `boss-people::account_risk_scores::score_for` intentionally — a
/// cross-crate dep would pull in an axum router and a pg schema that
/// boss-ml doesn't otherwise need. Keep the two in sync when the rules
/// change.
pub async fn run_churn_risk(
    repo: &dyn MlRepository,
    pool: &PgPool,
    today: NaiveDate,
) -> Result<GeneratorSummary, MlError> {
    let accounts: Vec<(String,)> = sqlx::query_as("SELECT id FROM accounts ORDER BY id")
        .fetch_all(pool)
        .await
        .map_err(|e| MlError::Storage(e.to_string()))?;

    let last_invoice: std::collections::HashMap<String, NaiveDate> =
        sqlx::query_as::<_, (String, NaiveDate)>(
            "SELECT account_id, MAX(issued_on) FROM invoices GROUP BY account_id",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| MlError::Storage(e.to_string()))?
        .into_iter()
        .collect();

    let open_tickets: std::collections::HashMap<String, i64> = sqlx::query_as::<_, (String, i64)>(
        "SELECT subject_id, COUNT(*)::bigint \
             FROM jobs \
             WHERE subject_kind = 'account' AND status IN ('open', 'blocked') \
             GROUP BY subject_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| MlError::Storage(e.to_string()))?
    .into_iter()
    .collect();

    let active_contract: std::collections::HashMap<String, bool> =
        sqlx::query_as::<_, (String, i64)>(
            "SELECT account_id, COUNT(*)::bigint \
             FROM service_agreements \
             WHERE status = 'active' AND end_date >= $1::date \
             GROUP BY account_id",
        )
        .bind(today)
        .fetch_all(pool)
        .await
        .map_err(|e| MlError::Storage(e.to_string()))?
        .into_iter()
        .map(|(id, count)| (id, count > 0))
        .collect();

    let last_note: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
        sqlx::query_as::<_, (String, chrono::DateTime<chrono::Utc>)>(
            "SELECT account_id, MAX(occurred_at) \
             FROM account_notes WHERE deleted_at IS NULL \
             GROUP BY account_id",
        )
        .fetch_all(pool)
        .await
        .map_err(|e| MlError::Storage(e.to_string()))?
        .into_iter()
        .collect();

    let mut written = 0;
    let mut skipped = 0;
    let date_tag = today.format("%Y%m%d").to_string();
    for (account_id,) in &accounts {
        let days_invoice = last_invoice
            .get(account_id)
            .map(|d| (today - *d).num_days());
        let tickets = open_tickets.get(account_id).copied().unwrap_or(0);
        let has_contract = active_contract.get(account_id).copied().unwrap_or(false);
        let days_note = last_note
            .get(account_id)
            .map(|ts| (today - ts.date_naive()).num_days());
        let (score, top_factor) = churn_score(days_invoice, tickets, has_contract, days_note);

        // Only write non-zero scores so the table isn't spammed with
        // "0/100 healthy" rows for every account every day.
        if score == 0 {
            skipped += 1;
            continue;
        }

        let input = CreatePredictionInput {
            id: format!("pred-{MODEL_CHURN}-{account_id}-{date_tag}"),
            model_id: MODEL_CHURN.to_string(),
            entity_type: "account".to_string(),
            entity_id: account_id.clone(),
            score: score as f64,
            payload: Some(json!({
                "top_factor": top_factor,
                "days_since_last_invoice": days_invoice,
                "open_ticket_count": tickets,
                "has_active_contract": has_contract,
                "days_since_last_note": days_note,
            })),
        };
        repo.create_prediction(&input).await?;
        written += 1;
    }

    Ok(GeneratorSummary {
        model_id: MODEL_CHURN,
        entities_scored: accounts.len(),
        predictions_written: written,
        predictions_skipped: skipped,
    })
}

/// Duplicate of `account_risk_scores::score_for`. Kept here so
/// boss-ml doesn't need to depend on boss-people.
fn churn_score(
    days_since_last_invoice: Option<i64>,
    open_tickets: i64,
    has_active_contract: bool,
    days_since_last_note: Option<i64>,
) -> (i32, &'static str) {
    let mut bumps: Vec<(i32, &'static str)> = Vec::new();
    match days_since_last_invoice {
        Some(d) if d > 180 => bumps.push((25, "no invoice in 180+ days")),
        Some(d) if d > 90 => bumps.push((15, "no invoice in 90+ days")),
        Some(d) if d > 30 => bumps.push((5, "no invoice in 30+ days")),
        None => bumps.push((20, "never invoiced")),
        _ => {}
    }
    if open_tickets > 5 {
        bumps.push((20, "5+ open tickets"));
    } else if open_tickets > 2 {
        bumps.push((10, "3+ open tickets"));
    }
    if !has_active_contract {
        bumps.push((15, "no active service contract"));
    }
    match days_since_last_note {
        Some(d) if d > 180 => bumps.push((15, "no contact in 180+ days")),
        Some(d) if d > 90 => bumps.push((10, "no contact in 90+ days")),
        None => bumps.push((10, "no recorded contact")),
        _ => {}
    }
    let score: i32 = bumps.iter().map(|(n, _)| *n).sum::<i32>().min(100);
    let top = bumps
        .iter()
        .max_by_key(|(n, _)| *n)
        .map(|(_, l)| *l)
        .unwrap_or("healthy");
    (score, top)
}

// ---------------------------------------------------------------------------
// Device MTBF
// ---------------------------------------------------------------------------

/// Mean time between failures per active system. "Failure" = any
/// `PartReplaced` or `ServiceJobOpened` event in `asset_events`.
/// Systems with fewer than 2 failure events don't get a prediction —
/// one data point isn't enough to fit even a mean.
pub async fn run_device_mtbf(
    repo: &dyn MlRepository,
    pool: &PgPool,
    today: NaiveDate,
) -> Result<GeneratorSummary, MlError> {
    let rows: Vec<(String, NaiveDate)> = sqlx::query_as(
        "SELECT asset_id, ts \
         FROM asset_events \
         WHERE kind IN ('PartReplaced', 'ServiceJobOpened') \
         ORDER BY asset_id, ts",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| MlError::Storage(e.to_string()))?;

    let mut by_system: std::collections::BTreeMap<String, Vec<NaiveDate>> =
        std::collections::BTreeMap::new();
    for (asset_id, ts) in rows {
        by_system.entry(asset_id).or_default().push(ts);
    }

    let mut written = 0;
    let mut skipped = 0;
    let scored = by_system.len();
    let date_tag = today.format("%Y%m%d").to_string();
    for (asset_id, timestamps) in &by_system {
        if timestamps.len() < 2 {
            skipped += 1;
            continue;
        }
        let spans: Vec<i64> = timestamps
            .windows(2)
            .map(|w| (w[1] - w[0]).num_days())
            .collect();
        let mean_days = spans.iter().sum::<i64>() as f64 / spans.len() as f64;
        let input = CreatePredictionInput {
            id: format!("pred-{MODEL_MTBF}-{asset_id}-{date_tag}"),
            model_id: MODEL_MTBF.to_string(),
            entity_type: "system".to_string(),
            entity_id: asset_id.clone(),
            score: mean_days,
            payload: Some(json!({
                "failure_event_count": timestamps.len(),
                "span_count": spans.len(),
                "first_event": timestamps.first().map(|d| d.to_string()),
                "last_event": timestamps.last().map(|d| d.to_string()),
            })),
        };
        repo.create_prediction(&input).await?;
        written += 1;
    }

    Ok(GeneratorSummary {
        model_id: MODEL_MTBF,
        entities_scored: scored,
        predictions_written: written,
        predictions_skipped: skipped,
    })
}

// ---------------------------------------------------------------------------
// Opportunity win-probability
// ---------------------------------------------------------------------------

/// Heuristic win-probability per open sale Job. Score in 0..=1:
/// - Baseline from step progression (completed_steps / total_steps)
/// - +0.20 if the subject account is platinum tier
/// - +0.10 if the subject account is gold tier
/// - Clamped to [0.0, 1.0]
///
/// Closed jobs are excluded — we only predict on in-flight pipeline.
pub async fn run_opportunity_win_prob(
    repo: &dyn MlRepository,
    pool: &PgPool,
    today: NaiveDate,
) -> Result<GeneratorSummary, MlError> {
    let jobs: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id::text, subject_kind, subject_id \
         FROM jobs \
         WHERE kind = 'sale' AND status IN ('open', 'blocked')",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| MlError::Storage(e.to_string()))?;

    // Pre-load account tiers so we're not doing N queries.
    let tiers: std::collections::HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT id, tier FROM accounts")
            .fetch_all(pool)
            .await
            .map_err(|e| MlError::Storage(e.to_string()))?
            .into_iter()
            .collect();

    let mut written = 0;
    let mut skipped = 0;
    let scored = jobs.len();
    let date_tag = today.format("%Y%m%d").to_string();
    for (job_id, subject_kind, subject_id) in &jobs {
        // Step progression.
        let step_counts: (i64, i64) = sqlx::query_as(
            "SELECT \
                COUNT(*) FILTER (WHERE status = 'done')::bigint, \
                COUNT(*)::bigint \
             FROM steps WHERE job_id = $1::uuid",
        )
        .bind(job_id)
        .fetch_one(pool)
        .await
        .map_err(|e| MlError::Storage(e.to_string()))?;
        let (done, total) = step_counts;
        if total == 0 {
            skipped += 1;
            continue;
        }
        let progression = done as f64 / total as f64;

        let tier_bump = if subject_kind == "account" {
            match tiers.get(subject_id).map(|s| s.as_str()) {
                Some("platinum") => 0.20,
                Some("gold") => 0.10,
                _ => 0.0,
            }
        } else {
            0.0
        };

        let score = (progression + tier_bump).clamp(0.0, 1.0);

        let input = CreatePredictionInput {
            id: format!("pred-{MODEL_WIN_PROB}-{job_id}-{date_tag}"),
            model_id: MODEL_WIN_PROB.to_string(),
            entity_type: "opportunity".to_string(),
            entity_id: job_id.clone(),
            score,
            payload: Some(json!({
                "steps_done": done,
                "steps_total": total,
                "progression": progression,
                "tier_bump": tier_bump,
                "account_id": if subject_kind == "account" { Some(subject_id.clone()) } else { None },
            })),
        };
        repo.create_prediction(&input).await?;
        written += 1;
    }

    Ok(GeneratorSummary {
        model_id: MODEL_WIN_PROB,
        entities_scored: scored,
        predictions_written: written,
        predictions_skipped: skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_account_scores_zero_with_healthy_top_factor() {
        let (score, top) = churn_score(Some(7), 1, true, Some(14));
        assert_eq!(score, 0);
        assert_eq!(top, "healthy");
    }

    #[test]
    fn dormant_account_scores_high() {
        let (score, top) = churn_score(Some(200), 0, false, Some(200));
        // 25 (invoice 180+) + 15 (no contract) + 15 (no contact 180+)
        assert_eq!(score, 55);
        assert_eq!(top, "no invoice in 180+ days");
    }

    #[test]
    fn score_clamps_to_100() {
        let (score, _) = churn_score(Some(9999), 50, false, Some(9999));
        assert!(score <= 100);
    }
}
