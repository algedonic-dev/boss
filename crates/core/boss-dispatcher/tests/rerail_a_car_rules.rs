//! The machine files the repair for a car left behind on a conflict
//! (design b35456ac, answering user-feedback ff4ae3d2), and the repair
//! closes itself when the car does.
//!
//! Measured 2026-09-28: the dock held exactly two cars, both refused on
//! merge conflicts after train #778 landed changes to the same files, and
//! each stayed behind until an operator dispatched `boss rerail` by hand —
//! d89bafeb read `skips = 13` by then. The conductor already rebases a
//! car before it stamps a conflict skip, so every stamped conflict is one
//! `git rebase` could not resolve: the repair is authorship, and the
//! missing piece was the actor, not the diagnosis.
//!
//! Both rules are read FROM their files under `infra/dispatcher/rules/`,
//! not copied here, and the key the trigger reads is the constant the
//! conductor writes (`boss_jobs::car::SKIPS`), so a renamed key reds this
//! file rather than silently retiring the trigger.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};
use boss_jobs::car::SKIPS;

mod common;

const TRIGGER: &str = "rerail-a-car-left-behind-on-a-conflict";
const OVERTAKEN: &str = "rerail-a-car-is-overtaken-when-its-car-closes";
const CAR: &str = "d89bafeb-0000-4000-8000-000000000001";
const BRANCH: &str = "fix/four-registry-write-doors-and-dispatcher-rules-ask-policy";

/// The conductor's own reading of a conflict skip, as it stamps it.
const CONFLICT: &str = "conflict: crates/core/boss-policy-client/src/lib.rs";

/// `open_job_exists` answering a fixed value and recording what it was
/// asked; every other helper is the real resolver's, so `starts_with` is
/// the function the dispatcher binds, not a stand-in for it.
struct Stub {
    open_repair: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
    real: InventoryHelpers,
}

impl Stub {
    fn new(open_repair: bool) -> Self {
        Self {
            open_repair,
            asked: std::sync::Mutex::new(Vec::new()),
            real: InventoryHelpers::new("http://unused.invalid", "http://unused.invalid"),
        }
    }
    fn asked_about(&self) -> Vec<(String, String)> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for Stub {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        if name != "open_job_exists" {
            return self.real.call(name, args);
        }
        if let (Some(Value::String(kind)), Some(Value::String(subject))) =
            (args.first(), args.get(1))
        {
            self.asked
                .lock()
                .expect("stub lock")
                .push((kind.clone(), subject.clone()));
        }
        Ok(Value::Bool(self.open_repair))
    }
}

/// A `jobs.job.updated` for a car, in the shape the metadata door
/// records it: the whole post-merge row (`serde_json::to_value(&job)`).
fn car_updated(skips: serde_json::Value, reason: serde_json::Value) -> serde_json::Value {
    let mut metadata = serde_json::json!({ "branch": BRANCH, "skip_reason": reason });
    metadata[SKIPS] = skips;
    serde_json::json!({
        "id": CAR,
        "kind": "ship-a-change",
        "subject": { "subject_kind": "custom", "id": BRANCH },
        "title": "Four registry write doors and the dispatcher rules ask policy",
        "status": "open",
        "metadata": metadata,
    })
}

fn fires(payload: &serde_json::Value, stub: &Stub) -> usize {
    let outcome = match_event(
        &common::authored_rule(TRIGGER),
        "jobs.job.updated",
        payload,
        stub,
    );
    assert!(
        outcome.skipped.is_empty(),
        "the predicate must evaluate, not skip: {:?}",
        outcome.skipped
    );
    outcome.matched.len()
}

/// THE TRIGGER, AT THE THIRD CONSECUTIVE CONFLICT. Not the second — 39%
/// of trains carry a skip that clears itself — and not the fourth, which
/// is what makes it once per streak (design b35456ac, decisions 2 and 5).
#[test]
fn the_third_conflict_skip_files_one_repair_and_no_other_count_does() {
    for (n, want) in [(1, 0), (2, 0), (3, 1), (4, 0), (13, 0)] {
        assert_eq!(
            fires(
                &car_updated(serde_json::json!(n), serde_json::json!(CONFLICT)),
                &Stub::new(false)
            ),
            want,
            "skips = {n}"
        );
    }
}

/// What it files: the repair protocol, about the car's own subject (the
/// key its dedup reads back), with the car, its branch and the reason on
/// the packet for the builder the step's agent block is handed to.
#[test]
fn the_repair_is_a_rerail_a_car_about_the_cars_subject_carrying_the_car() {
    let stub = Stub::new(false);
    let reg = common::authored_rule(TRIGGER);
    let payload = car_updated(serde_json::json!(3), serde_json::json!(CONFLICT));
    let hits = match_event(&reg, "jobs.job.updated", &payload, &stub).matched;
    assert_eq!(hits.len(), 1);
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    let get = |k: &str| {
        inv.args
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("arg {k} missing"))
    };
    assert_eq!(get("kind"), Value::String("rerail-a-car".into()));
    assert_eq!(get("subject_kind"), Value::String("custom".into()));
    assert_eq!(
        get("subject"),
        Value::String(BRANCH.into()),
        "the car's subject — its ORIGINAL branch, which a rerail does not change"
    );
    assert_eq!(get("metadata.car"), Value::String(CAR.into()));
    assert_eq!(get("metadata.branch"), Value::String(BRANCH.into()));
    assert_eq!(get("metadata.skip_reason"), Value::String(CONFLICT.into()));
    assert_eq!(
        stub.asked_about(),
        vec![("rerail-a-car".to_string(), BRANCH.to_string())],
        "one open repair per car: the dedup asks about the kind it files and the \
         subject it files it on"
    );
}

/// A repair already open for this car is not filed twice — the second
/// write that lands while `skips` still reads 3 (design decision 5).
#[test]
fn an_open_repair_for_the_car_files_no_second() {
    let payload = car_updated(serde_json::json!(3), serde_json::json!(CONFLICT));
    assert_eq!(fires(&payload, &Stub::new(true)), 0);
}

/// A car held for any other reason — a dock hold, a struck car, a branch
/// on neither remote, a consist refusal stamped over a counted streak —
/// is the left-behind alarm's case, not a conflict to author a way out of.
#[test]
fn a_car_skipped_for_any_other_reason_files_nothing() {
    for reason in [
        serde_json::json!("its dock re-gate is red on test"),
        serde_json::json!("held after 2 red trains — needs a look before it boards again"),
        serde_json::json!("branch fix/x is on neither the fork nor the forge"),
        serde_json::json!("consist check refused — lint x: a.rs"),
        serde_json::Value::Null,
    ] {
        let payload = car_updated(serde_json::json!(3), reason.clone());
        assert_eq!(fires(&payload, &Stub::new(false)), 0, "{reason}");
    }
    let mut no_reason = car_updated(serde_json::json!(3), serde_json::Value::Null);
    no_reason["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("skip_reason");
    assert_eq!(fires(&no_reason, &Stub::new(false)), 0, "absent reason");
}

/// Every packet write in the system rides this topic; another kind with
/// the same metadata is not a car.
#[test]
fn another_kind_is_not_a_car() {
    let mut payload = car_updated(serde_json::json!(3), serde_json::json!(CONFLICT));
    payload["kind"] = serde_json::json!("backlog-item");
    assert_eq!(fires(&payload, &Stub::new(false)), 0);
}

/// THE REPAIR CLOSES ITSELF (decision 7): any car close — merged,
/// abandoned, retired — completes the open repair naming that car as
/// `overtaken`, matched on the repair's own `car` key, so the rule leaves
/// no residue behind a car that landed while its repair ran.
#[test]
fn a_car_closing_completes_its_repair_as_overtaken() {
    let reg: Registry = common::authored_rule(OVERTAKEN);
    let closed = |kind: &str, outcome: &str| {
        serde_json::json!({
            "id": CAR,
            "closed_on": "2026-09-28",
            "kind": kind,
            "outcome": outcome,
            "title": "Four registry write doors",
            "subject_id": BRANCH,
            "parent_step_id": null,
        })
    };
    for outcome in ["merged", "abandoned"] {
        let o = match_event(
            &reg,
            "jobs.job.closed",
            &closed("ship-a-change", outcome),
            &Stub::new(false),
        );
        assert!(o.skipped.is_empty(), "{:?}", o.skipped);
        assert_eq!(o.matched.len(), 1, "outcome {outcome}");
        let inv = &o.matched[0].invocations[0];
        assert_eq!(inv.handler, "jobs.complete_step_matching");
        let get = |k: &str| {
            inv.args
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("kind"), Some(Value::String("rerail-a-car".into())));
        assert_eq!(get("step"), Some(Value::String("rerail".into())));
        assert_eq!(
            get("match_step"),
            None,
            "no step names the car: it is the packet's own metadata"
        );
        assert_eq!(get("match_field"), Some(Value::String("car".into())));
        assert_eq!(get("event_path"), Some(Value::String("id".into())));
        match get("done_metadata") {
            Some(Value::String(t)) => {
                let t: serde_json::Value =
                    serde_json::from_str(&t).expect("the template is a JSON object");
                assert_eq!(t["result"], "overtaken");
                assert!(
                    t["evidence"].as_str().is_some_and(|e| !e.is_empty()),
                    "the step requires evidence at done: {t}"
                );
            }
            other => panic!("done_metadata must be a string template, got {other:?}"),
        }
    }
    let other = match_event(
        &reg,
        "jobs.job.closed",
        &closed("backlog-item", "completed"),
        &Stub::new(false),
    );
    assert!(other.matched.is_empty(), "only a car's close");
}
