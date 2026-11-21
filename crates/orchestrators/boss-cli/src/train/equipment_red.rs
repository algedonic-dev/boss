//! A train red the gate DRIVE caused releases its cars unheld, unstruck
//! and owing no re-gate.
//!
//! Backlog 2e9a2449 (David approved 2026-11-03, after 118 gate receipts
//! from 11-01 12Z to 11-03 12Z: bays about 90% busy, a median io.full
//! stall of 34% overnight, 37% of bay-hours spent re-gating a branch
//! already gated). Both single-car trains of 2026-11-03 went red on the
//! gate drive, not on code: train 2258c95d's `test` check failed a 1 s
//! HTTP-fixture deadline in an interval that stalled on io.full for
//! 651 s of its 1710 s, and train 27614131's `the-converge-watches-postgres`
//! timed out a 60 s `git archive`. Each red held its car for a rebuild
//! and a car re-gate (`judged_red_hold_reason`), owed it a dock re-gate
//! (`mark_regates_owed`) and struck it — a bay and a train spent
//! learning that the drive was slow, about a car whose own green had
//! already passed the same check. Backlog d7e5b016 measured the same
//! split on one car: two reds on gate-bay-0 at 367 s and 120 s of
//! test-phase io.full, two greens on bay-1 at 1 s and 2 s.
//!
//! THE CLASSIFICATION is RECORDED, not re-derived (item c02fc3b3, car
//! T3b). The gate-run's `record-verdict` step carries two facts the
//! verdict recorder wrote from the receipt by the one rule in
//! `boss_jobs::failure_facts` — `equipment_fault` {kind refused | lost |
//! stall-deadline | unjudged} and `product_defect` — and this module reads
//! them ([`classify_red`]). A red is EQUIPMENT when forge CI is green (the
//! red is the gate's alone) and the gate-run recorded an equipment fault
//! that is not `unjudged` and NO product defect. The per-entry rule
//! (every failure entry a wait that ran out, in a check whose own
//! interval stalled past the line, the list whole — reviews f7f21501 and
//! 3eee83a8) is the recorder's, stated once in that module.
//!
//! A product defect, alone or beside an equipment fault, is CHANGE and
//! keeps today's rule — hold, strike, owe the re-gate. A gate-run that
//! records NEITHER fact (recorded before car T3a) is CHANGE too: the
//! verdict alone decides, as it did before either fact existed, and
//! nothing is backfilled. An `unjudged` fault — a red with no readable
//! verdict — is neither: `train_gate::standing` reads it as
//! `Standing::Unjudged`, the train is filed again once and an alarm names
//! the receipt, and no car is struck or released for it.
//!
//! WHAT THE RELEASE STILL DEPENDS ON. The car leaves on its OWN green:
//! no hold, no `regate_owed`, no strike, and the dock judges that green at
//! the next boarding exactly as it judges every car — a receipt that
//! vouches for the head, a GATE-STALE base re-gated (`dock_regate`) — so
//! "still current for the main the next train departs on" is the dock's
//! existing answer, not a second copy of it here. The train gate still
//! tests the next assembled tree.
//!
//! ONCE PER CAR, like the outside release (5541d813): a car that already
//! carries [`KEY_EQUIPMENT_RELEASE`] from an earlier train takes today's
//! path on its second equipment red, so a real timing defect that passed
//! its own gate by luck cannot ride train after train.

use super::*;

// THE RULE LIVES IN TIER 1 (item c02fc3b3). The stall line, the
// signatures, the receipt's global bound and the per-entry reading are
// `boss_jobs::failure_facts`, which the conductor's verdict recorder
// calls to write `equipment_fault` and `product_defect` on every
// gate-run (car T3a). Since car T3b this module READS those two recorded
// facts off the train gate's gate-run and re-derives nothing: what stays
// here is the TRAIN's half — forge CI — and what each fact earns the cars.
//
// WHAT T3b DELETED. `classify_red` used to take the receipt and the
// no-verdict sentence and judge the red itself: a second reading of the
// receipt beside the recorder's (CLAUDE.md §9a), and a `no_verdict` arm
// that read a red with no readable verdict as EQUIPMENT and released its
// cars on code the gate never judged. That shape is recorded `unjudged`
// now, and `train_gate::standing` reads it as `Standing::Unjudged`: it
// strikes nobody and releases nobody (the conductor on c02fc3b3).
#[cfg(test)]
pub(crate) use boss_jobs::failure_facts::EQUIPMENT_IO_FULL_SECONDS;
use boss_jobs::failure_facts::{EquipmentFault, FailureFacts};

/// The car stamp that spends its one equipment release: the train id.
pub(crate) const KEY_EQUIPMENT_RELEASE: &str = "equipment_red_release";

/// The train metadata key that carries the classification and its
/// numbers — on the train's record, for change and equipment alike.
pub(crate) const KEY_RED_CLASS: &str = "red_class";

/// What a red train's gate-run RECORDED caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedClass {
    /// The gate's equipment, as recorded: refused, lost or
    /// stall-deadline, with no product defect beside it.
    Equipment { fault: EquipmentFault },
    /// The gate judged nothing (`unjudged`): no strike, no release.
    Unjudged { why: String },
    /// The branch, or not proven otherwise: why it is not equipment.
    Change { why: String },
}

impl RedClass {
    /// The record's form ([`KEY_RED_CLASS`]), with the threshold the
    /// stall was judged against so a reader can re-judge it.
    pub(crate) fn to_value(&self) -> Value {
        let threshold = boss_jobs::failure_facts::EQUIPMENT_IO_FULL_SECONDS;
        match self {
            RedClass::Equipment { fault } => json!({
                "class": "equipment",
                "kind": fault.kind.as_str(),
                "why": fault.why,
                "threshold_io_full_s": threshold,
                "checks": fault.checks,
                "io_full_s": fault.io_full_s,
                "wall_s": fault.wall_s,
            }),
            RedClass::Unjudged { why } => json!({
                "class": "unjudged",
                "why": why,
                "threshold_io_full_s": threshold,
            }),
            RedClass::Change { why } => json!({
                "class": "change",
                "why": why,
                "threshold_io_full_s": threshold,
            }),
        }
    }
}

/// PURE: classify a red train from its forge verdict and the two facts
/// its train gate's gate-run RECORDED (`train_gate::recorded_facts`).
///
///   - forge CI not green: CHANGE — the red is not the gate's alone;
///   - UNRECORDED (a gate-run from before car T3a, or none read): CHANGE,
///     the verdict-only behaviour, with nothing re-derived or backfilled
///     (c02fc3b3, Q_old_records);
///   - a `product_defect`, alone or beside an equipment fault: CHANGE —
///     the product defect governs the strike;
///   - an `unjudged` equipment fault: UNJUDGED — no strike, no release;
///   - any other equipment fault: EQUIPMENT, which earns the release.
pub(crate) fn classify_red(forge_verdict: &str, recorded: Option<&FailureFacts>) -> RedClass {
    let change = |why: String| RedClass::Change { why };
    if forge_verdict != "green" {
        return change(format!(
            "forge CI reads {forge_verdict}, so the red is not the train gate's alone"
        ));
    }
    let Some(facts) = recorded else {
        return change(
            "the train gate's gate-run records neither failure fact (recorded before c02fc3b3, or \
             not read), so its verdict alone decides"
                .into(),
        );
    };
    if let Some(defect) = &facts.product_defect {
        return change(defect.why.clone());
    }
    if let Some(fault) = facts.releasing_fault() {
        return RedClass::Equipment {
            fault: fault.clone(),
        };
    }
    match facts.unjudged() {
        Some(fault) => RedClass::Unjudged {
            why: fault.why.clone(),
        },
        None => change("the train gate's record names no failure fact".into()),
    }
}

/// What an equipment red earns its cars, like `OutsideRelease`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EquipmentRelease {
    /// Released now, unheld, unstruck and owing no re-gate; every car is
    /// stamped [`KEY_EQUIPMENT_RELEASE`]. The note for the reason.
    Release(String),
    /// A car aboard already spent its one equipment release: today's
    /// path, and this note rides its reason.
    Spent(String),
}

/// PURE: the release an EQUIPMENT red earns the cars still riding train
/// `train` (`releasable_cars`). None for a CHANGE or UNJUDGED red, or when no car is
/// read as aboard — an unread consist proves nothing, and today's rule
/// decides.
pub(crate) fn equipment_release(
    train: &Value,
    class: &RedClass,
    cars: &[Value],
) -> Option<EquipmentRelease> {
    let RedClass::Equipment { fault } = class else {
        return None;
    };
    let why = &fault.why;
    let tid = train.get("id").and_then(Value::as_str).unwrap_or_default();
    let aboard = releasable_cars(cars, tid);
    if aboard.is_empty() {
        return None;
    }
    let spent: Vec<String> = aboard
        .iter()
        .filter_map(|c| {
            let prior = c
                .get("metadata")
                .and_then(|m| m.get(KEY_EQUIPMENT_RELEASE))
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty() && *p != tid)?;
            let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
            Some(format!("car {} (train {})", id8(id), id8(prior)))
        })
        .collect();
    if !spent.is_empty() {
        return Some(EquipmentRelease::Spent(format!(
            "classified equipment ({why}), but {} already had its one equipment release, so the \
             hold, the strike and the re-gate apply (2e9a2449)",
            spent.join(", ")
        )));
    }
    Some(EquipmentRelease::Release(format!(
        "classified equipment: {why} — cars released unheld and unstruck, owing no re-gate, to \
         board a later train on their own green, once each (2e9a2449)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::failure_facts::{EquipmentFaultKind, failure_facts};

    /// The class of a red whose train gate-run recorded what the verdict
    /// recorder writes for `receipt` — the writer's facts, read back. The
    /// receipt-shape cases below are 2e9a2449's, now driven end to end
    /// through the record (c02fc3b3, car T3b).
    fn class_of(forge: &str, receipt: &Value) -> RedClass {
        classify_red(forge, Some(&failure_facts("failed", Some(receipt))))
    }

    /// Train 2258c95d's gate-run 0de50487 receipt, trimmed: the one
    /// failing check, its `fails` line and excerpt as the runner wrote
    /// them, and its own interval as `runtime_evidence` measured it
    /// (1710 s of wall, 651 s of io.full).
    fn receipt_0de50487(io_full_usec: u64) -> Value {
        json!({
            "verdict": "failed",
            "mode": "auto",
            "head": "d60f4654397da2783e06798568cbf1d784a1dc4f",
            "checks": [
                {"name": "clippy", "result": "pass", "seconds": 138},
                {"name": "test", "result": "fail", "seconds": 1711},
            ],
            "fails": ["test: gcp_root_tree_sh::authenticated_network_main_preserves_history_and_old_generation_on_failure - panicked at crates/core/boss-testing/tests/groups/../gcp_root_tree_sh.rs:325:37: HTTP fixture did not start: port publication deadline expired; last read: Ok(\"33587\"); elapsed=4.754238073s; timeout=1s"],
            "fails_excerpt": {"test": "thread 'gcp_root_tree_sh::authenticated_network_main_preserves_history_and_old_generation_on_failure' (815368) panicked at crates/core/boss-testing/tests/groups/../gcp_root_tree_sh.rs:325:37:\nHTTP fixture did not start: port publication deadline expired"},
            "runtime_evidence": {"intervals": [
                {"from": "check-start:clippy", "to": "check-finish:clippy", "state": "measured",
                 "elapsed_ns": 138_000_000_000_u64, "stall_usec": {"io.full": 19_000_000}},
                {"from": "check-start:test", "to": "check-finish:test", "state": "measured",
                 "elapsed_ns": 1_710_000_000_000_u64, "stall_usec": {"io.full": io_full_usec}},
            ]}
        })
    }

    #[test]
    fn train_2258c95d_is_equipment_a_deadline_in_a_stalled_test_interval() {
        let r = receipt_0de50487(651_000_000);
        let class = class_of("green", &r);
        let RedClass::Equipment { fault } = &class else {
            panic!("expected equipment, got {class:?}");
        };
        assert_eq!(fault.kind, EquipmentFaultKind::StallDeadline);
        assert_eq!(fault.checks, vec!["test".to_string()]);
        assert_eq!((fault.io_full_s, fault.wall_s), (Some(651), Some(1710)));
        let why = &fault.why;
        assert!(why.contains("651 s of io.full"), "{why}");
        let v = class.to_value();
        assert_eq!(v["class"], "equipment");
        assert_eq!(v["threshold_io_full_s"], EQUIPMENT_IO_FULL_SECONDS);
        assert_eq!(v["io_full_s"], 651);
        assert_eq!(v["kind"], "stall-deadline");
    }

    #[test]
    fn the_same_deadline_under_the_stall_line_is_change() {
        // a8483713's 59 s: the branch is not given the benefit.
        let r = receipt_0de50487(59_000_000);
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("59 s") && why.contains("under")),
            "{class:?}"
        );
        assert_eq!(class.to_value()["class"], "change");
    }

    #[test]
    fn a_stalled_check_that_failed_on_an_assertion_is_change() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"] = json!([
            "test: foo::bar - panicked at crates/x/src/lib.rs:1:1: assertion `left == right` failed"
        ]);
        r["fails_excerpt"] =
            json!({"test": "assertion `left == right` failed\n  left: 1\n right: 2"});
        let class = class_of("green", &r);
        assert!(
            // The why names what it read (backlog 724007d3): an assertion,
            // not "no timeout or deadline", which an entry could refute.
            matches!(&class, RedClass::Change { why } if why.contains("a Rust assertion")),
            "{class:?}"
        );
    }

    #[test]
    fn a_failing_check_with_no_measured_interval_is_change() {
        // Train 27614131's shape: a roster lint timed out, and its
        // interval in the receipt is absent (or the replay's, unstalled).
        let r = json!({
            "verdict": "failed",
            "checks": [{"name": "the-converge-watches-postgres", "result": "fail"}],
            "fails": ["the-converge-watches-postgres: | ERROR: test_named_target_resolves_real_commit_and_renders_its_own_registry"],
            "fails_excerpt": {"the-converge-watches-postgres": "subprocess.TimeoutExpired: Command 'git archive' timed out after 60 seconds"},
            "runtime_evidence": {"intervals": []}
        });
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("no measured io.full")),
            "{class:?}"
        );
    }

    #[test]
    fn a_red_forge_or_a_missing_receipt_is_change() {
        let r = receipt_0de50487(651_000_000);
        assert!(matches!(class_of("failing", &r), RedClass::Change { .. }));
        assert!(matches!(
            classify_red("green", None),
            RedClass::Change { .. }
        ));
    }

    /// THE DELETED ARM (c02fc3b3): a red whose receipt judged nothing
    /// used to read EQUIPMENT here and release its cars on code the gate
    /// never judged. Recorded `unjudged`, it earns no release — and no
    /// strike: neither a product defect nor a releasing fault.
    #[test]
    fn no_check_judged_is_unjudged_and_earns_no_release() {
        let r = json!({"verdict": "failed", "fails": [], "error": "runner died"});
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Unjudged { why } if why.contains("runner died")),
            "{class:?}"
        );
        assert_eq!(class.to_value()["class"], "unjudged");
        let train = json!({"id": "t1"});
        assert_eq!(
            equipment_release(&train, &class, &[car("c1", json!({}))]),
            None
        );
    }

    /// Q_old_records: a gate-run recorded before car T3a carries neither
    /// fact, and the verdict alone decides — even for a receipt the
    /// recorder WOULD read as the drive's. Nothing is re-derived.
    #[test]
    fn an_unrecorded_gate_run_keeps_the_verdict_only_rule() {
        let class = classify_red("green", None);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("neither failure fact")),
            "{class:?}"
        );
    }

    #[test]
    fn every_failing_check_must_be_the_drives() {
        let mut r = receipt_0de50487(651_000_000);
        r["checks"] = json!([
            {"name": "test", "result": "fail"},
            {"name": "svelte-check", "result": "fail"},
        ]);
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.starts_with("svelte-check")),
            "{class:?}"
        );
    }

    // -- per failure ENTRY, not per check (review f7f21501, R1) ----------
    //
    // The runner writes one `fails` entry per failed test
    // (infra/gate-runner/run.sh `detail`), so one `test` check can carry
    // a deadline entry AND an unrelated assertion entry. Each entry must
    // be the drive's on its own message; nothing beside it — another
    // entry, the test's name, the excerpt — establishes its cause.

    /// The conservation assertion the review planted beside the deadline.
    const CONSERVATION: &str = "test: ledger::conservation - panicked at crates/core/boss-testing/tests/conservation.rs:12:3: assertion left == right failed: left 1 right 2";

    #[test]
    fn a_deadline_entry_does_not_waive_an_assertion_entry_in_the_same_check() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(CONSERVATION));
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("ledger::conservation")),
            "{class:?}"
        );
        let train = json!({"id": "t1"});
        assert_eq!(
            equipment_release(&train, &class, &[car("c1", json!({}))]),
            None,
            "a mixed red earns no release: hold, strike and re-gate as today"
        );
    }

    #[test]
    fn a_passing_mention_of_timeout_is_not_a_deadline_entry() {
        // The builder's own follow-up: a `timeout=5s` field in the output
        // of a test that failed on something else says nothing ran out.
        let mut r = receipt_0de50487(651_000_000);
        r["fails"] = json!([
            "test: rows::count - panicked at crates/x/src/lib.rs:4:2: expected 3 rows, got 2 (client timeout=5s)"
        ]);
        r["fails_excerpt"] = json!({"test": "test rows::slow ... ok (timeout=5s)\nexpected 3 rows, got 2 (client timeout=5s)"});
        let class = class_of("green", &r);
        assert!(matches!(&class, RedClass::Change { .. }), "{class:?}");
    }

    #[test]
    fn an_assertion_that_quotes_a_timeout_is_not_a_deadline_entry() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"] = json!([
            "test: watchdog::reads - panicked at crates/x/tests/w.rs:9:5: assertion failed: read.contains(\"timed out after 1s\")"
        ]);
        let class = class_of("green", &r);
        assert!(matches!(&class, RedClass::Change { .. }), "{class:?}");
    }

    #[test]
    fn a_test_named_for_a_deadline_is_not_a_deadline_entry() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"] = json!([
            "test: claims::times_out_on_deadline - panicked at crates/x/tests/c.rs:3:1: assertion `left == right` failed\n  left: 1\n right: 2"
        ]);
        r["fails_excerpt"] = json!({"test": "claims::times_out_on_deadline"});
        let class = class_of("green", &r);
        assert!(matches!(&class, RedClass::Change { .. }), "{class:?}");
    }

    #[test]
    fn an_omitted_or_unpanicked_failure_is_change() {
        // The runner names at most PER_CHECK tests and rolls up the rest;
        // a test that FAILED with no panic line has no message to judge.
        for unread in [
            "test: + 3 more failing test(s) not named here (4 failed in all) - the Job log replay lists them",
            "test: other::case - FAILED, with no panic line for it in this check's output",
        ] {
            let mut r = receipt_0de50487(651_000_000);
            r["fails"].as_array_mut().unwrap().push(json!(unread));
            let class = class_of("green", &r);
            assert!(
                matches!(&class, RedClass::Change { .. }),
                "{unread}: {class:?}"
            );
        }
    }

    #[test]
    fn a_fixture_that_did_not_start_without_a_wait_running_out_is_change() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"] = json!([
            "test: gcp_root_tree_sh::serves - panicked at crates/core/boss-testing/tests/gcp_root_tree_sh.rs:325:37: HTTP fixture did not start: process exited with status 101"
        ]);
        r["fails_excerpt"] = json!({"test": "HTTP fixture did not start"});
        let class = class_of("green", &r);
        assert!(matches!(&class, RedClass::Change { .. }), "{class:?}");
    }

    #[test]
    fn every_entry_the_drives_is_equipment_and_names_each_shape() {
        let mut r = receipt_0de50487(651_000_000);
        r["fails"].as_array_mut().unwrap().push(json!(
            "test: nats::publish - panicked at crates/core/boss-nats/tests/p.rs:7:9: publish: connection timed out"
        ));
        let class = class_of("green", &r);
        let RedClass::Equipment { fault } = &class else {
            panic!("expected equipment, got {class:?}");
        };
        let why = &fault.why;
        assert!(why.contains("deadline, timeout"), "{why}");
        assert!(why.contains("2 failure entries"), "{why}");
    }

    #[test]
    fn a_check_that_did_not_pass_under_any_word_must_be_evidenced() {
        // A result word other than pass (or refused) is a failure the
        // receipt must evidence, not a check to skip beside a deadline.
        let mut r = receipt_0de50487(651_000_000);
        r["checks"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "web unit", "result": "FAIL"}));
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.starts_with("web unit")),
            "{class:?}"
        );
    }

    // -- the receipt's GLOBAL bound (review 3eee83a8, P1) ----------------
    //
    // infra/gate-runner/run.sh `write` caps `fails` at TOTAL_ENTRIES on the
    // whole receipt and replaces the tail with ONE unprefixed marker,
    // "+ N more entries omitted - this receipt caps `fails` at 40 ...".
    // The entries it dropped are unread: any of them can be an assertion.

    /// The runner's global omission marker for `dropped` entries.
    fn omitted(dropped: usize) -> String {
        format!(
            "+ {dropped} more entr{} omitted - this receipt caps `fails` at 40 so it stays \
             readable; the Job log replay has the rest",
            if dropped == 1 { "y" } else { "ies" }
        )
    }

    /// Codex's native shape: eight failing checks, each over the stall
    /// line, each with deadline entries only — `visible` of them in all,
    /// dealt round-robin — with `tail` appended last.
    fn eight_stalled_checks(visible: usize, tail: Option<String>) -> Value {
        let names: Vec<String> = (1..=8).map(|n| format!("suite-{n}")).collect();
        let mut fails: Vec<String> = (0..visible)
            .map(|i| {
                format!(
                    "{}: t{i}::waits - panicked at crates/x/tests/t.rs:{i}:1: HTTP fixture did \
                     not start: port publication deadline expired; timeout=1s",
                    names[i % 8]
                )
            })
            .collect();
        fails.extend(tail);
        json!({
            "verdict": "failed",
            "checks": names.iter().map(|n| json!({"name": n, "result": "fail"})).collect::<Vec<_>>(),
            "fails": fails,
            "runtime_evidence": {"intervals": names.iter().map(|n| json!({
                "from": format!("check-start:{n}"), "to": format!("check-finish:{n}"),
                "state": "measured", "elapsed_ns": 900_000_000_000_u64,
                "stall_usec": {"io.full": 400_000_000_u64},
            })).collect::<Vec<_>>()}
        })
    }

    #[test]
    fn a_failure_list_truncated_at_the_global_bound_is_change() {
        // 39 deadline entries across eight checks, then the runner's own
        // unprefixed marker as the fortieth.
        let r = eight_stalled_checks(39, Some(omitted(2)));
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("omitted")),
            "{class:?}"
        );
        let train = json!({"id": "t1"});
        assert_eq!(
            equipment_release(&train, &class, &[car("c1", json!({}))]),
            None
        );
    }

    #[test]
    fn an_omission_marker_prefixed_or_not_is_change_below_the_bound() {
        for marker in [omitted(1), omitted(2), format!("suite-1: {}", omitted(2))] {
            let r = eight_stalled_checks(8, Some(marker.clone()));
            let class = class_of("green", &r);
            assert!(
                matches!(&class, RedClass::Change { why } if why.contains("omitted")),
                "{marker}: {class:?}"
            );
        }
    }

    #[test]
    fn a_failure_list_at_the_global_bound_without_a_marker_is_change() {
        // A list exactly at the cap may have lost its marker to a clip; it
        // is not read as complete.
        let r = eight_stalled_checks(40, None);
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Change { why } if why.contains("40")),
            "{class:?}"
        );
    }

    #[test]
    fn eight_stalled_checks_under_the_bound_with_no_marker_are_equipment() {
        // The control: the same eight checks, complete, are the drive's.
        let r = eight_stalled_checks(39, None);
        let class = class_of("green", &r);
        assert!(
            matches!(&class, RedClass::Equipment { fault } if fault.checks.len() == 8),
            "{class:?}"
        );
    }

    // `the_receipt_bound_is_the_runners` moved with the constant it pins
    // to boss_jobs::failure_facts (c02fc3b3).

    fn car(id: &str, md: Value) -> Value {
        let mut m = json!({"train": "t1"});
        for (k, v) in md.as_object().unwrap() {
            m[k] = v.clone();
        }
        json!({"id": id, "status": "open", "metadata": m})
    }

    #[test]
    fn an_equipment_red_releases_once_per_car() {
        let train = json!({"id": "t1"});
        let class = class_of("green", &receipt_0de50487(651_000_000));
        let fresh = [car("c1", json!({}))];
        assert!(matches!(
            equipment_release(&train, &class, &fresh),
            Some(EquipmentRelease::Release(n)) if n.contains("2e9a2449") && n.contains("no re-gate")
        ));
        let spent = [car("c1", json!({KEY_EQUIPMENT_RELEASE: "t0-earlier"}))];
        assert!(matches!(
            equipment_release(&train, &class, &spent),
            Some(EquipmentRelease::Spent(n)) if n.contains("already had its one")
        ));
        // A change red earns nothing; an unread consist proves nothing.
        let change = class_of("green", &receipt_0de50487(1));
        assert_eq!(equipment_release(&train, &change, &fresh), None);
        assert_eq!(equipment_release(&train, &class, &[]), None);
    }
}
