use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boss_policy_client::role_guard::RoleGuardReporter;
use boss_policy_client::role_reader::{RegistryRoles, RoleSnapshotClock, SnapshotRoleReader};
use boss_policy_client::role_reporting::{ReportMode, ReportTally};
use boss_policy_client::{AccessTier, User};
use serde_json::json;

struct Clock(Mutex<Instant>);
impl RoleSnapshotClock for Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

fn user() -> User {
    User {
        id: "alias".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Auditor,
        department: Some("it".into()),
        territory_account_ids: vec!["account".into()],
        direct_report_ids: vec!["person".into()],
    }
}

fn fixture(
    mode: ReportMode,
) -> (
    RoleGuardReporter,
    Arc<SnapshotRoleReader>,
    Arc<ReportTally>,
    Arc<Clock>,
) {
    let clock = Arc::new(Clock(Mutex::new(Instant::now())));
    let roles = Arc::new(SnapshotRoleReader::new(
        Duration::from_secs(30),
        clock.clone(),
    ));
    let tally = Arc::new(ReportTally::new(10));
    (
        RoleGuardReporter::new(roles.clone(), tally.clone(), Arc::new(mode)),
        roles,
        tally,
        clock,
    )
}

fn publish(roles: &SnapshotRoleReader, role: serde_json::Value) {
    let projection = RegistryRoles::from_sources(
        json!({"data":[{"id":"canonical", "aliases":["alias"], "role":role}],"total":1}),
        json!({"data":[], "total":0}),
        json!([]),
    )
    .unwrap();
    let ticket = roles.begin_refresh();
    assert!(roles.finish_refresh(ticket, Ok(projection)));
}

#[test]
fn direct_guard_compares_only_role_and_returns_the_original_answer() {
    let (reporter, roles, tally, _) = fixture(ReportMode::Report);
    publish(&roles, json!("engineering-agent"));
    let original = user();
    let calls = std::cell::Cell::new(0);
    let answer = reporter.evaluate("revoke-access-token", "admission", &original, |candidate| {
        calls.set(calls.get() + 1);
        assert_eq!(candidate.id, original.id);
        assert_eq!(candidate.access_tier, original.access_tier);
        assert_eq!(candidate.department, original.department);
        assert_eq!(
            candidate.territory_account_ids,
            original.territory_account_ids
        );
        assert_eq!(candidate.direct_report_ids, original.direct_report_ids);
        candidate.role == "platform-admin"
    });
    assert!(answer);
    assert_eq!(calls.get(), 2);
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    let observation = &report.rows[0].observation;
    assert_eq!(observation.asserted_allowed, Some(true));
    assert_eq!(observation.recorded_allowed, Some(false));
    assert_eq!(observation.would_deny, Some(true));
    assert_eq!(observation.recorded_actor.as_deref(), Some("canonical"));
    assert_eq!(original.role, "platform-admin");
}

#[test]
fn off_mode_evaluates_only_the_original_guard_and_emits_nothing() {
    let (reporter, _, tally, _) = fixture(ReportMode::Off);
    let calls = std::cell::Cell::new(0);
    assert!(reporter.evaluate("guard", "visibility", &user(), |_| {
        calls.set(calls.get() + 1);
        true
    }));
    assert_eq!(calls.get(), 1);
    assert!(tally.snapshot().rows.is_empty());
}

#[test]
fn unread_expired_and_missing_roles_are_unknown_comparisons() {
    for state in ["unread", "expired", "null"] {
        let (reporter, roles, tally, clock) = fixture(ReportMode::Report);
        if state != "unread" {
            publish(
                &roles,
                if state == "null" {
                    json!(null)
                } else {
                    json!("engineering-agent")
                },
            );
        }
        if state == "expired" {
            let now = *clock.0.lock().unwrap();
            *clock.0.lock().unwrap() = now + Duration::from_secs(30);
        }
        let calls = std::cell::Cell::new(0);
        assert!(reporter.evaluate("guard", "admission", &user(), |_| {
            calls.set(calls.get() + 1);
            true
        }));
        assert_eq!(calls.get(), 1, "{state}");
        let report = tally.snapshot();
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.rows[0].observation.recorded_allowed, None);
        assert_eq!(report.rows[0].observation.would_deny, None);
    }
}

#[test]
fn unread_candidate_context_is_not_a_false_denial() {
    let (reporter, roles, tally, _) = fixture(ReportMode::Report);
    publish(&roles, json!("engineering-agent"));
    assert!(reporter.observe_captured("assignment", "visibility", &user(), true, |_| None));
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].observation.asserted_allowed, Some(true));
    assert_eq!(report.rows[0].observation.recorded_allowed, None);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "context-unavailable"
    );
    assert_eq!(report.rows[0].observation.would_deny, None);
}

#[test]
fn captured_selection_compares_scope_without_claiming_an_admission() {
    let (reporter, roles, tally, _) = fixture(ReportMode::Report);
    publish(&roles, json!("engineering-agent"));
    let original = vec!["platform-admin".to_owned()];
    reporter.observe_selection("queue-selectors", &user(), &original, |candidate| {
        Some(vec![candidate.role.clone()])
    });
    let report = tally.snapshot();
    assert_eq!(report.rows.len(), 1);
    let observation = &report.rows[0].observation;
    assert_eq!(observation.action, "selection");
    assert_eq!(observation.asserted_allowed, None);
    assert_eq!(observation.recorded_allowed, None);
    assert_eq!(observation.would_deny, None);
    assert_eq!(observation.would_change_scope, Some(true));
    assert_eq!(original, vec!["platform-admin"]);
}

#[test]
fn an_unknown_selection_is_not_an_unchanged_scope() {
    let (reporter, roles, tally, _) = fixture(ReportMode::Report);
    publish(&roles, json!("engineering-agent"));
    reporter.observe_selection("schedule", &user(), &vec!["person"], |_| None);
    let report = tally.snapshot();
    assert_eq!(report.rows[0].observation.would_change_scope, None);
    assert_eq!(
        report.rows[0].observation.lookup_status,
        "context-unavailable"
    );
}

#[test]
fn role_only_context_compares_visibility_without_fabricating_a_policy_caller() {
    let (reporter, roles, tally, _) = fixture(ReportMode::Report);
    publish(&roles, json!("engineering-agent"));
    for original in [true, false] {
        let result = reporter.observe_role_context(
            "content-audience",
            "visibility",
            ("alias", "platform-admin"),
            original,
            |role| {
                assert_eq!(role, "engineering-agent");
                Some(!original)
            },
        );
        assert_eq!(result, original);
    }
    let rows = tally.snapshot().rows;
    assert_eq!(rows.len(), 2);
    for row in rows {
        let observed = row.observation;
        assert_eq!(observed.actor, "alias");
        assert_eq!(observed.recorded_actor.as_deref(), Some("canonical"));
        assert_ne!(observed.asserted_allowed, observed.recorded_allowed);
        assert_eq!(observed.would_deny, None);
    }
}
