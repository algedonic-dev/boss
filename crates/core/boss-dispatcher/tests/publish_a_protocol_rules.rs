//! The three rules that make a held workflow publish a packet on its
//! owner (design b1f319bc, decided by David 2026-11-05; backlog
//! 7955a488), pinned over the shipped rule files and the shipped
//! `publish-a-protocol` row, not copies:
//!
//! - `file-protocol-publishes-on-boss-gcp-converge-moved` — boss-gcp's
//!   checkout moving (a landed train reaching the publishing host) files
//!   `publish-drift --file-held`, the verb that files one packet per
//!   held kind whose tree is ahead of live;
//! - `publish-a-protocol-files-its-machine-publish` — the packet's
//!   `publish` step becoming ready (the owner signed) files
//!   `publish-workflow <kind>` on boss-gcp with the packet linked back;
//! - `complete-protocol-publish-on-publish-workflow-answered` — that
//!   verb's confirmed line completes `publish` with what went live.
//!
//! What is pinned HERE is each rule's selection and the args it hands
//! its handler, and — because the rule and the row are two files that
//! name one fact (CLAUDE.md §9a) — that the marker, the kind key, the
//! link and the step's required fields agree between them. The script
//! that honours the link is pinned in boss-testing's
//! publish_workflow_sh.rs; the filing verb in publish_drift_sh.rs.

use boss_dispatcher::rules::expr::{NoHelpers, Value};
use boss_dispatcher::rules::registry::match_event;

mod common;

const FILE_RULE: &str = "file-protocol-publishes-on-boss-gcp-converge-moved";
const SPAWN_RULE: &str = "publish-a-protocol-files-its-machine-publish";
const ANSWER_RULE: &str = "complete-protocol-publish-on-publish-workflow-answered";
const ROW: &str = "infra/platform/workflows/publish-a-protocol.toml";

fn arg(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing; got {args:?}"))
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

/// The shipped row, as TOML: the rule is held equal to it below.
fn row() -> toml::Value {
    let path = boss_testing::repo_root().join(ROW);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} ships: {e}", path.display()));
    toml::from_str::<toml::Value>(&text).expect("the row parses as a TOML document")
}

fn step(row: &toml::Value, title: &str) -> toml::Value {
    row["workflow"][0]["step"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["title"].as_str() == Some(title))
        .cloned()
        .unwrap_or_else(|| panic!("the row has a `{title}` step"))
}

// ---------------------------------------------------------------------------
// The filing: a landed edit of a held bundle file.
// ---------------------------------------------------------------------------

fn converge_run_done(workflow_kind: &str, metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "job_id": "c-1", "step_id": "s-run", "kind": "task",
        "subject_kind": "custom", "subject_id": "infra/maintenance-boss-gcp-converge",
        "workflow_kind": workflow_kind,
        "completed_on": "2026-11-05",
        "metadata": metadata,
        "notify_on_done": false,
        "spec_slug": "run"
    })
}

fn moved() -> serde_json::Value {
    serde_json::json!({
        "result": "ok",
        "converge_from": "a2fa9b0910000000000000000000000000000aaa",
        "converge_sha": "b1f319bc0000000000000000000000000000bbbb",
    })
}

#[test]
fn a_moved_boss_gcp_checkout_files_the_held_kinds_publish_packets() {
    let reg = common::authored_rule(FILE_RULE);
    let outcome = match_event(
        &reg,
        "step.done.task",
        &converge_run_done("maintenance-boss-gcp-converge", moved()),
        &NoHelpers,
    );
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1, "one filing per movement");
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    assert_eq!(arg(&inv.args, "kind"), s("ops-request"));
    assert_eq!(arg(&inv.args, "subject"), s("boss-gcp"));
    assert_eq!(arg(&inv.args, "metadata.host"), s("boss-gcp"));
    assert_eq!(arg(&inv.args, "metadata.verb"), s("publish-drift"));
    // The mode is an ARG the verb file admits, never packet prose: the
    // filing mode, so the check the sibling rule files stays a read.
    assert_eq!(
        arg(&inv.args, "metadata.args"),
        Value::List(vec![s("--file-held")])
    );
    assert_eq!(arg(&inv.args, "metadata.for_converge"), s("c-1"));

    // The verb file admits exactly that word as the mode.
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            boss_testing::repo_root().join("infra/ops/verbs/publish-drift.json"),
        )
        .expect("the publish-drift verb file"),
    )
    .expect("JSON");
    assert!(
        verb["params"][0]["one_of"]
            .as_array()
            .expect("one_of")
            .iter()
            .any(|m| m == "--file-held"),
        "publish-drift admits --file-held: {}",
        verb["params"]
    );
}

#[test]
fn an_unmoved_tick_a_failed_run_or_another_converge_files_nothing() {
    let reg = common::authored_rule(FILE_RULE);
    let mut same = moved();
    same["converge_sha"] = same["converge_from"].clone();
    let mut failed = moved();
    failed["result"] = serde_json::json!("failed");
    for (kind, md) in [
        ("maintenance-boss-gcp-converge", same),
        ("maintenance-boss-gcp-converge", failed),
        ("maintenance-forge-converge", moved()),
    ] {
        let outcome = match_event(
            &reg,
            "step.done.task",
            &converge_run_done(kind, md.clone()),
            &NoHelpers,
        );
        assert!(outcome.matched.is_empty(), "{kind} {md} must not file");
        assert!(
            outcome.skipped.is_empty(),
            "{kind} {md}: {:?}",
            outcome.skipped
        );
    }
}

// ---------------------------------------------------------------------------
// The machine publish: the owner signed.
// ---------------------------------------------------------------------------

fn step_ready(metadata: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "job_id": "p-1", "step_id": "s-publish", "kind": "task",
        "subject_kind": "custom", "subject_id": "ops-request",
        "assignee_id": null,
        "metadata": metadata
    })
}

#[test]
fn a_ready_publish_step_files_the_one_kind_publish_with_the_packet_linked() {
    let reg = common::authored_rule(SPAWN_RULE);
    let payload = step_ready(serde_json::json!({
        "ops_verb": "publish-workflow", "protocol_kind": "ops-request",
    }));
    let outcome = match_event(&reg, "step.ready.task", &payload, &NoHelpers);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1);
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    assert_eq!(arg(&inv.args, "kind"), s("ops-request"));
    assert_eq!(arg(&inv.args, "metadata.host"), s("boss-gcp"));
    assert_eq!(arg(&inv.args, "metadata.verb"), s("publish-workflow"));
    // The kind is the verb's one positional arg — real mode, no word.
    assert_eq!(
        arg(&inv.args, "metadata.args"),
        Value::List(vec![s("ops-request")])
    );
    // THE EDGE the script follows to the signature.
    assert_eq!(arg(&inv.args, "metadata.for_protocol_publish"), s("p-1"));
}

#[test]
fn other_task_steps_and_the_done_topic_do_not_fire_the_publish() {
    let reg = common::authored_rule(SPAWN_RULE);
    for md in [
        serde_json::json!({}),
        serde_json::json!({ "ops_verb": "publish-github-pr" }),
        serde_json::json!({ "ops_verb": "" }),
    ] {
        let outcome = match_event(&reg, "step.ready.task", &step_ready(md.clone()), &NoHelpers);
        assert!(outcome.matched.is_empty(), "{md} must not fire");
        assert!(outcome.skipped.is_empty(), "{md}: {:?}", outcome.skipped);
    }
    let done = step_ready(serde_json::json!({
        "ops_verb": "publish-workflow", "protocol_kind": "ops-request",
    }));
    assert!(
        match_event(&reg, "step.done.task", &done, &NoHelpers)
            .matched
            .is_empty(),
        "the machine completing the step must not file a second publish"
    );
}

/// §9a: the marker the rule reads and the kind it hands on are the
/// row's own keys on the step the rule names, and that step is ready
/// only on an approved, presence-signed approve.
#[test]
fn the_rule_reads_the_marker_and_the_kind_the_row_stamps_on_publish() {
    let row = row();
    let publish = step(&row, "publish");
    assert_eq!(publish["kind"].as_str(), Some("task"));
    let md = &publish["metadata_defaults"];
    assert_eq!(md["ops_verb"].as_str(), Some("publish-workflow"));
    assert_eq!(
        md["protocol_kind"].as_str(),
        Some("{metadata.protocol_kind}"),
        "the kind is rendered from the job, so the spawn hands on the kind itself"
    );
    assert_eq!(
        publish["ready_when"].as_str(),
        Some(
            "steps.approve.done AND steps.approve.metadata.decision = \"approved\" AND job.metadata.registry != \"pool\""
        ),
        "a protocol row's door, and only a protocol row's (backlog f399458f)"
    );
    let approve = step(&row, "approve");
    assert_eq!(approve["assurance_required"].as_str(), Some("presence"));
}

// ---------------------------------------------------------------------------
// The answer: what went live is the verb's read-back.
// ---------------------------------------------------------------------------

/// publish-workflow.sh's confirmed line, as it prints it (its last
/// `say`, pinned there by publish_workflow_sh.rs).
const CONFIRMED: &str = "publish-workflow: ops-request v13 -> v14 live at http://sor.invalid — confirmed by reading the active row back and comparing it to infra/platform/workflows/ops-request.toml (equal); packet 2b0c4d0e-0000-4000-8000-000000000001";

#[test]
fn an_answered_publish_completes_the_linked_step_with_what_went_live() {
    let reg = common::authored_rule(ANSWER_RULE);
    let closed = serde_json::json!({
        "id": "2b0c4d0e-0000-4000-8000-000000000001",
        "kind": "ops-request", "outcome": "answered",
        "closed_on": "2026-11-05", "parent_step_id": null,
    });
    let outcome = match_event(&reg, "jobs.job.closed", &closed, &NoHelpers);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1);
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    assert_eq!(arg(&inv.args, "verb"), s("publish-workflow"));
    assert_eq!(arg(&inv.args, "link"), s("for_protocol_publish"));
    assert_eq!(arg(&inv.args, "steps"), s("publish"));
    assert_eq!(arg(&inv.args, "on_failure"), s("annotate-and-alert"));

    let Value::String(pattern) = arg(&inv.args, "verdict_pattern") else {
        panic!("verdict_pattern is a string");
    };
    let re = regex::Regex::new(&pattern).expect("the pattern is a regex");
    let caps = re
        .captures(CONFIRMED)
        .unwrap_or_else(|| panic!("{pattern} reads the confirmed line"));
    assert_eq!(&caps["version"], "14");
    assert_eq!(&caps["from_version"], "13");
    assert_eq!(&caps["read_back"], "equal");
    // An observed equality withdraws an obligation; it never records a
    // publish by this packet or satisfies the published terminal.
    assert!(!re.is_match(&format!(
        "publish-workflow: EQUAL: ops-request v14; spec sha256 {}",
        "a".repeat(64)
    )));
    // A refusal is never an answer.
    assert!(
        !re.is_match(
            "publish-workflow: REFUSED — 'ops-request' is HELD out of every unattended publish"
        ),
        "a refused hold matches no answer"
    );

    // §9a: the fields written are exactly the step's required fields.
    let Value::String(done) = arg(&inv.args, "done_metadata") else {
        panic!("done_metadata is a string");
    };
    let done: serde_json::Value = serde_json::from_str(&done).expect("done_metadata is JSON");
    let mut written: Vec<String> = done.as_object().expect("object").keys().cloned().collect();
    written.sort();
    let publish = step(&row(), "publish");
    let mut required: Vec<String> = publish["fields"]
        .as_array()
        .expect("publish declares fields")
        .iter()
        .filter(|f| f["required"].as_bool() == Some(true))
        .map(|f| f["name"].as_str().unwrap_or_default().to_string())
        .collect();
    required.sort();
    assert_eq!(
        written, required,
        "the rule writes exactly what publish requires"
    );
}

// ---------------------------------------------------------------------------
// The pool row's door (backlog f399458f ask 2): the same two rules, for
// the `publish-pool` step and the `publish-pool` verb.
// ---------------------------------------------------------------------------

const POOL_SPAWN_RULE: &str = "publish-a-protocol-files-its-pool-publish";
const POOL_ANSWER_RULE: &str = "complete-pool-publish-on-publish-pool-answered";

#[test]
fn a_ready_pool_publish_step_files_the_pool_verb_with_the_packet_linked() {
    let reg = common::authored_rule(POOL_SPAWN_RULE);
    let payload = step_ready(serde_json::json!({
        "ops_verb": "publish-pool", "pool": "robots:opus",
    }));
    let outcome = match_event(&reg, "step.ready.task", &payload, &NoHelpers);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1);
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.spawn");
    assert_eq!(arg(&inv.args, "kind"), s("ops-request"));
    assert_eq!(arg(&inv.args, "metadata.host"), s("boss-gcp"));
    assert_eq!(arg(&inv.args, "metadata.verb"), s("publish-pool"));
    assert_eq!(
        arg(&inv.args, "metadata.args"),
        Value::List(vec![s("robots:opus")])
    );
    assert_eq!(arg(&inv.args, "metadata.for_protocol_publish"), s("p-1"));

    // Neither rule fires for the other's marker: one door per registry.
    let workflow = common::authored_rule(SPAWN_RULE);
    assert!(
        match_event(&workflow, "step.ready.task", &payload, &NoHelpers)
            .matched
            .is_empty(),
        "a pool step must not file publish-workflow"
    );
    let protocol = step_ready(serde_json::json!({
        "ops_verb": "publish-workflow", "protocol_kind": "ops-request",
    }));
    assert!(
        match_event(&reg, "step.ready.task", &protocol, &NoHelpers)
            .matched
            .is_empty(),
        "a protocol step must not file publish-pool"
    );
}

/// §9a: the marker and the arg are the row's own keys on `publish-pool`,
/// and that step is ready only on an approved packet for a pool row.
#[test]
fn the_pool_rule_reads_the_marker_and_the_name_the_row_stamps_on_publish_pool() {
    let row = row();
    let publish = step(&row, "publish-pool");
    assert_eq!(publish["kind"].as_str(), Some("task"));
    let md = &publish["metadata_defaults"];
    assert_eq!(md["ops_verb"].as_str(), Some("publish-pool"));
    assert_eq!(md["pool"].as_str(), Some("{metadata.row_name}"));
    assert_eq!(
        publish["ready_when"].as_str(),
        Some(
            "steps.approve.done AND steps.approve.metadata.decision = \"approved\" AND job.metadata.registry = \"pool\""
        )
    );
    // The verb the rule names exists and takes the one arg it hands on.
    let verb: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            boss_testing::repo_root().join("infra/ops/verbs/publish-pool.json"),
        )
        .expect("the publish-pool verb file"),
    )
    .expect("JSON");
    assert_eq!(verb["params"].as_array().map(Vec::len), Some(1));
}

/// publish-pool.sh's confirmed line, as it prints it (pinned there by
/// publish_pool_sh.rs).
const POOL_CONFIRMED: &str = "publish-pool: robots:opus v2 -> v3 live at http://sor.invalid — confirmed by reading the lineage back and comparing it to the signed row (equal); packet 2b0c4d0e-0000-4000-8000-000000000002";

#[test]
fn an_answered_pool_publish_completes_publish_pool_with_what_went_live() {
    let reg = common::authored_rule(POOL_ANSWER_RULE);
    let closed = serde_json::json!({
        "id": "2b0c4d0e-0000-4000-8000-000000000002",
        "kind": "ops-request", "outcome": "answered",
        "closed_on": "2026-11-08", "parent_step_id": null,
    });
    let outcome = match_event(&reg, "jobs.job.closed", &closed, &NoHelpers);
    assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
    assert_eq!(outcome.matched.len(), 1);
    let inv = &outcome.matched[0].invocations[0];
    assert_eq!(inv.handler, "jobs.complete_linked_step");
    assert_eq!(arg(&inv.args, "verb"), s("publish-pool"));
    assert_eq!(arg(&inv.args, "link"), s("for_protocol_publish"));
    assert_eq!(arg(&inv.args, "steps"), s("publish-pool"));
    assert_eq!(arg(&inv.args, "on_failure"), s("annotate-and-alert"));
    let Value::String(pattern) = arg(&inv.args, "verdict_pattern") else {
        panic!("verdict_pattern is a string");
    };
    let re = regex::Regex::new(&pattern).expect("the pattern is a regex");
    let caps = re
        .captures(POOL_CONFIRMED)
        .unwrap_or_else(|| panic!("{pattern} reads the confirmed line"));
    assert_eq!(&caps["version"], "3");
    assert_eq!(&caps["from_version"], "2");
    assert_eq!(&caps["read_back"], "equal");
    assert!(
        !re.is_match("publish-pool: REFUSED — publish-a-protocol 2b0c4d0e is not a signature"),
        "a refusal matches no answer"
    );
    let Value::String(done) = arg(&inv.args, "done_metadata") else {
        panic!("done_metadata is a string");
    };
    let done: serde_json::Value = serde_json::from_str(&done).expect("done_metadata is JSON");
    let mut written: Vec<String> = done.as_object().expect("object").keys().cloned().collect();
    written.sort();
    let mut required: Vec<String> = step(&row(), "publish-pool")["fields"]
        .as_array()
        .expect("publish-pool declares fields")
        .iter()
        .filter(|f| f["required"].as_bool() == Some(true))
        .map(|f| f["name"].as_str().unwrap_or_default().to_string())
        .collect();
    required.sort();
    assert_eq!(written, required);

    // The protocol row's answer rule does not take the pool verb's line.
    let workflow = common::authored_rule(ANSWER_RULE);
    let Value::String(wpattern) = arg(
        &match_event(&workflow, "jobs.job.closed", &closed, &NoHelpers).matched[0].invocations[0]
            .args,
        "verdict_pattern",
    ) else {
        panic!("verdict_pattern is a string");
    };
    assert!(
        !regex::Regex::new(&wpattern)
            .expect("the pattern is a regex")
            .is_match(POOL_CONFIRMED)
    );
}

/// A read-back equality witness takes the existing withdrawn terminal,
/// without pretending that this packet published a new version.
#[test]
fn a_live_equality_witness_can_end_the_obsolete_publish_obligation() {
    use boss_dispatcher::rules::expr::{Context, eval, parse};
    let terminal = step(&row(), "superseded");
    assert_eq!(terminal["terminal"]["outcome"].as_str(), Some("superseded"));
    let expr = parse(terminal["ready_when"].as_str().unwrap()).unwrap();
    for (by, translation, expected) in [
        (
            "workflow:agent-run:v14",
            "live v14 compared equal to the frozen tree",
            true,
        ),
        ("", "live v14 compared equal to the frozen tree", false),
        ("workflow:agent-run:v14", "", false),
    ] {
        let payload = serde_json::json!({"steps":{"filed":{"done":true}},"job":{"metadata":{"superseded_by":by,"supersession_translation":translation}}});
        assert_eq!(
            eval(
                &expr,
                &Context {
                    payload: &payload,
                    helpers: &NoHelpers
                }
            )
            .unwrap(),
            Value::Bool(expected)
        );
    }
}
