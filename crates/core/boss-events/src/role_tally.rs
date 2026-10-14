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
        launched(
            mode,
            Evidence::spawn(Gate::ActorRole, service, PgOutboxRecorder::shared(pool)),
            std::env::var(boss_core::gate_window::LAUNCH_RECORD_ENV).ok(),
        )
        .reading(Arc::new(PgGateEvidence::new(pool.clone()))),
    )
    .watched()
}

/// The recording tally [`durable`] builds, with what it reads from the
/// environment handed in — the path of the launcher's record — so a
/// test holds the whole of it, the roster stamp reaching the first
/// `actor_role.recording_began` included (the lesson of review 6858ef1d,
/// N1, on the machine gate's mount).
///
/// THE STAMP IS WHAT BINDS AN EXCUSE (backlog b13bd730). The actor-role
/// window excuses the services the launch record skips, and until this
/// car no actor-role start said which record its process was launched
/// under — so a service an older selection ran was inherited as clean by
/// a newer one that skips it. Stated BEFORE the tally's first start,
/// which `ReportTally::recording` states: the order is the point. An
/// unreadable record is an error stamp, never a refused boot.
pub fn launched(
    mode: Arc<dyn ReportModeSource>,
    evidence: Evidence,
    launch_record: Option<String>,
) -> ReportTally {
    boss_core::gate_window::state_launch_roster(&evidence, launch_record);
    ReportTally::recording(REPORT_CAPACITY, mode, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::gate_window::{ROSTER_FIELD, roster_generation};
    use boss_policy_client::role_reporting::ReportMode;

    const RECORD: &str = "boss-launch-record v1\n\
        start boss-jobs-api\n\
        skip boss-assets-api module equipment is not on in the tenant manifest\n\
        end 2\n";

    /// The first thing a launched tally says is its start, and the start
    /// names the selection: remove the stamp from `launched`, or state it
    /// after the tally is built, and this is red.
    #[tokio::test]
    async fn the_first_start_states_the_roster_the_process_was_launched_under() {
        let dir = boss_testing::scratch_dir("role-tally-launch-record");
        let path = dir.join("record");
        std::fs::write(&path, RECORD).unwrap();
        let (evidence, mut stated) = Evidence::channel(Gate::ActorRole, "jobs");
        let _tally = launched(
            Arc::new(ReportMode::Report),
            evidence,
            Some(path.to_string_lossy().to_string()),
        );
        let start = stated
            .try_recv()
            .expect("a launched tally states its start");
        assert_eq!(start.kind, "actor_role.recording_began");
        assert_eq!(
            start.payload[ROSTER_FIELD]["generation"],
            roster_generation(RECORD).unwrap(),
            "{}",
            start.payload
        );

        // No record: the start is still stated, with why it names none.
        let (evidence, mut stated) = Evidence::channel(Gate::ActorRole, "jobs");
        let _tally = launched(Arc::new(ReportMode::Report), evidence, None);
        let start = stated.try_recv().expect("an unlaunched tally still starts");
        let stamp = &start.payload[ROSTER_FIELD];
        assert!(stamp.get("generation").is_none(), "{stamp}");
        assert!(
            stamp["error"].as_str().unwrap().contains("is unset"),
            "{stamp}"
        );
    }
}
