//! `file-the-remedy-a-verb-declares-for-an-estate-finding` — an estate
//! finding files the passkey-gated request of every verb whose file
//! declares it remedies that finding (backlog 3df309bf, the
//! generalisation of car 1's boss-gcp rule).
//!
//! What the request carries, which verbs may be filed, and the dedup are
//! the handler's (`ops.file_remedies`, boss-dispatcher-handlers), pinned
//! in its own tests against the shipped verb files. What is pinned HERE
//! is the rule's shape over the shipped rule FILE: every estate
//! comparison reaches the handler with the refile window, and nothing
//! else does.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};
use serde_json::json;

mod common;

const RULE: &str = "file-the-remedy-a-verb-declares-for-an-estate-finding";
const HANDLER: &str = "ops.file_remedies";

fn rule() -> Registry {
    common::authored_rule(RULE)
}

/// The boss-gcp host comparison of 2026-09-28T10:25:01Z (event
/// b7c2d05a), the reading car 1 was pinned on, shortened.
fn boss_gcp_below_its_floor() -> serde_json::Value {
    json!({
        "counts": {"disk_tight": 1, "observed": 1},
        "findings": {"disk_tight": [{"disk_gb": 47, "floor_gb": 17, "free_gb": 11, "id": "boss-gcp"}]},
        "host": "boss-gcp", "observed_at": "2026-09-28T10:25:01Z",
        "observer": "boss-estate-observe-host", "scope": "host"
    })
}

/// Every comparison — a host's, the unit series', the cluster's —
/// reaches the handler, which reads the hard findings itself: no rule
/// predicate can iterate the verbs' declarations, and none is asked to.
/// No helper is called, so a comparison costs no jobs-API read here.
#[test]
fn every_estate_comparison_hands_the_handler_the_week_window() {
    let cluster = json!({"counts": {"not_ready": 0}, "findings": {"not_ready": []},
                         "scope": "kubernetes-nodes"});
    for payload in [boss_gcp_below_its_floor(), cluster] {
        let out = match_event(&rule(), "jobs.estate.compared", &payload, &NoHelpers);
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
        assert_eq!(out.matched.len(), 1, "{:?}", out.matched);
        let invs = &out.matched[0].invocations;
        assert_eq!(invs.len(), 1, "{invs:?}");
        assert_eq!(invs[0].handler, HANDLER);
        assert_eq!(
            invs[0].args,
            vec![("refile_after_hours".to_string(), Value::Int(168))],
            "a week: a remedy that ran need not clear its finding (review of car 4d80316f)"
        );
    }
}

/// The comparison is the one trigger: the observation it is computed
/// from, and the alarm packet it raises, do not file a remedy.
#[test]
fn only_the_comparison_topic_fires_it() {
    for topic in [
        "jobs.estate.observed",
        "jobs.job.created",
        "jobs.job.closed",
    ] {
        let out = match_event(&rule(), topic, &boss_gcp_below_its_floor(), &NoHelpers);
        assert!(out.matched.is_empty(), "{topic}: {:?}", out.matched);
    }
}

/// The boss-gcp-specific rule this one generalises is gone from the
/// tree, so the two cannot both file the same reclaim.
#[test]
fn the_boss_gcp_specific_rule_is_retired() {
    let old = boss_testing::repo_root()
        .join("infra/dispatcher/rules/file-reclaim-gcp-root-while-disk-tight-boss-gcp.toml");
    assert!(!old.exists(), "{} is back", old.display());
}
