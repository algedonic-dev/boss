//! `infra/lint/svelte-check.sh` — the frontend's type gate — is RUN here,
//! through its `--judge` half, against synthetic svelte-check output, so
//! every verdict below is one the lint actually gives.
//!
//! TWO GAPS, one car (backlogs c78e8dbb and 68056e4e, 2026-09-27).
//!
//! * The type check never read `apps/web/tests`. The tsconfig included
//!   `src`, `scripts` and web-kit only, so ~130 mocked specs that pin every
//!   page were type-checked by nothing; including them surfaced 21 errors
//!   in 10 files, each a spec that would fail at runtime (or pass by
//!   accident) on a shape nobody checked. The pin below keeps them in.
//! * The check exited 0 on 74 warnings in 28 files that no gate read, so
//!   a new warning landed unseen (CLAUDE.md §Diagnosis: a check nobody
//!   reads is a check that is not running). The lint now holds every file
//!   to its count in `apps/web/svelte-check-warnings.txt`: one more fails,
//!   one fewer passes and says which line to lower.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/svelte-check.sh";

/// Run the lint's pure verdict over a fixture output and baseline.
fn judge(tag: &str, output: &str, baseline: &str) -> Output {
    let dir = scratch::scratch_dir(&format!("svelte-check-judge-{tag}"));
    let out: PathBuf = dir.join("check.out");
    let base: PathBuf = dir.join("baseline.txt");
    scratch::write_file(&out, output);
    scratch::write_file(&base, baseline);
    Command::new("bash")
        .arg(repo_root().join(LINT))
        .arg("--judge")
        .arg(&out)
        .arg(&base)
        .output()
        .unwrap_or_else(|e| panic!("run {LINT}: {e}"))
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

const BASELINE: &str = "\
# a comment is not a file
2\tsrc/a/Page.svelte
1\t../../libs/web-kit/src/ui/Tabs.svelte
";

fn warning(file: &str) -> String {
    format!("1790506556049 WARNING \"{file}\" 20:1 \"Some warning\\nhttps://svelte.dev/e/x\"\n")
}

fn output(files: &[&str]) -> String {
    let body: String = files.iter().map(|f| warning(f)).collect();
    let n_files = {
        let mut v: Vec<&&str> = files.iter().collect();
        v.sort();
        v.dedup();
        v.len()
    };
    format!(
        "1790506556000 START \"/work/boss/apps/web\"\n{body}1790506556050 COMPLETED 900 FILES 0 ERRORS {} WARNINGS {n_files} FILES_WITH_PROBLEMS\n",
        files.len()
    )
}

#[test]
fn the_same_warnings_as_the_baseline_pass() {
    let o = judge(
        "same",
        &output(&[
            "src/a/Page.svelte",
            "src/a/Page.svelte",
            "../../libs/web-kit/src/ui/Tabs.svelte",
        ]),
        BASELINE,
    );
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
}

#[test]
fn one_more_warning_in_a_baselined_file_fails_naming_it() {
    let o = judge(
        "more",
        &output(&[
            "src/a/Page.svelte",
            "src/a/Page.svelte",
            "src/a/Page.svelte",
            "../../libs/web-kit/src/ui/Tabs.svelte",
        ]),
        BASELINE,
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(t.contains("src/a/Page.svelte"), "names the file: {t}");
    assert!(
        t.contains("3 warning(s), baseline 2"),
        "names both counts: {t}"
    );
}

#[test]
fn a_warning_in_a_file_the_baseline_does_not_name_fails() {
    let o = judge(
        "new-file",
        &output(&[
            "src/a/Page.svelte",
            "src/a/Page.svelte",
            "../../libs/web-kit/src/ui/Tabs.svelte",
            "src/b/New.svelte",
        ]),
        BASELINE,
    );
    let t = text(&o);
    assert_eq!(o.status.code(), Some(1), "{t}");
    assert!(
        t.contains("src/b/New.svelte: 1 warning(s), baseline 0"),
        "{t}"
    );
}

#[test]
fn fewer_warnings_pass_and_name_the_line_to_lower() {
    let o = judge("fewer", &output(&["src/a/Page.svelte"]), BASELINE);
    let t = text(&o);
    assert_eq!(o.status.code(), Some(0), "{t}");
    assert!(t.contains("lower"), "says the baseline can come down: {t}");
    assert!(t.contains("src/a/Page.svelte"), "{t}");
    assert!(
        t.contains("../../libs/web-kit/src/ui/Tabs.svelte"),
        "a file now at zero is named too: {t}"
    );
}

#[test]
fn an_output_without_its_completed_line_was_never_read() {
    let o = judge(
        "no-completed",
        "1790506556000 START \"/work/boss/apps/web\"\n",
        BASELINE,
    );
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
}

#[test]
fn a_count_that_disagrees_with_its_own_lines_was_never_read() {
    // The summary says 5 warnings; only 3 lines were parsed. Judging the
    // 3 would certify a run whose record the parser did not understand.
    let bad = output(&[
        "src/a/Page.svelte",
        "src/a/Page.svelte",
        "../../libs/web-kit/src/ui/Tabs.svelte",
    ])
    .replace("0 ERRORS 3 WARNINGS", "0 ERRORS 5 WARNINGS");
    let o = judge("disagree", &bad, BASELINE);
    assert_eq!(o.status.code(), Some(3), "{}", text(&o));
}

/// c78e8dbb: the specs are type-checked because the ONE tsconfig the gate
/// reads includes them. Dropping the line again is the regression.
#[test]
fn the_web_tsconfig_type_checks_the_tests() {
    let tsconfig = std::fs::read_to_string(repo_root().join("apps/web/tsconfig.json"))
        .unwrap_or_else(|e| panic!("read apps/web/tsconfig.json: {e}"));
    assert!(
        tsconfig.contains("\"tests/**/*.ts\""),
        "apps/web/tsconfig.json must include tests/**/*.ts, or svelte-check \
         never reads the mocked specs (backlog c78e8dbb)"
    );
}

/// The committed baseline is one the judge can read: every non-comment
/// line is `<count>\t<path>` with a positive count.
#[test]
fn the_committed_baseline_is_well_formed() {
    let body = std::fs::read_to_string(repo_root().join("apps/web/svelte-check-warnings.txt"))
        .unwrap_or_else(|e| panic!("read the baseline: {e}"));
    let rows: Vec<&str> = body
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert!(!rows.is_empty(), "the baseline lists no file");
    for row in rows {
        let (n, path) = row
            .split_once('\t')
            .unwrap_or_else(|| panic!("not `<count>\\t<path>`: {row:?}"));
        let n: u32 = n
            .parse()
            .unwrap_or_else(|_| panic!("count is not a number: {row:?}"));
        assert!(n > 0, "a zero line is a line to delete: {row:?}");
        assert!(!path.is_empty(), "no path: {row:?}");
    }
}
