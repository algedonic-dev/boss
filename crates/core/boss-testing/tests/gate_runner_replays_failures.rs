//! A failed gate must be explainable from the RECEIPT, and from
//! `kubectl logs` when the receipt is not enough.
//!
//! THE FAILURE THIS PINS. `gate.log` is written to `/gate-target`, a
//! volume only the gate container mounts. When that container exits the
//! last reader of the file is gone, so a red verdict left a receipt
//! naming WHICH check failed and no way at all to learn WHY. Three
//! branches were called red by the gate-runner and then passed those
//! same checks run by hand; no theory could be tested, because the
//! evidence was destroyed with the pod every time (backlog 9c7ed804).
//!
//! THE SECOND FAILURE, one level deeper (backlog 4a4d1227). Gate-run
//! 9dc722d2 recorded `verdict: failed` with a 62-entry `checks` array
//! correctly naming `test` as the single failure — and nothing at all
//! about WHICH test. The name was one grep away in the pod log, and the
//! pod log is reaped; the receipt is the durable record. So the same
//! extractor that replays the failed sections to stdout now also writes
//! the part a reader ACTS on — the failing test names and the panic line
//! with its `file:line` — into the receipt's `fails`, bounded, with
//! every reduction stated.
//!
//! ONE PARSER, TWO OUTPUTS. The replay and `fails` are the same fact
//! ("what did the failed checks say"), so they are read once from
//! gate.log rather than twice (CLAUDE.md §9a): the block writes the
//! replay text to a file the runner later `cat`s, and merges `fails`
//! into the receipt before it is reported.
//!
//! WHY THIS TEST RUNS THE EXTRACTOR INSTEAD OF READING IT. The sibling
//! test `build_image_retry.rs` asserts on the TEXT of a shell block, and
//! that is precisely how the `/tmp/kaniko.log` break shipped: the text
//! said "retry on a transient fault", every assertion passed, and the
//! script still died on the first line because the kaniko image has no
//! `/tmp`. A property you can only check by reading is a property you
//! are guessing at. So this test extracts the block from `run.sh`
//! exactly as it ships and EXECUTES it against crafted logs.
//!
//! Skips rather than fails when `python3` is absent, so a machine
//! without it does not manufacture a red.

use boss_testing::repo_root;
use serde_json::Value;
use std::io::Write;
use std::process::Command;

/// The failure-detail extractor, lifted out of `run.sh` verbatim.
///
/// It lives inline in the runner because the pod receives exactly one
/// file — the `gate-runner-script` ConfigMap is built with a single
/// `--from-file=run.sh=...`, so a second script would simply not be
/// there. Lifting it here is what makes it testable anyway.
fn extractor_source() -> String {
    let run_sh = repo_root().join("infra/gate-runner/run.sh");
    let src = std::fs::read_to_string(&run_sh)
        .unwrap_or_else(|e| panic!("reading {}: {e}", run_sh.display()));

    const OPEN: &str =
        "python3 - \"$RECEIPT\" /gate-target/gate.log /gate-target/failed-checks.txt <<'PY'";
    let start = src.find(OPEN).unwrap_or_else(|| {
        panic!(
            "no failure-detail heredoc in run.sh — if the runner stopped extracting failed \
             checks, a red gate is unexplainable again (neither the receipt nor the log names \
             the failing test) and this test is the thing that should have said so"
        )
    });
    let after_open = &src[start + OPEN.len()..];
    let body_start = after_open
        .find('\n')
        .expect("heredoc opener has no line ending")
        + 1;
    let body = &after_open[body_start..];
    let end = body
        .find("\nPY\n")
        .expect("failure-detail heredoc is not terminated by a PY marker");
    body[..end].to_string()
}

/// What one run of the extractor produced.
struct Extracted {
    /// The replay text it wrote for the runner to print.
    replay: String,
    /// The receipt file as it stands afterwards, raw.
    receipt: String,
    /// Whether the block exited 0.
    ok: bool,
    /// Anything it said on stdout (diagnostics only).
    stdout: String,
}

impl Extracted {
    /// `fails` as the receipt now carries it. `None` when the receipt is
    /// unparseable or the key is absent — the two cases this packet
    /// exists to tell apart from an empty list.
    fn fails(&self) -> Option<Vec<String>> {
        let v: Value = serde_json::from_str(&self.receipt).ok()?;
        Some(
            v.get("fails")?
                .as_array()?
                .iter()
                .map(|e| e.as_str().unwrap_or("<not a string>").to_string())
                .collect(),
        )
    }

    fn fails_joined(&self) -> String {
        self.fails().map(|f| f.join("\n")).unwrap_or_default()
    }

    /// `fails_excerpt` as the receipt carries it — check name to the
    /// bounded text of what that check said (5708cbd5). `None` when the
    /// receipt is unparseable or the key is absent, for the same reason
    /// `fails` tells those apart from empty.
    fn fails_excerpt(&self) -> Option<serde_json::Map<String, Value>> {
        let v: Value = serde_json::from_str(&self.receipt).ok()?;
        v.get("fails_excerpt")?.as_object().cloned()
    }

    fn excerpt_of(&self, check: &str) -> String {
        self.fails_excerpt()
            .and_then(|m| m.get(check).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }
}

/// Run the extractor over a crafted receipt + log.
fn run_extractor(receipt: &str, log: &str) -> Extracted {
    let dir = boss_testing::scratch_dir("gate-detail");

    let script = dir.join("extract.py");
    let receipt_path = dir.join("receipt.json");
    let log_path = dir.join("gate.log");
    let replay_path = dir.join("failed-checks.txt");
    for (path, body) in [
        (&script, extractor_source()),
        (&receipt_path, receipt.to_string()),
        (&log_path, log.to_string()),
    ] {
        let mut fh = std::fs::File::create(path).expect("write scratch file");
        fh.write_all(body.as_bytes()).expect("write scratch file");
    }

    let out = Command::new("python3")
        .arg(&script)
        .arg(&receipt_path)
        .arg(&log_path)
        .arg(&replay_path)
        .output()
        .expect("python3 runs");
    let result = Extracted {
        replay: std::fs::read_to_string(&replay_path).unwrap_or_default(),
        receipt: std::fs::read_to_string(&receipt_path).unwrap_or_default(),
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn python3_missing() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
}

const LOG: &str = "\
::group::gate: fmt
formatting is fine
::endgroup::
::group::gate: clippy
warning: unused variable `x`
error: aborting due to 1 previous error
::endgroup::
::group::gate: test
running 40 tests
test boss::thing ... FAILED
";

/// A real cargo test failure, in the shape cargo actually prints it —
/// copied from the run that filed backlog 4a4d1227.
const CARGO_LOG: &str = "\
::group::gate: test
running 7 tests
test sweeps::other_thing ... ok
test every_sweep_spawner_guards_on_its_own_subject ... FAILED

failures:

---- every_sweep_spawner_guards_on_its_own_subject stdout ----

thread 'every_sweep_spawner_guards_on_its_own_subject' panicked at crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5:
expected the seven daily sweep spawners, found 6 — if sweeps moved, move this pin with them
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    every_sweep_spawner_guards_on_its_own_subject

test result: FAILED. 6 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
::endgroup::
";

/// The same shape, as rustc prints it since it began putting the
/// thread id in the panic header — copied verbatim from gate-run
/// a1664c7e (2026-09-22), the run whose verdict said there was no
/// panic line.
const CARGO_LOG_WITH_THREAD_ID: &str = "\
::group::gate: test
running 7 tests
test a_lints_own_verdict_is_not_reported_as_a_scanning_failure ... ok
test every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner ... FAILED

failures:

---- every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner stdout ----

thread 'every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner' (32614) panicked at crates/core/boss-testing/tests/a_lint_that_scanned_nothing_is_red.rs:327:5:
2 of 58 pre-flight lints REPORTED A FINDING on this tree (each exited nonzero).
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

failures:
    every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner

test result: FAILED. 6 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
::endgroup::
";

fn red_receipt(checks: &str) -> String {
    format!("{{\"verdict\":\"failed\",\"head\":\"abc\",\"mode\":\"full\",\"checks\":[{checks}]}}")
}

#[test]
fn it_replays_only_the_checks_that_failed() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = red_receipt(
        "{\"name\":\"fmt\",\"result\":\"pass\"},\
         {\"name\":\"clippy\",\"result\":\"fail\"},\
         {\"name\":\"test\",\"result\":\"fail\"}",
    );
    let got = run_extractor(&receipt, LOG);
    let out = &got.replay;

    assert!(
        got.ok,
        "extractor should succeed when it has failures to report: {}",
        got.stdout
    );
    assert!(
        out.contains("error: aborting due to 1 previous error"),
        "the failing check's actual error must reach the replay — that is the entire point:\n{out}"
    );
    assert!(
        out.contains("test boss::thing ... FAILED"),
        "every failed check is replayed, not just the first:\n{out}"
    );
    assert!(
        !out.contains("formatting is fine"),
        "a PASSING check's output must not be replayed. A full gate.log is mostly successful \
         build chatter, and burying the three lines that matter is the same defect as \
         printing nothing:\n{out}"
    );
}

/// An unterminated group means the check was still running when the log
/// ended — a timeout, an OOM kill, a node reset mid-gate. That is one of
/// the cases most worth explaining, so it must not be dropped for want
/// of a closing marker.
/// A CHECK THAT COULD NOT REACH THE NETWORK JUDGED NOTHING (478347ad).
/// Gate-run 7522c115: `web-suite` failed with 232 lines of bun's
/// "error: Unable to connect. Is the computer able to access the url?"
/// and no test or compile failure; two minutes later the registry
/// answered in 0.06 s. The receipt recorded verdict=failed against the
/// branch, and its own `fails` line already said "no cargo test failure
/// in this check's output" over 232 connect errors — the diagnosis was
/// on the record and nothing acted on it. A check whose failure is only
/// connect/resolve errors, with no test, panic or compile failure, is a
/// REFUSAL: the receipt says `refused` with a `refused_because` naming
/// the check and the count, in the shape gate.sh writes for its own
/// disk-floor refusal, so every reader that spares the branch on a
/// refusal spares this one too.
#[test]
fn a_check_that_only_failed_to_reach_the_network_is_a_refusal_not_a_red() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let mut connect_errors = String::new();
    for _ in 0..40 {
        connect_errors.push_str(
            "error: Unable to connect. Is the computer able to access the url?\n\
             \n  https://registry.npmjs.org/svelte\n\n",
        );
    }
    let log = format!(
        "::group::gate: web install\nbun install v1.2.0\n{connect_errors}error: InstallFailed\n::endgroup::\n"
    );
    let receipt = red_receipt(
        "{\"name\":\"fmt\",\"result\":\"pass\"},\
         {\"name\":\"web install\",\"result\":\"fail\"}",
    );
    let got = run_extractor(&receipt, &log);
    assert!(got.ok, "{}", got.stdout);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(
        v["verdict"], "refused",
        "only connect errors and no judged failure: the run was refused, not the branch:\n{}",
        got.receipt
    );
    let why = v["refused_because"].as_str().unwrap_or("");
    assert!(
        why.contains("web install") && why.contains("network") && why.contains("40"),
        "refused_because names the check, the cause and the count: {why}"
    );
    assert!(
        got.fails_joined().contains("no cargo test failure"),
        "the fails ladder still says what it saw: {}",
        got.fails_joined()
    );
}

/// The same connect errors BESIDE a judged failure are noise around a
/// red, not a refusal: a test that failed is a verdict on the branch.
#[test]
fn connect_errors_beside_a_judged_failure_stay_a_red() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let log = format!(
        "::group::gate: test\nerror: Unable to connect. Is the computer able to access the url?\n\
         error: Unable to connect. Is the computer able to access the url?\n{}",
        LOG.split("::group::gate: test\n").nth(1).unwrap_or("")
    );
    let receipt = red_receipt("{\"name\":\"test\",\"result\":\"fail\"}");
    let got = run_extractor(&receipt, &log);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(v["verdict"], "failed", "{}", got.receipt);
    assert!(v.get("refused_because").is_none(), "{}", got.receipt);
}

#[test]
fn it_keeps_what_a_killed_check_managed_to_say() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(&red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"), LOG);

    assert!(got.ok);
    assert!(
        got.replay.contains("test boss::thing ... FAILED"),
        "an unterminated ::group:: (the check was killed) must still be replayed:\n{}",
        got.replay
    );
}

/// "No receipt" and "no failed check" are the cases where the extractor
/// cannot explain anything, and both used to exit non-zero so run.sh's
/// `|| tail -200` fired. The fallback now lives INSIDE the one parser —
/// it writes the raw tail into the replay itself — because a reduction
/// decided by a shell `||` chain is a reduction nobody can see.
#[test]
fn it_falls_back_to_the_raw_tail_when_it_cannot_explain_anything() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }

    let got = run_extractor("{ this is not json", LOG);
    assert!(
        got.replay.contains("formatting is fine"),
        "an unreadable receipt must still leave the reader the raw log tail:\n{}",
        got.replay
    );
    assert_eq!(
        got.receipt, "{ this is not json",
        "an unreadable receipt is evidence too — it must not be rewritten or truncated"
    );

    let all_passed = red_receipt("{\"name\":\"fmt\",\"result\":\"pass\"}");
    let got = run_extractor(&all_passed, LOG);
    assert!(
        got.replay.contains("outside a check"),
        "a failed verdict with no failed check means the run died OUTSIDE a check (headroom \
         guard, crash before the receipt) — it must say which case it hit:\n{}",
        got.replay
    );
    assert!(
        got.replay.contains("formatting is fine"),
        "...and still hand over the raw tail:\n{}",
        got.replay
    );
    assert!(
        got.fails_joined().contains("outside a check"),
        "the receipt is the durable record, so it must carry that finding too, not just the \
         pod log:\n{}",
        got.fails_joined()
    );
}

/// A check named in the receipt but absent from the log is the signal
/// that gate.sh changed its grouping. Saying so is what stops this
/// extractor from rotting into silence.
#[test]
fn it_says_so_when_a_failed_check_has_no_block() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(&red_receipt("{\"name\":\"web\",\"result\":\"fail\"}"), LOG);

    assert!(got.ok);
    assert!(
        got.replay.contains("no ::group:: block"),
        "an absent block must be reported, not rendered as an empty section:\n{}",
        got.replay
    );
    assert!(
        got.fails_joined().contains("no ::group:: block"),
        "and the receipt must say so as well — it is the copy that outlives the pod:\n{}",
        got.fails_joined()
    );
}

// ---------------------------------------------------------------------
// backlog 4a4d1227 — the receipt names the failing TEST
// ---------------------------------------------------------------------

/// `fails` was `null` on the red receipt that filed this packet, and
/// `[]` on the green ones beside it — so a reader could not tell "no
/// failures" from "nobody wrote the field". The field is now always
/// present, and empty means empty.
#[test]
fn a_green_receipt_carries_an_empty_fails_not_a_missing_one() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let green = "{\"verdict\":\"green\",\"head\":\"abc\",\"mode\":\"full\",\
                 \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\"}]}";
    let got = run_extractor(green, "::group::gate: fmt\nfine\n::endgroup::\n");

    assert!(got.ok, "green must not be an error path: {}", got.stdout);
    assert_eq!(
        got.fails().as_deref(),
        Some(&[][..]),
        "a green receipt must carry `fails: []` — present and empty. `null` on one verdict and \
         `[]` on the other is the asymmetry this packet is about:\n{}",
        got.receipt
    );
}

/// The whole point: the name a reader acts on, in the durable record.
#[test]
fn a_red_receipt_names_the_failing_test_and_where_it_panicked() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG,
    );
    let fails = got.fails_joined();

    assert!(got.ok, "extractor failed: {}", got.stdout);
    assert!(
        fails.contains("every_sweep_spawner_guards_on_its_own_subject"),
        "the failing TEST name is what the reader acts on; the check name only gets them to \
         `test`:\n{fails}"
    );
    assert!(
        fails.contains("crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5"),
        "the panic's file:line is the other half — without it the name still has to be \
         located:\n{fails}"
    );
    assert!(
        fails.contains("found 6"),
        "the panic MESSAGE says what was actually wrong:\n{fails}"
    );
    assert!(
        fails.contains("test"),
        "the failing check stays named, so `fails` reads on its own:\n{fails}"
    );
}

/// Every field the receipt already carried keeps its place and meaning —
/// 62 named checks with per-check seconds is what made the last
/// diagnosis a six-minute read. This change is additive.
#[test]
fn it_leaves_every_other_receipt_field_alone() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    const RICH: &str = r#"{"verdict":"failed","mode":"full","scope":"","head":"68ef2957",
        "dirty":false,"host":"gate-x","ci":true,"free_gb":61,"unverifiable":[],
        "checks":[{"name":"fmt","result":"pass","seconds":3},
                  {"name":"test","result":"fail","seconds":96}]}"#;
    let got = run_extractor(RICH, CARGO_LOG);

    let before: Value = serde_json::from_str(RICH).expect("fixture parses");
    let after: Value = serde_json::from_str(&got.receipt).expect("receipt still parses");
    for (k, v) in before.as_object().expect("object") {
        assert_eq!(
            after.get(k),
            Some(v),
            "field `{k}` must survive the merge untouched:\n{}",
            got.receipt
        );
    }
    assert!(
        after.get("fails").is_some(),
        "…and `fails` (with `fails_excerpt`) is the only addition:\n{}",
        got.receipt
    );
}

/// The bound is deliberate and STATED. A suite failing two hundred tests
/// must not write a receipt nobody can read — and must not quietly drop
/// the rest either, which is the `docker build -q` / last-16-KB defect
/// class (CLAUDE.md §Diagnosis).
#[test]
fn it_caps_the_named_tests_and_says_how_many_it_left_out() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let mut log = String::from("::group::gate: test\nrunning 200 tests\n");
    for i in 0..40 {
        log.push_str(&format!("test suite::case_{i} ... FAILED\n"));
    }
    log.push_str("\nfailures:\n\n");
    for i in 0..40 {
        log.push_str(&format!("    suite::case_{i}\n"));
    }
    log.push_str("test result: FAILED. 160 passed; 40 failed\n::endgroup::\n");

    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        &log,
    );
    let fails = got.fails().expect("fails is present");

    assert!(
        fails.len() <= 12,
        "the receipt must stay readable — {} entries is not a receipt:\n{fails:#?}",
        fails.len()
    );
    let joined = fails.join("\n");
    assert!(
        joined.contains("40"),
        "the COUNT of what was left out has to be in the record, or the reduction is silent — \
         the exact defect the 778 KB log truncated to 16 KB taught us:\n{joined}"
    );
    assert!(
        fails.iter().all(|e| e.len() <= 600),
        "no single entry may be unbounded either:\n{joined}"
    );
}

/// Not every check is cargo-shaped. A lint, `svelte-check`, a shell
/// script — the extractor must hand over what the check actually said
/// and SAY that it could not parse it, rather than inventing a test
/// name it never saw.
#[test]
fn a_non_cargo_check_gets_its_own_words_not_an_invented_test_name() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    const SVELTE: &str = "\
::group::gate: web-svelte-check
svelte-check found 1 error
Error: src/it/yard/yard.svelte:12:3 Type 'string' is not assignable to 'number'
::endgroup::
";
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-svelte-check\",\"result\":\"fail\"}"),
        SVELTE,
    );
    let fails = got.fails_joined();

    assert!(got.ok, "extractor failed: {}", got.stdout);
    assert!(
        fails.contains("yard.svelte:12:3"),
        "a non-cargo check's own error line is still what the reader acts on:\n{fails}"
    );
    assert!(
        !fails.contains("panicked"),
        "and nothing may be claimed about a shape that was never there:\n{fails}"
    );
}

/// THE THIRD FAILURE, one level deeper again (backlog 5708cbd5). Train
/// #361's red gate (2026-09-14) recorded `test: the_real_run_refuses…
/// - FAILED, with no panic line for it in this check's output` — the
/// test named, the reason absent, because the assertion text lived only
/// in the pod log's replay and the pod log is reaped. So the SAME lines
/// the replay prints for a failed check ride the receipt as
/// `fails_excerpt: {check: text}`, beside `fails`: the record then says
/// WHY, and the red-train alert can carry it without kubectl.
#[test]
fn a_red_receipt_carries_each_failed_checks_excerpt_and_only_theirs() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let receipt = red_receipt(
        "{\"name\":\"fmt\",\"result\":\"pass\"},\
         {\"name\":\"clippy\",\"result\":\"fail\"},\
         {\"name\":\"test\",\"result\":\"fail\"}",
    );
    let got = run_extractor(&receipt, LOG);
    assert!(got.ok, "extractor failed: {}", got.stdout);

    let excerpt = got
        .fails_excerpt()
        .expect("fails_excerpt is present on a red receipt");
    let mut keys: Vec<&String> = excerpt.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["clippy", "test"],
        "one excerpt per FAILED check — the passing one has nothing to explain:\n{}",
        got.receipt
    );
    assert!(
        got.excerpt_of("clippy")
            .contains("error: aborting due to 1 previous error"),
        "the failing check's own words are the excerpt:\n{}",
        got.receipt
    );
    assert!(
        got.excerpt_of("test")
            .contains("test boss::thing ... FAILED"),
        "every failed check gets its excerpt, not just the first:\n{}",
        got.receipt
    );
    assert!(
        !got.receipt.contains("formatting is fine"),
        "a passing check's output never reaches the receipt:\n{}",
        got.receipt
    );
}

/// The excerpt is the replay's selection, not a new one: the panic
/// block with its `file:line` and message is in it verbatim, so the
/// alert that attaches it says what the operator otherwise reads by
/// `kubectl logs` — which the forge, the yard and orient cannot run.
#[test]
fn the_excerpt_carries_the_panic_block_the_replay_prints() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let excerpt = got.excerpt_of("test");
    for needle in [
        "---- every_sweep_spawner_guards_on_its_own_subject stdout ----",
        "panicked at crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5:",
        "expected the seven daily sweep spawners, found 6",
    ] {
        assert!(
            excerpt.contains(needle),
            "the excerpt must carry `{needle}` — the same lines the replay prints:\n{excerpt}"
        );
        assert!(
            got.replay.contains(needle),
            "…and the replay still prints it (one selection, two outputs):\n{}",
            got.replay
        );
    }
}

/// THE PANIC LINE CARRIES A THREAD ID, AND THE PARSER MUST STILL READ
/// IT (backlog 2dc742c1, 2026-09-22). Gate-run a1664c7e recorded
/// `test: every_preflight_lint_scans_something… - FAILED, with no panic
/// line for it in this check's output` while the panic — its file:line
/// and its whole assertion message — sat in the SAME receipt's
/// `fails_excerpt`. The verdict-naming path missed a panic it was
/// already holding: rustc prints `thread '<name>' (<tid>) panicked at
/// <file:line:col>:` and the parser's pattern allowed nothing between
/// the closing quote and `panicked`. CLAUDE.md §Diagnosis: a verdict
/// someone must go re-derive is not a verdict — and this one had the
/// answer in its hand.
#[test]
fn a_panic_line_with_a_thread_id_is_still_attributed_to_its_test() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG_WITH_THREAD_ID,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails_joined();
    assert!(
        !fails.contains("no panic line"),
        "the panic is right there in the check's output; saying there is none sends a \
         reader to re-derive what the record already holds:\n{fails}"
    );
    assert!(
        fails.contains("every_preflight_lint_scans_something_or_says_why_it_is_not_a_scanner")
            && fails.contains("a_lint_that_scanned_nothing_is_red.rs:327:5:")
            && fails.contains("2 of 58 pre-flight lints REPORTED A FINDING"),
        "`fails` names the test, where it panicked and what it said:\n{fails}"
    );
}

/// The shape of an assertion whose message is a HEADER line and then
/// the output it asserted on — copied verbatim from gate-run 2510ac65
/// (2026-09-24), whose `fails` entry ended at the header's colon.
const CARGO_LOG_MULTI_LINE_MESSAGE: &str = "\
::group::gate: test
running 22 tests
test a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time ... FAILED

failures:

---- a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time stdout ----

thread 'a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time' (118329) panicked at crates/core/boss-testing/tests/boss_api_sh.rs:1149:5:
the failure names the call, the elapsed time, and that relaunching is safe:
curl: (7) Failed to connect to sor.test port 7900: No route to host
boss-api: GET /api/jobs: the jobs API refused every connection for 0s (1 attempts, curl exit 7; waited out for up to 0s in case it was a rollout) \u{2014} nothing was sent, so nothing landed and relaunching is safe

note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time

test result: FAILED. 21 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s
::endgroup::
";

/// A PANIC MESSAGE IS EVERY LINE UP TO THE NOTE, NOT THE FIRST ONE
/// (backlog b53dca8c, 2026-09-24). Gate-run 2510ac65's `fails` read
/// "panicked at …boss_api_sh.rs:1149:5: the failure names the call, the
/// elapsed time, and that relaunching is safe:" — the assertion's own
/// header, ending at the colon that introduces what it saw. What it saw
/// ("refused every connection for 0s") was the second and third lines,
/// so the verdict named the test and the line and not the cause; only
/// the excerpt beside it did. `fails` is what the alert and the yard
/// quote, and a verdict someone must go re-derive is not a verdict.
#[test]
fn a_multi_line_panic_message_keeps_its_body_in_fails() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG_MULTI_LINE_MESSAGE,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails().expect("fails is present");
    let entry = fails
        .iter()
        .find(|e| e.contains("a_roll_that_outlasts_the_window_fails_naming_the_elapsed_time"))
        .unwrap_or_else(|| panic!("the failing test is named:\n{fails:#?}"));
    assert!(
        entry.contains("boss_api_sh.rs:1149:5")
            && entry.contains("the failure names the call")
            && entry.contains("refused every connection for 0s"),
        "`fails` carries the message's body — the cause — not only its header:\n{entry}"
    );
    assert!(
        !entry.contains("RUST_BACKTRACE") && !entry.contains("test result"),
        "the message ends where cargo's own note begins:\n{entry}"
    );
    assert_eq!(
        fails.len(),
        1,
        "still one entry per failing test:\n{fails:#?}"
    );
}

/// A green receipt carries `fails_excerpt: {}` — present and empty, for
/// the reason `fails` is `[]` and never `null`: "nothing failed" and
/// "nobody wrote the field" must not look the same to a reader.
#[test]
fn a_green_receipt_carries_an_empty_excerpt_not_a_missing_one() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let green = "{\"verdict\":\"green\",\"head\":\"abc\",\"mode\":\"full\",\
                 \"checks\":[{\"name\":\"fmt\",\"result\":\"pass\"}]}";
    let got = run_extractor(green, "::group::gate: fmt\nfine\n::endgroup::\n");
    assert!(got.ok, "green must not be an error path: {}", got.stdout);
    assert_eq!(
        got.fails_excerpt().map(|m| m.len()),
        Some(0),
        "a green receipt must carry `fails_excerpt: {{}}`:\n{}",
        got.receipt
    );
}

/// THE BOUND. The receipt rides the record-verdict step's metadata and
/// the runner passes it as ONE argv string to the report-back (a 128 KB
/// ceiling per argument on Linux), so an excerpt is capped per check
/// (~6 KB) and in all (~24 KB) — and each cap says what it left out,
/// because a silent reduction is the 778 KB-log-tailed-to-16 KB defect
/// wearing a different hat (CLAUDE.md §Diagnosis).
#[test]
fn the_excerpt_is_bounded_per_check_and_in_all_and_says_so() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    // Eight failed checks, each 300 lines of 100 characters: 30 KB per
    // check, 240 KB in all — a receipt nobody could pass along.
    let names: Vec<String> = (0..8).map(|i| format!("check-{i}")).collect();
    let mut log = String::new();
    for n in &names {
        log.push_str(&format!("::group::gate: {n}\n"));
        for i in 0..300 {
            log.push_str(&format!(
                "{n} line {i:03} {}\n",
                "x".repeat(100 - 16 - n.len())
            ));
        }
        log.push_str(&format!("error: {n} failed at the end\n::endgroup::\n"));
    }
    let checks = names
        .iter()
        .map(|n| format!("{{\"name\":\"{n}\",\"result\":\"fail\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    let got = run_extractor(&red_receipt(&checks), &log);
    assert!(got.ok, "extractor failed: {}", got.stdout);

    let excerpt = got.fails_excerpt().expect("fails_excerpt is present");
    assert_eq!(
        excerpt.len(),
        8,
        "every failed check has an entry, even one that only says it was omitted:\n{}",
        got.receipt
    );
    let total: usize = excerpt
        .values()
        .map(|v| v.as_str().map_or(0, str::len))
        .sum();
    assert!(
        total <= 26_000,
        "the whole excerpt must stay far under the transport's ceiling — {total} chars is not \
         a receipt:\n{}",
        got.receipt
    );
    for (name, text) in &excerpt {
        let text = text.as_str().expect("excerpt is a string");
        assert!(
            text.len() <= 6_600,
            "no single check's excerpt may be unbounded — {name} is {} chars",
            text.len()
        );
        assert!(
            text.contains("omitted"),
            "every reduction states itself — {name}'s excerpt was cut and does not say so:\n{text}"
        );
    }
    let first = got.excerpt_of("check-0");
    assert!(
        first.contains("error: check-0 failed at the end"),
        "a bounded excerpt still spends its budget on the failure line, not the chatter \
         above it:\n{first}"
    );
}

/// THE NOISE NAMED INSTEAD OF THE VERDICT (backlog 3a6f61d6). Measured
/// 2026-09-18 on red-train alert 64a17c4b (train ccaf018e, gate
/// c924dbe0): for the web-suite check the alert said "no cargo test
/// failure in this check's output; 264 error line(s), first 5:" and
/// quoted five `error: Unable to connect. Is the computer able to
/// access the url?` lines — bun's proxy noise for the backend the mocked
/// runner never starts, printed in every PASSING run too — while the
/// same excerpt held Playwright's own verdict: the `✘ 15 …
/// interaction-crawl … shard 2/4` line, `- Expected - 1 / + Received +
/// 5`, `[/ux/views] id: not clickable: locator.click: Timeout 3000ms
/// exceeded`, and `2 failed / 102 passed`. The operator had to pull the
/// Job log to learn it was a 3 s click timeout. So for a check whose
/// output carries Playwright's markers, `fails` ranks THEM: the ✘ lines,
/// the `N failed` roll-up, the `Error:` line, the Expected/Received diff
/// as one entry. The fixture is that excerpt, written by hand from those
/// lines. (The connect noise itself was deleted at its source on
/// 2026-09-19, 82b87a09 — the mocked dev-server answers a miss locally —
/// so the filter this test once pinned went with it; what remains
/// pinned is the ranking.)
const WEB_SUITE_RED: &str = "\
::group::gate: web-suite
  ✓  14 [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 1/4 (28.1s)
  ✘  15 [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 2/4 (31.4s)
  ✓  16 [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 3/4 (27.9s)
  ✘  17 [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 4/4 (30.2s)


  1) [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 2/4

    Error: expect(received).toEqual(expected) // deep equality

    - Expected  - 1
    + Received  + 5

      Array [
    -   Array [],
    +   \"[/ux/views] id: not clickable: locator.click: Timeout 3000ms exceeded.\",
    +   \"[/ux/views] id: not clickable: locator.click: Timeout 3000ms exceeded.\",
    +   \"[/ux/views] id: not clickable: locator.click: Timeout 3000ms exceeded.\",
    +   \"[/ux/views] id: not clickable: locator.click: Timeout 3000ms exceeded.\",
    +   \"[/ux/views] id: not clickable: locator.click: Timeout 3000ms exceeded.\",
      ]

      64 |
    > 65 |   expect(problems).toEqual([]);
         |                    ^

  2 failed
    [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 2/4
    [chromium] › tests/mocked/interaction-crawl.spec.ts:40:5 › interaction crawl › shard 4/4
  102 passed (2.1m)
::endgroup::
";

#[test]
fn a_web_suite_red_names_the_failing_spec_and_its_diff() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite\",\"result\":\"fail\"}"),
        WEB_SUITE_RED,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails().expect("fails is on the receipt");
    let joined = fails.join("\n");

    let quoted: Vec<&String> = fails.iter().filter(|e| e.contains("| ")).collect();
    assert!(
        quoted.first().is_some_and(|e| e.contains("✘")
            && e.contains("interaction-crawl")
            && e.contains("shard 2/4")),
        "the first quoted line is the failing spec:\n{joined}"
    );
    assert!(
        joined.contains("2 failed"),
        "the run's own roll-up is on the receipt:\n{joined}"
    );
    assert!(
        joined.contains("Timeout 3000ms exceeded"),
        "the Received value — the 3 s click timeout the operator had to pull the Job log \
         for — rides the receipt:\n{joined}"
    );
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(
        v["verdict"], "failed",
        "two failed specs is a verdict on the branch:\n{}",
        got.receipt
    );
}

/// A `✘` IS NOT A FAILURE UNTIL PLAYWRIGHT SAYS SO (backlog 42981848).
/// Red gate-run 5b5a04d8 (2026-09-28) quoted "9 playwright verdict
/// line(s), first 5", and the first four were
/// `a-page-error-fails-the-spec.mocked.spec.ts` :25, :34, :52 and :62 -
/// each marked `test.fail(true, …)`, which the list reporter prints with
/// a `✘` and counts PASSED - while `it-map-routes.mocked.spec.ts:163`,
/// one of the two specs that really timed out, was cut. The `✘` line
/// alone cannot say whether a failure was expected; Playwright's own
/// roll-up can: its `N failed` block lists exactly the tests whose
/// outcome was unexpected (an expected failure is in `N passed`, a
/// flaky retry in `N flaky`). The fixture is that run's log in its own
/// shape - the ✘ and roll-up lines copied from the receipt, including
/// the trailing space the roll-up prints after each title - with the
/// passing specs between cut to one.
const EXPECTED_FAILURE_RED: &str = "\
::group::gate: web-suite (unit+build+mocked)
  ✓     5 [chromium] › tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:43:3 › a declared throw › passes when it is the throw the spec declared (212ms)
  ✘     6 [chromium] › tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:25:1 › an uncaught throw fails a spec that asserts nothing about it (205ms)
  ✘    11 [chromium] › tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:34:1 › an unhandled rejection fails the spec too (129ms)
  ✘    13 [chromium] › tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:52:3 › a declared throw › still fails the spec when a DIFFERENT throw arrives beside it (182ms)
  ✘    14 [chromium] › tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:62:3 › a declared throw › fails the spec when it never arrives, so the declaration cannot outlive its throw (47ms)
  ✓    15 [chromium] › tests/mocked/pages.mocked.spec.ts:9:3 › a page renders (1.2s)
  ✘   585 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure (4.0m)
  ✘   586 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means (4.0m)
  ✓  1158 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:532:3 › /it/design — empty, failed and malformed reads › CURRENT, gap 2 (8c0e11d8): the empty claim is unconditional (1.2s)


  1) [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure

    Test timeout of 240000ms exceeded while setting up \"page\".

  2) [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means

    Test timeout of 240000ms exceeded while setting up \"page\".

  2 failed
    [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure
    [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means
  1169 passed (9.1m)
error: script \"test:mocked\" exited with code 1
::endgroup::
";

#[test]
fn an_expected_failure_is_not_quoted_as_a_verdict_and_the_real_ones_are() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite (unit+build+mocked)\",\"result\":\"fail\"}"),
        EXPECTED_FAILURE_RED,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails().expect("fails is on the receipt");
    let joined = fails.join("\n");
    let quoted: Vec<&String> = fails.iter().filter(|e| e.contains("| ✘")).collect();

    assert!(
        !joined.contains("a-page-error-fails-the-spec"),
        "a test.fail spec that failed as told PASSED - Playwright counts it in `1169 passed` - \
         and must not be quoted as a verdict:\n{joined}"
    );
    assert_eq!(
        quoted.len(),
        2,
        "exactly the two specs Playwright's roll-up counts failed are quoted:\n{joined}"
    );
    assert!(
        quoted[0].contains("it-design-controls.mocked.spec.ts:522:3")
            && quoted[0].contains("(4.0m)")
            && quoted[1].contains("it-map-routes.mocked.spec.ts:163:1")
            && quoted[1].contains("(4.0m)"),
        "each real failure is quoted whole, its duration with it - the 4.0m is what says \
         timeout:\n{joined}"
    );
    assert!(
        joined.contains("4 ✘ line(s) not quoted") && joined.contains("test.fail"),
        "the ✘ lines set aside are counted and the reason stated, not dropped in \
         silence:\n{joined}"
    );
    assert!(
        got.replay
            .contains("a-page-error-fails-the-spec.mocked.spec.ts:25:1"),
        "the replay is not reduced - it still holds every line the check printed:\n{}",
        got.replay
    );
}

/// With no roll-up - a run killed before Playwright's epilogue - nothing
/// on the page can tell an expected failure from a real one, so every
/// `✘` stays quoted and the receipt SAYS it could not tell them apart,
/// rather than guessing either way.
#[test]
fn without_a_roll_up_every_cross_is_quoted_and_the_receipt_says_why() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let (head, _) = EXPECTED_FAILURE_RED
        .split_once("\n\n\n  1) ")
        .expect("the fixture has a failure detail");
    let log = format!("{head}\n::endgroup::\n");
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite (unit+build+mocked)\",\"result\":\"fail\"}"),
        &log,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let joined = got.fails_joined();
    assert!(
        joined.contains("| ✘ 6 [chromium]") && joined.contains("it-design-controls"),
        "every ✘ is still quoted, in order:\n{joined}"
    );
    assert!(
        joined.contains("no Playwright roll-up"),
        "…and the receipt says why it could not set the expected ones aside:\n{joined}"
    );
}

/// A TIMEOUT'S CAUSE RIDES BESIDE ITS `✘` (backlog a766e20d). The same
/// red gate-run 5b5a04d8, as its receipt's own `fails_excerpt` carried
/// the web suite - copied from that receipt verbatim, trailing spaces
/// (`\x20`) and all, dropping only the excerpt's "264 line(s) omitted"
/// head. Both real failures timed out, and the line that says so,
/// `Test timeout of 240000ms exceeded while setting up "page".`, matched
/// no verdict pattern, so `fails` never quoted the CAUSE - of the kind
/// of red that made 8 same-head reds in 7 days. Meanwhile the two
/// `Error Context: test-results/…` path lines were quoted as errors,
/// because `RE_ERROR` takes `Error` followed by a space: a path, not a
/// verdict, spending two of the five quoted lines.
const TIMEOUT_RED: &str = "\
::group::gate: web-suite (unit+build+mocked)
  ✘   585 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure (4.0m)
  ✘   586 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means (4.0m)
  ✓  1158 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:532:3 › /it/design — empty, failed and malformed reads › CURRENT, gap 2 (8c0e11d8): the empty claim is unconditional — the page reads only the design-doc stations (1.2s)
  ✓  1157 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:184:1 › a section with no reading is drawn as a hollow tube, and an undeclared one stays dashed (1.6s)
  ✓  1159 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:550:3 › /it/design — empty, failed and malformed reads › a refused review-queue read is a failure line in the page's words, never the empty state (859ms)
  ✓  1160 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:206:1 › a routes read that fails draws the stations and no guessed track, and says so (915ms)
  ✓  1161 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:561:3 › /it/design — empty, failed and malformed reads › a review-queue read that never answers (network down) is a failure line too (926ms)
  ✓  1162 [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:214:1 › a drawn section with no rate yet opens a panel that says what declares it (1.3s)
  ✓  1163 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:570:3 › /it/design — empty, failed and malformed reads › CURRENT, U3 (UNFILED): a failed read shows the fallback header, whose eyebrow now says IT (839a7f0f) and whose subtitle names the deleted corpus (1.3s)
  ✓  1164 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:583:3 › /it/design — empty, failed and malformed reads › U2 (3bbb194a) is FIXED — a failed review-queue read leaves WORKING and OUT standing: the decided read is made (944ms)
  ✓  1165 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:597:3 › /it/design — empty, failed and malformed reads › both reads refused: two failure lines, each in its own words and with its own Retry (861ms)
  ✓  1166 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:609:3 › /it/design — empty, failed and malformed reads › U4 (3bbb194a) is FIXED — the review queue's Retry re-runs its read, and only its read (971ms)
  ✓  1167 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:633:3 › /it/design — empty, failed and malformed reads › a review-queue Retry that fails again stays the failure line, with its Retry (1.4s)
  ✓  1168 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:645:3 › /it/design — empty, failed and malformed reads › a refused decided read is its own failure line, and the review queue still renders (912ms)
  ✓  1169 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:657:3 › /it/design — empty, failed and malformed reads › U4 (3bbb194a) is FIXED — the decided panel's Retry re-runs its read, and only its read (844ms)
  ✓  1170 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:681:3 › /it/design — empty, failed and malformed reads › U1 (67825067): a malformed 200 from the review queue is the failure line, never \"Nothing is waiting\" (647ms)
  ✓  1171 [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:698:3 › /it/design — empty, failed and malformed reads › U1 (67825067): a malformed 200 from the decided station is its own failure line, and the review queue still renders (684ms)


  1) [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure\x20

    Test timeout of 240000ms exceeded while setting up \"page\".

    Error Context: test-results/it-design-controls.mocked--648ea--empty-state-and-no-failure-chromium/error-context.md

  2) [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means\x20

    Test timeout of 240000ms exceeded while setting up \"page\".

    Error Context: test-results/it-map-routes.mocked-a-rou-9f9d8-he-key-says-what-that-means-chromium/error-context.md

  2 failed
    [chromium] › tests/mocked/it-design-controls.mocked.spec.ts:522:3 › /it/design — empty, failed and malformed reads › an empty review queue paints the empty state and no failure\x20
    [chromium] › tests/mocked/it-map-routes.mocked.spec.ts:163:1 › a route only the moves record supports is drawn dashed red, and the key says what that means\x20
  1169 passed (9.1m)
error: script \"test:mocked\" exited with code 1
::endgroup::
";

#[test]
fn a_timed_out_specs_reason_is_quoted_beside_it_and_an_error_context_path_is_not() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite (unit+build+mocked)\",\"result\":\"fail\"}"),
        TIMEOUT_RED,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails().expect("fails is on the receipt");
    let joined = fails.join("\n");
    let quoted: Vec<&String> = fails.iter().filter(|e| e.contains("| ✘")).collect();
    let reason = "Test timeout of 240000ms exceeded while setting up \"page\".";

    assert_eq!(quoted.len(), 2, "both real failures are quoted:\n{joined}");
    assert!(
        quoted[0].contains("it-design-controls.mocked.spec.ts:522:3") && quoted[0].contains(reason),
        "the first timeout's cause is quoted on its own ✘ entry:\n{joined}"
    );
    assert!(
        quoted[1].contains("it-map-routes.mocked.spec.ts:163:1") && quoted[1].contains(reason),
        "…and the second's on its own:\n{joined}"
    );
    assert!(
        !joined.contains("Error Context:"),
        "an `Error Context:` line is the path of a file, not a verdict, and is never quoted \
         as one:\n{joined}"
    );
    assert!(
        joined.contains("3 playwright verdict line(s)"),
        "the two ✘ lines (each with its cause) and the roll-up, and nothing else:\n{joined}"
    );
    assert!(
        got.replay
            .contains("Error Context: test-results/it-map-routes"),
        "the replay is not reduced - the paths are still in it:\n{}",
        got.replay
    );
}

/// The control for the `Error Context:` refusal: a real `Error:` line in
/// the same failure block is still quoted, so the refusal is of that
/// one label and not of every line beginning `Error`.
#[test]
fn a_real_error_line_beside_an_error_context_is_still_quoted() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let log = TIMEOUT_RED.replacen(
        "\n    Error Context: test-results/it-map-routes",
        "\n    Error: expect(locator).toBeVisible() failed\n\n    Error Context: test-results/it-map-routes",
        1,
    );
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite (unit+build+mocked)\",\"result\":\"fail\"}"),
        &log,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let joined = got.fails_joined();
    assert!(
        joined.contains("| Error: expect(locator).toBeVisible() failed"),
        "a real error line is a verdict line:\n{joined}"
    );
    assert!(!joined.contains("Error Context:"), "{joined}");
}

/// The same run's `fails_context` spent its budget the same way: the four
/// `test.fail` specs' error contexts came first, and the one it omitted
/// was `it-map-routes.mocked.spec.ts:163`'s. Every context is still kept
/// - the replay has them whole - but the specs Playwright counts failed
/// are placed first, so a bounded receipt holds THEIR page.
#[test]
fn the_error_contexts_of_the_real_failures_come_first() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let check = "web-suite (unit+build+mocked)";
    let located = |spec: &str, loc: &str| {
        context_block(spec, 60).replace(&format!("tests/mocked/{spec}.mocked.spec.ts:85:3"), loc)
    };
    let body: String = [
        located(
            "a-page-error-1",
            "tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:25:1",
        ),
        located(
            "a-page-error-2",
            "tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:34:1",
        ),
        located(
            "a-page-error-3",
            "tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:52:3",
        ),
        located(
            "a-page-error-4",
            "tests/mocked/a-page-error-fails-the-spec.mocked.spec.ts:62:3",
        ),
        located(
            "it-design-controls",
            "tests/mocked/it-design-controls.mocked.spec.ts:522:3",
        ),
        located(
            "it-map-routes",
            "tests/mocked/it-map-routes.mocked.spec.ts:163:1",
        ),
    ]
    .concat();
    let (receipt, dir) = receipt_with_evidence(check, Some(&body));
    let got = run_extractor(&receipt, EXPECTED_FAILURE_RED);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.ok, "extractor failed: {}", got.stdout);

    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    let context = v["fails_context"][check].as_str().unwrap_or_default();
    assert!(
        context.contains("it-map-routes.mocked.spec.ts:163:1")
            && context.contains("it-design-controls.mocked.spec.ts:522:3"),
        "both real failures' pages ride the receipt:\n{context}"
    );
    let real = context
        .find("it-map-routes.mocked.spec.ts:163:1")
        .unwrap_or(usize::MAX);
    assert!(
        context
            .find("a-page-error-fails-the-spec.mocked.spec.ts")
            .is_none_or(|expected| expected > real),
        "…ahead of the expected failures':\n{context}"
    );
    assert!(
        context.contains("counts failed come first"),
        "the order is stated:\n{context}"
    );
    assert!(
        context.contains("6 spec context(s) in all"),
        "the count still names every context there was:\n{context}"
    );
    assert!(
        got.replay
            .contains("a-page-error-fails-the-spec.mocked.spec.ts:25:1"),
        "the replay keeps every context whole"
    );
}

/// THE BUDGET GOES TO THE MARKER AND THE LAST WORDS (backlog 4077889a).
/// For a web-suite red the first failure marker is the per-spec `✘`
/// near the top of Playwright's list, and the `Error:`, the
/// Expected/Received diff and the `N failed` roll-up are at the END;
/// a head-only cut from the marker held the passing-spec chatter
/// between and lost the verdict. The fixture puts 100 passing-spec
/// lines between the `✘` line and the failure detail - more than the
/// excerpt's character budget - so the budget must be spent on the
/// marker and the check's last words, and the middle cut must state
/// itself. (Until 2026-09-19 the bulk in this fixture was the mocked
/// runner's connect noise, filtered before the window; 82b87a09 deleted
/// the noise at its source and the filter with it, and the window is
/// the raw tail again.)
#[test]
fn the_web_suite_excerpt_holds_the_failing_spec_line_and_the_verdict_below_it() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let (head, tail) = WEB_SUITE_RED
        .split_once("\n\n  1) ")
        .expect("the fixture has a failure detail");
    let between: String = (0..100)
        .map(|i| {
            format!(
                "  ✓  {} [chromium] › tests/mocked/pages.spec.ts:9:3 › page {i} renders \
                 (1.2s)\n",
                i + 18
            )
        })
        .collect();
    let log = format!("{head}\n{between}\n  1) {tail}");
    let got = run_extractor(
        &red_receipt("{\"name\":\"web-suite\",\"result\":\"fail\"}"),
        &log,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let excerpt = got.excerpt_of("web-suite");
    assert!(
        excerpt.contains("✘  15 [chromium]") && excerpt.contains("shard 2/4 (31.4s)"),
        "the excerpt holds the per-spec ✘ line:\n{excerpt}"
    );
    assert!(
        excerpt.contains("Timeout 3000ms exceeded") && excerpt.contains("2 failed"),
        "…and still the verdict below it:\n{excerpt}"
    );
    assert!(
        excerpt.contains("omitted between the first failure marker and the check's last words"),
        "the cut between them states itself:\n{excerpt}"
    );
    assert!(
        got.replay.contains("shard 2/4 (31.4s)") && got.replay.contains("2 failed"),
        "the replay holds the ✘ line and the verdict:\n{}",
        got.replay
    );
}

/// A `--no-fail-fast` run (backlog 3bef4198): the failing binary's block,
/// then every later binary's passing output, then cargo's own roll-call
/// of the targets that failed.
fn no_fail_fast_log(passing_lines: usize) -> String {
    let mut log = String::from(
        "::group::gate: test\n\
         \x20    Running tests/sweep_spawn_guards.rs (target/debug/deps/sweep_spawn_guards-0a1b)\n\
         \n\
         running 2 tests\n\
         test sweeps::other_thing ... ok\n\
         test every_sweep_spawner_guards_on_its_own_subject ... FAILED\n\
         \n\
         failures:\n\
         \n\
         ---- every_sweep_spawner_guards_on_its_own_subject stdout ----\n\
         \n\
         thread 'every_sweep_spawner_guards_on_its_own_subject' panicked at crates/core/boss-dispatcher/tests/sweep_spawn_guards.rs:79:5:\n\
         expected the seven daily sweep spawners, found 6\n\
         note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\
         \n\
         \n\
         failures:\n\
         \x20   every_sweep_spawner_guards_on_its_own_subject\n\
         \n\
         test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n\
         \n\
         error: test failed, to rerun pass `-p boss-dispatcher --test sweep_spawn_guards`\n",
    );
    for i in 0..passing_lines {
        if i % 500 == 0 {
            log.push_str(&format!(
                "     Running tests/later_{i}.rs (target/debug/deps/later_{i}-ffff)\n\nrunning 500 tests\n"
            ));
        }
        log.push_str(&format!("test later::case_{i:05} ... ok\n"));
    }
    log.push_str(
        "test result: ok. 500 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n\
         \n\
         error: 1 target failed:\n\
         \x20   `-p boss-dispatcher --test sweep_spawn_guards`\n\
         ::endgroup::\n",
    );
    log
}

/// THE FAILURE IS NOT THE LAST THING A `--no-fail-fast` RUN SAYS (backlog
/// 3bef4198). With the gate's `cargo test` running every binary, a
/// failure in an early binary is followed by thousands of lines of later
/// binaries passing. The parser read only the last 2 000 lines of a
/// check and the replay only the last 300, so the failure the flag
/// exists to keep would have been read by nothing: `fails` would say "no
/// failing test", and the excerpt would be passing-test chatter. The
/// whole check is parsed, and every failure block from before the tail
/// is replayed ahead of it, saying what was left out between.
#[test]
fn a_red_early_in_a_no_fail_fast_run_is_still_named_replayed_and_excerpted() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        &no_fail_fast_log(6_000),
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails_joined();
    assert!(
        fails.contains("every_sweep_spawner_guards_on_its_own_subject")
            && fails.contains("sweep_spawn_guards.rs:79:5")
            && fails.contains("found 6"),
        "a failure 6 000 lines above the end of the check is named with its panic:\n{fails}"
    );
    assert!(
        got.replay
            .contains("thread 'every_sweep_spawner_guards_on_its_own_subject' panicked")
            && got.replay.contains(
                "error: test failed, to rerun pass `-p boss-dispatcher --test sweep_spawn_guards`"
            ),
        "the replay carries the early failure block and the binary it came from:\n{}",
        &got.replay[..got.replay.len().min(4_000)]
    );
    assert!(
        got.replay.contains("error: 1 target failed:"),
        "…and still the check's last words:\n{}",
        &got.replay[got.replay.len().saturating_sub(2_000)..]
    );
    assert!(
        got.replay.contains("omitted"),
        "the passing output left out between them states itself"
    );
    assert!(
        got.replay.len() < 60_000,
        "the replay is the failure blocks and the tail, not the whole check: {} chars",
        got.replay.len()
    );
    let excerpt = got.excerpt_of("test");
    assert!(
        excerpt.contains("sweep_spawn_guards.rs:79:5") && excerpt.contains("1 target failed"),
        "the excerpt holds the panic and cargo's roll-call of failed targets:\n{excerpt}"
    );
}

/// THE 00:01Z TRAIN, 2026-09-28 (backlog 4d928d0a). Train
/// `train/20260928-0001` was disassembled on `web-suite` and its receipt's
/// `fails` said only `error: script "test:unit" exited with code 1` —
/// "no cargo test failure in this check's output; 1 error line(s)" —
/// while the excerpt beside it named the file and the test that timed
/// out. `fails` is what the alert and the yard read, so it must name the
/// unit test: the file bun ran alone, the `(fail)` line, and why. The
/// first block is that gate's own output, verbatim; the second is bun
/// 1.3.14's shape for an assertion and a thrown error, captured on the
/// dev pod (an `error:` line and its `at` location print ABOVE the
/// `(fail)` line; a timeout's reason prints BELOW it).
const BUN_UNIT_RED: &str = "\
::group::gate: web-suite (unit+build+mocked)
$ bun scripts/each-test-file-alone.ts src scripts

===== ./scripts/bunfig-keys-take-effect.test.ts — exit 1, run alone =====
bun test v1.3.14 (0d9b296a)

scripts/bunfig-keys-take-effect.test.ts:
(pass) the bunfig sets exactly the keys this file reads back [1.15ms]
(pass) [test] preload: the rune shim ran before this file [0.04ms]
killed 1 dangling process
(fail) [serve.static] plugins: the dev-server compiles the root component, and an empty bunfig does not [30000.09ms]
  ^ this test timed out after 30000ms.

 2 pass
 1 fail
 2 expect() calls
Ran 3 tests across 1 file. [30.15s]

===== ./src/jobs/annotations.test.ts — exit 1, run alone =====
bun test v1.3.14 (0d9b296a)

src/jobs/annotations.test.ts:
(pass) passes [0.02ms]
7 | test('compares wrong', () => {
8 |   expect(1 + 1).toBe(3);
                    ^
error: expect(received).toBe(expected)

Expected: 3
Received: 2

      at <anonymous> (/gate-target/repo/apps/web/src/jobs/annotations.test.ts:8:17)
(fail) compares wrong [0.12ms]
11 | test('throws a type error', () => {
12 |   const o: any = undefined;
                      ^
TypeError: undefined is not an object (evaluating '(void 0).x')
      at <anonymous> (/gate-target/repo/apps/web/src/jobs/annotations.test.ts:12:18)
(fail) throws a type error [0.06ms]

 1 pass
 2 fail
Ran 3 tests across 1 file. [74.00ms]

===== ./src/never-loads.test.ts — exit 1, run alone =====
bun test v1.3.14 (0d9b296a)

src/never-loads.test.ts:

# Unhandled error between tests
-------------------------------
error: Cannot find module './gone' from '/gate-target/repo/apps/web/src/never-loads.test.ts'
-------------------------------

 0 pass
 1 fail
 1 error
Ran 1 test across 1 file. [5.00ms]

each-test-file-alone: 151/154 files passed (2513 tests), each in its own process, 4 at a time, in 38.2s (slowest: ./scripts/bunfig-keys-take-effect.test.ts 30155ms)
each-test-file-alone: 3 file(s) FAIL when run alone: ./scripts/bunfig-keys-take-effect.test.ts, ./src/jobs/annotations.test.ts, ./src/never-loads.test.ts
error: script \"test:unit\" exited with code 1
::endgroup::
";

#[test]
fn a_unit_test_red_names_the_file_the_failing_test_and_why() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let check = "web-suite (unit+build+mocked)";
    let got = run_extractor(
        &red_receipt(&format!("{{\"name\":\"{check}\",\"result\":\"fail\"}}")),
        BUN_UNIT_RED,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let fails = got.fails().expect("fails is on the receipt");
    let joined = fails.join("\n");
    let entry = |needle: &str| fails.iter().find(|e| e.contains(needle)).cloned();

    let timeout = entry("[serve.static] plugins")
        .unwrap_or_else(|| panic!("the timed-out unit test is named on the receipt:\n{joined}"));
    assert!(
        timeout.contains("./scripts/bunfig-keys-take-effect.test.ts")
            && timeout.contains("timed out after 30000ms"),
        "…with its file and its reason:\n{timeout}"
    );
    let assertion =
        entry("compares wrong").unwrap_or_else(|| panic!("a failed assertion is named:\n{joined}"));
    assert!(
        assertion.contains("./src/jobs/annotations.test.ts")
            && assertion.contains("error: expect(received).toBe(expected)")
            && assertion.contains("Received: 2")
            && assertion.contains("annotations.test.ts:8:17"),
        "…with its file, the error bun printed above it, and where:\n{assertion}"
    );
    let thrown = entry("throws a type error")
        .unwrap_or_else(|| panic!("a thrown error is named:\n{joined}"));
    assert!(
        thrown.contains("TypeError: undefined is not an object"),
        "…with the error it threw, not the assertion before it:\n{thrown}"
    );
    let unloaded = entry("./src/never-loads.test.ts")
        .unwrap_or_else(|| panic!("a file that failed before any test is named:\n{joined}"));
    assert!(
        unloaded.contains("Cannot find module"),
        "…with the error that stopped it:\n{unloaded}"
    );
    assert!(
        !joined.contains("no cargo test failure"),
        "a unit-test red is not described by what it is not:\n{joined}"
    );
}

/// Where a Playwright error context the gate kept is read from: a file
/// the receipt names under `evidence`, in a scratch dir of its own.
fn receipt_with_evidence(check: &str, context: Option<&str>) -> (String, std::path::PathBuf) {
    let dir = boss_testing::scratch_dir("gate-detail-evidence");
    let path = dir.join("receipt.error-context.md");
    if let Some(body) = context {
        boss_testing::write_file(&path, body);
    }
    let receipt = format!(
        "{{\"verdict\":\"failed\",\"head\":\"abc\",\"mode\":\"full\",\
         \"evidence\":{{\"{check}\":\"{}\"}},\
         \"checks\":[{{\"name\":\"{check}\",\"result\":\"fail\"}}]}}",
        path.display()
    );
    (receipt, dir)
}

/// One spec's error context, in the shape Playwright 1.61 writes
/// `test-results/<test>/error-context.md`, under the header gate.sh
/// writes above each one.
fn context_block(spec: &str, snapshot_lines: usize) -> String {
    let mut s = format!(
        "===== apps/web/test-results/{spec}/error-context.md =====\n\
         # Instructions\n\n- Following Playwright test failed.\n\n\
         # Test info\n\n- Name: {spec}\n- Location: tests/mocked/{spec}.mocked.spec.ts:85:3\n\n\
         # Error details\n\n```\nError: expect(locator).toBeVisible() failed\n```\n\n\
         # Page snapshot\n\n```yaml\n"
    );
    for i in 0..snapshot_lines {
        s.push_str(&format!(
            "- generic [ref=e{i}]: waiting on /api/yard/shop-floor row {i}\n"
        ));
    }
    s.push_str("```\n\n");
    s
}

/// THE PAGE A TIMED-OUT SPEC WAS WAITING ON (backlog 4d928d0a). Dock
/// re-gate 7b2acdc4 went red on one mocked spec at the 1.0 m test
/// timeout; Playwright had written what the page showed to
/// `test-results/…/error-context.md`, the list reporter printed only its
/// PATH, and the file died with the pod — so the diagnosis rested on
/// reproducing it, which failed. gate.sh now keeps every such file whole
/// beside the receipt and names it under `evidence`; the runner copies
/// it onto the durable record as `fails_context`, and into the replay.
#[test]
fn a_web_suite_red_carries_the_error_context_the_receipt_names() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let check = "web-suite (unit+build+mocked)";
    let body = context_block("it-department-map-stations", 5);
    let (receipt, dir) = receipt_with_evidence(check, Some(&body));
    let log = format!(
        "::group::gate: {check}\n  ✘  1 [chromium] › x (60.0s)\n  1 failed\n::endgroup::\n"
    );
    let got = run_extractor(&receipt, &log);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.ok, "extractor failed: {}", got.stdout);

    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    let context = v["fails_context"][check].as_str().unwrap_or_default();
    assert!(
        context.contains("waiting on /api/yard/shop-floor row 4")
            && context.contains("tests/mocked/it-department-map-stations.mocked.spec.ts:85:3"),
        "the page snapshot rides the receipt, whole when it fits:\n{}",
        got.receipt
    );
    assert!(
        got.replay.contains("waiting on /api/yard/shop-floor row 4"),
        "…and the replay:\n{}",
        got.replay
    );
    assert!(
        v.get("evidence").is_some(),
        "the receipt still names the file it read:\n{}",
        got.receipt
    );
}

/// Bounded, per spec and in all, and every cut says so — a mass-fail of
/// sixty specs must not write a receipt nobody can pass along, and must
/// not quietly drop fifty-seven of them either.
#[test]
fn the_error_context_is_bounded_per_spec_and_per_check_and_says_so() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let check = "web-suite (unit+build+mocked)";
    let body: String = (0..60)
        .map(|i| context_block(&format!("spec-{i:02}"), 200))
        .collect();
    let (receipt, dir) = receipt_with_evidence(check, Some(&body));
    let log = format!("::group::gate: {check}\n  60 failed\n::endgroup::\n");
    let got = run_extractor(&receipt, &log);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.ok, "extractor failed: {}", got.stdout);

    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    let context = v["fails_context"][check].as_str().unwrap_or_default();
    assert!(
        context.len() <= 6_600,
        "one check's context is bounded — {} chars",
        context.len()
    );
    assert!(
        context.contains("spec-00") && context.contains("spec-01"),
        "the budget is shared between specs, not spent on the first one alone:\n{context}"
    );
    assert!(
        context.contains("omitted"),
        "every cut states itself:\n{context}"
    );
    assert!(
        context.contains("60 spec context(s)"),
        "…and says how many there were:\n{}",
        &context[context.len().saturating_sub(600)..]
    );
}

/// A named file that is not there is said, not skipped: an absent key
/// would read as "Playwright wrote nothing".
#[test]
fn an_evidence_file_that_cannot_be_read_is_said_not_skipped() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let check = "web-suite (unit+build+mocked)";
    let (receipt, dir) = receipt_with_evidence(check, None);
    let log = format!("::group::gate: {check}\n  1 failed\n::endgroup::\n");
    let got = run_extractor(&receipt, &log);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    let context = v["fails_context"][check].as_str().unwrap_or_default();
    assert!(
        context.contains("could not be read"),
        "an unreadable evidence file is stated on the receipt:\n{}",
        got.receipt
    );
}

/// `fails_context` is always present, `{}` when there is nothing, for
/// the reason `fails` is `[]` (a missing field and an empty one must not
/// look the same).
#[test]
fn a_receipt_with_no_evidence_carries_an_empty_context_not_a_missing_one() {
    if python3_missing() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let got = run_extractor(
        &red_receipt("{\"name\":\"test\",\"result\":\"fail\"}"),
        CARGO_LOG,
    );
    assert!(got.ok, "extractor failed: {}", got.stdout);
    let v: Value = serde_json::from_str(&got.receipt).expect("receipt is JSON");
    assert_eq!(
        v["fails_context"].as_object().map(|m| m.len()),
        Some(0),
        "no evidence named, so `fails_context: {{}}`:\n{}",
        got.receipt
    );
}
