//! A failed web suite keeps what Playwright saw, whole, beside the
//! receipt — and the receipt names the file.
//!
//! MEASURED 2026-09-27 (backlog 4d928d0a). Dock re-gate 7b2acdc4 went
//! red on ONE mocked spec at the 1.0 m test timeout, on a tree that did
//! not touch it. Playwright had written what the page was showing to
//! `apps/web/test-results/<test>/error-context.md` — the page snapshot a
//! reader needs to say what the spec was waiting on — and the list
//! reporter printed only that file's PATH. The file lived in the gate
//! pod's workspace and died with it, so the diagnosis rested on
//! reproducing the failure, which did not reproduce (5/5, 190/190), and
//! the red went down as unexplained. CLAUDE.md §Diagnosis: capture to a
//! file and print on failure; never reduce a record before storing it.
//!
//! So on a failed web suite gate.sh copies every error context Playwright
//! wrote DURING THAT CHECK (newer than a marker taken before it — an
//! older `test-results/` from a previous local run is not this run's
//! evidence) into one file beside the receipt, prints it whole in a
//! `::group::gate-evidence: <check>` block of its own (outside the
//! check's group, so the runner's excerpt of the check keeps its budget
//! for Playwright's verdict), and names it on the receipt under
//! `evidence`. The gate-runner folds it onto the durable record as
//! `fails_context` (pinned in `gate_runner_replays_failures.rs`).
//!
//! This lifts the bracketed block out of gate.sh verbatim and RUNS it,
//! for the reason `gate_runner_replays_failures.rs` gives: a property
//! only checked by reading the text is a property guessed at.

use boss_testing::repo_root;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

const OPEN: &str = "# --- web evidence (begin) ---";
const CLOSE: &str = "# --- web evidence (end) ---";
const CHECK: &str = "web-suite (unit+build+mocked)";

fn gate_sh() -> String {
    let path = repo_root().join("infra/gate.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn block() -> String {
    let src = gate_sh();
    let start = src
        .find(OPEN)
        .unwrap_or_else(|| panic!("infra/gate.sh has no `{OPEN}` block"));
    let end = src[start..]
        .find(CLOSE)
        .unwrap_or_else(|| panic!("infra/gate.sh's web evidence block is not closed"));
    src[start..start + end].to_string()
}

fn write_at(path: &Path, body: &str, mtime: SystemTime) {
    if let Some(parent) = path.parent() {
        boss_testing::create_dir(parent);
    }
    boss_testing::write_file(path, body);
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(mtime))
        .unwrap_or_else(|e| panic!("set mtime on {}: {e}", path.display()));
}

struct Ran {
    stdout: String,
    stderr: String,
    ok: bool,
}

/// Run the lifted block in `tree` against a marker taken "now": the
/// `old` context predates it, `fresh` (if any) follows it.
fn run(tag: &str, fresh: Option<&str>) -> (Ran, std::path::PathBuf) {
    let tree = boss_testing::scratch_dir(&format!("gate-web-evidence-{tag}"));
    let results = tree.join("apps/web/test-results");
    let now = SystemTime::now();
    write_at(
        &results.join("stale-run/error-context.md"),
        "STALE: a previous run's page\n",
        now - Duration::from_secs(3_600),
    );
    let marker = tree.join("since");
    write_at(&marker, "", now);
    if let Some(body) = fresh {
        write_at(
            &results.join("it-department-map-stations/error-context.md"),
            body,
            now + Duration::from_secs(5),
        );
    }
    let script = tree.join("run.sh");
    boss_testing::write_file(
        &script,
        &format!(
            "set -euo pipefail\n\
             GATE_RECEIPT=\"$PWD/receipt.json\"\n\
             EVIDENCE=()\n\
             {block}\n\
             keep_error_context '{CHECK}' \"$PWD/since\"\n\
             for e in ${{EVIDENCE[@]+\"${{EVIDENCE[@]}}\"}}; do printf 'EVIDENCE=%s\\n' \"$e\"; done\n\
             printf 'JSON=%s\\n' \"$(evidence_json)\"\n",
            block = block()
        ),
    );
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(&tree)
        .output()
        .expect("bash runs");
    (
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            ok: out.status.success(),
        },
        tree,
    )
}

fn json_line(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("JSON="))
        .unwrap_or_else(|| panic!("no JSON= line:\n{stdout}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("evidence_json is not JSON ({e}): {line}"))
}

#[test]
fn a_failed_suites_error_context_is_kept_whole_printed_and_named() {
    let snapshot = "# Page snapshot\n\n```yaml\n- heading \"not found\" [level=1]\n```\n";
    let (ran, tree) = run("fresh", Some(snapshot));
    assert!(ran.ok, "the block failed:\n{}\n{}", ran.stdout, ran.stderr);

    let kept = tree.join("receipt.error-context.md");
    let body = std::fs::read_to_string(&kept).unwrap_or_else(|e| {
        panic!(
            "no evidence file at {}: {e}\n{}",
            kept.display(),
            ran.stdout
        )
    });
    assert!(
        body.contains(
            "===== apps/web/test-results/it-department-map-stations/error-context.md ====="
        ) && body.contains("heading \"not found\""),
        "the file holds the context whole, under the path it came from:\n{body}"
    );
    assert!(
        !body.contains("STALE"),
        "a context older than the check is not this run's evidence:\n{body}"
    );
    assert!(
        ran.stdout
            .contains(&format!("::group::gate-evidence: {CHECK}"))
            && ran.stdout.contains("heading \"not found\""),
        "the context is printed whole, in a group of its own:\n{}",
        ran.stdout
    );
    assert!(
        !ran.stdout.contains(&format!("::group::gate: {CHECK}")),
        "never inside the check's own group, whose excerpt is budgeted for the verdict"
    );
    let named = json_line(&ran.stdout);
    assert_eq!(
        named[CHECK].as_str(),
        Some(kept.to_str().expect("utf-8 path")),
        "the receipt names the file, by an absolute path: {named}"
    );
    let _ = std::fs::remove_dir_all(&tree);
}

#[test]
fn a_failure_with_no_new_context_says_so_and_names_nothing() {
    let (ran, tree) = run("none", None);
    assert!(ran.ok, "the block failed:\n{}\n{}", ran.stdout, ran.stderr);
    assert!(
        !tree.join("receipt.error-context.md").exists(),
        "no context was written during the check, so no evidence file"
    );
    assert!(
        ran.stdout.contains("no error context"),
        "the absence is stated, not silent:\n{}",
        ran.stdout
    );
    assert_eq!(
        json_line(&ran.stdout).as_object().map(|m| m.len()),
        Some(0),
        "nothing is named when nothing was kept"
    );
    let _ = std::fs::remove_dir_all(&tree);
}

/// An error context in Playwright's own shape, for the spec at `location`.
fn context_for(location: &str, marker: &str) -> String {
    format!(
        "# Instructions\n\n- Following Playwright test failed.\n\n# Test info\n\n\
         - Name: {marker}\n- Location: {location}\n\n# Error details\n\n```\n{marker}\n```\n"
    )
}

/// Run the keeper over two fresh contexts - a spec Playwright counted
/// failed and a `test.fail` spec it counted passed - with the check's
/// own output, as the call site tees it, in `out.log`.
fn run_with_output(tag: &str, output: &str) -> (Ran, std::path::PathBuf) {
    let tree = boss_testing::scratch_dir(&format!("gate-web-evidence-{tag}"));
    let results = tree.join("apps/web/test-results");
    let now = SystemTime::now();
    let marker = tree.join("since");
    write_at(&marker, "", now);
    write_at(
        &results.join("a-page-error-fails-the-spe-6824b-chromium/error-context.md"),
        &context_for(
            "tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:25:1",
            "EXPECTED: a test.fail spec, counted passed",
        ),
        now + Duration::from_secs(5),
    );
    write_at(
        &results.join("it-map-routes.mocked-a-rou-9f9d8-chromium/error-context.md"),
        &context_for(
            "tests/mocked/it-map-routes.mocked.spec.ts:163:1",
            "REAL: the spec that timed out",
        ),
        now + Duration::from_secs(5),
    );
    boss_testing::write_file(&tree.join("out.log"), output);
    let script = tree.join("run.sh");
    boss_testing::write_file(
        &script,
        &format!(
            "set -euo pipefail\n\
             GATE_RECEIPT=\"$PWD/receipt.json\"\n\
             EVIDENCE=()\n\
             {block}\n\
             keep_error_context '{CHECK}' \"$PWD/since\" \"$PWD/out.log\"\n\
             printf 'JSON=%s\\n' \"$(evidence_json)\"\n",
            block = block()
        ),
    );
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(&tree)
        .output()
        .expect("bash runs");
    (
        Ran {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            ok: out.status.success(),
        },
        tree,
    )
}

/// Gate-run 5b5a04d8's roll-up, in Playwright's shape (trailing space
/// after each title, as the receipt carried it).
const ROLL_UP: &str = "  ✘   586 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red (4.0m)


  1) [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red\x20

    Test timeout of 240000ms exceeded while setting up \"page\".

  1 failed
    [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red\x20
  1169 passed (9.1m)
error: script \"test:mocked\" exited with code 1
";

/// AN EXPECTED FAILURE'S PAGE IS NOT EVIDENCE (backlog a766e20d).
/// Playwright writes an error context for every `test.fail` spec too,
/// and counts it PASSED. Gate-run 5b5a04d8's receipt spent its context
/// budget on four of them first and omitted a real timeout's page; the
/// runner now ranks them last (42981848), but they still filled the
/// evidence file and the replay. So the keeper reads the check's own
/// output for Playwright's `N failed` roll-up and keeps only the
/// contexts of the specs it lists - naming each one it set aside, so
/// the reduction is on the record rather than silent.
#[test]
fn a_context_whose_spec_the_roll_up_does_not_count_failed_is_set_aside_and_named() {
    let (ran, tree) = run_with_output("rollup", ROLL_UP);
    assert!(ran.ok, "the block failed:\n{}\n{}", ran.stdout, ran.stderr);
    let body = std::fs::read_to_string(tree.join("receipt.error-context.md"))
        .unwrap_or_else(|e| panic!("no evidence file: {e}\n{}", ran.stdout));
    assert!(
        body.contains("REAL: the spec that timed out"),
        "the context of the spec the roll-up counts failed is kept:\n{body}"
    );
    assert!(
        !body.contains("EXPECTED:"),
        "a test.fail spec's context is not kept as this red's evidence:\n{body}"
    );
    assert!(
        ran.stdout.contains("set aside")
            && ran
                .stdout
                .contains("a-page-error-fails-the-spe-6824b-chromium/error-context.md"),
        "the context set aside is named, with the reason:\n{}",
        ran.stdout
    );
    let _ = std::fs::remove_dir_all(&tree);
}

/// Without a roll-up to read - a run killed before Playwright's epilogue,
/// or an output that could not be captured - nothing tells an expected
/// failure from a real one, so every context is kept, and it says why.
#[test]
fn without_a_roll_up_every_context_is_kept_and_the_log_says_why() {
    let (ran, tree) = run_with_output("no-rollup", "  ✘   586 [chromium] › killed mid-run\n");
    assert!(ran.ok, "the block failed:\n{}\n{}", ran.stdout, ran.stderr);
    let body = std::fs::read_to_string(tree.join("receipt.error-context.md"))
        .unwrap_or_else(|e| panic!("no evidence file: {e}\n{}", ran.stdout));
    assert!(
        body.contains("REAL:") && body.contains("EXPECTED:"),
        "every context is kept when the roll-up cannot sort them:\n{body}"
    );
    assert!(
        ran.stdout.contains("no Playwright roll-up"),
        "…and the log says it could not sort them:\n{}",
        ran.stdout
    );
    let _ = std::fs::remove_dir_all(&tree);
}

/// When EVERY context was an expected failure's, nothing is named on the
/// receipt, and the log says what was written and why none was kept.
#[test]
fn when_every_context_is_set_aside_nothing_is_named_and_the_log_says_so() {
    let only_other = ROLL_UP.replace("it-map-routes.mocked.spec.ts:163:1", "other.spec.ts:1:1");
    let (ran, tree) = run_with_output("all-aside", &only_other);
    assert!(ran.ok, "the block failed:\n{}\n{}", ran.stdout, ran.stderr);
    assert!(
        !tree.join("receipt.error-context.md").exists(),
        "no evidence file when every context was set aside:\n{}",
        ran.stdout
    );
    assert_eq!(
        json_line(&ran.stdout).as_object().map(|m| m.len()),
        Some(0),
        "nothing is named when nothing was kept"
    );
    assert!(
        ran.stdout.contains("2 error context(s)") && ran.stdout.contains("none kept"),
        "the absence is stated with its count:\n{}",
        ran.stdout
    );
    let _ = std::fs::remove_dir_all(&tree);
}

/// `output_to` is the tee the call site wraps the check in: the output
/// still reaches the log whole, a copy lands in the file, and the
/// check's OWN exit status is what `check` sees - not tee's.
#[test]
fn output_to_keeps_the_output_and_the_checks_own_status() {
    let tree = boss_testing::scratch_dir("gate-web-evidence-output-to");
    let script = tree.join("run.sh");
    boss_testing::write_file(
        &script,
        &format!(
            "set -euo pipefail\n\
             GATE_RECEIPT=\"$PWD/receipt.json\"\n\
             {block}\n\
             s=0\n\
             output_to \"$PWD/o.log\" bash -c 'echo said-out; echo said-err >&2; exit 3' || s=$?\n\
             printf 'STATUS=%s\\n' \"$s\"\n\
             t=0\n\
             output_to '' bash -c 'echo bare; exit 4' || t=$?\n\
             printf 'BARE=%s\\n' \"$t\"\n",
            block = block()
        ),
    );
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(&tree)
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "the block failed:\n{stdout}");
    assert!(
        stdout.contains("STATUS=3") && stdout.contains("BARE=4"),
        "the check's own status, with or without a file:\n{stdout}"
    );
    assert!(
        stdout.contains("said-out") && stdout.contains("said-err") && stdout.contains("bare"),
        "the output still reaches the log:\n{stdout}"
    );
    let copy = std::fs::read_to_string(tree.join("o.log")).unwrap_or_default();
    assert!(
        copy.contains("said-out") && copy.contains("said-err"),
        "and a copy, both streams, lands in the file:\n{copy}"
    );
    let _ = std::fs::remove_dir_all(&tree);
}

/// The call site tees the web suite's output and hands the file to the
/// keeper, so the roll-up the keeper sorts by is THIS run's.
#[test]
fn the_web_suite_check_hands_its_own_output_to_the_keeper() {
    let src = gate_sh();
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("check \"{CHECK}\"")))
        .unwrap_or_else(|| panic!("gate.sh no longer runs `{CHECK}`"));
    assert!(
        line.contains("output_to \"$web_out\""),
        "the web-suite check runs through output_to into $web_out: {line}"
    );
    assert!(
        src.lines()
            .any(|l| l.contains("keep_error_context") && l.contains("\"$web_out\"")),
        "the keeper is handed that same file"
    );
}

/// The call site: the keeper runs when, and only when, the web suite
/// failed, against a marker taken before it started.
#[test]
fn the_web_suite_check_is_followed_by_the_keeper_on_failure() {
    let src = gate_sh();
    let at = src
        .find(&format!("check \"{CHECK}\""))
        .unwrap_or_else(|| panic!("gate.sh no longer runs `{CHECK}`"));
    let before = &src[..at];
    let after = &src[at..];
    let next: Vec<&str> = after.lines().skip(1).take(4).collect();
    assert!(
        next.iter()
            .any(|l| l.contains("CHECK_STATUS") && l.contains("keep_error_context")),
        "the line after the web-suite check keeps its error context on failure: {next:#?}"
    );
    let marker = before.rfind("web_since=").unwrap_or(0);
    assert!(
        marker > 0 && at - marker < 600,
        "a marker is taken just before the web-suite check starts"
    );
    assert!(
        src.find(OPEN).is_some_and(|o| o < at),
        "the keeper is defined above its caller"
    );
}
