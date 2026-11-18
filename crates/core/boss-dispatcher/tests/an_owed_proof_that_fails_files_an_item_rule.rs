//! The `file-a-backlog-item-when-an-owed-proof-fails` rule — the loud
//! half of the obligation an owed proof becomes (backlog b9005734,
//! approved by David 2026-09-24: the obligation "re-runs the probe when
//! the event fires and files loudly if it fails").
//!
//! A car that closed LANDED with its proof owed is off every surface
//! that reads open cars: the shed counts cars standing at `proven`, and
//! the hourly recheck reads open cars only. When its declared event
//! fires, the forge door runs the probe and writes the verdict on the
//! closed car as `proof_attempt` — and until this rule, a verdict that
//! was not a pass and not a not-yet reached nobody. That is the silent
//! failure the correctness protocol forbids, and it would have started
//! the day the exit that closes such a car was published.
//!
//! Read FROM the rule's file under `infra/dispatcher/rules/`, not copied
//! here, and the keys the predicate reads are the constants the forge
//! door writes (`boss_jobs::car::PROOF_OWED`, `PROOF_ATTEMPT`), so a
//! renamed key reds this file rather than silently retiring the alarm.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::registry::match_event;
use boss_jobs::car::{PROOF_ATTEMPT, PROOF_OWED};

mod common;

const RULE: &str = "file-a-backlog-item-when-an-owed-proof-fails";
const CAR: &str = "8e7785ee-0000-4000-8000-000000000001";
const BRANCH: &str = "feat/a-stripe-charge-reaches-the-ledger";

/// `open_job_exists` answering a fixed value and recording what it was
/// asked; every other helper is the real resolver's.
struct Stub {
    open_item: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
    real: InventoryHelpers,
}

impl Stub {
    fn new(open_item: bool) -> Self {
        Self {
            open_item,
            asked: std::sync::Mutex::new(Vec::new()),
            real: InventoryHelpers::new("http://unused.invalid", "http://unused.invalid"),
        }
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
        Ok(Value::Bool(self.open_item))
    }
}

/// The attempt the forge door writes when the probe ran and did NOT
/// prove (`prove.rs` `attempt_json`): `not_yet` false, the reason in
/// `why`.
fn judged_false() -> serde_json::Value {
    serde_json::json!({
        "at": "2026-11-15T05:00:00+00:00",
        "exit": 1,
        "probe": "boss-sor-read /api/ledger/charges | jq -e '.total > 0'",
        "expect": "charged",
        "why": "the probe exited 1: A probe that fails is evidence AGAINST the claim",
        "not_yet": false,
        "unrunnable": false,
        "host": "forge",
    })
}

/// A `jobs.job.updated` for a car, in the shape the metadata door
/// records it: the whole post-merge row.
fn car(status: &str, owed: &str, attempt: serde_json::Value) -> serde_json::Value {
    let mut metadata = serde_json::json!({ "branch": BRANCH, "merged": "true" });
    metadata[PROOF_OWED] = serde_json::json!(owed);
    metadata[PROOF_ATTEMPT] = attempt;
    serde_json::json!({
        "id": CAR,
        "kind": "ship-a-change",
        "subject": { "subject_kind": "custom", "id": BRANCH },
        "title": "A Stripe charge reaches the ledger",
        "status": status,
        "metadata": metadata,
    })
}

fn hits(topic: &str, payload: &serde_json::Value, stub: &Stub) -> Vec<Vec<(String, Value)>> {
    let outcome = match_event(&common::authored_rule(RULE), topic, payload, stub);
    assert!(
        outcome.skipped.is_empty(),
        "the predicate must evaluate, not skip: {:?}",
        outcome.skipped
    );
    outcome
        .matched
        .iter()
        .map(|m| {
            assert_eq!(m.invocations.len(), 1);
            assert_eq!(m.invocations[0].handler, "jobs.spawn");
            m.invocations[0].args.clone()
        })
        .collect()
}

fn get(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(name, _)| name == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing: {args:?}"))
}

/// THE ALARM: a closed car that still owes its proof, whose last run
/// did not prove it, files ONE backlog-item about the car's own subject
/// (the key the dedup reads back), carrying the car and what the probe
/// said, on the queue the backlog's owner already answers for.
#[test]
fn a_closed_car_whose_owed_proof_did_not_prove_files_one_backlog_item() {
    let stub = Stub::new(false);
    let fired = hits(
        "jobs.job.updated",
        &car("closed", "true", judged_false()),
        &stub,
    );
    assert_eq!(fired.len(), 1, "one item per false verdict");
    let args = &fired[0];
    assert_eq!(get(args, "kind"), Value::String("backlog-item".into()));
    assert_eq!(get(args, "subject_kind"), Value::String("custom".into()));
    assert_eq!(get(args, "subject"), Value::String(BRANCH.into()));
    assert_eq!(get(args, "metadata.car"), Value::String(CAR.into()));
    assert_eq!(
        get(args, "metadata.why"),
        Value::String(judged_false()["why"].as_str().unwrap().into())
    );
    assert_eq!(
        get(args, "metadata.probe"),
        Value::String(judged_false()["probe"].as_str().unwrap().into())
    );
    assert_eq!(
        stub.asked.lock().unwrap().clone(),
        vec![("backlog-item".to_string(), BRANCH.to_string())],
        "the dedup asks for an open item about the car's own subject"
    );
}

/// A probe that could not run (a tool the forge lacks) is not a not-yet
/// either: nothing will retry it on a closed car, so it is loud too.
#[test]
fn an_owed_proof_that_could_not_run_is_loud_too() {
    let mut attempt = judged_false();
    attempt["unrunnable"] = serde_json::json!(true);
    attempt["exit"] = serde_json::Value::Null;
    let fired = hits(
        "jobs.job.updated",
        &car("closed", "true", attempt),
        &Stub::new(false),
    );
    assert_eq!(fired.len(), 1);
}

/// Everything that is NOT the silent case stays quiet: a not-yet (the
/// obligation is still waiting), a paid debt, an open car (the shed
/// already shows it red), a car with no attempt, one later disproved or
/// unlanded (its own terminal said so), and an item already open.
#[test]
fn a_wait_a_payment_an_open_car_or_an_open_item_files_nothing() {
    let mut not_yet = judged_false();
    not_yet["not_yet"] = serde_json::json!(true);
    let mut disproved = car("closed", "true", judged_false());
    disproved["metadata"]["disproved"] = serde_json::json!("true");
    let mut unlanded = car("closed", "true", judged_false());
    unlanded["metadata"]["unlanded"] = serde_json::json!("true");
    let mut no_attempt = car("closed", "true", serde_json::Value::Null);
    no_attempt["metadata"]
        .as_object_mut()
        .unwrap()
        .remove(PROOF_ATTEMPT);
    for (what, payload) in [
        ("a not-yet", car("closed", "true", not_yet)),
        ("a paid debt", car("closed", "paid", judged_false())),
        ("an open car", car("open", "true", judged_false())),
        ("no attempt", no_attempt),
        ("disproved", disproved),
        ("unlanded", unlanded),
    ] {
        assert!(
            hits("jobs.job.updated", &payload, &Stub::new(false)).is_empty(),
            "{what} files nothing"
        );
    }
    assert!(
        hits(
            "jobs.job.updated",
            &car("closed", "true", judged_false()),
            &Stub::new(true)
        )
        .is_empty(),
        "an item already open about this car is not filed twice"
    );
}

/// A car of another kind, and another topic, never fire it.
#[test]
fn another_kind_or_topic_files_nothing() {
    let mut other = car("closed", "true", judged_false());
    other["kind"] = serde_json::json!("pr-train");
    assert!(hits("jobs.job.updated", &other, &Stub::new(false)).is_empty());
    assert!(
        hits(
            "jobs.job.closed",
            &car("closed", "true", judged_false()),
            &Stub::new(false)
        )
        .is_empty()
    );
}
