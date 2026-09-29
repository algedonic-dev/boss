//! The gate copies its warm seed BEFORE it clones, and says so on the receipt.
//!
//! THE HAZARD (backlog 646c0ade, the gate-runner twin of the wt-cargo
//! false binary in 1150c518). `cp -a` keeps the seed's mtimes, and cargo
//! judges a path crate fresh when its dep-info is not older than its
//! sources. So a seeded artifact NEWER than a cloned source file is
//! trusted, and the run links the seeding run's build of that crate —
//! main's or a train's tree, not the branch under test: a false green.
//!
//! Until this car the copy ran AFTER `git clone`, and the order held only
//! by timing. Triage run 4814939a read 60 gate pods' logs on 2026-09-28:
//! a reader that clones and then blocks on the shared flock while a
//! refresh holds the exclusive one copies the NEW seed, whose newest
//! artifact was written at least one svelte-check (14 s at the fastest)
//! before the refresh; the reader's clone-to-flock gap reached 11.4 s.
//! Every run was clone-newer, by 2.6 s at the worst pairing — a race
//! won by svelte-check's duration.
//!
//! With the copy first, every seeded artifact was written before the
//! seed was installed, which is before the copy, which is before every
//! file the clone writes: clone-newer by construction. The first pin
//! reads the script's ORDER (not a comment); the other two hold the
//! receipt to carrying the measured order, so it is evidence on every
//! gate-run packet rather than a belief about one.

use boss_testing::{repo_root, scratch_dir};
use std::fs;
use std::path::Path;
use std::process::Command;

const RUN_SH: &str = "infra/gate-runner/run.sh";

fn run_sh() -> String {
    fs::read_to_string(repo_root().join(RUN_SH)).expect("read run.sh")
}

/// The lines run.sh EXECUTES, in order — comments are prose and may
/// mention the clone or the seed anywhere.
fn executed_lines() -> Vec<String> {
    run_sh()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn only_line(lines: &[String], what: &str, pred: impl Fn(&str) -> bool) -> usize {
    let hits: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| pred(l))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "run.sh must have exactly one {what}; found {}: {:?}",
        hits.len(),
        hits.iter().map(|&i| &lines[i]).collect::<Vec<_>>()
    );
    hits[0]
}

/// THE ORDER PIN. The seed copy is CALLED before the clone runs, and the
/// order is measured after the checkout (the last write to the sources).
#[test]
fn the_seed_copy_precedes_the_clone() {
    let lines = executed_lines();
    let seed_call = only_line(&lines, "seed_target call", |l| {
        l.contains("seed_target") && !l.contains("seed_target()")
    });
    let clone = only_line(&lines, "git clone", |l| {
        l.trim_start().starts_with("git clone")
    });
    let checkout = only_line(&lines, "branch checkout", |l| {
        l.trim_start().starts_with("git checkout -B")
    });
    let measure = only_line(&lines, "seed_order_json call", |l| {
        l.contains("seed_order_json") && !l.contains("seed_order_json()")
    });
    assert!(
        seed_call < clone,
        "the seed copy must run BEFORE `git clone` (line {seed_call} vs {clone} of the executed \
         lines): after it, a reader blocked on the shared flock copies a seed refreshed after its \
         clone, and an artifact newer than a source file reads fresh to cargo (646c0ade)"
    );
    assert!(
        checkout < measure,
        "the seed order must be measured after the branch checkout, the last write to the sources"
    );
}

/// Lift `seed_order_json` out of run.sh as it ships and run it.
fn seed_order(target: &Path, repo: &Path) -> serde_json::Value {
    let script = format!(
        "set -euo pipefail; eval \"$(sed -n '/^seed_order_json()/,/^}}/p' {})\"; seed_order_json {} {}",
        repo_root().join(RUN_SH).display(),
        target.display(),
        repo.display()
    );
    let out = Command::new("bash").arg("-c").arg(script).output().unwrap();
    assert!(
        out.status.success(),
        "seed_order_json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(text.lines().count(), 1, "one line of JSON: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn file_at(path: &Path, epoch: i64) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
    let s = Command::new("touch")
        .arg("-d")
        .arg(format!("@{epoch}"))
        .arg(path)
        .status()
        .unwrap();
    assert!(s.success());
}

const T: i64 = 1_790_000_000;

#[test]
fn a_seed_older_than_every_source_reads_clone_newer() {
    let dir = scratch_dir("seed-order-clone-newer");
    let (target, repo) = (dir.join("target"), dir.join("repo"));
    file_at(
        &target.join("debug/deps/libx-0123456789abcdef.rlib"),
        T - 100,
    );
    file_at(&target.join("debug/.fingerprint/x/dep-lib-x"), T - 90);
    file_at(&repo.join("crates/x/src/lib.rs"), T - 10);
    file_at(&repo.join("Cargo.toml"), T);
    // .git is not a source: an old pack in it must not be read as one.
    file_at(&repo.join(".git/objects/pack/old.pack"), T - 5000);

    let got = seed_order(&target, &repo);
    assert_eq!(got["order"], "clone-newer", "{got}");
    assert_eq!(got["margin_s"].as_f64(), Some(80.0), "{got}");
    assert!(got["seed_newest_artifact"].is_string(), "{got}");
    assert!(got["clone_oldest_source"].is_string(), "{got}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_artifact_newer_than_a_source_is_named_not_passed() {
    let dir = scratch_dir("seed-order-seed-newer");
    let (target, repo) = (dir.join("target"), dir.join("repo"));
    file_at(&target.join("debug/deps/libx-0123456789abcdef.rlib"), T);
    file_at(&repo.join("crates/x/src/lib.rs"), T - 30);
    file_at(&repo.join("Cargo.toml"), T + 5);

    let got = seed_order(&target, &repo);
    assert_eq!(got["order"], "SEED-NEWER", "{got}");
    assert_eq!(got["margin_s"].as_f64(), Some(-30.0), "{got}");

    // Equal is not newer: cargo reads an artifact no older than its
    // source as fresh, so a tie is the hazard, not the safe side.
    file_at(&repo.join("crates/x/src/lib.rs"), T);
    assert_eq!(seed_order(&target, &repo)["order"], "SEED-NEWER");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cold_target_says_cold() {
    let dir = scratch_dir("seed-order-cold");
    let (target, repo) = (dir.join("target"), dir.join("repo"));
    fs::create_dir_all(&target).unwrap();
    file_at(&repo.join("Cargo.toml"), T);

    let got = seed_order(&target, &repo);
    assert_eq!(got["order"], "cold", "{got}");
    assert!(got["seed_newest_artifact"].is_null(), "{got}");
    let _ = fs::remove_dir_all(&dir);
}

const SUM_BEGIN: &str = "# --- receipt summary (begin) ---";
const SUM_END: &str = "# --- receipt summary (end) ---";

/// Run the receipt-summary block as it ships with `SEED_ORDER` set.
fn summarize(tag: &str, receipt_body: &str, seed_order: &str) -> serde_json::Value {
    let src = run_sh();
    let block = &src[src.find(SUM_BEGIN).expect("summary begin marker")
        ..src.find(SUM_END).expect("summary end marker")];
    let dir = scratch_dir(&format!("seed-order-summary-{tag}"));
    let receipt = dir.join("receipt.json");
    fs::write(&receipt, receipt_body).unwrap();
    fs::write(dir.join("order.json"), seed_order).unwrap();
    let harness = dir.join("harness.sh");
    fs::write(
        &harness,
        format!(
            "set -euo pipefail\nRECEIPT={r}\nHEAD_SHA=deadbeef\nSEED_ORDER=$(cat {o})\n{block}\nprintf '%s' \"$SUMMARY\"\n",
            r = receipt.display(),
            o = dir.join("order.json").display(),
        ),
    )
    .unwrap();
    let out = Command::new("bash").arg(&harness).output().unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "summary block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(
        text.lines().count(),
        1,
        "the summary stays one line: {text}"
    );
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// THE ORDER IS EVIDENCE: the reported receipt carries it, readable or not.
#[test]
fn the_reported_receipt_carries_the_seed_order() {
    let order = r#"{"order":"clone-newer","margin_s":12.5,"seed_newest_artifact":"a","clone_oldest_source":"b"}"#;
    let want: serde_json::Value = serde_json::from_str(order).unwrap();

    let got = summarize("readable", r#"{"verdict":"green","head":"c0ffee"}"#, order);
    assert_eq!(got["seed_order"], want, "{got}");
    assert_eq!(
        got["verdict"], "green",
        "the rest of the receipt stands: {got}"
    );

    let got = summarize("unreadable", "not json", order);
    assert_eq!(got["verdict"], "unreadable", "{got}");
    assert_eq!(got["seed_order"], want, "{got}");
}
