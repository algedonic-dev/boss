//! The one statement budget every Postgres fold fixture runs on, and the
//! stall it states rather than waits for. Spelled once, in
//! `boss_events::statement_budget` behind its `test-support` feature,
//! which this crate's own `test-support` turns on; this module re-exports
//! them for the crate's adapters and holds them against the crate's
//! production budgets (backlog 03a5dbdc). Until then they were spelled
//! here, `pub(crate)`, and respelled by the integration tests and by
//! boss-events, which could not import them. The claims history adapter
//! carried the budget alone until backlog ebbf38c6 found three more 2 s
//! budgets its tests passed through — the lease operations PgJobs runs
//! (bind, sweep, discharge) and the audit tail the claims source reads —
//! each still losing to a loaded gate drive.

pub(crate) use boss_events::statement_budget::{
    FIXTURE_BUDGET, MEASURED_STALL, after_the_measured_stall,
};

/// The stall is longer than every production budget it stands in front
/// of, so a fixture that passes on it has outlasted what production
/// would have refused.
const _: () =
    assert!(MEASURED_STALL.as_millis() > crate::claims_console::STATEMENT_BUDGET.as_millis());
#[cfg(feature = "postgres")]
const _: () =
    assert!(MEASURED_STALL.as_millis() > crate::resource_leases_pg::STATEMENT_BUDGET.as_millis());
