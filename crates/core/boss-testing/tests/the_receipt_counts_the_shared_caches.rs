//! A gate's receipt says which download caches it used and what they held.
//!
//! THE DEFECT (backlog e30098d3, finding 3 of review 2f67531c). CARGO_HOME
//! and, since 4bbf875d, bun's install cache live on the shared seed
//! volume, and the only trace of that in a gate's record was one stdout
//! line, `gate-runner: download caches CARGO_HOME=… BUN_INSTALL_CACHE_DIR=…`,
//! which dies with the pod log when the Job is reaped (~24 h). Nothing
//! durable said a gate ran on the seed's caches, let alone that they were
//! populated, and the receipt's `web install` seconds cannot stand in for
//! it: measured 2026-10-19 over 52 gate-runs on 2026-10-16, the install
//! took 0-3 s with an empty per-run cache (runner 5185619e) and 0-1 s on
//! the seed's (runner a5281011).
//!
//! THE FIX. run.sh counts what each cache holds — crate archives under
//! CARGO_HOME, package directories under BUN_INSTALL_CACHE_DIR — before
//! the checks and after them, and the receipt carries both counts with
//! the two paths as `caches`. A start above zero is a gate that began on
//! a populated cache; an end equal to it is a run in which nothing was
//! downloaded into it.
//!
//! These pins lift the blocks of run.sh as they ship and run them.

use boss_testing::{repo_root, scratch_dir, write_file};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

const RUN_SH: &str = "infra/gate-runner/run.sh";

fn run_sh() -> String {
    fs::read_to_string(repo_root().join(RUN_SH)).expect("read run.sh")
}

/// The text between two markers of run.sh, exactly as it ships.
fn block(begin: &str, end: &str) -> String {
    let src = run_sh();
    let a = src
        .find(begin)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{begin}` marker"));
    let b = src
        .find(end)
        .unwrap_or_else(|| panic!("{RUN_SH} has no `{end}` marker"));
    assert!(a < b, "`{begin}` must precede `{end}`");
    src[a..b].to_string()
}

fn evidence_block() -> String {
    block(
        "# --- cache evidence (begin) ---",
        "# --- cache evidence (end) ---",
    )
}

/// A cache laid out the way cargo and bun 1.3 lay theirs: crate archives
/// one directory under `registry/cache`, bun's `<name>@<version>@@@1`
/// package directories at the top or under a scope, beside the per-name
/// directories of SYMLINKS bun keeps, its `.tmp`, and a loose file.
fn populate(cargo: &Path, bun: &Path) {
    let put = |path: &Path, body: &str| {
        fs::create_dir_all(path.parent().expect("a parent")).unwrap();
        write_file(path, body);
    };
    let index = cargo.join("registry/cache/index.crates.io-1949cf8c6b5b557f");
    put(&index.join("serde-1.0.200.crate"), "x");
    put(&index.join("libc-0.2.150.crate"), "x");
    put(&cargo.join("registry/src/index/serde-1.0.200/lib.rs"), "x");
    put(&bun.join("acorn@8.19.0@@@1/package.json"), "{}");
    put(
        &bun.join("@sveltejs/load-config@0.2.3@@@1/package.json"),
        "{}",
    );
    fs::create_dir_all(bun.join("acorn")).unwrap();
    symlink(bun.join("acorn@8.19.0@@@1"), bun.join("acorn/8.19.0@@@1")).unwrap();
    put(&bun.join(".tmp/svelte@5.0.0@@@1/package.json"), "{}");
    put(&bun.join("loose.npm"), "x");
}

/// Run the start block with the caches the runner chose; returns
/// (CACHE_START, stdout).
fn start(seed: &Path, cargo: &Path, bun: &Path) -> (String, String) {
    let script = format!(
        "set -euo pipefail\ntrap 'echo TRAPPED; exit 99' ERR\nSEED={seed}\nCARGO_HOME={cargo}\nBUN_INSTALL_CACHE_DIR={bun}\n{block}\nprintf 'START=%s' \"$CACHE_START\"\n",
        seed = seed.display(),
        cargo = cargo.display(),
        bun = bun.display(),
        block = evidence_block(),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success() && !stdout.contains("TRAPPED"),
        "the cache evidence block failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (log, start) = stdout
        .rsplit_once("START=")
        .unwrap_or_else(|| panic!("no START: {stdout}"));
    (start.to_string(), log.to_string())
}

#[test]
fn the_start_block_counts_what_each_cache_holds() {
    let dir = scratch_dir("receipt-caches-start");
    let seed = dir.join("seed");
    let cargo = seed.join("cargo");
    let bun = seed.join("bun-install-cache");
    populate(&cargo, &bun);
    let (full, log) = start(&seed, &cargo, &bun);

    // Fresh caches: the runner has just made both directories, empty.
    let fresh = dir.join("fresh");
    let (fresh_cargo, fresh_bun) = (fresh.join("cargo"), fresh.join("bun"));
    fs::create_dir_all(&fresh_cargo).unwrap();
    fs::create_dir_all(&fresh_bun).unwrap();
    let (empty, _) = start(&fresh, &fresh_cargo, &fresh_bun);

    // A cache that is not there cannot be counted, and says so.
    let (gone, _) = start(&fresh, &fresh.join("no-cargo"), &fresh.join("no-bun"));
    let _ = fs::remove_dir_all(&dir);

    assert_eq!(
        full,
        format!(
            "{} {} {} 2 2",
            seed.display(),
            cargo.display(),
            bun.display()
        ),
        "two crate archives and two bun packages — not the registry sources, the symlinks, .tmp or a loose file"
    );
    assert!(
        log.contains("gate-runner: cache contents at start: 2 crate archive(s) in CARGO_HOME, 2 package(s) in BUN_INSTALL_CACHE_DIR"),
        "{log}"
    );
    assert!(empty.ends_with(" 0 0"), "{empty}");
    assert!(gone.ends_with(" - -"), "{gone}");
}

/// Run the receipt-summary block with the cache counts set.
fn summarize(dir: &Path, receipt_body: &str, vars: &str) -> serde_json::Value {
    let receipt = dir.join("receipt.json");
    write_file(&receipt, receipt_body);
    let script = format!(
        "set -euo pipefail\nRECEIPT={r}\nHEAD_SHA=deadbeef\n{vars}\n{block}\nprintf '%s' \"$SUMMARY\"\n",
        r = receipt.display(),
        block = block(
            "# --- receipt summary (begin) ---",
            "# --- receipt summary (end) ---"
        ),
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "summary block failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

#[test]
fn the_receipt_carries_the_caches_and_their_counts() {
    let dir = scratch_dir("receipt-caches-summary");
    let seeded = "CACHE_START='/gate-seed /gate-seed/cargo /gate-seed/bun-install-cache 812 431'\nCACHE_END='815 431'";
    let readable = summarize(&dir, r#"{"verdict":"green","head":"c0ffee"}"#, seeded);
    let unreadable = summarize(&dir, "not json", seeded);
    let cold = summarize(
        &dir,
        r#"{"verdict":"green"}"#,
        "CACHE_START='/gate-seed /gate-target/cargo /gate-target/.bun/install/cache 0 -'\nCACHE_END='640 -'",
    );
    let none = summarize(&dir, r#"{"verdict":"green"}"#, "");
    let _ = fs::remove_dir_all(&dir);

    for got in [&readable, &unreadable] {
        let c = &got["caches"];
        assert_eq!(c["cargo_home"], "/gate-seed/cargo", "{got}");
        assert_eq!(
            c["bun_install_cache_dir"], "/gate-seed/bun-install-cache",
            "{got}"
        );
        assert_eq!(c["on_seed"], true, "{got}");
        assert_eq!(c["cargo_crates"]["start"], 812, "{got}");
        assert_eq!(c["cargo_crates"]["end"], 815, "{got}");
        assert_eq!(c["bun_packages"]["start"], 431, "{got}");
        assert_eq!(c["bun_packages"]["end"], 431, "{got}");
        assert!(
            c["method"].as_str().unwrap_or("").contains("integrity"),
            "the method says what the counts do not prove: {got}"
        );
    }
    assert_eq!(readable["verdict"], "green", "{readable}");

    // ABSENT IS NEVER A ZERO: an uncounted cache is null, and a runner
    // that counted nothing adds no key.
    let c = &cold["caches"];
    assert_eq!(c["on_seed"], false, "{cold}");
    assert_eq!(c["cargo_crates"]["start"], 0, "{cold}");
    assert!(c["bun_packages"]["start"].is_null(), "{cold}");
    assert!(c["bun_packages"]["end"].is_null(), "{cold}");
    assert!(none.get("caches").is_none(), "{none}");
}

/// The lines run.sh EXECUTES, in order — comments are prose.
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
        "run.sh must have exactly one {what}; found {}",
        hits.len()
    );
    hits[0]
}

/// THE ORDER PIN. The start is counted after the runner chooses and
/// makes the caches and before gate.sh; the end after gate.sh and before
/// the summary that carries both.
#[test]
fn the_counts_bracket_the_checks() {
    let lines = executed_lines();
    let chosen = only_line(&lines, "BUN_INSTALL_CACHE_DIR on the seed", |l| {
        l.trim() == r#"export BUN_INSTALL_CACHE_DIR="$SEED/bun-install-cache""#
    });
    let counted = only_line(&lines, "CACHE_START", |l| {
        l.trim() == "CACHE_START=\"$SEED $CARGO_HOME $BUN_INSTALL_CACHE_DIR $(cache_counts)\""
    });
    let gate = only_line(&lines, "gate.sh call", |l| {
        l.contains("./infra/gate.sh ${GATE_MODE:-}")
    });
    let ended = only_line(&lines, "CACHE_END", |l| {
        l.trim() == "CACHE_END=$(cache_counts)"
    });
    let summary = only_line(&lines, "receipt summary", |l| {
        l.trim_start().starts_with("SUMMARY=$(python3")
    });
    assert!(
        chosen < counted,
        "the start is counted after the caches are chosen"
    );
    assert!(counted < gate, "the start is counted before the checks");
    assert!(gate < ended, "the end is counted after the checks");
    assert!(ended < summary, "the end is counted before the summary");
}
