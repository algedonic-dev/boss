//! A finished chore whose settle was interrupted cannot eat every later run.
//! The old completion is immutable; the current invocation needs its own record.
use boss_testing::{create_dir, repo_root, scratch_dir, without_proxy, write_exec, write_file};
use serde_json::{Value, json};
use std::process::Command;

fn exercise(
    original: &str,
    refuse: bool,
    plain_retry: bool,
    readback: &str,
) -> (Vec<Value>, Vec<Value>, String) {
    exercise_with_dispatcher_lag(original, refuse, plain_retry, readback, None)
}

fn exercise_with_dispatcher_lag(
    original: &str,
    refuse: bool,
    plain_retry: bool,
    readback: &str,
    close_after_reads: Option<u32>,
) -> (Vec<Value>, Vec<Value>, String) {
    let dir = scratch_dir("maintenance-finished-recovery");
    create_dir(&dir.join("lib"));
    for name in [
        "boss-maintenance-wrap.sh",
        "boss-step.sh",
        "lib/secret-header.sh",
        "lib/jq.sh",
    ] {
        write_file(
            &dir.join(name),
            &std::fs::read_to_string(repo_root().join("infra").join(name)).unwrap(),
        );
    }
    write_exec(
        &dir.join("boss-api-curl.sh"),
        &format!(
            "#!/usr/bin/env bash\nexec python3 '{}' --fixture-api \"$@\"\n",
            repo_root()
                .join("infra/forge/probe-maintenance-recovery.py")
                .display()
        ),
    );
    let old = json!({"id":"old","kind":if plain_retry {"fixture-protocol"} else {"maintenance-conservation-invariants"},"status":"open",
    "metadata":{},"steps":[
        {"id":"old-run","job_id":"old","spec_slug":"run","title":"run","status":"completed",
         "completed_at":"2026-10-23T15:21:10Z","metadata":{"result":original,"output":"original exact output\n"}},
        {"id":"old-outcome","job_id":"old","spec_slug":"completed","status":"pending","metadata":{}}
    ]});
    let store = dir.join("store.json");
    write_file(&store, &json!([old.clone()]).to_string());
    let requests = dir.join("requests");
    create_dir(&dir.join("bin"));
    // Count the real wrapper's one-second waits without spending a minute
    // on the measured 52.6-second dispatcher tail in every test run.
    write_exec(
        &dir.join("bin/sleep"),
        "#!/bin/sh\n[ \"$1\" = 1 ] || exit 1\nprintf '1\\n' >> \"$FIXTURE_SLEEPS\"\n",
    );
    let launch = dir.join("launch.sh");
    let lifecycle = if plain_retry {
        format!(
            "bash '{}' fixture-protocol run result=ok 'output=original exact output\n'\nbash '{}' fixture-protocol run\n",
            dir.join("boss-step.sh").display(),
            dir.join("boss-step.sh").display()
        )
    } else {
        format!(
            "bash '{}' maintenance-conservation-invariants 'Conservation'\nbash '{}' maintenance-conservation-invariants run result=ok 'output=current distinct output'\n",
            dir.join("boss-maintenance-wrap.sh").display(),
            dir.join("boss-step.sh").display()
        )
    };
    write_file(&launch, &lifecycle);
    let out = without_proxy(&mut Command::new("bash"))
        .arg(&launch)
        .env("BOSS_JOBS_URL", "http://fixture.invalid:7900")
        .env("BOSS_API_CURL", dir.join("boss-api-curl.sh"))
        .env("FIXTURE_STORE", &store)
        .env("FIXTURE_REQUESTS", &requests)
        .env("FIXTURE_REFUSE_SETTLE", if refuse { "1" } else { "0" })
        .env("FIXTURE_READBACK", readback)
        .env(
            "FIXTURE_CLOSE_AFTER_READS",
            close_after_reads.map(|n| n.to_string()).unwrap_or_default(),
        )
        .env("FIXTURE_SLEEPS", dir.join("sleeps"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("KUBERNETES_SERVICE_HOST", "fixture")
        .env_remove("BOSS_RUN_SUMMARY_FILE")
        .env_remove("BOSS_NODE_ID")
        .env_remove("HOST_ID")
        .output()
        .unwrap();
    let jobs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    let calls = std::fs::read_to_string(&requests)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let text = format!(
        "exit {:?}\n{}\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        jobs[0]["steps"][0], old["steps"][0],
        "old completion changed: {text}"
    );
    if close_after_reads.is_some() && jobs.len() == 2 {
        let sleeps = std::fs::read_to_string(dir.join("sleeps")).unwrap();
        assert_eq!(sleeps.lines().count(), 53, "{text}");
    }
    (jobs, calls, text)
}

#[test]
fn a_dispatcher_closing_after_fifty_three_seconds_still_records_this_invocation() {
    let (jobs, _, text) = exercise_with_dispatcher_lag("ok", false, false, "ok", Some(54));
    assert_eq!(
        jobs.len(),
        2,
        "measured dispatcher tail lost the current invocation: {text}"
    );
    assert_eq!(
        jobs[1]["steps"][0]["metadata"]["output"], "current distinct output",
        "{text}"
    );
}

#[test]
fn a_finished_run_is_settled_before_a_distinct_current_run_is_recorded() {
    for original in ["ok", "exit-code"] {
        let (jobs, calls, text) = exercise(original, false, false, "ok");
        assert_eq!(
            jobs.len(),
            2,
            "current invocation has no distinct packet: {text}"
        );
        assert_eq!(jobs[0]["status"], "closed", "{text}");
        assert_eq!(
            jobs[0]["steps"][1]["metadata"]["outcome_kind"],
            if original == "ok" {
                "completed"
            } else {
                "aborted"
            }
        );
        assert_eq!(
            jobs[1]["steps"][0]["metadata"]["output"], "current distinct output",
            "{text}"
        );
        assert_eq!(jobs[1]["status"], "closed", "{text}");
        assert!(
            !calls
                .iter()
                .any(|c| c["method"] == "PATCH"
                    && c["path"] == "/api/jobs/old/steps/old-run/metadata")
        );
    }
}

#[test]
fn an_unsettled_finished_packet_never_claims_the_current_output_was_recorded() {
    let (jobs, calls, text) = exercise("ok", true, false, "ok");
    assert_eq!(
        jobs.len(),
        1,
        "a refusal must not create competing open packets"
    );
    assert_eq!(jobs[0]["status"], "open");
    assert!(
        calls.iter().any(|c| c["method"] == "PUT"),
        "settle was never retried: {text}"
    );
    assert!(
        text.contains("unrecorded") || text.contains("not recorded"),
        "current evidence was silently discarded: {text}"
    );
    assert!(
        text.contains("current distinct output"),
        "the full unrecorded evidence must survive the refusal: {text}"
    );
}

#[test]
fn an_unchanged_generic_step_retry_is_idempotent_and_keeps_its_original_facts() {
    let (jobs, calls, text) = exercise("ok", false, true, "ok");
    assert_eq!(jobs.len(), 1, "a plain retry is no new invocation: {text}");
    assert_eq!(jobs[0]["status"], "closed", "{text}");
    assert_eq!(calls.iter().filter(|c| c["method"] == "PUT").count(), 1);
    assert!(
        !calls
            .iter()
            .any(|c| c["method"] == "PATCH" || c["method"] == "POST")
    );
    assert!(text.contains("original evidence retained"), "{text}");
    assert!(
        text.contains("exit Some(0)"),
        "a plain status retry with no new facts stays harmless: {text}"
    );
}

#[test]
fn a_closed_predecessor_with_unread_closure_does_not_discard_current_evidence() {
    assert_unrecorded_closed_predecessor("unread");
}

#[test]
fn a_closed_predecessor_with_an_empty_closure_answer_is_not_verified() {
    assert_unrecorded_closed_predecessor("empty");
}

fn assert_unrecorded_closed_predecessor(readback: &str) {
    let (jobs, calls, text) = exercise("ok", false, false, readback);
    assert_eq!(
        jobs.len(),
        1,
        "unverified closure must not admit another packet: {text}"
    );
    assert_eq!(
        jobs[0]["status"], "closed",
        "the original status retry really landed"
    );
    assert!(!calls.iter().any(|c| c["method"] == "POST"));
    assert!(
        text.contains("exit Some(1)"),
        "lost current accounting must not report success: {text}"
    );
    assert!(
        text.contains("not recorded") && text.contains("current distinct output"),
        "full current evidence must survive even with no open packet: {text}"
    );
}

#[test]
fn reusable_fixture_entrypoint_runs_the_real_transport_without_claiming_installation() {
    let out = without_proxy(&mut Command::new("python3"))
        .arg(repo_root().join("infra/forge/probe-maintenance-recovery.py"))
        .arg("--self-test")
        .env("BOSS_JOBS_URL", "http://must-not-be-used.invalid:7900")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "standalone localhost mechanism: {text}"
    );
    assert!(
        text.contains("maintenance recovery fixture self-test passed"),
        "{text}"
    );
    assert!(
        !text.contains("installed maintenance recovery records each invocation"),
        "simulation is not production proof: {text}"
    );
}

#[test]
fn reusable_fixture_entrypoint_refuses_a_source_copy_as_an_installed_artifact() {
    let out = without_proxy(&mut Command::new("python3"))
        .arg(repo_root().join("infra/forge/probe-maintenance-recovery.py"))
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(75),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reusable_probe_shape_keeps_the_real_unidentified_reader_guard() {
    let standalone = "python3 /var/lib/boss/tree/current/infra/forge/probe-maintenance-recovery.py";
    assert_eq!(
        boss_jobs::probe::reads_the_sor_unidentified(standalone),
        None
    );
    let real_reader = "python3 -c \"import urllib.request,os; urllib.request.urlopen(os.environ['BOSS_JOBS_URL']+'/api/jobs')\"";
    assert_eq!(
        boss_jobs::probe::reads_the_sor_unidentified(real_reader),
        Some("python3")
    );
}

#[test]
fn reusable_fixture_rejects_a_request_without_its_named_automation_header() {
    let out = without_proxy(&mut Command::new("python3"))
        .arg(repo_root().join("infra/forge/probe-maintenance-recovery.py"))
        .args([
            "--fixture-api",
            "http://fixture.invalid/api/jobs?kind=fixture",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("actual named automation header"));
}
