//! `infra/lint/rows-still-in-v1-words.sh` RUNS, and its count is a
//! number (backlog 53abeb8d, David 2026-11-08: "systematically update
//! protocols as we touch them to reflect the new design paradigm").
//!
//! The lint does not fail on a v1 row yet: the station tree bundle is
//! additive, and the old keys stay wherever a live reader still reads
//! them. What it owes is a number that falls as rows move, so the
//! retirement is something read rather than felt. A count nobody can
//! parse is the "check nobody reads" of CLAUDE.md §Diagnosis, so this
//! pins three things: on the real tree it exits 0 and prints
//! `v1-rows:<n>` with n a number; on a planted tree it counts exactly
//! the rows planted in v1 words and names their files; and a v2 key it
//! names that the station row type does not carry is refused, so the
//! vocabulary it prints (and `boss brief` reads out of it) cannot drift
//! from the code that reads the rows (CLAUDE.md §9a).

use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_file};
use std::path::Path;
use std::process::Command;

const LINT: &str = "infra/lint/rows-still-in-v1-words.sh";

fn run(root: &Path) -> (i32, String, String) {
    let out = Command::new("bash")
        .arg(root.join(LINT))
        .output()
        .unwrap_or_else(|e| panic!("run {LINT}: {e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The number after `v1-rows:` on the lint's own line, if it is one.
fn count(stdout: &str) -> Option<u64> {
    stdout
        .split_whitespace()
        .find_map(|w| w.strip_prefix("v1-rows:"))
        .and_then(|n| n.parse().ok())
}

#[test]
fn the_lint_runs_on_the_tree_and_its_count_is_a_number() {
    let (code, stdout, stderr) = run(&repo_root());
    assert_eq!(
        code, 0,
        "the lint reports, it does not fail yet:\n{stdout}\n{stderr}"
    );
    assert!(
        count(&stdout).is_some(),
        "no `v1-rows:<number>` in the lint's output:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("rows-still-in-v1-words: scanned "),
        "the lint names how many rows it read (lib/scanned.sh):\n{stdout}"
    );
}

/// A tree holding only what the lint reads: itself, its scanned
/// library, the station row type, and the rows planted by the caller.
fn planted(station_fields: &str) -> std::path::PathBuf {
    let root = scratch_dir("rows-v1-words");
    let real = repo_root();
    for dir in [
        "infra/lint/lib",
        "infra/platform/stations",
        "infra/platform/workflows",
        "crates/core/boss-jobs/src",
    ] {
        create_dir(&root.join(dir));
    }
    copy_exec(&real.join(LINT), &root.join(LINT));
    copy_exec(
        &real.join("infra/lint/lib/scanned.sh"),
        &root.join("infra/lint/lib/scanned.sh"),
    );
    write_file(
        &root.join("crates/core/boss-jobs/src/stations.rs"),
        &format!("pub struct StationSpec {{\n{station_fields}}}\n"),
    );
    root
}

const EVERY_V2_FIELD: &str = "    pub id: Option<String>,\n    pub parent: Option<String>,\n    \
     pub mode: Option<String>,\n    pub capacity: Option<i32>,\n    pub machines: Vec<String>,\n    \
     pub allocation: BTreeMap<String, u32>,\n";

#[test]
fn a_planted_tree_counts_exactly_its_v1_rows_and_names_their_files() {
    let root = planted(EVERY_V2_FIELD);
    let st = root.join("infra/platform/stations");
    let wf = root.join("infra/platform/workflows");
    write_file(
        &st.join("clean.toml"),
        "[[station]]\nname = \"clean\"\nid = \"c\"\nparent = \"r\"\n",
    );
    write_file(
        &st.join("old-dept.toml"),
        "[[station]]\nname = \"old-dept\"\ndepartment = \"it\"\n",
    );
    write_file(
        &st.join("old-floor.toml"),
        "[[station]]\nname = \"old-floor\"\n\n[station.floor]\nx = 1\n",
    );
    write_file(
        &wf.join("owned.toml"),
        "[[workflow]]\nkind = \"owned\"\nmetadata = { owner = \"r\" }\n",
    );
    write_file(
        &wf.join("unowned.toml"),
        "[[workflow]]\nkind = \"unowned\"\nmetadata = { owner_role = \"x\" }\n",
    );
    write_file(
        &wf.join("dept.toml"),
        "[[workflow]]\nkind = \"dept\"\nmetadata = { owner = \"r\", department = \"it\" }\n",
    );
    let (code, stdout, stderr) = run(&root);
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(count(&stdout), Some(4), "{stdout}\n{stderr}");
    for named in [
        "stations/old-dept.toml",
        "stations/old-floor.toml",
        "workflows/unowned.toml",
        "workflows/dept.toml",
    ] {
        assert!(stdout.contains(named), "{named} is not named:\n{stdout}");
    }
    for clean in ["stations/clean.toml", "workflows/owned.toml"] {
        assert!(
            !stdout.contains(clean),
            "{clean} is named but clean:\n{stdout}"
        );
    }
    assert!(stdout.contains("scanned 6 "), "{stdout}");
}

#[test]
fn a_v2_key_the_station_row_does_not_carry_is_refused() {
    // `allocation` missing from the row type: the lint's own vocabulary
    // has drifted from the code, and a printed word nobody reads is the
    // retyped rule this lint exists to replace.
    let root = planted(
        "    pub id: Option<String>,\n    pub parent: Option<String>,\n    pub mode: Option<String>,\n    \
         pub capacity: Option<i32>,\n    pub machines: Vec<String>,\n",
    );
    write_file(
        &root.join("infra/platform/stations/a.toml"),
        "[[station]]\nname = \"a\"\n",
    );
    let (code, stdout, stderr) = run(&root);
    assert_eq!(code, 1, "{stdout}\n{stderr}");
    assert!(
        stderr.contains("allocation"),
        "the refusal names the key:\n{stderr}"
    );
}
