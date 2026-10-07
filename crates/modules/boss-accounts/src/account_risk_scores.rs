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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Model id whose predictions back the watchlist response. Owned
/// here (not in config) because this endpoint is tied to a
/// specific seeded model row in
/// `boss-ml::bootstrap::seed_phase_two_candidates`.
const CHURN_RISK_MODEL_ID: &str = "mdl-account-churn-risk-v1";

/// How often the batch that writes these predictions runs: daily, per
/// `infra/ml/boss-ml-inference-batch.timer` (`OnCalendar=*-*-* 02:30:00`).
/// Held to that file by `the_stale_window_is_one_timer_cadence_plus_the_grace`.
const BATCH_CADENCE_HOURS: i64 = 24;
/// Slack for a run that starts late or takes a while, so last night's
/// score is not called stale in the minutes before tonight's lands.
const GRACE_HOURS: i64 = 2;
/// A score older than this was not rewritten by the last run the batch
/// should have made — the batch stopped, or stopped reaching this
/// account (backlog 8ddaefcd; page audit 08b0c4f8 GAP 12).
pub const STALE_AFTER_HOURS: i64 = BATCH_CADENCE_HOURS + GRACE_HOURS;

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

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-27T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn scored_at(id: &str, score: i32, at: DateTime<Utc>) -> Prediction {
        Prediction {
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
            scored_at: at,
        }
    }

    fn scored(id: &str, score: i32) -> Prediction {
        scored_at(id, score, now())
    }

    /// Backlog 9269d612 (page audit 08b0c4f8 GAP 8): `total_scored` was
    /// `filtered.len()` AFTER `.take(limit)`, so it was the page size
    /// under another name and /watchlist could not tell 200 scored from
    /// 200 of 350. It counts every score past the cutoff, before the page.
    #[test]
    fn total_scored_counts_past_the_limit() {
        let scores = (0..5).map(|i| scored(&format!("a{i}"), i * 10)).collect();
        let list = rank(scores, 0, 2, now());
        assert_eq!(list.total_scored, 5);
        let top: Vec<i32> = list.accounts.iter().map(|s| s.score).collect();
        assert_eq!(top, vec![40, 30], "the page is the highest scores");
    }

    #[test]
    fn total_scored_honours_the_cutoff() {
        let scores = (0..5).map(|i| scored(&format!("a{i}"), i * 10)).collect();
        let list = rank(scores, 20, 200, now());
        assert_eq!(list.total_scored, 3);
        assert_eq!(list.accounts.len(), 3);
    }

    /// Backlog 8ddaefcd (page audit 08b0c4f8 GAP 12): the read carried no
    /// time at all, so a batch that stopped weeks ago painted exactly
    /// like one that ran last night. Each score carries the prediction's
    /// own `created_at`, and the list the newest of them — taken over
    /// EVERY prediction read, past the cutoff and the limit, because it
    /// answers "when did the model last write", not "when was this page's
    /// top row written".
    #[test]
    fn each_score_carries_its_time_and_the_list_its_newest() {
        let old = now() - chrono::Duration::days(20);
        let newest = now() - chrono::Duration::hours(3);
        let scores = vec![
            scored_at("high-old", 90, old),
            scored_at("low-new", 0, newest),
        ];
        let list = rank(scores, 50, 1, now());
        assert_eq!(list.accounts.len(), 1);
        assert_eq!(list.accounts[0].scored_at, old);
        assert_eq!(
            list.scored_as_of,
            Some(newest),
            "the newest prediction counts though the cutoff hides its row"
        );
    }

    #[test]
    fn nothing_scored_has_no_as_of_time() {
        let list = rank(Vec::new(), 0, 200, now());
        assert_eq!(list.scored_as_of, None);
        assert_eq!(list.stale_after_hours, STALE_AFTER_HOURS);
    }

    /// A score is stale once it is older than one batch cadence plus
    /// the grace — not before: the score written at 02:30 is still the
    /// current one at 02:29 the next day.
    #[test]
    fn a_score_older_than_the_batch_cadence_is_stale() {
        let window = chrono::Duration::hours(STALE_AFTER_HOURS);
        let scores = vec![
            scored_at("fresh", 30, now() - chrono::Duration::hours(1)),
            scored_at("edge", 20, now() - window),
            scored_at("stale", 10, now() - window - chrono::Duration::minutes(1)),
        ];
        let list = rank(scores, 0, 200, now());
        let stale: Vec<(&str, bool)> = list
            .accounts
            .iter()
            .map(|s| (s.account_id.as_str(), s.stale))
            .collect();
        assert_eq!(
            stale,
            vec![("fresh", false), ("edge", false), ("stale", true)]
        );
    }

    /// A fact that lives twice gets an equality test (CLAUDE.md §9a):
    /// the stale window is derived from the batch timer's cadence, and
    /// the timer lives in infra/ml. If the timer stops being daily, this
    /// names the pair rather than letting every score read stale — or a
    /// week-old batch read fresh.
    #[test]
    fn the_stale_window_is_one_timer_cadence_plus_the_grace() {
        let path = boss_testing::repo_root().join("infra/ml/boss-ml-inference-batch.timer");
        let timer = std::fs::read_to_string(&path).unwrap();
        let on_calendar: Vec<&str> = timer
            .lines()
            .filter_map(|l| l.trim().strip_prefix("OnCalendar="))
            .collect();
        assert_eq!(
            on_calendar.len(),
            1,
            "{} should fire on exactly one OnCalendar",
            path.display()
        );
        assert!(
            on_calendar[0].starts_with("*-*-* "),
            "{} fires on {:?}, not daily; BATCH_CADENCE_HOURS says 24",
            path.display(),
            on_calendar[0]
        );
        assert_eq!(BATCH_CADENCE_HOURS, 24);
        assert_eq!(STALE_AFTER_HOURS, BATCH_CADENCE_HOURS + GRACE_HOURS);
    }
}

#[derive(Clone)]
pub struct RiskScoresState {
    pub pool: Arc<PgPool>,
    pub role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
}

pub fn risk_scores_router(pool: PgPool) -> Router {
    risk_scores_router_with_reports(pool, None)
}

pub fn risk_scores_router_with_reports(
    pool: PgPool,
    role_guards: Option<Arc<boss_policy_client::role_guard::RoleGuardReporter>>,
) -> Router {
    let state = RiskScoresState {
        pool: Arc::new(pool),
        role_guards,
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

/// One account's newest prediction as read from `ml_predictions`,
/// before it is judged against the clock.
#[derive(Debug, Clone)]
struct Prediction {
    account_id: String,
    account_name: String,
    score: i32,
    top_factor: String,
    factors: RiskFactors,
    scored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskScore {
    pub account_id: String,
    pub account_name: String,
    pub score: i32,
    pub top_factor: String,
    pub factors: RiskFactors,
    /// The prediction's own `created_at`. The read carried no time, so a
    /// score from a batch that stopped weeks ago looked exactly like
    /// last night's (backlog 8ddaefcd; page audit 08b0c4f8 GAP 12).
    pub scored_at: DateTime<Utc>,
    /// Older than `STALE_AFTER_HOURS`: the last run the batch should
    /// have made did not rewrite it.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RiskScoreList {
    pub accounts: Vec<RiskScore>,
    pub total_scored: usize,
    /// The newest prediction across EVERY account read — past the
    /// cutoff and the limit — so it says when the model last wrote, not
    /// when this page's top row was written. `null` when nothing is
    /// scored.
    pub scored_as_of: Option<DateTime<Utc>>,
    /// The window `stale` is judged against, so a reader can say it in
    /// words without keeping its own copy of the cadence.
    pub stale_after_hours: i64,
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
    let original = is_trusted_or_broad(&user);
    let allowed = state.role_guards.as_ref().map_or(original, |reporter| {
        reporter.observe_captured(
            "account-risk-scores",
            "admission",
            &user,
            original,
            |candidate| Some(is_trusted_or_broad(candidate)),
        )
    });
    if !allowed {
        return (
            StatusCode::FORBIDDEN,
            "account risk scores are shown only to roles with broad account access",
        )
            .into_response();
    }
    let limit = params.limit.clamp(1, 200);
    // WALL time, not the clock port: a score's age is judged against its
    // `created_at`, a record stamp the database wrote on the wall clock,
    // by a batch a wall-clock systemd timer runs. The business clock
    // answers "what day is it in the company's timeline"; in sim mode it
    // drifts from both, and would call every score stale (or none).
    let now = boss_clock_client::wall_now();
    match read_latest_predictions(&state.pool).await {
        Ok(scores) => Json(rank(scores, params.min_score, limit, now)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// The highest `limit` scores at or above `min_score`, and how many
/// there are in all. `total_scored` is counted BEFORE the limit: it was
/// counted after, so a 200-row page reported 200 scored whatever the
/// true number (backlog 9269d612; page audit 08b0c4f8 GAP 8), and a cap
/// read as a count.
///
/// `now` is the instant each score's age is judged against; the handler
/// passes the wall clock, because `created_at` is the database's
/// `NOW()` at write time.
fn rank(
    mut scores: Vec<Prediction>,
    min_score: i32,
    limit: usize,
    now: DateTime<Utc>,
) -> RiskScoreList {
    let scored_as_of = scores.iter().map(|s| s.scored_at).max();
    let stale_before = now - chrono::Duration::hours(STALE_AFTER_HOURS);
    scores.sort_by_key(|s| std::cmp::Reverse(s.score));
    let eligible: Vec<Prediction> = scores
        .into_iter()
        .filter(|s| s.score >= min_score)
        .collect();
    let total_scored = eligible.len();
    RiskScoreList {
        accounts: eligible
            .into_iter()
            .take(limit)
            .map(|p| RiskScore {
                stale: p.scored_at < stale_before,
                account_id: p.account_id,
                account_name: p.account_name,
                score: p.score,
                top_factor: p.top_factor,
                factors: p.factors,
                scored_at: p.scored_at,
            })
            .collect(),
        total_scored,
        scored_as_of,
        stale_after_hours: STALE_AFTER_HOURS,
    }
}

/// Read the latest prediction per account from `ml_predictions`
/// for the churn-risk model and join `accounts` to recover the
/// account name. Empty result means the dispatcher hasn't run
/// yet — the watchlist degrades to an empty list rather than
/// running an on-demand recompute.
async fn read_latest_predictions(pool: &PgPool) -> Result<Vec<Prediction>, String> {
    let rows: Vec<(String, String, serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT a.id, a.name, p.payload, p.created_at \
         FROM ( \
             SELECT DISTINCT ON (entity_id) entity_id, payload, created_at \
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
    for (id, name, payload, scored_at) in rows {
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
        out.push(Prediction {
            account_id: id,
            account_name: name,
            score,
            top_factor,
            factors,
            scored_at,
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
