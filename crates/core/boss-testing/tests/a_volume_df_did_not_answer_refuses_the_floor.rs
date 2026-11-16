//! `infra/gate.sh`'s disk floor refuses a run when `df` does not answer
//! for EVERY volume the run writes, and the receipt names the volume its
//! `free_gb` was measured on. Run, not read: a synthetic tree and a fake
//! `df` that answers per path the way GNU df does.
//!
//! THE DEFECT (backlog 8a08c00d, found by reviewer run 4030293f of car
//! 8997bf7b, 2026-10-19). Once the floor judged several volumes (backlog
//! 878e5a0f) it handed df every path at once, sent df's stderr to
//! /dev/null, and refused only when NO row came back. GNU df answers one
//! row per path it can read and, for one it cannot, prints the complaint
//! on stderr and no row — measured on the dev pod 2026-10-21:
//! `df -Pk /tmp /tmp /nonexistent-x /work` printed three rows for four
//! paths and exited 1. So with 900 GB on the target and "Permission
//! denied" on TMPDIR at a 12 GB floor, `--lint` passed the floor on the
//! one row that answered and labelled it with every writer, claiming a
//! volume was measured that was not. Before that car any df failure
//! refused.
//!
//! And the receipt's `free_gb` became the lowest free space across the
//! volumes, with nothing naming which — on the gate pod the smaller of
//! /gate-target and the node's /tmp.
//!
//! The fake below decides its answer from the PATH it is handed, so the
//! floor is judged on what it asked for, path by path.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FLOOR_GB: &str = "12";

struct Fixture {
    dir: PathBuf,
    /// df cannot read anything under here: stderr, no row, exit 1.
    denied: PathBuf,
    /// Readable on df's first call, denied on every later one — a
    /// volume that goes away mid-run.
    later: PathBuf,
    /// 900 GB on df's first call, 1 GB on every later one.
    shrinks: PathBuf,
    /// 50 GB, always: `/roomy`.
    roomy: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let dir = boss_testing::scratch_dir(&format!("disk-floor-unanswered-{tag}"));
        let dir = dir.canonicalize().expect("a scratch dir canonicalizes");
        let denied = dir.join("denied");
        let later = dir.join("later");
        let shrinks = dir.join("shrinks");
        let roomy = dir.join("roomy");
        for d in [&denied, &later, &shrinks, &roomy] {
            boss_testing::create_dir(&d.join("tmp"));
        }
        let calls = dir.join("df-calls");
        // One row per path argument, in order, and for a path it cannot
        // read the complaint on stderr, no row, and exit 1 — GNU df's own
        // shape. The call count is per CALL, not per row, so "later"
        // models one poll seeing the volume and the next one not.
        boss_testing::write_exec(
            &dir.join("df"),
            &format!(
                "#!/usr/bin/env bash\n\
                 n=$(cat '{calls}' 2>/dev/null || echo 0)\n\
                 echo $((n+1)) > '{calls}'\n\
                 echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
                 rc=0\n\
                 for a in \"$@\"; do\n\
                 p=$(realpath -m -- \"$a\")\n\
                 case \"$p/\" in\n\
                 '{denied}'/*) echo \"df: $a: Permission denied\" >&2; rc=1 ;;\n\
                 '{later}'/*) if [ \"$n\" -lt 1 ]; then echo '/dev/later 1 1 943718400 1% /later'; \
                 else echo \"df: $a: Permission denied\" >&2; rc=1; fi ;;\n\
                 '{shrinks}'/*) if [ \"$n\" -lt 1 ]; then echo '/dev/shrinks 1 1 943718400 1% /shrinks'; \
                 else echo '/dev/shrinks 1 1 1048576 99% /shrinks'; fi ;;\n\
                 *) echo '/dev/roomy 1 1 52428800 50% /roomy' ;;\n\
                 esac\n\
                 done\n\
                 exit $rc\n",
                calls = calls.display(),
                denied = denied.display(),
                later = later.display(),
                shrinks = shrinks.display(),
            ),
        );
        Fixture {
            dir,
            denied,
            later,
            shrinks,
            roomy,
        }
    }

    /// A one-commit repository holding this tree's gate.sh and the one
    /// lint the roster refuses to run without.
    fn tree(&self) -> PathBuf {
        let tree = self.roomy.join("checkout");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(
            &tree,
            &[
                "-c",
                "user.email=disk-floor@test",
                "-c",
                "user.name=disk-floor",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        tree
    }

    fn receipt(&self) -> PathBuf {
        self.dir.join("receipt.json")
    }

    /// `gate.sh --lint` — the mode that writes two volumes, TMPDIR and
    /// the cargo target — with each placed where the case needs it.
    fn lint(&self, tmp: &Path, target: &Path) -> Output {
        let tree = self.tree();
        Command::new("bash")
            .arg(tree.join("infra/gate.sh"))
            .arg("--lint")
            .current_dir(&tree)
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", FLOOR_GB)
            .env("BOSS_GATE_RECEIPT", self.receipt())
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            .env("TMPDIR", tmp)
            .env("CARGO_TARGET_DIR", target)
            .env_remove("CARGO_BUILD_JOBS")
            .env_remove("WT_JOBS")
            .output()
            .unwrap_or_else(|e| panic!("run gate.sh --lint in {}: {e}", tree.display()))
    }

    fn read_receipt(&self, t: &str) -> serde_json::Value {
        let body = std::fs::read_to_string(self.receipt())
            .unwrap_or_else(|e| panic!("a mid-run refusal writes a receipt ({e}):\n{t}"));
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("the receipt must be JSON ({e}): {body}\n{t}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn keyed_unanswered_reader_preserves_the_actual_target_and_writer() {
    let f = Fixture::new("keyed-target-reader");
    let source = std::fs::read_to_string(boss_testing::repo_root().join("infra/gate.sh")).unwrap();
    let mut script = String::from("set -euo pipefail\nQUICK=0\nLINT=1\n");
    for name in [
        "gate_floor_volumes",
        "gate_existing_path",
        "gate_unanswered_volumes",
    ] {
        let start = source.find(&format!("{name}() {{\n")).unwrap();
        let end = source[start..].find("\n}\n").unwrap() + start + 3;
        script.push_str(&source[start..end]);
    }
    script.push_str("gate_unanswered_volumes\n");
    let target = f.denied.join("target");
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("TMPDIR", f.roomy.join("tmp"))
        .env("CARGO_TARGET_DIR", &target)
        .env("BOSS_GATE_DF_CMD", f.dir.join("df"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\tthe cargo target\n", target.display()),
        "the keyed roster must hand df the actual path and conserve the writer: {}",
        text(&out)
    );
}

#[test]
fn keyed_unmeasured_fallback_preserves_every_actual_path_and_writer() {
    let f = Fixture::new("keyed-fallback");
    let source = std::fs::read_to_string(boss_testing::repo_root().join("infra/gate.sh")).unwrap();
    let mut script = String::from("set -euo pipefail\nQUICK=0\nLINT=1\nGITHUB_ACTIONS=false\n");
    for name in [
        "gate_floor_volumes",
        "gate_existing_path",
        "gate_unanswered_volumes",
        "gh_escape_data",
        "gh_annotate_refusal",
        "gate_refuse_unmeasured",
    ] {
        let start = source.find(&format!("{name}() {{")).unwrap();
        let end = source[start..].find("\n}\n").unwrap() + start + 3;
        script.push_str(&source[start..end]);
    }
    script.push_str("gate_refuse_unmeasured 'to start'\n");
    let tmp = f.roomy.join("tmp");
    let target = f.roomy.join("target");
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("TMPDIR", &tmp)
        .env("CARGO_TARGET_DIR", &target)
        .env("BOSS_GATE_DF_CMD", f.dir.join("df"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let t = text(&out);
    assert!(
        t.contains(&format!(
            "{} — the lint roster's scratch (TMPDIR)",
            tmp.display()
        )),
        "{t}"
    );
    assert!(
        t.contains(&format!("{} — the cargo target", target.display())),
        "{t}"
    );
    assert!(
        !t.lines().any(|line| line.trim_start().starts_with("tmp — ")
            || line.trim_start().starts_with("target — ")),
        "keys are not paths: {t}"
    );
}

/// The reviewer's own case: the target answers well over the floor and
/// TMPDIR answers "Permission denied". The floor has not measured
/// TMPDIR, so it refuses — and names the path it could not read and
/// what writes there.
#[test]
fn one_failed_df_row_refuses_the_floor_and_names_the_path() {
    let f = Fixture::new("tmp-denied");
    let tmp = f.denied.join("tmp");
    let out = f.lint(&tmp, &f.roomy.join("target"));
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a volume df did not answer for is a refusal (exit 2), not a pass on the volumes \
         that did answer (backlog 8a08c00d):\n{t}"
    );
    assert!(
        !t.contains("pre-flight: not run here"),
        "the run must not start on a floor it never measured:\n{t}"
    );
    let named = t
        .lines()
        .any(|l| l.contains(&tmp.display().to_string()) && l.contains("TMPDIR"));
    assert!(
        named,
        "the refusal names the path df did not answer for, and what writes there:\n{t}"
    );
    assert!(
        t.contains("Permission denied"),
        "df's own complaint reaches the log — it is the only copy of WHY:\n{t}"
    );
}

/// The receipt names the volume its `free_gb` was read on, and carries
/// every volume measured, so the number is not a minimum over volumes
/// nobody can tell apart.
#[test]
fn the_receipt_names_the_volume_free_gb_measured() {
    let f = Fixture::new("receipt-volume");
    let target = f.shrinks.join("target");
    let out = f.lint(&f.roomy.join("tmp"), &target);
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "the target's volume shrank to 1 GB after startup, so the run refuses to continue:\n{t}"
    );
    assert!(t.contains("Refusing to continue"), "{t}");
    let r = f.read_receipt(&t);
    assert_eq!(r["verdict"], "refused", "{r}");
    assert_eq!(r["free_gb"], 1, "free_gb is the binding volume's: {r}");
    assert_eq!(
        r["free_gb_on"], "/shrinks",
        "the receipt names the volume free_gb was measured on: {r}"
    );
    let volumes = r["free_volumes"]
        .as_array()
        .unwrap_or_else(|| panic!("the receipt carries every volume measured: {r}"));
    let on = |mount: &str| {
        volumes
            .iter()
            .find(|v| v["mount"] == mount)
            .unwrap_or_else(|| panic!("no {mount} in free_volumes: {r}"))
    };
    assert_eq!(on("/roomy")["free_gb"], 50, "{r}");
    assert!(
        on("/roomy")["writes"]
            .as_str()
            .is_some_and(|w| w.contains("TMPDIR")),
        "each volume says what writes there: {r}"
    );
    assert_eq!(on("/shrinks")["free_gb"], 1, "{r}");
    assert!(
        on("/shrinks")["writes"].as_str().is_some_and(
            |w| w.contains("cargo target") && w.contains(&target.display().to_string())
        ),
        "each volume says what writes there: {r}"
    );
}

/// The same hole one poll later: TMPDIR answered at startup and stops
/// answering mid-run. The run refuses to continue, and the receipt it
/// leaves says so — a refusal, naming the path — and stays JSON, with
/// `free_gb` null rather than a number nobody measured.
#[test]
fn a_volume_that_stops_answering_mid_run_refuses_with_a_receipt() {
    let f = Fixture::new("tmp-later");
    let tmp = f.later.join("tmp");
    let out = f.lint(&tmp, &f.roomy.join("target"));
    let t = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a volume df stopped answering for mid-run is a refusal:\n{t}"
    );
    let r = f.read_receipt(&t);
    assert_eq!(r["verdict"], "refused", "{r}");
    assert!(
        r["refused_because"]
            .as_str()
            .is_some_and(|w| w.contains(&tmp.display().to_string())),
        "the refusal names the path df did not answer for: {r}"
    );
    assert!(
        r["free_gb"].is_null(),
        "free_gb is null when a volume went unmeasured, never a minimum over the rest: {r}"
    );
}
