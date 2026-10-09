//! A service's actor-role tally, wired to the log (backlog e0bdba74):
//! its facts go out through the service's outbox and its window is read
//! back from `audit_log`. One definition for every service binary, so
//! the wiring is not twenty-seven copies of three constructors.

use std::sync::Arc;

use boss_core::gate_evidence::{Evidence, Gate};
use boss_policy_client::role_reporting::{ReportModeSource, ReportTally};
use boss_policy_client::role_service::REPORT_CAPACITY;
use sqlx::PgPool;

use crate::PgGateEvidence;
use crate::outbox::PgOutboxRecorder;

/// The tally `service` hands its report wiring. With a pool, every
/// shape `enforce` would answer differently is stated on the log at its
/// first sighting and the report's window is read from the log. Without
/// one — a binary started with no database — the tally still counts and
/// still follows the mode, and its answer says it is no durable window;
/// nothing here refuses a boot. Call it inside the runtime.
pub fn durable(
    service: &str,
    mode: Arc<dyn ReportModeSource>,
    pool: Option<&PgPool>,
) -> Arc<ReportTally> {
    let Some(pool) = pool else {
        return boss_policy_client::role_service::unrecorded_tally(service, mode);
    };
    Arc::new(
        ReportTally::recording(
            REPORT_CAPACITY,
            mode,
            Evidence::spawn(Gate::ActorRole, service, PgOutboxRecorder::shared(pool)),
        )
        .reading(Arc::new(PgGateEvidence::new(pool.clone()))),
    )
    .watched()
}
