//! The gate runner leaves its verdict and writes nothing to the system
//! of record (backlog 934ccad1; design bdc60b65, question `gate-verdict`,
//! decided 2026-10-06).
//!
//! WHY. The gate runner Job runs a car's branch — its build scripts, its
//! tests, its `infra/gate.sh` — so it must never hold the estate machine
//! token (design c395e62c), and it reported its own verdict to the jobs
//! API with none: GET the packet, PATCH the step, PUT the step, on every
//! gate (413 would-refuse facts in the 26 h read on 2026-10-07). Once the
//! machine door enforces, those writes are refused and every gate verdict
//! is lost. The decision: the runner writes nothing; it leaves its
//! receipt in its pod log and the conductor records it.
//!
//! WHAT IS HELD HERE, by running the blocks `run.sh` ships:
//!   - handed `GATE_VERDICT_CARRIER=pod-log` (the manifest renders it
//!     beside the Job label the conductor reads), the runner makes NO
//!     request — not for its verdict, not for a refusal, not when it
//!     dies early — and leaves two lines in its log plus the second of
//!     them in its termination message;
//!   - handed nothing (a manifest from a branch cut before this landed:
//!     the manifest is the launcher's tree, the script is the cluster's
//!     ConfigMap), it reports as it always did, because nobody would
//!     record for it;
//!   - the label and the variable travel together in the manifest, so
//!     the conductor never reads a runner that also writes, and never
//!     ignores one that does not.
//!
//! The reader of what this leaves is boss-cli `train/carried_verdict.rs`;
//! its own test lifts the same block and parses what it prints.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run_sh() -> String {
    let path = repo_root().join("infra/gate-runner/run.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The text between two marker lines of run.sh, markers excluded.
fn block(begin: &str, end: &str) -> String {
    let sh = run_sh();
    let a = sh
        .find(begin)
        .unwrap_or_else(|| panic!("run.sh has no `{begin}` marker"));
    let b = sh
        .find(end)
        .unwrap_or_else(|| panic!("run.sh has no `{end}` marker"));
    assert!(a < b, "`{begin}` must precede `{end}`");
    sh[a + begin.len()..b].to_string()
}

fn report_block() -> String {
    block(
        "# --- report-back (begin) ---",
        "# --- report-back (end) ---",
    )
}

fn carrier_block() -> String {
    block(
        "# --- verdict carrier (begin) ---",
        "# --- verdict carrier (end) ---",
    )
}

fn delivery_block() -> String {
    block(
        "# --- verdict delivery (begin) ---",
        "# --- verdict delivery (end) ---",
    )
}

/// One run of the lifted blocks: what it printed, what its termination
/// message holds, and every request it tried to make.
struct Ran {
    stdout: String,
    termination: Option<String>,
    requests: String,
    status: i32,
}

/// Run `body` after the report and carrier blocks, with a `curl` that
/// records its arguments and answers "connection refused" (exit 7).
/// `carrier` is the value the manifest hands the runner, if any.
fn run(name: &str, carrier: Option<&str>, termination_log: Option<&Path>, body: &str) -> Ran {
    let dir: PathBuf = scratch_dir(name);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let requests = dir.join("requests");
    let _ = std::fs::remove_file(&requests);
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\nexit 7\n",
            requests.display()
        ),
    );
    let term = termination_log
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dir.join("termination-log"));
    let _ = std::fs::remove_file(&term);
    let script = format!(
        "set -euo pipefail\nJOBS_API=http://127.0.0.1:1\nGATE_RUN_JOB_ID=pkt-1\n\
         ACTOR='{{\"id\":\"automation:gate-runner\"}}'\n{}\n{}\n{body}\n",
        report_block(),
        carrier_block()
    );
    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        // One retry, no wait. (Empty would read as unset: the script's
        // default is five minutes of backoff.)
        .env("GATE_REPORT_BACKOFF", "0")
        .env("GATE_TERMINATION_LOG", &term)
        .env("NATIVE_JOB_NAME", "gate-fix-x-abc12")
        .env_remove("GATE_VERDICT_CARRIER");
    if let Some(c) = carrier {
        cmd.env("GATE_VERDICT_CARRIER", c);
    }
    let out = cmd.output().expect("bash runs");
    Ran {
        stdout: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        termination: std::fs::read_to_string(&term).ok(),
        requests: std::fs::read_to_string(&requests).unwrap_or_default(),
        status: out.status.code().unwrap_or(-1),
    }
}

const RECEIPT: &str = r#"{"verdict":"failed","head":"26b7e3081a52923879dd07cda85c2f3f74e7abdf","checks":[{"name":"test","result":"fail","seconds":400}],"fails":["test: a_thing - FAILED"],"runtime_evidence":{"state":"sampled"}}"#;

/// The trailer run.sh must leave for `payload`, computed here.
fn trailer(verdict: &str, payload: &str) -> String {
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("sha256sum");
    boss_testing::feed_stdin(&mut child, payload.as_bytes());
    let out = child.wait_with_output().unwrap();
    let digest = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    format!(
        "gate-runner: receipt-end v1 job=gate-fix-x-abc12 verdict={verdict} bytes={} sha256={digest}",
        payload.len()
    )
}

/// THE DECISION. Handed the carrier, the runner delivers its verdict
/// without one request: the receipt and its trailer in the log, the
/// trailer in the termination message, and the run counted as delivered.
#[test]
fn a_carrier_runner_leaves_its_verdict_and_makes_no_request() {
    let body = format!(
        "VERDICT=failed\nSUMMARY='{RECEIPT}'\nRECEIPT=/nonexistent/receipt.json\n{}\necho \"REPORTED=$REPORTED\"\n",
        delivery_block()
    );
    let ran = run("runner-leaves-verdict", Some("pod-log"), None, &body);
    assert_eq!(ran.status, 0, "{}", ran.stdout);
    assert_eq!(
        ran.requests, "",
        "a carrier runner made a request to the system of record:\n{}",
        ran.requests
    );
    let want = trailer("failed", RECEIPT);
    let lines: Vec<&str> = ran.stdout.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == want)
        .unwrap_or_else(|| panic!("no trailer line `{want}` in:\n{}", ran.stdout));
    assert_eq!(
        lines[at - 1],
        format!("gate-runner: receipt {RECEIPT}"),
        "the receipt line is the line before its trailer, whole"
    );
    assert_eq!(
        ran.termination.as_deref(),
        Some(format!("{want}\n").as_str()),
        "the termination message is the trailer line and nothing else"
    );
    assert!(ran.stdout.contains("REPORTED=1"), "{}", ran.stdout);
    assert!(
        !ran.stdout.contains("UNREPORTED") && !ran.stdout.contains("WARN"),
        "{}",
        ran.stdout
    );
}

/// The carried receipt states `runtime_evidence` when the run left none
/// — the default the old report added on its way out — and a receipt
/// that already has it, or that jq cannot read, is carried untouched.
#[test]
fn the_carried_receipt_states_runtime_evidence_and_is_otherwise_untouched() {
    let deliver = |tag: &str, summary: &str| {
        let body = format!(
            "VERDICT=failed\nSUMMARY='{summary}'\nRECEIPT=/nonexistent/receipt.json\n{}\n",
            delivery_block()
        );
        let ran = run(tag, Some("pod-log"), None, &body);
        assert_eq!(ran.requests, "");
        ran.stdout
            .lines()
            .find_map(|l| l.strip_prefix("gate-runner: receipt "))
            .unwrap_or_else(|| panic!("no receipt line:\n{}", ran.stdout))
            .to_string()
    };
    assert_eq!(
        deliver(
            "runner-evidence-absent",
            r#"{"verdict":"failed","head":"abc"}"#
        ),
        r#"{"verdict":"failed","head":"abc","runtime_evidence":{"state":"unavailable","collection_errors":["runner ended before source collector was available"]}}"#
    );
    assert_eq!(deliver("runner-evidence-present", RECEIPT), RECEIPT);
    assert_eq!(
        deliver("runner-evidence-unreadable", "not json"),
        "not json"
    );
}

/// THE ROLLOUT. A manifest from a branch cut before this landed hands
/// the runner no carrier and labels no Job, so the conductor will not
/// record for it: the same script must report as it always did, and
/// must not write a termination message nobody asked for. Any other
/// value is not the carrier either.
#[test]
fn a_runner_handed_no_carrier_still_reports_for_itself() {
    let body = format!(
        "VERDICT=failed\nSUMMARY='{RECEIPT}'\nRECEIPT=/nonexistent/receipt.json\n{}\necho \"REPORTED=$REPORTED\"\n",
        delivery_block()
    );
    for (i, carrier) in [None, Some(""), Some("true"), Some("pod-logs")]
        .into_iter()
        .enumerate()
    {
        let ran = run(&format!("runner-reports-{i}"), carrier, None, &body);
        assert!(
            ran.requests.contains("/api/jobs/pkt-1"),
            "handed {carrier:?}, the runner did not report to its packet:\n{}",
            ran.stdout
        );
        assert!(
            ran.stdout.contains("REPORTED=0"),
            "the stub refuses every connection, so the report cannot have landed:\n{}",
            ran.stdout
        );
        assert_eq!(
            ran.termination, None,
            "handed {carrier:?}, the runner wrote a termination message"
        );
        // The frame is printed either way: it is the log's own copy.
        assert!(ran.stdout.contains(&trailer("failed", RECEIPT)));
    }
}

/// A run that ends BEFORE its checks — it died, or it refused to start —
/// leaves that the same way: a whole frame whose receipt is an object
/// stating the same word, and no request.
#[test]
fn an_early_end_is_left_the_same_way() {
    for (verdict, call, must) in [
        (
            "lost",
            r#"settle_early lost "runner died before a receipt: line 657" "$(early_receipt lost error "runner died before a receipt: line 657")""#,
            r#""error":"runner died before a receipt: line 657""#,
        ),
        (
            "refused",
            r#"r=$(early_receipt refused refused_because "gate disk: REFUSED: not the gate disk"); settle_early refused "$r" "$r""#,
            r#""refused_because":"gate disk: REFUSED: not the gate disk""#,
        ),
    ] {
        let ran = run(
            &format!("runner-early-{verdict}"),
            Some("pod-log"),
            None,
            call,
        );
        assert_eq!(ran.status, 0, "{}", ran.stdout);
        assert_eq!(ran.requests, "", "an early `{verdict}` made a request");
        let lines: Vec<&str> = ran.stdout.lines().collect();
        let payload = lines
            .iter()
            .find_map(|l| l.strip_prefix("gate-runner: receipt "))
            .unwrap_or_else(|| panic!("no receipt line:\n{}", ran.stdout));
        assert!(
            payload.contains(&format!(r#""verdict":"{verdict}""#)) && payload.contains(must),
            "{payload}"
        );
        let want = trailer(verdict, payload);
        assert!(lines.contains(&want.as_str()), "{}", ran.stdout);
        assert_eq!(
            ran.termination.as_deref(),
            Some(format!("{want}\n").as_str())
        );
        // And handed no carrier, the same call reports instead.
        let old = run(&format!("runner-early-old-{verdict}"), None, None, call);
        assert!(old.requests.contains("/api/jobs/pkt-1"), "{}", old.stdout);
        assert_eq!(old.termination, None);
    }
}

/// A termination message that cannot be written is said, never fatal:
/// the frame is in the log either way, and the conductor settles a
/// container that ended with no message `lost` by name — a run must not
/// die of its own carrier, and must not be silent about it.
#[test]
fn a_termination_message_that_cannot_be_written_is_said_not_fatal() {
    let body = format!("leave_verdict failed '{RECEIPT}'\necho after\n");
    let ran = run(
        "runner-unwritable-termination",
        Some("pod-log"),
        Some(Path::new("/nonexistent-dir/termination-log")),
        &body,
    );
    assert_eq!(ran.status, 0, "{}", ran.stdout);
    assert!(ran.stdout.contains(&trailer("failed", RECEIPT)));
    assert!(
        ran.stdout
            .contains("gate-runner: WARN: the termination message could not be written"),
        "{}",
        ran.stdout
    );
    assert!(ran.stdout.contains("after"));
}

/// THE WORD FOLLOWS THE RECEIPT AFTER THE REWRITE (review 7e5356f7, N1).
/// The failure-detail block can rewrite the receipt to a refusal after
/// VERDICT was taken from it; the lines between that block and the
/// summary take the word again, so the step and the receipt say one
/// thing. Lifted and run: a refusing receipt moves the word, any other
/// receipt — or none — leaves it.
#[test]
fn the_word_follows_a_receipt_rewritten_to_a_refusal() {
    let lines = block(
        "# --- failure detail (end) ---",
        "# --- receipt summary (begin) ---",
    );
    let dir = scratch_dir("runner-word-follows-receipt");
    for (i, (receipt, want)) in [
        (
            Some(r#"{"verdict":"refused","refused_because":"no route to the registry"}"#),
            "refused",
        ),
        (Some(r#"{"verdict":"failed"}"#), "failed"),
        (Some("not json"), "failed"),
        (None, "failed"),
    ]
    .into_iter()
    .enumerate()
    {
        let file = dir.join(format!("receipt-{i}.json"));
        let _ = std::fs::remove_file(&file);
        if let Some(body) = receipt {
            boss_testing::write_file(&file, body);
        }
        let script = format!(
            "set -euo pipefail\nVERDICT=failed\nRECEIPT='{}'\n{lines}\necho \"VERDICT=$VERDICT\"\n",
            file.display()
        );
        let out = Command::new("bash")
            .args(["-c", &script])
            .output()
            .expect("bash");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains(&format!("VERDICT={want}")),
            "receipt {receipt:?}: {said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// WHAT THE LAUNCHER HANDS THE POD REACHES ITS ONE READER AND NOTHING
/// UNDER TEST (gate-run 5d576fba). The manifest hands the gate container
/// the carrier; the launcher hands it the edit level. Both are
/// environment, so both reached every process in the pod — the branch's
/// gate.sh and every test it runs — and a test that lifts the carrier
/// block ran as a carrier because the pod said so: red, and a `lost`
/// trailer naming the real Job written to the real termination message.
/// run.sh keeps the carrier as a shell variable and exports it to
/// nothing; gate.sh takes the handed level out of its environment and
/// puts it back for the pre-flight alone.
#[test]
fn what_the_pod_is_handed_reaches_its_reader_and_no_child() {
    // run.sh: after the carrier block the script still knows its layout,
    // and a child knows nothing of it.
    let ran = run(
        "runner-carrier-not-inherited",
        Some("pod-log"),
        None,
        "carried && echo script=carried\n\
         bash -c 'echo \"child=${GATE_VERDICT_CARRIER:-unset}/${GATE_TERMINATION_LOG:-unset}\"'\n",
    );
    assert_eq!(ran.status, 0, "{}", ran.stdout);
    assert!(ran.stdout.contains("script=carried"), "{}", ran.stdout);
    assert!(
        ran.stdout.contains("child=unset/unset"),
        "a child of run.sh inherited the layout:\n{}",
        ran.stdout
    );
    // gate.sh: the lines it ships, lifted. Inside run_preflight a child
    // is handed the level; before it and after it, none is.
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).unwrap();
    let (begin, end) = (
        "# --- handed edit level (begin) ---",
        "# --- handed edit level (end) ---",
    );
    let a = gate.find(begin).expect("gate.sh scopes the handed level");
    let b = gate.find(end).expect("and closes the block");
    let rest = &gate[b..];
    let def = rest.find("run_preflight() {\n").expect("run_preflight");
    let first = rest[def + "run_preflight() {\n".len()..]
        .lines()
        .next()
        .unwrap();
    assert!(
        first.contains("local -x BOSS_EDIT_LEVEL_ANSWER=\"$GATE_EDIT_LEVEL_HANDED\""),
        "the pre-flight's first line hands the level to its own children: {first}"
    );
    // Before ANY check: the block precedes every `check "` call that
    // runs a suite.
    assert!(
        a < gate.find("\ncheck \"").expect("gate.sh runs checks"),
        "the handed level is still exported when the first check runs"
    );
    let see = r#"bash -c 'echo "$0=${BOSS_EDIT_LEVEL_ANSWER:-unset}"'"#;
    for (handed, inside) in [
        (Some(r#"{"edit_level":null}"#), r#"{"edit_level":null}"#),
        (None, "unset"),
    ] {
        let script = format!(
            "set -euo pipefail\n{}\nrun_preflight() {{\n{first}\n{see} inside\n}}\n{see} before\nrun_preflight\n{see} after\n",
            &gate[a..b]
        );
        let mut cmd = Command::new("bash");
        cmd.args(["-c", &script])
            .env_remove("BOSS_EDIT_LEVEL_ANSWER");
        if let Some(h) = handed {
            cmd.env("BOSS_EDIT_LEVEL_ANSWER", h);
        }
        let out = cmd.output().expect("bash");
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            said.lines().collect::<Vec<_>>(),
            vec![
                "before=unset".to_string(),
                format!("inside={inside}"),
                "after=unset".to_string()
            ],
            "handed {handed:?}"
        );
    }
}

/// run.sh as it ships, comments dropped.
fn printed() -> String {
    run_sh()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// EVERY REQUEST THE RUNNER CAN MAKE SITS BEHIND THE CARRIER QUESTION.
/// The behaviour above runs three blocks; this holds the rest of the
/// file: no `curl` and no `$JOBS_API` outside the report block and the
/// delivery's old-layout arm, and `report` called from exactly the two
/// places that ask `carried` first.
#[test]
fn nothing_outside_the_old_layouts_arms_can_reach_the_jobs_api() {
    let sh = run_sh();
    let mut rest = sh.clone();
    for (begin, end) in [
        (
            "# --- report-back (begin) ---",
            "# --- report-back (end) ---",
        ),
        (
            "# --- verdict delivery (begin) ---",
            "# --- verdict delivery (end) ---",
        ),
    ] {
        let a = rest.find(begin).expect(begin);
        let b = rest.find(end).expect(end);
        rest.replace_range(a..b, "");
    }
    for (n, line) in rest.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with('#') {
            continue;
        }
        // The address's own default is a name, not a request.
        if code.starts_with("JOBS_API=\"${JOBS_API:-") {
            continue;
        }
        assert!(
            !code.contains("curl") && !code.contains("JOBS_API"),
            "run.sh reaches for the jobs API outside the old layout's arms (line ~{}): {line}",
            n + 1
        );
    }
    // `report` itself: called by the early settle's old-layout arm and by
    // the delivery's, and nowhere else.
    let calls: Vec<String> = printed()
        .lines()
        .filter(|l| {
            let l = l.trim_start();
            (l.contains("report \"$") || l.contains("report lost") || l.contains("report refused"))
                && !l.starts_with("report() {")
                && !l.contains("report_once")
                && !l.contains("report_write")
        })
        .map(|l| l.trim().to_string())
        .collect();
    assert_eq!(
        calls,
        vec![
            r#"report "$1" "$2" || true"#.to_string(),
            r#"elif report "$VERDICT" "$SUMMARY"; then"#.to_string(),
        ],
        "a new caller of `report` writes to the jobs API from a pod that must not"
    );
    let delivery = delivery_block();
    let carried_at = delivery
        .find("if carried; then")
        .expect("the delivery asks the layout first");
    let report_at = delivery
        .find(r#"elif report "$VERDICT" "$SUMMARY"; then"#)
        .expect("the old layout's report");
    assert!(carried_at < report_at);
    let early = carrier_block();
    let settle = &early[early.find("settle_early() {").expect("settle_early")..];
    let settle = &settle[..settle.find("\n}\n").expect("its end")];
    assert!(
        settle.find("if carried; then").expect("asks the layout")
            < settle
                .find("report \"$1\"")
                .expect("the old layout's report"),
        "{settle}"
    );
}

/// THE LABEL AND THE VARIABLE TRAVEL TOGETHER. The conductor reads a Job
/// by its label; the runner stops writing by its variable. One without
/// the other is a verdict written twice or never: so the manifest states
/// both, with the one value the reader and the script both spell.
#[test]
fn the_manifest_hands_the_carrier_to_the_job_and_the_runner_together() {
    let manifest =
        std::fs::read_to_string(repo_root().join("infra/gate-runner/gate-runner.yaml")).unwrap();
    let code: Vec<&str> = manifest
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert_eq!(
        code.iter()
            .filter(|l| l.trim() == "boss.dev/verdict-carrier: pod-log")
            .count(),
        1,
        "the Job's own metadata carries the carrier label, once"
    );
    assert_eq!(
        code.iter()
            .filter(|l| l.trim() == "- {name: GATE_VERDICT_CARRIER, value: pod-log}")
            .count(),
        1,
        "the gate container is handed the carrier, once"
    );
    // The label is on the JOB (the launcher's metadata block), beside the
    // packet label every reader selects on — not on the pod template.
    let label = code
        .iter()
        .position(|l| l.trim() == "boss.dev/verdict-carrier: pod-log")
        .unwrap();
    let packet = code
        .iter()
        .position(|l| l.trim() == "boss.dev/packet: $GATE_RUN_JOB_ID")
        .expect("the packet label");
    // The Job's own `spec:` — the first after its packet label (the
    // manifest's PVC documents come before the Job and have theirs).
    let spec = packet
        + code[packet..]
            .iter()
            .position(|l| *l == "spec:")
            .expect("the Job's spec");
    assert!(
        packet < spec && label < spec && label.abs_diff(packet) <= 2,
        "the carrier label sits in the Job's metadata, beside the packet label"
    );
    // One spelling, three files (§9a): the reader's constants, the
    // script's test, and the manifest's two lines.
    let reader = std::fs::read_to_string(
        repo_root().join("crates/orchestrators/boss-cli/src/train/carried_verdict.rs"),
    )
    .unwrap();
    assert!(
        reader.contains(r#"pub(crate) const CARRIER_LABEL: &str = "boss.dev/verdict-carrier";"#)
    );
    assert!(reader.contains(r#"pub(crate) const CARRIER_POD_LOG: &str = "pod-log";"#));
    assert!(
        carrier_block().contains(r#"carried() { [ "$GATE_VERDICT_CARRIER" = pod-log ]; }"#),
        "the script asks for the same word the manifest hands it"
    );
}
