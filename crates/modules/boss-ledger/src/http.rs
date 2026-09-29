//! HTTP surface for the ledger. v1d ships read-only endpoints:
//!
//! - `GET /api/ledger/health`
//! - `GET /api/ledger/accounts` — full chart of accounts
//! - `POST /api/ledger/accounts/batch` — the tenant declares its chart,
//!   insert-if-absent by code (backlog 41af5195; `accounts` module)
//! - `POST /api/ledger/tax/batch` — the tenant declares its tax kinds
//!   and sales-tax rates, insert-if-absent (backlog 7f163e58;
//!   `tax_registry` module); `GET /api/ledger/tax-kinds` and
//!   `GET /api/ledger/sales-tax-rates` read them back
//! - `GET /api/ledger/trial-balance?as_of=YYYY-MM-DD` — per-account totals
//! - `GET /api/ledger/entries?account_code=XXXX&limit=N` — drill-down entries
//! - `GET /api/ledger/entries?fact_id=UUID` — entries for a specific fact
//! - `GET /api/ledger/entries/:id` — single entry with lines
//!
//! Posting is synchronous inside domain writes (see `postgres::post_fact_in_tx`),
//! so this service is a pure read layer.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use boss_policy::User;
use sqlx::PgPool;

mod accounts;
mod author;
mod bank_settlements;
mod bills;
mod entries;
mod facts;
mod keg_deposits;
mod payroll;
mod periods;
mod posting_rules;
mod revenue;
mod statements;
mod tax;
mod tax_registry;

use accounts::*;
use bank_settlements::*;
use bills::*;
use entries::*;
use facts::*;
use keg_deposits::*;
use payroll::*;
use periods::*;
use posting_rules::*;
use revenue::*;
use statements::*;
use tax::*;
use tax_registry::*;

/// Who may write, asked of policy (backlog 34f0a954, 2026-09-28):
/// `action` on `resource` through the registry-write ladder
/// ([`boss_policy_client::writes::require_registry_write`], the classes
/// doors' since 553cf479) — no caller 401, a deny or a grant narrower
/// than `all` 403 (the books belong to no person and no department), a
/// policy service that cannot answer its own 503. Until then these doors
/// checked `reject_if_auditor` alone, so the router-wide `ledger` READ
/// grant was all a caller needed to post a journal entry, pay a bill or
/// file a tax return: the `smoke-tester` fixture role, which holds that
/// read by the shipped defaults and is not on the gateway's read-only
/// floor, wrote the ledger through every one of them. That check refused
/// the role string "auditor", which no seed, fixture or core role
/// carries, and was deleted once no door called it (backlog 432f0eb4).
///
/// The deploy superuser holds every action asked here from the platform
/// defaults — the finance page's operator, and what the dispatcher's
/// rules, `boss tenant publish` and a tenant engine's prepare sign as;
/// the simulator is admitted by the binary's
/// `SimBypassPolicyClient::from_env`; a tenant grants its finance leads
/// `ledger` in its own seed.
async fn require_write(
    parts: &mut axum::http::request::Parts,
    state: &Arc<LedgerApiState>,
    action: boss_policy_client::Action,
    resource: boss_policy_client::Resource,
) -> Result<User, Response> {
    use axum::extract::FromRequestParts;
    let boss_policy_client::CurrentUser(user) =
        boss_policy_client::CurrentUser::from_request_parts(parts, state).await?;
    boss_policy_client::writes::require_registry_write(
        state.policy.as_ref(),
        &user,
        action,
        resource,
    )
    .await?;
    Ok(user)
}

/// The caller of a door that ADDS a row to the books — a journal entry,
/// a settlement, a bill, a payroll run, a filing — admitted by
/// Create on `ledger` ([`require_write`]). An extractor rather
/// than a call in each handler so it runs before the body is read: a
/// refusal needs no well-formed body and says nothing about one.
pub(super) struct LedgerCreate(pub(super) User);

/// The caller of a door that CHANGES a row already on the books —
/// settling, sweeping, paying, remitting, superseding — admitted by
/// Update on `ledger` ([`require_write`]).
pub(super) struct LedgerUpdate(pub(super) User);

/// The caller of a door that publishes a posting or projection rule,
/// admitted by Create on `posting-rule` ([`require_write`], backlog
/// 432f0eb4). Not `ledger`: the posting path takes the NEWEST version of
/// a fact kind's rule (`load_newest_rule_in_tx`), so a holder of the
/// `ledger` grant a tenant gives its finance leads could publish version
/// N+1 and redirect every later automated posting without writing one
/// entry. The rules are the operating model's machinery, like the chart
/// (`ledger-account`) and the tax regime.
pub(super) struct PostingRuleCreate(pub(super) User);

/// The caller of the rate-schedule upsert, admitted by Create on
/// `tax-regime` ([`require_write`], backlog 432f0eb4) — the resource the
/// tenant's filing kinds and sales-tax rates are declared under. It rode
/// `ledger` Update until then; Create, because the door is an upsert and
/// a grant that may only change a row must not add one.
pub(super) struct TaxRegimeCreate(pub(super) User);

/// Each extractor above is one `(action, resource)` pair asked through
/// [`require_write`]; the pair is the whole difference between them.
macro_rules! asks {
    ($door:ident, $action:ident, $resource:ident) => {
        impl axum::extract::FromRequestParts<Arc<LedgerApiState>> for $door {
            type Rejection = Response;

            async fn from_request_parts(
                parts: &mut axum::http::request::Parts,
                state: &Arc<LedgerApiState>,
            ) -> Result<Self, Response> {
                require_write(
                    parts,
                    state,
                    boss_policy_client::Action::$action,
                    boss_policy_client::Resource::$resource(),
                )
                .await
                .map(Self)
            }
        }
    };
}

asks!(LedgerCreate, Create, ledger);
asks!(LedgerUpdate, Update, ledger);
asks!(PostingRuleCreate, Create, posting_rule);
asks!(TaxRegimeCreate, Create, tax_regime);

#[derive(Clone)]
pub struct LedgerApiState {
    pub pool: PgPool,
    /// Domain publisher for upstream `ledger.*` events. `None` in tests
    /// and when the binary launches without `nats_url`. Every
    /// fact-write site emits its corresponding `ledger.<thing>` event
    /// when this is `Some` so `rebuild_facts` has audit_log rows to
    /// project from.
    pub publisher: Option<Arc<boss_core::publisher::DomainPublisher>>,
    /// Authoritative clock. Every handler that stamps a date into a
    /// financial_fact or audit event reads `now` via
    /// `state.clock.now().await`. Production wires
    /// `ReqwestClockClient` pointing at the deployed `boss-clock-api`
    /// (wall mode); demo wires it pointing at the sim-mode
    /// clock-api. Services never inspect headers or env vars to
    /// learn whether time is sim or wall — the Clock decides.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
    /// Policy engine for the read gate below. Required: until backlog
    /// 7048afa8 (2026-09-26) this was an `Option` and `None` let every
    /// read through — fail-open by configuration, guarded only by a
    /// comment. Now a surface cannot be built without a client, and a
    /// test that wants the gate out of its way says so by wiring
    /// `PermissivePolicyClient`. `boss-ledger-api` wires the real
    /// engine, pinned by `tests/the_ledger_api_wires_policy.rs`.
    pub policy: Arc<dyn boss_policy_client::PolicyClient>,
}

/// Router-wide gate on READING `/api/ledger/*`.
///
/// Everything here is the company's finances: the trial balance, all
/// three statements, every journal entry, tax liability, bills. None
/// of it had any read gate at all, so a caller who could reach the port
/// could read the whole ledger.
///
/// A layer rather than a per-handler check on purpose: a forgotten read
/// gate is invisible until it is someone else's incident, and a layer
/// cannot be forgotten by a new route. The WRITE gate is per door
/// ([`LedgerCreate`] / [`LedgerUpdate`]) because each door names its
/// own verb; `tests/a_ledger_write_asks_policy.rs` reads every write
/// route off this router and refuses one that admits a ledger reader.
///
/// `/health` is exempt: monitoring probes it without a session, and it
/// returns no financial data.
async fn require_ledger_read(
    State(state): State<Arc<LedgerApiState>>,
    boss_policy_client::CurrentUser(user): boss_policy_client::CurrentUser,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if req.uri().path().ends_with("/health") {
        return next.run(req).await;
    }
    match state
        .policy
        .check(
            &user,
            boss_policy_client::Action::Read,
            boss_policy_client::Resource::ledger(),
        )
        .await
    {
        Ok(d) if d.is_allowed() => next.run(req).await,
        Ok(_) => (
            StatusCode::FORBIDDEN,
            "reading /api/ledger/* requires a `ledger` read grant",
        )
            .into_response(),
        Err(e) => {
            // Fail closed. An unreachable policy engine must not open
            // the company's books — and the refusal is the one every
            // door gives: 503 + Retry-After for an outage (backlog
            // fe9d212c).
            tracing::warn!(error = %e, "ledger read gate: policy check failed");
            e.into_response()
        }
    }
}

/// Build the enrichment stamp for in-tx event recording (outbox
/// phase 2): the caller's actor + the authoritative timestamp, with
/// `_simulated` resolved by the publisher's clock probe when one is
/// wired. Fact-write handlers record their `ledger.*` events in the
/// DOMAIN TRANSACTION via this stamp (see
/// `events::record_ledger_event_in_tx`); nothing publishes them
/// post-commit anymore. The actor is [`author::signer`], the same one a
/// write records as its author (backlog 7bf42e2b).
pub(crate) async fn event_stamp(
    state: &LedgerApiState,
    user: &boss_policy_client::User,
) -> boss_core::publisher::EventStamp {
    stamp_as(state, author::signer(user)).await
}

/// [`event_stamp`] for a door that has already resolved who signs — the
/// registry-write ladder's actor (backlog 59deda40), which has no
/// fallback author on any path.
pub(crate) async fn stamp_as(
    state: &LedgerApiState,
    actor: boss_core::actor::ActorId,
) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(actor).await,
        None => boss_core::publisher::EventStamp::new("ledger", actor),
    }
}

/// The chart and tax doors' policy question (backlog 59deda40,
/// 2026-09-28): Create on the door's own registry, through the
/// registry-write ladder
/// ([`boss_policy_client::writes::require_registry_write`], the classes
/// doors' since 553cf479) — no caller 401, a deny or a grant narrower
/// than `all` 403, a policy service that cannot answer 503 — and on an
/// allow the stamp the door's facts are signed with. Until then both
/// doors checked the caller's access tier and never asked policy, so no
/// rule could widen or narrow who declares the chart or the tax regime.
/// The sim is admitted by the binary's `SimBypassPolicyClient::from_env`
/// (85e7f10f).
async fn authorize_declaration(
    state: &LedgerApiState,
    user: &User,
    resource: boss_policy_client::Resource,
) -> Result<boss_core::publisher::EventStamp, Response> {
    let actor = boss_policy_client::writes::require_registry_write(
        state.policy.as_ref(),
        user,
        boss_policy_client::Action::Create,
        resource,
    )
    .await?;
    Ok(stamp_as(state, actor).await)
}

pub fn router(state: LedgerApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/ledger/health", get(health))
        .route("/api/ledger/accounts", get(list_accounts))
        .route(
            "/api/ledger/accounts/batch",
            axum::routing::post(declare_accounts_batch),
        )
        // The tenant's tax regime as registry data (backlog 7f163e58):
        // the door `boss tenant publish` sends seeds/tax.toml through,
        // insert-if-absent, and the two reads of what it landed.
        .route(
            "/api/ledger/tax/batch",
            axum::routing::post(declare_tax_batch),
        )
        .route("/api/ledger/tax-kinds", get(list_tax_kinds))
        .route("/api/ledger/sales-tax-rates", get(list_sales_tax_rates))
        .route("/api/ledger/trial-balance", get(trial_balance))
        .route("/api/ledger/income-statement", get(income_statement))
        .route("/api/ledger/balance-sheet", get(balance_sheet))
        .route("/api/ledger/cash-flow", get(cash_flow_statement))
        .route("/api/ledger/entries", get(list_entries))
        .route("/api/ledger/entries/{id}", get(get_entry))
        .route(
            "/api/ledger/periods",
            get(list_periods_handler).post(create_period_handler),
        )
        .route(
            "/api/ledger/periods/{id}/lock",
            axum::routing::post(lock_handler),
        )
        .route(
            "/api/ledger/periods/{id}/unlock",
            axum::routing::post(unlock_handler),
        )
        .route(
            "/api/ledger/periods/{id}/close",
            axum::routing::post(close_period_handler),
        )
        .route(
            "/api/ledger/journal-entries",
            axum::routing::post(create_manual_entry),
        )
        .route(
            "/api/ledger/financial-facts/sum",
            axum::routing::post(financial_facts_sum_handler),
        )
        .route(
            "/api/ledger/financial-facts/{id}/supersede",
            axum::routing::post(supersede_fact_handler),
        )
        .route(
            "/api/ledger/bank-settlements",
            get(list_bank_settlements).post(create_bank_settlement),
        )
        .route(
            "/api/ledger/bank-settlements/from-paid-invoice",
            axum::routing::post(create_bank_settlement_from_paid_invoice),
        )
        .route(
            "/api/ledger/bank-settlements/{id}/settle",
            axum::routing::post(settle_bank_settlement),
        )
        .route(
            "/api/ledger/bank-settlements/sweep",
            axum::routing::post(sweep_bank_settlements),
        )
        .route(
            "/api/ledger/payroll-runs",
            get(list_payroll_runs).post(create_payroll_run),
        )
        .route(
            "/api/ledger/payroll-runs/synthesize",
            axum::routing::post(synthesize_payroll_run),
        )
        .route("/api/ledger/payroll-runs/{id}", get(get_payroll_run))
        .route(
            "/api/ledger/tax-filings",
            get(list_tax_filings).post(create_tax_filing),
        )
        .route("/api/ledger/tax-filings/{id}", get(get_tax_filing))
        .route(
            "/api/ledger/tax-filings/{id}/remit",
            axum::routing::post(remit_tax_filing),
        )
        .route(
            "/api/ledger/tax-accruals",
            axum::routing::post(create_tax_accrual),
        )
        .route(
            "/api/ledger/keg-deposit-settlements",
            axum::routing::post(create_keg_deposit_settlement),
        )
        // Graduated excise rates as registry data (brewery-fidelity Q4):
        // jurisdiction-keyed, effective-dated tier schedules the accrual
        // endpoint resolves instead of trusting a flat rule arg.
        .route(
            "/api/ledger/excise-rate-schedules",
            get(list_excise_rate_schedules).put(upsert_excise_rate_schedule),
        )
        .route("/api/ledger/tax-liability", get(tax_liability_summary))
        // Posting rules and event→fact projections as registry data
        // (backlog a40541cb): the doors `boss tenant publish` sends
        // seeds/posting_rules.toml and seeds/fact_projection_rules.toml
        // through, insert-if-absent, one `.declared` fact per landed row.
        .route("/api/ledger/posting-rules", get(list_posting_rules_handler))
        .route(
            "/api/ledger/posting-rules/batch",
            axum::routing::post(publish_posting_rules_handler),
        )
        .route(
            "/api/ledger/fact-projection-rules",
            get(list_projection_rules_handler),
        )
        .route(
            "/api/ledger/fact-projection-rules/batch",
            axum::routing::post(publish_projection_rules_handler),
        )
        .route(
            "/api/ledger/revenue-schedules",
            axum::routing::post(create_revenue_schedule),
        )
        .route(
            "/api/ledger/deferred-revenue-runoff",
            get(deferred_revenue_runoff),
        )
        .route(
            "/api/ledger/cogs-recognized",
            axum::routing::post(cogs_recognized_handler),
        )
        .route(
            "/api/ledger/inventory-transferred",
            axum::routing::post(inventory_transferred_handler),
        )
        .route(
            "/api/ledger/inventory-capitalized",
            axum::routing::post(inventory_capitalized_handler),
        )
        // General accounts-payable bills (rent, utilities, …) — routed to a
        // GL debit account by `bill_category`, decoupled from inventory POs.
        .route("/api/ledger/bills", get(list_bills).post(create_bill))
        .route(
            "/api/ledger/bills/pay-run",
            axum::routing::post(batch_pay_bills),
        )
        .route("/api/ledger/bills/{id}/pay", axum::routing::post(pay_bill))
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            require_ledger_read,
        ))
        .with_state(shared)
}

// --- health ---------------------------------------------------------------

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-ledger-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// --- shared helpers -------------------------------------------------------

fn default_currency() -> String {
    "USD".to_string()
}

fn ledger_err(e: crate::error::LedgerError) -> Response {
    use crate::error::LedgerError;
    let status = match &e {
        LedgerError::UnknownAccount(_)
        | LedgerError::InvalidPayload { .. }
        | LedgerError::Unbalanced { .. }
        | LedgerError::LockedPeriod { .. }
        | LedgerError::UnknownFactKind(_)
        | LedgerError::TaxKindNotRegistered { .. } => StatusCode::BAD_REQUEST,
        // A caller error naming the row (the classes door's 422 for an
        // unregistered kind), never a storage failure.
        LedgerError::InvalidChart(_) | LedgerError::InvalidTaxSeed(_) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        LedgerError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string()).into_response()
}

fn storage_err(e: sqlx::Error) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Backlog fe9d212c: the read gate answered a policy outage as 503
    /// "policy engine unavailable" with no Retry-After, so a caller
    /// could not tell it apart from any other 503 or learn when to ask
    /// again. It answers through `PolicyClientError`'s one rendering
    /// now. The real adapter against a port nothing listens on; the
    /// pool is lazy and never reached, because the gate refuses first.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_read_gate_answers_a_policy_outage_as_503_with_retry_after() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dark = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let app = router(LedgerApiState {
            pool: PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap(),
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
            policy: Arc::new(boss_policy_client::ReqwestPolicyClient::new("ledger", dark)),
        });
        let resp = app
            .oneshot(
                Request::get("/api/ledger/accounts")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(
                boss_policy_client::POLICY_OUTAGE_RETRY_AFTER_SECS
                    .to_string()
                    .as_str()
            )
        );
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(String::from_utf8_lossy(&body), "policy-unreachable");
    }
}
