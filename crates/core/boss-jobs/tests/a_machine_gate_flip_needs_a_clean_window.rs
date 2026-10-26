use boss_jobs::release_window::{judge_window_receipt, machine_gate_mode};
use serde_json::json;

fn manifest(mode: &str) -> String {
    format!("kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\ndata:\n  mode: {mode}\n")
}

#[test]
fn the_mode_is_read_as_yaml_including_aliases_escapes_and_block_scalars() {
    use boss_core::machine_gate::Mode;
    for mode in ["enforce", "\"\\u0065nforce\"", "|\n    enforce"] {
        assert_eq!(machine_gate_mode(&manifest(mode)).unwrap(), Mode::Enforce);
    }
    let alias = "kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\nword: &word enforce\ndata:\n  mode: *word\n";
    assert_eq!(machine_gate_mode(alias).unwrap(), Mode::Enforce);
    assert_eq!(
        machine_gate_mode(&manifest("report")).unwrap(),
        Mode::Report
    );
    assert!(machine_gate_mode("").is_err());
    assert!(machine_gate_mode(&manifest("unknown")).is_err());
    assert!(
        machine_gate_mode(&format!("{}  mode: report\n", manifest("enforce"))).is_err(),
        "duplicate keys are not a mode"
    );
    let merge = "kind: ConfigMap\nmetadata:\n  name: boss-machine-gate\nword: &word {mode: enforce}\ndata:\n  <<: *word\n";
    assert!(
        machine_gate_mode(merge).is_err(),
        "an unexpanded merge is refused"
    );
    assert!(
        machine_gate_mode(&format!(
            "{}---\n{}",
            manifest("report"),
            manifest("enforce")
        ))
        .is_err(),
        "duplicate ConfigMaps are ambiguous"
    );
}

fn clean() -> serde_json::Value {
    json!({"gate":"machine-gate", "from":"2026-10-22T00:00:00Z", "now":"2026-10-23T02:00:00Z",
        "clean_since":"2026-10-21T00:00:00Z", "covers_requested_window":true,
        "log_error":null, "log":{"dirty":[], "not_clean":[], "coverage":[{"service":"jobs", "gap":null}]},
        "not_clean":[], "input_errors":[], "roster_generations":[{"generation":"sha256:fixture"}],
        "required_services":["jobs"], "live":[{"service":"jobs", "error":null, "not_clean":[],
            "snapshot":{"mode":"report", "overflow":0, "not_clean":[], "rows":[]}}]})
}

fn rust_accepts(window: &serde_json::Value) -> bool {
    judge_window_receipt(
        &json!({"head":"head", "exit_code":0,"window":window}),
        "head",
        "2026-10-23T02:00:00Z",
    )
    .is_ok()
}

fn shell_accepts(window: &serde_json::Value) -> bool {
    use std::process::{Command, Stdio};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = Command::new("bash")
        .arg(root.join("infra/ops/gate-window-is-clean.sh"))
        .args(["machine-gate", "26"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(&mut child, window.to_string().as_bytes());
    child.wait().unwrap().success()
}

#[test]
fn release_and_the_shell_judge_refuse_each_missing_or_dirty_half() {
    assert!(rust_accepts(&clean()));
    assert!(shell_accepts(&clean()));
    for pointer in [
        "/gate",
        "/from",
        "/now",
        "/clean_since",
        "/covers_requested_window",
        "/log",
        "/log/dirty",
        "/log/not_clean",
        "/log/coverage",
        "/not_clean",
        "/input_errors",
        "/required_services",
        "/live",
        "/live/0/snapshot",
        "/live/0/snapshot/rows",
        "/live/0/snapshot/mode",
        "/live/0/snapshot/overflow",
        "/live/0/snapshot/not_clean",
        "/roster_generations",
    ] {
        let mut missing = clean();
        *missing.pointer_mut(pointer).unwrap() = json!(null);
        assert!(!rust_accepts(&missing), "missing {pointer}");
        assert_eq!(rust_accepts(&missing), shell_accepts(&missing), "{pointer}");
    }
    for (pointer, dirty) in [
        ("/log/dirty", json!([{"kind":"miss"}])),
        ("/log/coverage/0/gap", json!("end missing")),
        ("/log_error", json!("unread")),
        ("/live/0/error", json!("unread")),
        ("/live/0/snapshot/mode_error", json!("invalid")),
        ("/live/0/snapshot/overflow", json!(1)),
        (
            "/live/0/snapshot/rows",
            json!([{"presented":"none","peer":"peer","user":"user","method":"GET","route":"/api/jobs","count":1}]),
        ),
        ("/not_clean", json!(["gap"])),
        ("/input_errors", json!(["unread"])),
        ("/roster_generations", json!([])),
        ("/required_services", json!([])),
    ] {
        let mut bad = clean();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        bad.pointer_mut(parent).unwrap()[key] = dirty;
        assert!(!rust_accepts(&bad), "dirty {pointer}");
        assert_eq!(rust_accepts(&bad), shell_accepts(&bad), "{pointer}");
    }
}

#[test]
fn a_short_window_and_a_receipt_for_another_head_are_refused() {
    let mut short = clean();
    short["from"] = json!("2026-10-23T01:00:00Z");
    assert!(!rust_accepts(&short));
    assert!(!shell_accepts(&short));
    let receipt = json!({"head":"old", "exit_code":0, "window":clean()});
    assert!(judge_window_receipt(&receipt, "head", "2026-10-23T02:00:00Z").is_err());
    assert!(
        judge_window_receipt(
            &json!({"head":"head","exit_code":1,"window":clean()}),
            "head",
            "2026-10-23T02:00:00Z"
        )
        .is_err()
    );
}

struct SourceReader(Option<boss_jobs::first_record::FirstRecord>);
#[async_trait::async_trait]
impl boss_jobs::car_release::ExecutorRecordReader for SourceReader {
    async fn protocol_version(
        &self,
        kind: &str,
        version: i64,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        Ok(Some(
            json!({"kind":kind,"version":version,"status":"active","steps":[{"title":"review"}]}),
        ))
    }
    async fn executor_original(
        &self,
        _: uuid::Uuid,
        _: uuid::Uuid,
        key: &str,
    ) -> anyhow::Result<Option<boss_jobs::first_record::FirstRecord>> {
        if key != "review.source" {
            return Ok(None);
        }
        Ok(self.0.clone())
    }
}

fn source_fixture(
    diff: &str,
) -> (
    serde_json::Value,
    serde_json::Value,
    boss_jobs::first_record::FirstRecord,
) {
    use boss_core::{
        actor::ActorId,
        job::{JobId, StepId},
        publisher::EventStamp,
    };
    let run_id = uuid::Uuid::from_u128(1);
    let building = uuid::Uuid::from_u128(2);
    let car = json!({"id":uuid::Uuid::from_u128(3).to_string(), "metadata":{}});
    let mut run = json!({"id":run_id.to_string(),"steps":[{"id":building.to_string(),"spec_slug":"building"}]});
    let source = json!({"car":car["id"], "head":"1111111111111111111111111111111111111111", "main":"2222222222222222222222222222222222222222", "base":"3333333333333333333333333333333333333333", "path":"infra/cluster/manifests/boss.yaml", "mode":"100644","base_mode":"100644", "base_manifest":manifest("report"), "manifest":manifest(if diff.contains("mode: enforce") { "enforce" } else { "report" }), "diff":diff});
    let record = boss_jobs::first_record::FirstRecord::new(
        JobId::from_uuid(run_id),
        StepId::from_uuid(building),
        "review.source",
        &source,
        &EventStamp::new("test", ActorId::automation("review")),
        uuid::Uuid::from_u128(4),
    );
    run["metadata"]["review"]["source_binding"] =
        json!({"digest":record.digest,"event_id":record.event_id});
    (car, run, record)
}

#[tokio::test]
async fn the_release_reads_the_original_source_through_its_port_and_fails_closed() {
    use boss_jobs::release_window::judge_source_window;
    let head = "1111111111111111111111111111111111111111";
    let (mut car, run, record) = source_fixture("");
    assert!(
        judge_source_window(
            &SourceReader(Some(record.clone())),
            &car,
            &run,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_ok()
    );
    assert!(
        judge_source_window(
            &SourceReader(None),
            &car,
            &run,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err()
    );
    let mut unbound = run.clone();
    unbound["metadata"]["review"]["source_binding"] = json!(null);
    assert!(
        judge_source_window(
            &SourceReader(Some(record.clone())),
            &car,
            &unbound,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err()
    );
    let mut forged = record.clone();
    forged.value["diff"] = json!("changed");
    assert!(
        judge_source_window(
            &SourceReader(Some(forged)),
            &car,
            &run,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err()
    );
    assert!(
        judge_source_window(
            &SourceReader(Some(record)),
            &car,
            &run,
            "4444444444444444444444444444444444444444",
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err()
    );
    let (_, flip_run, flip) = source_fixture(
        "diff --git a/infra/cluster/manifests/boss.yaml b/infra/cluster/manifests/boss.yaml\n+  mode: enforce\n",
    );
    assert!(
        judge_source_window(
            &SourceReader(Some(flip.clone())),
            &car,
            &flip_run,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err()
    );
    car["metadata"]["machine_gate_window"] = json!({"head":head,"exit_code":0,"window":clean()});
    assert!(
        judge_source_window(
            &SourceReader(Some(flip)),
            &car,
            &flip_run,
            head,
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn the_actual_release_door_and_yard_decision_both_require_the_window() {
    use boss_jobs::car_release::{JudgedHold, ReleaseAsk, decision, judge};
    let head = "1111111111111111111111111111111111111111";
    let (mut car, mut run, record) = source_fixture(
        "diff --git a/infra/cluster/manifests/boss.yaml b/infra/cluster/manifests/boss.yaml\n+  mode: enforce\n",
    );
    car["kind"] = json!("ship-a-change");
    car["workflow_version"] = json!(1);
    car["steps"] = json!([{"id":uuid::Uuid::from_u128(5).to_string(),"spec_slug":"review","title":"Open for review","status":"active","metadata":{"hold":"David reads the window","hold_sha":head,"agent_run":run["id"],"releaser":"emp-david"}}]);
    run["kind"] = json!("agent-run");
    run["metadata"] = json!({"review":{"car":car["id"],"reviewed_sha":head,"verdict":"release","at":"2026-10-23T02:00:00Z","source_binding":{"digest":record.digest,"event_id":record.event_id}}});
    let reader = SourceReader(Some(record));
    let ask = ReleaseAsk {
        review: run["id"].as_str().unwrap().into(),
        hold: JudgedHold::of(&car),
        ..Default::default()
    };
    assert!(
        judge(
            &reader,
            &car,
            &run,
            &ask,
            "emp-david",
            "2026-10-23T02:00:00Z"
        )
        .await
        .unwrap_err()
        .contains("window")
    );
    assert!(
        !decision(&reader, &car, Ok(&run), "2026-10-23T02:00:00Z")
            .await
            .releasable
    );
    car["metadata"]["machine_gate_window"] = json!({"head":head,"exit_code":0,"window":clean()});
    let granted = judge(
        &reader,
        &car,
        &run,
        &ask,
        "emp-david",
        "2026-10-23T02:00:00Z",
    )
    .await
    .unwrap();
    assert_eq!(
        granted.record["review_source"]["machine_gate_window"],
        car["metadata"]["machine_gate_window"]
    );
    assert!(
        decision(&reader, &car, Ok(&run), "2026-10-23T02:00:00Z")
            .await
            .releasable
    );
    assert!(
        judge(
            &reader,
            &car,
            &run,
            &ask,
            "automation:release",
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_err(),
        "the David reservation still holds"
    );
}

#[test]
fn the_receipt_must_be_a_recent_successful_machine_read() {
    let receipt = json!({"head":"head","exit_code":0,"window":clean()});
    assert!(
        judge_window_receipt(&receipt, "head", "2026-10-23T02:16:00Z")
            .unwrap_err()
            .contains("15 minutes")
    );
    assert!(
        judge_window_receipt(&receipt, "head", "2026-10-23T01:59:59Z")
            .unwrap_err()
            .contains("future")
    );
    for result in [json!(1), json!(2), json!(null), json!("0")] {
        let mut invalid = receipt.clone();
        invalid["exit_code"] = result;
        assert!(judge_window_receipt(&invalid, "head", "2026-10-23T02:00:00Z").is_err());
    }
}

#[test]
fn a_clean_window_may_hold_presentations_enforce_already_accepts() {
    for presented in [
        "previous",
        "reader.current",
        "reader.next",
        "reader.previous",
        "asked",
    ] {
        let mut w = clean();
        w["live"][0]["snapshot"]["rows"] = json!([{"presented":presented,"peer":"peer","user":"user","method":"GET","route":"/api/jobs","count":1.0}]);
        assert!(rust_accepts(&w), "{presented}");
        assert!(shell_accepts(&w), "{presented}");
    }
}

#[tokio::test]
async fn a_review_can_attest_that_its_repository_has_no_stack_manifest() {
    use boss_core::{actor::ActorId, publisher::EventStamp};
    let (car, mut run, record) = source_fixture("");
    let mut value = record.value.clone();
    value["mode"] = json!("absent");
    value["base_mode"] = json!("absent");
    value["manifest"] = json!(null);
    value["base_manifest"] = json!(null);
    let source = boss_jobs::first_record::FirstRecord::new(
        record.job_id,
        record.step_id,
        "review.source",
        &value,
        &EventStamp::new("test", ActorId::automation("review")),
        uuid::Uuid::from_u128(6),
    );
    run["metadata"]["review"]["source_binding"] =
        json!({"digest":source.digest,"event_id":source.event_id});
    assert!(
        boss_jobs::release_window::judge_source_window(
            &SourceReader(Some(source)),
            &car,
            &run,
            "1111111111111111111111111111111111111111",
            "2026-10-23T02:00:00Z"
        )
        .await
        .is_ok()
    );
}
