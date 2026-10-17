//! The `publish-measure-lands-the-drift-reading` rule (backlog
//! f4bafd6b, design 393bdef6 Q3) — publish-to-github's `measure`
//! completed from the machine's own drift reading, pinned over the
//! shipped rule file, not a copy.
//!
//! Measured 2026-10-09..16: two measure completions, both by an agent,
//! after waits of 41.8 h and 96.9 h, while `publish-github-pr --measure`
//! had written the reading onto the packet minutes after it opened. What
//! is pinned HERE is the selection: the topic the reading's own metadata
//! PATCH emits, and a `when` that hands the handler only a CLEAN reading
//! with nothing blocking — a FAILED scan or foreign mirror content stays
//! the agent's, whose procedure quotes the lint's report. The handler's
//! own tests (jobs_complete_step_from_record.rs) judge the copy.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::match_event;

mod common;

const RULE: &str = "publish-measure-lands-the-drift-reading";

/// The `jobs.job.updated` a metadata PATCH emits: the packet as merged.
fn updated(kind: &str, drift: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": "6d9f16f6-7626-41c8-a67b-23ada2016dd2",
        "kind": kind,
        "status": "open",
        "opened_at": "2026-10-15T00:00:41.599014973+00:00",
        "metadata": { "drift_refresh": drift, "drift_refreshed_at": "2026-10-16T00:04:27Z" },
    })
}

fn clean() -> serde_json::Value {
    serde_json::json!({
        "secrets_scan": "clean", "blocking": "", "has_drift": "true",
        "measured_at": "2026-10-16T00:04:27Z",
    })
}

fn fires(payload: &serde_json::Value) -> usize {
    let out = match_event(
        &common::authored_rule(RULE),
        "jobs.job.updated",
        payload,
        &NoHelpers,
    );
    assert!(
        out.skipped.is_empty(),
        "the rule must evaluate, not fail: {:?}",
        out.skipped
    );
    out.matched.len()
}

#[test]
fn a_clean_reading_on_a_publish_packet_hands_the_handler_the_measure_step() {
    let reg = common::authored_rule(RULE);
    let out = match_event(
        &reg,
        "jobs.job.updated",
        &updated("publish-to-github", clean()),
        &NoHelpers,
    );
    assert_eq!(out.matched.len(), 1, "{:?}", out.skipped);
    let inv = &out.matched[0].invocations;
    assert_eq!(inv.len(), 1);
    assert_eq!(inv[0].handler, "jobs.complete_step_from_record");
}

/// What the agent keeps: a scan that did not come back clean, a
/// blocking note, a reading from before the verb wrote `blocking` at
/// all (ABSENT is unequal to every literal), and every other packet.
#[test]
fn a_reading_the_agent_must_judge_selects_nothing() {
    let mut failed = clean();
    failed["secrets_scan"] = serde_json::json!("FAILED");
    let mut blocking = clean();
    blocking["blocking"] = serde_json::json!("mirror-has-unmerged-commits");
    let mut old = clean();
    old.as_object_mut().unwrap().remove("blocking");
    for drift in [failed, blocking, old] {
        assert_eq!(
            fires(&updated("publish-to-github", drift.clone())),
            0,
            "{drift}"
        );
    }
    assert_eq!(fires(&updated("backlog-item", clean())), 0);
    let mut bare = updated("publish-to-github", clean());
    bare["metadata"] = serde_json::json!({});
    assert_eq!(fires(&bare), 0, "no reading, nothing to land");
}
