//! The `open-doc-flatten-on-release-cut` rule (backlog 04c6e143) — a
//! release that is CUT opens the documentation flatten the corpus owes
//! it, pinned over the shipped rule directory, not a copy.
//!
//! WHY (measured 2026-09-25, re-measured 2026-09-27 on 94c6bfd9).
//! `doc-flatten` v1 is the protocol that folds settled decisions into
//! `docs/architecture-decisions.md`, and its own description says it
//! runs "each release". It had 0 packets ever while 72 design-docs
//! closed, because nothing opened it: no file under
//! `infra/dispatcher/rules/` named it. Opening a periodic chore is
//! mechanical, so it is the machine's (CLAUDE.md §Diagnosis,
//! "Mechanical operations belong to the machine").
//!
//! WHICH EVENT. The live `cut-a-release` v1 row (the tenant's protocol,
//! design 3613f0af; it is registry data, not a file in this tree) runs
//! decided -> changelog -> approve -> tag -> mirror -> announce and ends
//! at one of two terminals: `released` (after announce) or `withdrawn`
//! (approve said anything but approved). The release is CUT only at
//! `released`, so the rule fires on the packet's close with that
//! outcome and on nothing else — a withdrawn release changed no
//! baseline and owes no flatten.
//!
//! THE DEDUP KEY is the release, not the protocol. Each flatten is
//! opened ABOUT the release that earned it (`subject = id`), and the
//! guard asks whether a flatten for THAT release is open, so a
//! redelivered close opens no twin. Keying on `doc-flatten` alone would
//! be the dedup that silently retires a cadence: one flatten left open
//! would swallow every later release's.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

mod common;

const RULE: &str = "open-doc-flatten-on-release-cut";
const RELEASE_ID: &str = "7d3a9c1e-2b4f-4e6a-9c8d-1f2e3a4b5c6d";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// `open_job_exists` answering a fixed value, recording the
/// `(kind, subject)` it was asked about so the dedup KEY is asserted,
/// not only the verdict.
struct StubOpen {
    answer: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
}

impl StubOpen {
    fn new(answer: bool) -> Self {
        Self {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn asked_about(&self) -> Vec<(String, String)> {
        self.asked.lock().expect("stub lock").clone()
    }
}

impl HelperResolver for StubOpen {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_job_exists" => {
                if let (Some(Value::String(kind)), Some(Value::String(subject))) =
                    (args.first(), args.get(1))
                {
                    self.asked
                        .lock()
                        .expect("stub lock")
                        .push((kind.clone(), subject.clone()));
                }
                Ok(Value::Bool(self.answer))
            }
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

/// A `jobs.job.closed` payload as boss-jobs builds it — the roster
/// `event_kinds` declares for the topic (id, closed_on, kind, outcome,
/// title, subject_id, parent_step_id).
fn closed(kind: &str, outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "id": RELEASE_ID,
        "closed_on": "2026-09-27T10:00:00Z",
        "kind": kind,
        "outcome": outcome,
        "title": "Release v0.4.0",
        "subject_id": "boss",
        "parent_step_id": null,
    })
}

fn arg(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

#[test]
fn a_release_that_was_cut_opens_a_doc_flatten_about_that_release() {
    let reg = rule();
    let stub = StubOpen::new(false);
    let outcome = match_event(
        &reg,
        "jobs.job.closed",
        &closed("cut-a-release", "released"),
        &stub,
    );
    assert!(
        outcome.skipped.is_empty(),
        "the rule must evaluate, not fail: {:?}",
        outcome.skipped
    );
    let hits = outcome.matched;
    assert_eq!(hits.len(), 1, "exactly one rule fires: {hits:?}");
    assert_eq!(hits[0].invocations.len(), 1);
    let inv = &hits[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    let args = &inv.args;
    assert_eq!(arg(args, "kind"), Value::String("doc-flatten".into()));
    assert_eq!(arg(args, "subject_kind"), Value::String("custom".into()));
    assert_eq!(
        arg(args, "subject"),
        Value::String(RELEASE_ID.into()),
        "the flatten is ABOUT the release that earned it"
    );
    assert_eq!(
        arg(args, "metadata.for_release"),
        Value::String(RELEASE_ID.into()),
        "the release packet is one read away from the flatten"
    );
    assert_eq!(
        arg(args, "metadata.release_title"),
        Value::String("Release v0.4.0".into())
    );
    assert_eq!(
        stub.asked_about(),
        vec![("doc-flatten".to_string(), RELEASE_ID.to_string())],
        "the dedup is keyed on THIS release, never on the protocol alone, \
         so one flatten left open cannot swallow the next release's"
    );
}

/// A redelivered close finds the flatten it already opened and opens
/// no twin.
#[test]
fn a_flatten_already_open_for_the_release_suppresses_a_twin() {
    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed("cut-a-release", "released"),
        &StubOpen::new(true),
    )
    .matched;
    assert!(hits.is_empty(), "redelivery must not open a twin: {hits:?}");
}

/// Only a release that was CUT owes a flatten: a withdrawn one changed
/// no baseline, and no other protocol's close is a release.
#[test]
fn a_withdrawn_release_and_other_protocols_open_nothing() {
    let reg = rule();
    for (kind, outcome) in [
        ("cut-a-release", "withdrawn"),
        ("ops-request", "released"),
        ("design-doc", "approved"),
        ("doc-flatten", "recorded"),
    ] {
        let stub = StubOpen::new(false);
        let hits = match_event(&reg, "jobs.job.closed", &closed(kind, outcome), &stub).matched;
        assert!(
            hits.is_empty(),
            "{kind}/{outcome} must not open a flatten: {hits:?}"
        );
    }
}

/// The step topics are not subscribed: a release packet's steps going
/// done (its `announce` among them) are not the release being cut.
#[test]
fn only_the_close_is_subscribed() {
    let reg = rule();
    for topic in ["step.done.task", "step.ready.task", "step.done.outcome"] {
        let hits = match_event(
            &reg,
            topic,
            &closed("cut-a-release", "released"),
            &StubOpen::new(false),
        )
        .matched;
        assert!(hits.is_empty(), "{topic} must not fire: {hits:?}");
    }
}

/// The kind and subject kind this rule spawns are the ones the platform
/// bundle declares for `doc-flatten` — read from that file, so a rename
/// on either side is caught at the tree rather than as a refused POST on
/// the first release.
#[test]
fn the_spawned_kind_is_the_platform_bundles_doc_flatten() {
    let path = boss_testing::repo_root().join("infra/platform/workflows/doc-flatten.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let doc: toml::Value = toml::from_str(&text).expect("doc-flatten.toml parses");
    let wf = &doc["workflow"][0];
    assert_eq!(wf["kind"].as_str(), Some("doc-flatten"));
    let subject_kinds: Vec<&str> = wf["subject_kinds"]
        .as_array()
        .expect("subject_kinds is a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    let reg = rule();
    let hits = match_event(
        &reg,
        "jobs.job.closed",
        &closed("cut-a-release", "released"),
        &StubOpen::new(false),
    )
    .matched;
    assert_eq!(hits.len(), 1, "{hits:?}");
    let args = &hits[0].invocations[0].args;
    assert_eq!(
        arg(args, "kind"),
        Value::String(wf["kind"].as_str().unwrap_or_default().into())
    );
    match arg(args, "subject_kind") {
        Value::String(sk) => assert!(
            subject_kinds.contains(&sk.as_str()),
            "doc-flatten admits {subject_kinds:?}, the rule spawns {sk}"
        ),
        other => panic!("subject_kind is not a string: {other:?}"),
    }
}
