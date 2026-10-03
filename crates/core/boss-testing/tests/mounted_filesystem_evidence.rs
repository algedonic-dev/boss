//! Run the mounted recorder and idle reader through their actual shell/HTTP
//! ports. A native completed backup is evidence; du, a simulated packet or
//! an older successful attempt cannot substitute for a mounted statfs.
use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const VOLUME: &str = "pvc-98ccf090-32f2-415a-ac79-029cfe41456e";
const GIB: i64 = 1 << 30;

fn dated(offset: i64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("{offset} seconds"),
            "+%Y-%m-%dT%H:%M:%SZ",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Door {
    url: String,
    calls: Arc<Mutex<Vec<(String, String, String)>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Door {
    fn new(body: Value, status: u16) -> Self {
        Self::raw(body.to_string(), status)
    }
    fn raw(body: String, status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let saved = calls.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let (stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("accept: {e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut headers = String::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    headers.push_str(&line);
                }
                let mut data = vec![0; length];
                reader.read_exact(&mut data).unwrap();
                saved
                    .lock()
                    .unwrap()
                    .push((first, headers, String::from_utf8(data).unwrap()));
                let response = &body;
                write!(reader.get_mut(), "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self {
            url,
            calls,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Door {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn sample() -> Value {
    json!({"schema": 1, "namespace": "boss", "claim": "boss-backups", "volume": VOLUME,
        "mount": "/backup", "mount_source": format!("/dev/longhorn/{VOLUME}"),
        "pod_uid": "pod-uid", "pod": "boss-pg-backup-123-abc", "node": "cp-1",
        "kubernetes_job_uid": "job-uid", "position": "final", "observed_at": dated(-600),
        "capacity_bytes": 20*GIB, "used_bytes": 3*GIB, "free_bytes": 16*GIB})
}
fn native(sample: Value) -> Value {
    json!({"id":"native-job", "kind":"maintenance-backup", "status":"closed", "partition":"real", "simulated":false,
        "opened_at":dated(-1200), "steps":[{"id":"run-step", "job_id":"native-job", "spec_slug":"run", "kind":"task",
        "status":"completed", "assignee_id":"automation:boss-step", "completed_by":"automation:boss-step",
        "completed_at":dated(-500), "metadata":{"result":"ok", "filesystem_sample":sample}}]})
}
fn attempt() -> Value {
    json!({"metadata":{"uid":"job-uid", "namespace":"boss", "name":"boss-pg-backup-123", "creationTimestamp":dated(-1200),
        "ownerReferences":[{"kind":"CronJob", "name":"boss-pg-backup", "controller":true}]},
        "status":{"succeeded":1,"failed":0,"active":0,"completionTime":dated(-400),"conditions":[{"type":"Complete","status":"True"}]}})
}

struct World {
    dir: PathBuf,
    bin: PathBuf,
}
impl World {
    fn new(tag: &str) -> Self {
        let dir = scratch_dir(tag);
        let bin = dir.join("bin");
        create_dir(&bin);
        create_dir(&dir.join("lib"));
        create_dir(&dir.join("no-token"));
        create_dir(&dir.join("tmp"));
        write_file(&dir.join("sources.json"), &json!([{"namespace":"boss", "claim":"boss-backups", "workflow":"maintenance-backup", "step":"run", "cronjob":"boss-pg-backup", "actor":"automation:boss-step"}]).to_string());
        write_file(
            &dir.join("jobs.json"),
            &json!({"items":[attempt()]}).to_string(),
        );
        write_file(
            &dir.join("mountinfo"),
            &format!("42 35 8:1 / /backup rw,relatime - ext4 /dev/longhorn/{VOLUME} rw\n"),
        );
        write_file(&dir.join("stat.txt"), "4096 5242880 4456448 4194304\n");
        write_exec(
            &bin.join("sudo"),
            r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_DIR/kube.calls"
case " $* " in
  *' get jobs -n boss '*) cat "$STUB_DIR/jobs.json"; exit "${KUBE_STATUS:-0}" ;;
esac
echo "unexpected kubernetes call: $*" >&2
exit 99
"#,
        );
        write_exec(
            &bin.join("stat"),
            r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_DIR/stat.calls"
cat "$STUB_DIR/stat.txt"
exit "${STAT_STATUS:-0}"
"#,
        );
        for script in ["boss-step.sh", "boss-api-curl.sh"] {
            std::fs::copy(repo_root().join("infra").join(script), dir.join(script)).unwrap();
        }
        for file in ["secret-header.sh", "curl-through-a-roll.sh", "jq.sh"] {
            std::fs::copy(
                repo_root().join("infra/lib").join(file),
                dir.join("lib").join(file),
            )
            .unwrap();
        }
        Self { dir, bin }
    }
    fn run(&self, script: &str, args: &[&str], url: &str, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new("bash");
        c.arg(repo_root().join(script))
            .args(args)
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("STUB_DIR", &self.dir)
            .env("TMPDIR", self.dir.join("tmp"))
            .env("BOSS_VOLUME_SAMPLE_SOURCES", self.dir.join("sources.json"))
            .env("BOSS_MACHINE_TOKEN_DIR", self.dir.join("no-token"))
            .env("BOSS_MACHINE_TOKEN_HOSTS", "")
            .env("BOSS_OPS_DIR", self.dir.join("ops"))
            .env("BOSS_FORGE_REGISTRY_HOST", "reg.test")
            .env("JOBS_API", url)
            .env("BOSS_JOBS_URL", url)
            .env("KUBERNETES_SERVICE_HOST", "fixture-only")
            .env("BOSS_FS_MOUNTINFO", self.dir.join("mountinfo"))
            .env("BOSS_FS_POD_UID", "pod-uid")
            .env("BOSS_FS_POD", "boss-pg-backup-123-abc")
            .env("BOSS_FS_NAMESPACE", "boss")
            .env("BOSS_FS_NODE", "cp-1")
            .env("BOSS_FS_JOB_UID", "job-uid")
            .env("BOSS_FS_STEP_SCRIPT", self.dir.join("boss-step.sh"))
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .env_remove("BOSS_STEP_DRY_RUN")
            .env_remove("BOSS_STEP_OUTPUT_FILE")
            .env_remove("BOSS_STEP_ACTOR")
            .env_remove("SERVICE_RESULT")
            .env_remove("HOST_ID")
            .env_remove("BOSS_NODE_ID");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }
    fn reader(&self, jobs: Value, status: u16) -> (Value, Door) {
        let door = Door::new(jobs, status);
        let out = self.run(
            "infra/estate/read-volume-sample.sh",
            &["boss", "boss-backups", VOLUME],
            &door.url,
            &[],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
        (value, door)
    }
}

#[test]
fn a_dated_final_native_sample_survives_pod_gc_and_keeps_its_provenance() {
    let w = World::new("mounted-reuse");
    let s = sample();
    let (v, door) = w.reader(json!({"total":1,"data":[native(s.clone())]}), 200);
    assert_eq!(v["capacity_bytes"], 20 * GIB, "{v}");
    assert_eq!(v["free_bytes"], 16 * GIB, "{v}");
    assert_eq!(v["sample"]["observed_at"], s["observed_at"], "{v}");
    assert_eq!(v["sample"]["pod_uid"], "pod-uid");
    assert_eq!(v["native_job_id"], "native-job");
    assert_eq!(v["native_step_id"], "run-step");
    assert_eq!(
        v["max_age_seconds"], 86400,
        "derive the existing daily cadence, {v}"
    );
    assert!(v.get("unread").is_none(), "{v}");
    let calls = std::fs::read_to_string(w.dir.join("kube.calls")).unwrap();
    assert!(
        !calls.contains(" get pods "),
        "the completed Pod need not survive: {calls}"
    );
    assert!(
        calls.contains(" get jobs -n boss ") && calls.contains("--request-timeout=20s"),
        "{calls}"
    );
    let requests = door.calls.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.starts_with("GET /api/jobs?"));
    assert!(requests[0].0.contains("partition=real") && requests[0].0.contains("full=true"));
    assert!(requests[0].1.contains("automation:estate-observer-host"));
}

#[test]
fn low_or_zero_available_space_is_still_a_measurement_for_the_existing_floor() {
    for free in [0, GIB] {
        let w = World::new(&format!("mounted-low-{free}"));
        let mut s = sample();
        s["free_bytes"] = json!(free);
        let (v, _) = w.reader(json!({"total":1,"data":[native(s)]}), 200);
        assert_eq!(v["free_bytes"], free, "{v}");
        assert!(
            v.get("tight").is_none() && v.get("floor_bytes").is_none(),
            "only compare_volumes judges the floor: {v}"
        );
    }
}

#[test]
fn stale_wrong_identity_simulated_failed_and_malformed_samples_stay_blind() {
    let mut cases: Vec<(&str, Value)> = Vec::new();
    for (field, value) in [
        ("namespace", json!("other")),
        ("claim", json!("other")),
        ("volume", json!("pvc-recreated")),
        ("kubernetes_job_uid", json!("older-job")),
        ("pod_uid", json!("")),
        ("position", json!("start")),
        ("observed_at", json!("2000-01-01T00:00:00Z")),
        ("observed_at", json!(dated(60))),
        ("observed_at", json!("bad date")),
        ("capacity_bytes", json!(0)),
        ("capacity_bytes", json!(-1)),
        ("capacity_bytes", json!(1.5)),
        ("free_bytes", json!(-1)),
        ("free_bytes", json!(21 * GIB)),
        ("free_bytes", json!("100")),
        ("used_bytes", json!(null)),
        ("unread", json!("statfs failed")),
    ] {
        let mut s = sample();
        s[field] = value;
        cases.push((field, native(s)));
    }
    for (field, value) in [
        ("partition", json!("simulated")),
        ("simulated", json!(true)),
        ("kind", json!("other")),
    ] {
        let mut j = native(sample());
        j[field] = value;
        cases.push((field, j));
    }
    for (field, value) in [
        ("completed_by", json!("another-actor")),
        ("assignee_id", json!("another-actor")),
        ("status", json!("active")),
        ("completed_at", json!(dated(-700))),
        ("job_id", json!("different-job")),
        ("completed_at", json!(null)),
        ("spec_slug", json!("other")),
    ] {
        let mut j = native(sample());
        j["steps"][0][field] = value;
        cases.push((field, j));
    }
    for (index, (field, j)) in cases.into_iter().enumerate() {
        let w = World::new(&format!("mounted-bad-{index}"));
        let (v, _) = w.reader(json!({"total":1,"data":[j]}), 200);
        assert!(
            v["free_bytes"].is_null() && v["capacity_bytes"].is_null(),
            "{field}: {v}"
        );
        assert!(
            v["unread"].as_str().is_some_and(|s| !s.is_empty()),
            "{field}: {v}"
        );
    }
}

#[test]
fn a_newer_missing_or_failed_attempt_invalidates_an_older_positive_sample() {
    for missing in [true, false] {
        let w = World::new(&format!("mounted-new-{missing}"));
        let mut newer = native(sample());
        newer["id"] = json!("newer-native");
        newer["opened_at"] = json!(dated(-400));
        newer["steps"][0]["job_id"] = json!("newer-native");
        if missing {
            newer["steps"][0]["metadata"] = json!({});
        } else {
            newer["steps"][0]["metadata"]["filesystem_sample"]["unread"] = json!("new stat failed");
        }
        let (v, _) = w.reader(json!({"total":2,"data":[native(sample()),newer]}), 200);
        assert!(
            v["free_bytes"].is_null(),
            "must not search backward for the first good sample: {v}"
        );
    }
    let w = World::new("mounted-unrecorded-attempt");
    let mut newer = attempt();
    newer["metadata"]["uid"] = json!("unrecorded-job");
    newer["metadata"]["creationTimestamp"] = json!(dated(-300));
    write_file(
        &w.dir.join("jobs.json"),
        &json!({"items":[attempt(),newer]}).to_string(),
    );
    let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
    assert!(
        v["free_bytes"].is_null(),
        "a failed recording must not preserve old health: {v}"
    );
}

#[test]
fn fractional_native_attempts_and_aliases_cannot_keep_superseded_figures() {
    let second = dated(-1200).trim_end_matches('Z').to_owned();
    for (old_fraction, new_fraction) in [
        (".100Z", ".900Z"),
        (".123456788Z", ".123456789Z"),
        (".100Z", ".1Z"),
        ("Z", ".000Z"),
    ] {
        for reverse in [false, true] {
            let w = World::new("mounted-native-fraction");
            let mut older = native(sample());
            older["opened_at"] = json!(format!("{second}{old_fraction}"));
            let mut newer = older.clone();
            newer["id"] = json!("newer-native");
            newer["opened_at"] = json!(format!("{second}{new_fraction}"));
            newer["steps"][0]["job_id"] = json!("newer-native");
            newer["steps"][0]["metadata"] = json!({});
            let jobs = if reverse {
                json!([older, newer])
            } else {
                json!([newer, older])
            };
            let (v, _) = w.reader(json!({"total":2,"data":jobs}), 200);
            assert!(
                v["free_bytes"].is_null() && v["unread"].is_string(),
                "{old_fraction}/{new_fraction}, reverse={reverse}: {v}"
            );
        }
    }
    // Distinct fractional instants remain usable when the newest attempt is
    // the one that actually owns the successful measurement.
    let w = World::new("mounted-native-fraction-positive");
    let mut older = native(sample());
    older["opened_at"] = json!(format!("{second}.100Z"));
    older["steps"][0]["metadata"] = json!({});
    let mut newer = native(sample());
    newer["opened_at"] = json!(format!("{second}.900Z"));
    newer["id"] = json!("newer-native");
    newer["steps"][0]["job_id"] = json!("newer-native");
    let (v, _) = w.reader(json!({"total":2,"data":[newer,older]}), 200);
    assert_eq!(v["free_bytes"], 16 * GIB, "{v}");
    assert_eq!(v["native_job_id"], "newer-native", "{v}");
}

#[test]
fn fractional_kubernetes_attempts_and_aliases_cannot_keep_old_health() {
    let second = dated(-1200).trim_end_matches('Z').to_owned();
    for (old_fraction, new_fraction) in [
        (".100Z", ".900Z"),
        (".123456788Z", ".123456789Z"),
        (".100Z", ".1Z"),
        ("Z", ".000Z"),
    ] {
        for reverse in [false, true] {
            let w = World::new("mounted-kube-fraction");
            let mut older = attempt();
            older["metadata"]["creationTimestamp"] = json!(format!("{second}{old_fraction}"));
            let mut newer = older.clone();
            newer["metadata"]["uid"] = json!("unrecorded-job");
            newer["metadata"]["creationTimestamp"] = json!(format!("{second}{new_fraction}"));
            let attempts = if reverse {
                json!([older, newer])
            } else {
                json!([newer, older])
            };
            write_file(
                &w.dir.join("jobs.json"),
                &json!({"items":attempts}).to_string(),
            );
            let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
            assert!(
                v["free_bytes"].is_null() && v["unread"].is_string(),
                "{old_fraction}/{new_fraction}, reverse={reverse}: {v}"
            );
        }
    }
    let w = World::new("mounted-kube-fraction-positive");
    let mut older = attempt();
    older["metadata"]["uid"] = json!("old-job");
    older["metadata"]["creationTimestamp"] = json!(format!("{second}.100Z"));
    let mut latest = attempt();
    latest["metadata"]["creationTimestamp"] = json!(format!("{second}.900Z"));
    write_file(
        &w.dir.join("jobs.json"),
        &json!({"items":[latest,older]}).to_string(),
    );
    let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
    assert_eq!(v["free_bytes"], 16 * GIB, "{v}");
}

#[test]
fn controller_identity_requires_nonempty_strings_on_both_sides() {
    for value in [
        None,
        Some(json!(null)),
        Some(json!("")),
        Some(json!(42)),
        Some(json!([])),
        Some(json!({})),
        Some(json!(false)),
    ] {
        for side in ["both", "sample", "inventory"] {
            let w = World::new("mounted-controller-identity");
            let mut s = sample();
            let mut a = attempt();
            for (target, key) in [(&mut s, "kubernetes_job_uid"), (&mut a["metadata"], "uid")] {
                if side == "both" || (side == "sample") == (key == "kubernetes_job_uid") {
                    if let Some(value) = &value {
                        target[key] = value.clone();
                    } else {
                        target.as_object_mut().unwrap().remove(key);
                    }
                }
            }
            write_file(&w.dir.join("jobs.json"), &json!({"items":[a]}).to_string());
            let (v, _) = w.reader(json!({"total":1,"data":[native(s)]}), 200);
            assert!(
                v["free_bytes"].is_null() && v["unread"].is_string(),
                "{side}, {value:?}: {v}"
            );
        }
    }
}

#[test]
fn fractional_start_and_completion_bounds_cannot_be_rounded_into_consistency() {
    let second = dated(-600).trim_end_matches('Z').to_owned();
    for boundary in ["native-start", "kube-start", "native-finish", "kube-finish"] {
        let w = World::new("mounted-fractional-bounds");
        let mut s = sample();
        let start = boundary.ends_with("start");
        s["observed_at"] = json!(format!("{second}.{}Z", if start { "100" } else { "900" }));
        let mut j = native(s);
        let mut a = attempt();
        let date = json!(format!("{second}.{}Z", if start { "900" } else { "100" }));
        match boundary {
            "native-start" => j["opened_at"] = date,
            "kube-start" => a["metadata"]["creationTimestamp"] = date,
            "native-finish" => j["steps"][0]["completed_at"] = date,
            "kube-finish" => a["status"]["completionTime"] = date,
            _ => unreachable!(),
        }
        write_file(&w.dir.join("jobs.json"), &json!({"items":[a]}).to_string());
        let (v, _) = w.reader(json!({"total":1,"data":[j]}), 200);
        assert!(
            v["free_bytes"].is_null() && v["unread"].is_string(),
            "{boundary}: {v}"
        );
    }
}

#[test]
fn absent_denied_truncated_and_failed_inventory_reads_stay_blind() {
    for (tag, body, status) in [
        ("missing", json!({"total":0,"data":[]}), 200),
        ("denied", json!({"error":"forbidden"}), 403),
        ("dark", json!({}), 503),
        (
            "truncated",
            json!({"total":2,"data":[native(sample())]}),
            200,
        ),
        (
            "malformed",
            json!({"total":"1","data":[native(sample())]}),
            200,
        ),
    ] {
        let w = World::new(&format!("mounted-{tag}"));
        let (v, _) = w.reader(body, status);
        assert!(
            v["free_bytes"].is_null() && v["unread"].is_string(),
            "{tag}: {v}"
        );
    }
    let w = World::new("mounted-no-kube-jobs");
    write_file(&w.dir.join("jobs.json"), "{\"items\":[]}");
    let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
    assert!(
        v["free_bytes"].is_null(),
        "no source identity is BLIND: {v}"
    );
}

#[test]
fn job_status_preserves_retry_and_unrecorded_failure_boundaries_after_pod_gc() {
    for (tag, status) in [
        ("running", json!({"succeeded":0,"active":1})),
        (
            "failed",
            json!({"failed":1,"conditions":[{"type":"Failed","status":"True"}]}),
        ),
        (
            "retried",
            json!({"succeeded":1,"failed":1,"completionTime":dated(-400),"conditions":[{"type":"Complete","status":"True"}]}),
        ),
        ("unknown", json!({})),
        (
            "completion-before-sample",
            json!({"succeeded":1,"completionTime":dated(-700),"conditions":[{"type":"Complete","status":"True"}]}),
        ),
    ] {
        let w = World::new(&format!("mounted-kube-{tag}"));
        let mut a = attempt();
        a["status"] = status;
        write_file(&w.dir.join("jobs.json"), &json!({"items":[a]}).to_string());
        let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
        assert!(
            v["free_bytes"].is_null(),
            "{tag} cannot keep old health without a single completed attempt: {v}"
        );
    }
}

#[test]
fn recorder_samples_the_exact_mount_and_preserves_capture_errors() {
    let w = World::new("mounted-capture");
    let out = w.run(
        "infra/boss-filesystem-sample.sh",
        &[
            "--print",
            "maintenance-backup",
            "run",
            "boss-backups",
            "/backup",
            "final",
        ],
        "http://unused",
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary: Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = &summary["filesystem_sample"];
    assert_eq!(s["volume"], VOLUME);
    assert_eq!(s["capacity_bytes"], 20 * GIB);
    assert_eq!(s["free_bytes"], 16 * GIB);
    assert_eq!(s["used_bytes"], 3 * GIB);
    assert_eq!(s["kubernetes_job_uid"], "job-uid");
    assert!(s["observed_at"].is_string());
    assert!(
        std::fs::read_to_string(w.dir.join("stat.calls"))
            .unwrap()
            .contains(" /backup")
    );
    for (tag, text, status) in [
        ("zero", "4096 0 0 0", "0"),
        ("zero-block", "000 10 5 5", "0"),
        ("malformed", "garbage", "0"),
        ("failed", "4096 5242880 4456448 4194304", "1"),
    ] {
        write_file(&w.dir.join("stat.txt"), text);
        let out = w.run(
            "infra/boss-filesystem-sample.sh",
            &[
                "--print",
                "maintenance-backup",
                "run",
                "boss-backups",
                "/backup",
                "final",
            ],
            "http://unused",
            &[("STAT_STATUS", status)],
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            v["filesystem_sample"]["free_bytes"].is_null()
                && v["filesystem_sample"]["unread"].is_string(),
            "{tag}: {v}"
        );
    }
    write_file(
        &w.dir.join("mountinfo"),
        "42 35 8:1 / /not-backup rw - ext4 /dev/root rw\n",
    );
    let out = w.run(
        "infra/boss-filesystem-sample.sh",
        &[
            "--print",
            "maintenance-backup",
            "run",
            "boss-backups",
            "/backup",
            "final",
        ],
        "http://unused",
        &[],
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["filesystem_sample"]["free_bytes"].is_null(),
        "a directory on root is not this mount: {v}"
    );
}

#[test]
fn the_generic_metadata_only_step_door_does_not_complete_the_backup() {
    let w = World::new("mounted-record-door");
    let mut job = native(sample());
    job["status"] = json!("open");
    job["steps"][0]["status"] = json!("active");
    job["steps"][0]["metadata"]["filesystem_sample"]["unread"] =
        json!("the earlier capture failed");
    let door = Door::new(json!({"total":1,"data":[job]}), 200);
    let out = w.run(
        "infra/boss-filesystem-sample.sh",
        &[
            "maintenance-backup",
            "run",
            "boss-backups",
            "/backup",
            "start",
        ],
        &door.url,
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = door.calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "GET then PATCH only: {calls:?}");
    assert!(
        calls[1]
            .0
            .starts_with("PATCH /api/jobs/native-job/steps/run-step/metadata ")
    );
    assert!(calls[1].1.contains("automation:boss-step"));
    let merged: Value = serde_json::from_str(&calls[1].2).unwrap();
    assert_eq!(merged["filesystem_sample"]["position"], "start");
    assert!(
        merged["filesystem_sample"]["unread"].is_null(),
        "a successful capture must replace the old failure in the deep merge: {merged}"
    );
    assert!(
        merged.get("result").is_none(),
        "capture is not the backup verdict: {merged}"
    );
}

#[test]
fn declared_cadence_is_the_expiry_bound_instead_of_a_copied_nightly_constant() {
    let w = World::new("mounted-cadence");
    let path = w.dir.join("cadence.toml");
    for (minutes, accepted) in [(30, true), (5, false)] {
        write_file(
            &path,
            &format!("args = {{ \"interval_minutes.maintenance-backup\" = \"{minutes}\" }}\n"),
        );
        let door = Door::new(json!({"total":1,"data":[native(sample())]}), 200);
        let out = w.run(
            "infra/estate/read-volume-sample.sh",
            &["boss", "boss-backups", VOLUME],
            &door.url,
            &[("BOSS_VOLUME_SAMPLE_CADENCE", path.to_str().unwrap())],
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["free_bytes"].is_number(), accepted, "{minutes}: {v}");
        if accepted {
            assert_eq!(v["max_age_seconds"], 1800);
        }
    }
}

#[test]
fn metadata_only_without_any_evidence_refuses_instead_of_saying_recorded() {
    let w = World::new("metadata-only-empty");
    let mut job = native(sample());
    job["status"] = json!("open");
    job["steps"][0]["status"] = json!("active");
    let door = Door::new(json!({"total":1,"data":[job]}), 200);
    let out = w.run(
        "infra/boss-step.sh",
        &["maintenance-backup", "run"],
        &door.url,
        &[("BOSS_STEP_METADATA_ONLY", "1")],
    );
    assert!(
        !out.status.success(),
        "a merge with no evidence cannot be recorded: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn metadata_only_is_a_real_merge_without_a_status_transition() {
    let w = World::new("metadata-only-port");
    let mut job = native(sample());
    job["status"] = json!("open");
    job["steps"][0]["status"] = json!("active");
    let door = Door::new(json!({"total":1,"data":[job]}), 200);
    write_file(
        &w.dir.join("summary.json"),
        &json!({"filesystem_sample":sample()}).to_string(),
    );
    let out = w.run(
        "infra/boss-step.sh",
        &["maintenance-backup", "run"],
        &door.url,
        &[
            ("BOSS_STEP_METADATA_ONLY", "1"),
            (
                "BOSS_RUN_SUMMARY_FILE",
                w.dir.join("summary.json").to_str().unwrap(),
            ),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = door.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        2,
        "metadata-only must never PUT completed: {calls:?}"
    );
    assert!(calls[1].0.starts_with("PATCH "));
}

#[test]
fn metadata_only_refuses_unowned_simulated_terminal_or_absent_packets() {
    for tag in [
        "simulated",
        "wrong-owner",
        "terminal",
        "missing",
        "truncated",
    ] {
        let w = World::new(&format!("metadata-only-{tag}"));
        let mut job = native(sample());
        job["status"] = json!("open");
        job["steps"][0]["status"] = json!("active");
        match tag {
            "simulated" => {
                job["partition"] = json!("simulated");
                job["simulated"] = json!(true);
            }
            "wrong-owner" => job["steps"][0]["assignee_id"] = json!("someone-else"),
            "terminal" => job["steps"][0]["status"] = json!("completed"),
            _ => {}
        }
        let body = if tag == "missing" {
            json!({"total":0,"data":[]})
        } else {
            json!({"total":if tag=="truncated" {2} else {1},"data":[job]})
        };
        let door = Door::new(body, 200);
        let out = w.run(
            "infra/boss-step.sh",
            &["maintenance-backup", "run", "capture=sample"],
            &door.url,
            &[("BOSS_STEP_METADATA_ONLY", "1")],
        );
        assert!(
            !out.status.success(),
            "{tag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            door.calls.lock().unwrap().len(),
            1,
            "{tag}: refuse before any write"
        );
    }
}

#[test]
fn duplicate_or_contradictory_native_http_documents_cannot_supply_a_sample() {
    let good = json!({"total":1,"data":[native(sample())]}).to_string();
    for (tag, second) in [
        ("duplicate", good.clone()),
        ("contradictory", json!({"total":0,"data":[]}).to_string()),
    ] {
        let w = World::new(&format!("mounted-two-native-{tag}"));
        let door = Door::raw(format!("{good}\n{second}\n"), 200);
        let out = w.run(
            "infra/estate/read-volume-sample.sh",
            &["boss", "boss-backups", VOLUME],
            &door.url,
            &[],
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(
            v["free_bytes"].is_null() && v["unread"].is_string(),
            "{tag}: first-document false success: {v}"
        );
    }
}

#[test]
fn duplicate_or_contradictory_inventory_and_registry_documents_are_blind() {
    for kind in ["inventory", "registry"] {
        for duplicate in [true, false] {
            let w = World::new(&format!("mounted-two-{kind}-{duplicate}"));
            let path = w.dir.join(if kind == "inventory" {
                "jobs.json"
            } else {
                "sources.json"
            });
            let good = std::fs::read_to_string(&path).unwrap();
            let bad = if duplicate {
                good.clone()
            } else if kind == "inventory" {
                json!({"items":[]}).to_string()
            } else {
                "[]".to_owned()
            };
            write_file(&path, &format!("{good}\n{bad}\n"));
            let (v, _) = w.reader(json!({"total":1,"data":[native(sample())]}), 200);
            assert!(
                v["free_bytes"].is_null() && v["unread"].is_string(),
                "{kind}/{duplicate}: {v}"
            );
        }
    }
}

#[test]
fn metadata_only_demands_one_whole_http_envelope_before_any_patch() {
    let mut job = native(sample());
    job["status"] = json!("open");
    job["steps"][0]["status"] = json!("active");
    let good = json!({"total":1,"data":[job]}).to_string();
    for (tag, body) in [
        ("duplicate", format!("{good}\n{good}\n")),
        (
            "contradictory",
            format!("{{\"total\":0,\"data\":[]}}\n{good}\n"),
        ),
    ] {
        let w = World::new(&format!("metadata-two-{tag}"));
        let door = Door::raw(body, 200);
        let out = w.run(
            "infra/boss-step.sh",
            &["maintenance-backup", "run", "capture=sample"],
            &door.url,
            &[("BOSS_STEP_METADATA_ONLY", "1")],
        );
        assert!(
            !out.status.success(),
            "{tag}: accepted a stream of envelopes"
        );
        assert_eq!(
            door.calls.lock().unwrap().len(),
            1,
            "{tag}: no PATCH from an ambiguous response"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("entire native open-job answer"),
            "{tag}: refuse the envelope explicitly rather than rely on downstream parsing: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
