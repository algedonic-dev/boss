//! The receipt says how full the gate's Postgres data volume got and what
//! the gate container's memory did — as EVIDENCE, and the verdict is never
//! touched (backlog 9cd74fd5; review bc011e92, finding F1).
//!
//! The data directory is a memory volume with a size limit
//! (the_gate_postgres_data_lives_in_memory.rs). A limit nobody reads is an
//! argument, so the collector reads the volume at every check boundary and
//! from a watcher between them, and the receipt carries the peak, a `full`
//! mark, and the gate cgroup's memory peak and OOM-kill count.
//!
//! THE COLLECTOR DECIDES NOTHING. The first version of this car rewrote
//! `failed` to `refused` when the run failed beside a full volume or a
//! kill. That is co-occurrence, not cause: the collector does not know
//! which check failed or why, a car whose own tests fill the database or
//! eat the gate's memory earned its red, and every reader of `refused`
//! relaunches without bound and strikes nobody — so a deterministic red
//! would have been relaunched for ever. These tests pin the opposite:
//! whatever the evidence says, `merge` leaves the verdict gate.sh wrote.
//! An eviction or a kill of the whole pod leaves no receipt at all, and
//! the conductor already settles that `lost`.
//!
//! Every test names its own layout: a gate pod hands BOSS_GATE_PGDATA to
//! every process in it, these tests included.

use boss_testing::{repo_root, scratch_dir};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn helper() -> PathBuf {
    repo_root().join("infra/gate-runner/runtime-evidence.py")
}

/// The collector, with no data volume and the host's real cgroup unless
/// the caller says otherwise.
fn collector(args: &[&str]) -> Command {
    let mut cmd = Command::new("python3");
    cmd.arg(helper()).args(args).env_remove("BOSS_GATE_PGDATA");
    cmd
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn side(journal: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pgdata", journal.display()))
}

fn readings(journal: &Path, rows: &[(&str, u64, u64)]) {
    let text: String = rows
        .iter()
        .enumerate()
        .map(|(i, (label, used, capacity))| {
            json!({"label": label, "at": format!("2026-10-08T00:00:{i:02}+00:00"),
                   "used_bytes": used, "capacity_bytes": capacity})
            .to_string()
                + "\n"
        })
        .collect();
    std::fs::write(side(journal), text).unwrap();
}

fn report(journal: &Path) -> Value {
    let out = ok(collector(&["report", journal.to_str().unwrap()])
        .output()
        .unwrap());
    serde_json::from_slice(&out.stdout).unwrap()
}

fn merged(journal: &Path, receipt: &Path, verdict: &str) -> Value {
    std::fs::write(
        receipt,
        json!({"verdict": verdict, "fails": ["original failure"]}).to_string(),
    )
    .unwrap();
    ok(collector(&[
        "merge",
        journal.to_str().unwrap(),
        receipt.to_str().unwrap(),
    ])
    .output()
    .unwrap());
    serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap()
}

const GIB: u64 = 1 << 30;

/// A sample reads the volume it is pointed at, and the report carries the
/// PEAK over every reading — not the last one: scratch databases are
/// dropped again, so a volume that was nearly full reads small at the end.
#[test]
fn a_sample_reads_the_data_volume_and_the_report_carries_its_peak() {
    let dir = scratch_dir("pgdata-evidence-sample");
    let journal = dir.join("samples.jsonl");
    let volume = dir.join("volume");
    std::fs::create_dir_all(&volume).unwrap();
    ok(
        collector(&["sample", journal.to_str().unwrap(), "runner-start"])
            .env("BOSS_GATE_PGDATA", &volume)
            .output()
            .unwrap(),
    );
    let text = std::fs::read_to_string(side(&journal)).expect("the sample wrote a reading");
    let row: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(row["label"], "runner-start");
    assert!(row["capacity_bytes"].as_u64().unwrap() > 0, "{row}");
    assert!(row["used_bytes"].as_u64().unwrap() <= row["capacity_bytes"].as_u64().unwrap());

    readings(
        &journal,
        &[
            ("check-start:test", GIB / 10, 6 * GIB),
            ("watch", 2 * GIB, 6 * GIB),
            ("check-finish:test", GIB / 5, 6 * GIB),
        ],
    );
    let pg = &report(&journal)["pgdata"];
    assert_eq!(pg["state"], "measured", "{pg}");
    assert_eq!(pg["peak_bytes"], 2 * GIB);
    assert_eq!(pg["peak_label"], "watch");
    assert_eq!(pg["last_bytes"], GIB / 5);
    assert_eq!(pg["capacity_bytes"], 6 * GIB);
    assert_eq!(pg["readings"], 3);
    assert_eq!(pg["watch_readings"], 1);
    assert_eq!(pg["full"], false);
}

/// ABSENT IS NOT ZERO. A runner rendered from an older manifest mounts no
/// data volume; its receipt must say nothing was read, never "0 bytes".
#[test]
fn a_gate_with_no_data_volume_says_unavailable_never_zero() {
    let dir = scratch_dir("pgdata-evidence-absent");
    let journal = dir.join("samples.jsonl");
    ok(
        collector(&["sample", journal.to_str().unwrap(), "runner-start"])
            .output()
            .unwrap(),
    );
    assert!(!side(&journal).exists(), "no volume declared, no reading");
    let pg = &report(&journal)["pgdata"];
    assert_eq!(pg["state"], "unavailable", "{pg}");
    assert!(pg["peak_bytes"].is_null(), "{pg}");
    assert!(pg["reason"].as_str().unwrap().contains("BOSS_GATE_PGDATA"));

    // A volume that cannot be read is recorded as that, and is not a peak.
    ok(
        collector(&["sample", journal.to_str().unwrap(), "check-start:test"])
            .env("BOSS_GATE_PGDATA", dir.join("not-mounted"))
            .output()
            .unwrap(),
    );
    let pg = &report(&journal)["pgdata"];
    assert_eq!(pg["state"], "unavailable", "{pg}");
    assert!(pg["peak_bytes"].is_null(), "{pg}");
    assert_eq!(pg["errors"], 1);

    // Boundary readings alone are a LOWER bound on the peak, and say so.
    readings(&journal, &[("check-start:test", GIB, 6 * GIB)]);
    let pg = &report(&journal)["pgdata"];
    assert_eq!(pg["state"], "partial", "{pg}");
    assert_eq!(pg["peak_bytes"], GIB);
    assert_eq!(pg["watch_readings"], 0);
}

/// Everything `merge` may add to a receipt: one key. A verdict rewrite
/// would have to add `refused_because` or change `verdict`; both are read
/// here on every receipt these tests merge.
fn assert_only_evidence_was_added(value: &Value, verdict: &str, what: &str) {
    assert_eq!(
        value["verdict"], verdict,
        "{what}: the collector's merge changed the verdict gate.sh wrote. It holds \
         co-occurrence, not cause — which check failed is not something it reads — and a \
         `refused` is relaunched without bound and strikes nobody: {value}"
    );
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["fails", "runtime_evidence", "verdict"],
        "{what}: merge adds runtime_evidence and nothing else: {value}"
    );
    assert_eq!(value["fails"][0], "original failure", "{what}");
    assert!(
        value["runtime_evidence"]["reclassified"].is_null(),
        "{what}: {value}"
    );
}

/// A FAILED RUN BESIDE A FULL VOLUME STAYS FAILED, and the receipt shows
/// the volume was full. Green stays green beside the same volume.
#[test]
fn a_gate_that_failed_beside_a_full_data_volume_stays_failed_and_the_receipt_shows_it() {
    let dir = scratch_dir("pgdata-evidence-full");
    let journal = dir.join("samples.jsonl");
    ok(
        collector(&["sample", journal.to_str().unwrap(), "runner-finish"])
            .output()
            .unwrap(),
    );
    let receipt = dir.join("receipt.json");

    // Full at the PEAK, an hour before the end: scratch databases are
    // dropped again, so the last reading has room.
    readings(
        &journal,
        &[
            ("watch", 6 * GIB - 4096, 6 * GIB),
            ("check-finish:test", GIB, 6 * GIB),
        ],
    );
    for verdict in ["failed", "green", "refused", "lost"] {
        let value = merged(&journal, &receipt, verdict);
        assert_only_evidence_was_added(&value, verdict, "full volume");
        let pg = &value["runtime_evidence"]["pgdata"];
        assert_eq!(pg["full"], true, "{verdict}: {pg}");
        assert_eq!(pg["peak_bytes"], 6 * GIB - 4096);
        assert_eq!(pg["capacity_bytes"], 6 * GIB);
        assert_eq!(pg["last_bytes"], GIB);
    }

    readings(&journal, &[("watch", 2 * GIB, 6 * GIB)]);
    let value = merged(&journal, &receipt, "failed");
    assert_only_evidence_was_added(&value, "failed", "volume with room");
    assert_eq!(value["runtime_evidence"]["pgdata"]["full"], false);
}

/// `full` IS 95% OF THE VOLUME, AT THE PEAK. The mark is only evidence,
/// and evidence a reader acts on has to mean one thing: 94% is not full
/// (so the threshold is not lower), 96% is (so it is not higher), and a
/// volume that was full and emptied again is still marked.
#[test]
fn the_full_mark_is_ninety_five_percent_of_the_volume_at_its_peak() {
    let dir = scratch_dir("pgdata-evidence-threshold");
    let journal = dir.join("samples.jsonl");
    // A size 95% of which is a whole number of bytes.
    let capacity = 6_000_000_000u64;
    for (percent, full) in [(50u64, false), (94, false), (95, true), (96, true)] {
        readings(
            &journal,
            &[
                ("watch", capacity / 100 * percent, capacity),
                ("check-finish:test", GIB / 10, capacity),
            ],
        );
        let pg = &report(&journal)["pgdata"];
        assert_eq!(
            pg["full"], full,
            "a peak at {percent}% of the volume: runtime-evidence.py marks `full` at 95% \
             (PGDATA_FULL): {pg}"
        );
    }
}

fn cgroup(dir: &Path, oom_kill: u64, peak: u64) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("cpu.stat"), "usage_usec 100\n").unwrap();
    std::fs::write(dir.join("memory.peak"), format!("{peak}\n")).unwrap();
    std::fs::write(
        dir.join("memory.events"),
        format!("low 0\nhigh 0\nmax 3\noom 1\noom_kill {oom_kill}\noom_group_kill 0\n"),
    )
    .unwrap();
}

fn sampled(journal: &Path, cg: &Path, label: &str, kills: u64, peak: u64) {
    cgroup(cg, kills, peak);
    ok(collector(&["sample", journal.to_str().unwrap(), label])
        .env("BOSS_RUNTIME_CGROUP_ROOT", cg)
        .env("GATE_MEMORY_LIMIT", (32 * GIB).to_string())
        .output()
        .unwrap());
}

/// A FAILED RUN BESIDE AN OOM KILL STAYS FAILED, and the receipt counts
/// the kill: `oom_kill` in the gate cgroup's memory.events, as a DELTA
/// over the run. The memory summary is on every receipt whatever the
/// verdict, because the receipt's samples are the first thing its byte
/// bound drops. (On a live gate the kubelet sets memory.oom.group, so a
/// kill in the gate container takes run.sh with it and there is no
/// receipt: this count is evidence a receipt will rarely carry.)
#[test]
fn a_gate_that_failed_beside_an_oom_kill_stays_failed_and_the_receipt_counts_it() {
    let dir = scratch_dir("pgdata-evidence-oom");
    let cg = dir.join("cgroup");
    let receipt = dir.join("receipt.json");
    for (name, kills, verdict) in [
        ("killed", [2u64, 3], "failed"),
        ("quiet", [2, 2], "failed"),
        ("killed-green", [0, 1], "green"),
    ] {
        let journal = dir.join(format!("{name}.jsonl"));
        sampled(&journal, &cg, "runner-start", kills[0], 3 * GIB);
        sampled(&journal, &cg, "runner-finish", kills[1], 14 * GIB);
        let value = merged(&journal, &receipt, verdict);
        assert_only_evidence_was_added(&value, verdict, name);
        let memory = &value["runtime_evidence"]["memory"];
        assert_eq!(memory["state"], "measured", "{name}: {memory}");
        assert_eq!(memory["peak_bytes"], 14 * GIB);
        assert_eq!(memory["limit_bytes"], 32 * GIB);
        assert_eq!(memory["oom_kill"], kills[1] - kills[0]);
    }
    // One sample is no interval: nothing is counted.
    let journal = dir.join("one.jsonl");
    sampled(&journal, &cg, "runner-finish", 5, GIB);
    let value = merged(&journal, &receipt, "failed");
    assert_only_evidence_was_added(&value, "failed", "one sample");
    assert!(value["runtime_evidence"]["memory"]["oom_kill"].is_null());
}

/// A COUNT ACROSS TWO CONTAINER LIFETIMES IS NOT A COUNT. memory.events
/// belongs to one cgroup; if the first and last sample read different
/// ones (a restarted container: another cgroup directory), their
/// difference is two unrelated numbers. The receipt says null, not 1.
#[test]
fn an_oom_count_across_a_container_restart_is_absent_never_a_number() {
    let dir = scratch_dir("pgdata-evidence-lifetime");
    let journal = dir.join("samples.jsonl");
    let (before, after) = (dir.join("cgroup-before"), dir.join("cgroup-after"));
    sampled(&journal, &before, "runner-start", 2, 3 * GIB);
    sampled(&journal, &after, "runner-finish", 3, 14 * GIB);
    let memory = &report(&journal)["memory"];
    assert_eq!(memory["state"], "measured", "{memory}");
    assert_eq!(memory["peak_bytes"], 14 * GIB);
    assert!(
        memory["oom_kill"].is_null(),
        "the first and last sample read two different cgroups (a restart); 3 - 2 is not a \
         kill count for either: {memory}"
    );
}

/// THE WATCHER READS BETWEEN BOUNDARIES AND ENDS BY ITSELF. It is started
/// in the background by run.sh, so it must stop on a count (here) or when
/// its parent is gone, and must never need a signal: the gate image has
/// no `kill`.
#[test]
fn the_watcher_reads_until_its_bound_and_stops() {
    let dir = scratch_dir("pgdata-evidence-watch");
    let journal = dir.join("samples.jsonl");
    let volume = dir.join("volume");
    std::fs::create_dir_all(&volume).unwrap();
    ok(collector(&["watch", journal.to_str().unwrap(), "0", "3"])
        .env("BOSS_GATE_PGDATA", &volume)
        .output()
        .unwrap());
    let text = std::fs::read_to_string(side(&journal)).unwrap();
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.lines().all(|l| l.contains("\"label\":\"watch\"")));
    // With no volume declared it has nothing to watch: it exits at once,
    // takes no reading, and SAYS so where the receipt reads it.
    let quiet = dir.join("quiet.jsonl");
    ok(collector(&["watch", quiet.to_str().unwrap(), "0", "3"])
        .output()
        .unwrap());
    assert!(!side(&quiet).exists());
    let said = std::fs::read_to_string(watch_log(&quiet)).unwrap();
    assert!(said.contains("watcher not started"), "{said}");
}

fn watch_log(journal: &Path) -> PathBuf {
    PathBuf::from(format!("{}.watch", journal.display()))
}

/// A DEAD WATCHER DOES NOT LOOK LIKE ONE THAT NEVER STARTED (review
/// bc011e92, F5). run.sh used to launch it with `> /dev/null 2>&1`. Now
/// it writes its own words beside the journal — the start, its FIRST
/// failed reading, never a line per reading — and the receipt carries
/// them, so `watch_readings: 0` comes with the reason.
#[test]
fn the_receipt_tells_a_watcher_that_never_started_from_one_that_failed() {
    let dir = scratch_dir("pgdata-evidence-watcher");
    let volume = dir.join("volume");
    std::fs::create_dir_all(&volume).unwrap();

    // Never started: boundary readings only, no watcher file.
    let never = dir.join("never.jsonl");
    ok(
        collector(&["sample", never.to_str().unwrap(), "runner-start"])
            .env("BOSS_GATE_PGDATA", &volume)
            .output()
            .unwrap(),
    );
    let pg = &report(&never)["pgdata"];
    assert_eq!(pg["state"], "partial", "{pg}");
    assert_eq!(pg["watch_readings"], 0);
    assert_eq!(pg["watcher"]["state"], "not started", "{pg}");
    assert!(
        pg["note"].as_str().unwrap().contains("No watcher started"),
        "{pg}"
    );

    // Started and read: one start line however many readings.
    let read = dir.join("read.jsonl");
    let out = ok(collector(&["watch", read.to_str().unwrap(), "0", "5"])
        .env("BOSS_GATE_PGDATA", &volume)
        .output()
        .unwrap());
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "the watcher writes to its own file, not to the runner's log"
    );
    let said = std::fs::read_to_string(watch_log(&read)).unwrap();
    assert_eq!(
        said.lines().count(),
        1,
        "one line for five readings: {said}"
    );
    assert!(said.contains("watcher started:"), "{said}");
    let pg = &report(&read)["pgdata"];
    assert_eq!(pg["state"], "measured", "{pg}");
    assert_eq!(pg["watch_readings"], 5);
    assert_eq!(pg["watcher"]["state"], "started");
    assert!(pg["note"].is_null(), "{pg}");

    // Started and every reading failed (the mount is gone): the start
    // line, ONE failure line for five failed readings, and the receipt
    // says partial with the watcher's own words. Zero stays zero.
    let failed = dir.join("failed.jsonl");
    ok(
        collector(&["sample", failed.to_str().unwrap(), "runner-start"])
            .env("BOSS_GATE_PGDATA", &volume)
            .output()
            .unwrap(),
    );
    ok(collector(&["watch", failed.to_str().unwrap(), "0", "5"])
        .env("BOSS_GATE_PGDATA", dir.join("not-mounted"))
        .output()
        .unwrap());
    let said = std::fs::read_to_string(watch_log(&failed)).unwrap();
    assert_eq!(said.lines().count(), 2, "{said}");
    assert!(
        said.lines().nth(1).unwrap().contains("reading 1 failed"),
        "{said}"
    );
    let pg = &report(&failed)["pgdata"];
    assert_eq!(pg["state"], "partial", "{pg}");
    assert_eq!(pg["watch_readings"], 0, "a failed reading is not a reading");
    assert_eq!(pg["errors"], 5);
    assert_eq!(pg["watcher"]["state"], "started");
    let note = pg["note"].as_str().unwrap();
    assert!(
        note.contains("The watcher started and recorded no reading") && note.contains("failed"),
        "{note}"
    );
}
