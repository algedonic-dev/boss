//! Churn risk score — explainable composite over each account's
//! activity signals. Used by the watchlist panel on Exec and CTO
//! dashboards.
//!
//! The endpoint reads the most recent prediction per account from
//! `ml_predictions` for `model_id='mdl-account-churn-risk-v1'`.
//! Predictions are written by the
//! `account-churn-risk-composite-v1` plugin (boss-ml-plugins)
//! when `boss-ml-api` runs `infer-batch` on a daily cron. The
//! plugin computes the four-signal bumps below normalised to
//! 0.0..=1.0; the integer 0..=100 score and the human-readable
//! factor labels are carried verbatim in the prediction payload,
//! which is the `RiskScore` shape this endpoint returns.
//!
//! ## Signals
//!
//! - **Invoice cadence.** Days since the most recent invoice issue.
//!   Long gaps are the strongest churn signal — a Boss customer
//!   normally shows up monthly or quarterly through service work.
//! - **Open tickets.** Direct `subject_kind=account` job count with
//!   status in (open, blocked). A v2 can fold in system-subject jobs.
//! - **Contract status.** Any active service_agreement whose
//!   end_date is still in the future.
//! - **Engagement recency.** Days since the most recent account_note.
//!   No formal NPS yet, so "has anyone talked to them lately" is the
//!   best proxy.
//!
//! Score starts at 0 (healthy); every signal adds bumps up to 100.
//! The `top_factor` string names the biggest bump so the UI can
//! render a one-sentence explanation next to the number.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_policy::{AccessTier, User};
use boss_policy_client::CurrentUser;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Model id whose predictions back the watchlist response. Owned
/// here (not in config) because this endpoint is tied to a
/// specific seeded model row in
/// `boss-ml::bootstrap::seed_phase_two_candidates`.
const CHURN_RISK_MODEL_ID: &str = "mdl-account-churn-risk-v1";

/// Operator tier, or a role with broad account access. NOT a request
/// with no `x-boss-user`: that arrives as `role=guest` and was admitted
/// by name here — the full watchlist for nobody, while a signed-in
/// service tech was refused (backlog 2f4be936; decided under e84de48e,
/// David 2026-09-25: a request without the identity header is not
/// trusted).
fn is_trusted_or_broad(user: &User) -> bool {
    user.access_tier == AccessTier::Operator
        || boss_core::roles::has_broad_account_access(&user.role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_without_the_identity_header_is_refused() {
        assert!(!is_trusted_or_broad(&User::anonymous()));
    }

    fn scored(id: &str, score: i32) -> RiskScore {
        RiskScore {
            account_id: id.to_string(),
            account_name: id.to_string(),
            score,
            top_factor: "healthy".to_string(),
            factors: RiskFactors {
                days_since_last_invoice: None,
                open_ticket_count: 0,
                has_active_contract: false,
                days_since_last_note: None,
            },
        }
    }

    /// Backlog 9269d612 (page audit 08b0c4f8 GAP 8): `total_scored` was
    /// `filtered.len()` AFTER `.take(limit)`, so it was the page size
    /// under another name and /watchlist could not tell 200 scored from
    /// 200 of 350. It counts every score past the cutoff, before the page.
    #[test]
    fn total_scored_counts_past_the_limit() {
        let scores = (0..5).map(|i| scored(&format!("a{i}"), i * 10)).collect();
        let list = rank(scores, 0, 2);
        assert_eq!(list.total_scored, 5);
        let top: Vec<i32> = list.accounts.iter().map(|s| s.score).collect();
        assert_eq!(top, vec![40, 30], "the page is the highest scores");
    }

    #[test]
    fn total_scored_honours_the_cutoff() {
        let scores = (0..5).map(|i| scored(&format!("a{i}"), i * 10)).collect();
        let list = rank(scores, 20, 200);
        assert_eq!(list.total_scored, 3);
        assert_eq!(list.accounts.len(), 3);
    }
}

#[derive(Clone)]
pub struct RiskScoresState {
    pub pool: Arc<PgPool>,
}

pub fn risk_scores_router(pool: PgPool) -> Router {
    let state = RiskScoresState {
        pool: Arc::new(pool),
    };
    Router::new()
        .route("/api/people/accounts/risk-scores", get(list_risk_scores))
        .with_state(state)
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskFactors {
    pub days_since_last_invoice: Option<i64>,
    pub open_ticket_count: i64,
    pub has_active_contract: bool,
    pub days_since_last_note: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskScore {
    pub account_id: String,
    pub account_name: String,
    pub score: i32,
    pub top_factor: String,
    pub factors: RiskFactors,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskScoreList {
    pub accounts: Vec<RiskScore>,
    pub total_scored: usize,
}

#[derive(Debug, Deserialize)]
struct ListParams {
    /// Return only the top N at-risk accounts. Default 10, max 200
    /// so the endpoint can also back a future full-watchlist route.
    #[serde(default = "default_limit")]
    limit: usize,
    /// Minimum score cutoff so a healthy asset base doesn't fill the
    /// panel with scores of 0. Default 1 to always include everyone
    /// who triggered at least one bump; callers can pass 0 for every
    /// account.
    #[serde(default = "default_min_score")]
    min_score: i32,
}

fn default_limit() -> usize {
    10
}
fn default_min_score() -> i32 {
    1
}

async fn list_risk_scores(
    State(state): State<RiskScoresState>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<ListParams>,
) -> Response {
    // Security gate: account risk scores include financial + churn
    // signals. Only roles with broad account access (exec / VP /
    // manager) see the cross-account watchlist; everyone else is
    // REFUSED. This used to answer `200 {accounts: []}` "so the panel
    // degrades cleanly", and /watchlist painted it as "No accounts
    // match those filters." — a denial read as nothing at risk, the
    // false-empty class (backlog 3f0cdca8; page audit 08b0c4f8 GAP 5,
    // 2026-09-23). A refusal the page can name is the clean degrade.
    if !is_trusted_or_broad(&user) {
        return (
            StatusCode::FORBIDDEN,
            "account risk scores are shown only to roles with broad account access",
        )
            .into_response();
    }
    let limit = params.limit.clamp(1, 200);
    match read_latest_predictions(&state.pool).await {
        Ok(scores) => Json(rank(scores, params.min_score, limit)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// The highest `limit` scores at or above `min_score`, and how many
/// there are in all. `total_scored` is counted BEFORE the limit: it was
/// counted after, so a 200-row page reported 200 scored whatever the
/// true number (backlog 9269d612; page audit 08b0c4f8 GAP 8), and a cap
/// read as a count.
fn rank(mut scores: Vec<RiskScore>, min_score: i32, limit: usize) -> RiskScoreList {
    scores.sort_by_key(|s| std::cmp::Reverse(s.score));
    let eligible: Vec<RiskScore> = scores
        .into_iter()
        .filter(|s| s.score >= min_score)
        .collect();
    let total_scored = eligible.len();
    RiskScoreList {
        accounts: eligible.into_iter().take(limit).collect(),
        total_scored,
    }
}

/// Read the latest prediction per account from `ml_predictions`
/// for the churn-risk model and join `accounts` to recover the
/// account name. Empty result means the dispatcher hasn't run
/// yet — the watchlist degrades to an empty list rather than
/// running an on-demand recompute.
async fn read_latest_predictions(pool: &PgPool) -> Result<Vec<RiskScore>, String> {
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT a.id, a.name, p.payload \
         FROM ( \
             SELECT DISTINCT ON (entity_id) entity_id, payload \
             FROM ml_predictions \
             WHERE model_id = $1 AND entity_type = 'account' \
             ORDER BY entity_id, created_at DESC \
         ) p \
         JOIN accounts a ON a.id = p.entity_id \
         ORDER BY a.id",
    )
    .bind(CHURN_RISK_MODEL_ID)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("ml_predictions read: {e}"))?;

    let mut out = Vec::with_capacity(rows.len());
    for (id, name, payload) in rows {
        let factors = parse_factors(&payload).ok_or_else(|| {
            format!("malformed factors payload on prediction for {id}: {payload}")
        })?;
        let score = payload
            .get("score")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| format!("missing score on prediction for {id}"))?
            as i32;
        let top_factor = payload
            .get("top_factor")
            .and_then(|v| v.as_str())
            .unwrap_or("healthy")
            .to_string();
        out.push(RiskScore {
            account_id: id,
            account_name: name,
            score,
            top_factor,
            factors,
        });
    }
    Ok(out)
}

fn parse_factors(payload: &serde_json::Value) -> Option<RiskFactors> {
    let f = payload.get("factors")?;
    Some(RiskFactors {
        days_since_last_invoice: f.get("days_since_last_invoice").and_then(|v| v.as_i64()),
        open_ticket_count: f
            .get("open_ticket_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
        has_active_contract: f
            .get("has_active_contract")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        days_since_last_note: f.get("days_since_last_note").and_then(|v| v.as_i64()),
    })
}
