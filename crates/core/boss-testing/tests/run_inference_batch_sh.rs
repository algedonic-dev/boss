//! `infra/ml/run-inference-batch.sh` must be BEST-EFFORT per model.
//!
//! The daily ML infer-batch trigger iterates a list of independent
//! models and POSTs infer-batch on each. It ran the loop under
//! `set -euo pipefail` with a bare `curl -sSf`, so the FIRST model
//! whose batch failed aborted the loop — every model after it was
//! skipped and left its predictions stale, silently, until someone
//! read the journal. One flaky model took out the rest of the run.
//!
//! The fix makes each model best-effort: a failure is logged and the
//! loop continues, and the script exits non-zero at the end if any
//! model failed, so systemd (Type=oneshot) and the maintenance packet
//! still record the degraded run.
//!
//! The ONE thing best-effort must NOT break is the single real
//! ordering dependency the file documents: high-churn-risk reads what
//! account-churn-risk writes, so if churn-risk fails, high-churn-risk
//! must be SKIPPED (not run on stale input) — while the other
//! independent models still run.
//!
//! These tests drive the real script with a fake `curl` (injected via
//! `BOSS_ML_CURL`, the script's test seam) that records which model
//! ids it was asked to infer and fails for a chosen set. The same
//! shape as `gate_sh.rs` overriding `df` via `BOSS_GATE_DF_CMD`.

use boss_testing::repo_root;

const SCRIPT: &str = "infra/ml/run-inference-batch.sh";

/// The full ordered model list the script iterates, as the source of
/// truth for what a full run should touch. Kept in sync by
/// `the_model_list_and_dependency_comment_survive` below.
const ALL_MODELS: &[&str] = &[
    "mdl-account-churn-risk-v1",
    "mdl-next-action-contract-expiring-v1",
    "mdl-next-action-past-due-invoice-v1",
    "mdl-next-action-missing-primary-contact-v1",
    "mdl-next-action-high-churn-risk-v1",
    "mdl-next-action-stalled-service-ticket-v1",
    "mdl-next-action-preventive-maintenance-due-v1",
];

const CHURN_MODEL: &str = "mdl-account-churn-risk-v1";
const DEPENDENT_MODEL: &str = "mdl-next-action-high-churn-risk-v1";

struct RunOutcome {
    success: bool,
    /// The exit code itself — the refusal without an address is 78.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// The model ids the fake curl was actually invoked with, in order.
    invoked: Vec<String>,
    /// What the run left for its packet in `BOSS_RUN_SUMMARY_FILE`
    /// (the unit's ExecStopPost merges it onto the `run` step).
    summary: serde_json::Value,
}

/// How the fake ML API answers one run.
#[derive(Default)]
struct Fake<'a> {
    /// Model ids whose infer-batch fails (curl exit 22, an HTTP error).
    fail: &'a [&'a str],
    /// Model ids that answer 2xx with an EMPTY body — a wrong target
    /// that answers instead of erroring.
    empty: &'a [&'a str],
    /// `written` in every BatchInferReport the fake returns.
    written: u32,
    /// Leave `BOSS_ML_API_URL` unset.
    no_url: bool,
}

/// Drive the real script with a fake curl. `fail` lists the model ids
/// whose infer-batch should fail (curl exit 22, an HTTP error); every
/// other model answers a BatchInferReport that wrote two predictions.
fn run_with_failures(fail: &[&str]) -> RunOutcome {
    run(Fake {
        fail,
        written: 2,
        ..Fake::default()
    })
}

fn run(fake_api: Fake) -> RunOutcome {
    let root = repo_root();
    let dir = boss_testing::scratch_dir("boss-infer-batch");

    let log = dir.join("invoked.log");
    let fake = dir.join("curl");
    // Fake curl: last arg is the URL; extract the model id between
    // `/models/` and `/infer-batch`, record it, then fail if it is in
    // FAKE_CURL_FAIL (space separated).
    boss_testing::write_exec(
        &fake,
        "#!/usr/bin/env bash\n\
         url=\"\"\n\
         for a in \"$@\"; do url=\"$a\"; done\n\
         id=\"${url##*/models/}\"\n\
         id=\"${id%%/*}\"\n\
         echo \"$id\" >> \"$FAKE_CURL_LOG\"\n\
         for f in $FAKE_CURL_FAIL; do\n\
           if [ \"$f\" = \"$id\" ]; then exit 22; fi\n\
         done\n\
         for f in $FAKE_CURL_EMPTY; do\n\
           if [ \"$f\" = \"$id\" ]; then exit 0; fi\n\
         done\n\
         printf '{\"model_id\":\"%s\",\"written\":%s,\"skipped\":0,\"errors\":[]}' \"$id\" \"$FAKE_CURL_WRITTEN\"\n\
         exit 0\n",
    );

    let summary_file = dir.join("summary.json");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(root.join(SCRIPT))
        .env("BOSS_ML_CURL", &fake)
        .env("BOSS_RUN_SUMMARY_FILE", &summary_file)
        .env("FAKE_CURL_LOG", &log)
        .env("FAKE_CURL_FAIL", fake_api.fail.join(" "))
        .env("FAKE_CURL_EMPTY", fake_api.empty.join(" "))
        .env("FAKE_CURL_WRITTEN", fake_api.written.to_string())
        .env_remove("BOSS_ML_API_URL")
        // No machine token in these cases, whatever the machine running
        // them holds: the token cells below bring their own mount.
        .env("BOSS_MACHINE_TOKEN_DIR", dir.join("no-mount"))
        .env("BOSS_SOR_ENV", dir.join("no-sor.env"))
        .current_dir(&root);
    if !fake_api.no_url {
        cmd.env("BOSS_ML_API_URL", "http://ml.test");
    }
    let out = cmd.output().expect("run run-inference-batch.sh");
    let summary = std::fs::read_to_string(&summary_file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null);

    let invoked = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    RunOutcome {
        success: out.status.success(),
        code: out.status.code(),
        summary,
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        invoked,
    }
}

/// The defect, directly: a mid-list model failing must NOT abort the
/// run. Every model after the failing one still gets its infer-batch,
/// and the script exits non-zero so the failure is recorded.
#[test]
fn one_failing_model_does_not_skip_the_rest() {
    // Fail an INDEPENDENT model in the middle of the list (#2). Its
    // failure must not stop #3..#7, and — since churn (#1) succeeds —
    // must not skip the dependent model either.
    let failing = "mdl-next-action-contract-expiring-v1";
    let r = run_with_failures(&[failing]);

    assert_eq!(
        r.invoked, ALL_MODELS,
        "a single failing model aborted the loop — the rest were skipped. \
         invoked: {:?}\nstderr: {}",
        r.invoked, r.stderr
    );
    assert!(
        !r.success,
        "a model failed, so the script must exit non-zero for systemd to \
         record it.\nstdout: {}\nstderr: {}",
        r.stdout, r.stderr
    );
    assert!(
        r.stderr.contains(failing),
        "the failure must name the model on stderr.\nstderr: {}",
        r.stderr
    );
}

/// The dependency guard: if account-churn-risk fails, the dependent
/// high-churn-risk model is SKIPPED (never invoked) rather than run on
/// stale input — but the OTHER independent models still run.
#[test]
fn dependent_model_is_skipped_when_churn_fails() {
    let r = run_with_failures(&[CHURN_MODEL]);

    assert!(
        r.invoked.iter().any(|m| m == CHURN_MODEL),
        "the churn model must still be attempted.\ninvoked: {:?}",
        r.invoked
    );
    assert!(
        !r.invoked.iter().any(|m| m == DEPENDENT_MODEL),
        "the dependent model ran on stale input — it must be SKIPPED when \
         churn-risk fails.\ninvoked: {:?}\nstderr: {}",
        r.invoked,
        r.stderr
    );

    // Every OTHER model (independent of churn) must still have run.
    for m in ALL_MODELS {
        if *m == DEPENDENT_MODEL {
            continue;
        }
        assert!(
            r.invoked.iter().any(|x| x == m),
            "independent model {m} was skipped when only churn failed — \
             the guard must skip ONLY the dependent.\ninvoked: {:?}",
            r.invoked
        );
    }

    assert!(
        !r.success,
        "churn failed and its dependent was skipped — the run is degraded \
         and must exit non-zero.\nstderr: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains(DEPENDENT_MODEL) && r.stderr.to_lowercase().contains("skip"),
        "the skip must be logged, naming the dependent model.\nstderr: {}",
        r.stderr
    );
}

/// A failure in an unrelated model must NOT trip the dependency guard:
/// churn succeeded, so high-churn-risk must still run.
#[test]
fn dependent_model_runs_when_only_an_unrelated_model_fails() {
    // Fail a model that comes AFTER the dependent in the list too, to
    // be sure the guard keys on churn's outcome, not on any failure.
    let r = run_with_failures(&["mdl-next-action-stalled-service-ticket-v1"]);

    assert!(
        r.invoked.iter().any(|m| m == DEPENDENT_MODEL),
        "high-churn-risk was skipped even though churn-risk succeeded — the \
         guard must depend on churn's outcome only.\ninvoked: {:?}\nstderr: {}",
        r.invoked,
        r.stderr
    );
    assert_eq!(
        r.invoked, ALL_MODELS,
        "all models should have been attempted.\ninvoked: {:?}",
        r.invoked
    );
    assert!(!r.success, "a model failed; exit must be non-zero.");
}

/// The happy path: everything succeeds, everything runs, exit 0.
#[test]
fn all_models_run_and_exit_zero_on_full_success() {
    let r = run_with_failures(&[]);

    assert_eq!(
        r.invoked, ALL_MODELS,
        "a clean run must infer every model, in order.\ninvoked: {:?}",
        r.invoked
    );
    assert!(
        r.success,
        "a run with no failures must exit 0.\nstdout: {}\nstderr: {}",
        r.stdout, r.stderr
    );
}

/// The readable model list and the dependency comment are load-bearing
/// documentation the fix promised to keep. This also pins ALL_MODELS
/// above against the script, so a model added to one without the other
/// names itself here rather than silently under-testing.
#[test]
fn the_model_list_and_dependency_comment_survive() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read script");
    for m in ALL_MODELS {
        assert!(
            script.contains(m),
            "model {m} is in this test's ALL_MODELS but not in {SCRIPT} — \
             the two have drifted"
        );
    }
    assert!(
        script.contains("Order matters")
            && script.contains("BEFORE next-action-high-churn-risk reads them"),
        "the ordering-dependency comment was dropped from {SCRIPT}; it is the \
         one thing that explains the skip guard"
    );
}

// ---------------------------------------------------------------------
// What the run tells its packet (backlog 9599babc, 2026-09-23).
//
// Seven nightly packets, 2026-09-17 .. 2026-09-23, closed `result=ok`
// and carried nothing else, while the system of record's risk-scores
// answered `total_scored: 0` against an accounts directory holding one
// account. The batch POSTed to 127.0.0.1:7070 on boss-gcp — the retired
// second stack's ML API, over the retired stack's database — and every
// POST answered 2xx, so systemd said success and the packet said ok.
// `ok` was true of the HTTP calls and said nothing about where the
// predictions went or how many there were.
// ---------------------------------------------------------------------

fn field<'a>(r: &'a RunOutcome, key: &str) -> &'a str {
    r.summary
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "the run's summary has no string `{key}`: {}\nstderr: {}",
                r.summary, r.stderr
            )
        })
}

/// A clean run records how many predictions it wrote, per model and in
/// total, which ML API it asked, and its own exit status.
#[test]
fn a_clean_run_records_the_count_the_target_and_its_exit() {
    let r = run_with_failures(&[]);
    assert!(r.success, "stderr: {}", r.stderr);
    // Seven models, each answering written=2.
    assert_eq!(field(&r, "predictions_written"), "14");
    assert_eq!(field(&r, "exit_status"), "0");
    assert_eq!(field(&r, "ml_api_url"), "http://ml.test");
    assert_eq!(
        r.summary["predictions_by_model"][CHURN_MODEL],
        serde_json::json!(2),
        "each model's own count must ride the packet: {}",
        r.summary
    );
}

/// THE ZERO IS LOUD. Every model answered and none of them wrote a
/// prediction: that is the shape of seven `ok` nights against the wrong
/// database, and it must fail the run, name itself, and still record
/// the count and the exit.
#[test]
fn a_run_that_writes_no_prediction_fails_and_says_so() {
    let r = run(Fake {
        written: 0,
        ..Fake::default()
    });
    assert_eq!(r.invoked, ALL_MODELS, "every model still runs");
    assert!(
        !r.success,
        "a batch that wrote nothing exited 0 — the packet would say ok again.\nstderr: {}",
        r.stderr
    );
    assert!(
        r.stderr.contains("0 predictions written"),
        "the failure must say what it is.\nstderr: {}",
        r.stderr
    );
    assert_eq!(field(&r, "predictions_written"), "0");
    assert_eq!(field(&r, "exit_status"), "1");
}

/// A 2xx that is not a BatchInferReport is not a success: whatever
/// answered is not the ML API this batch means, and its silence about
/// a count is the wrong-target shape (CLAUDE.md §Doors).
#[test]
fn an_answer_that_is_not_a_batch_report_is_a_failed_model() {
    let odd = "mdl-next-action-past-due-invoice-v1";
    let r = run(Fake {
        empty: &[odd],
        written: 2,
        ..Fake::default()
    });
    assert!(!r.success, "stderr: {}", r.stderr);
    assert!(
        r.stderr.contains(odd) && r.stderr.contains("not a BatchInferReport"),
        "the model whose answer carried no count must be named.\nstderr: {}",
        r.stderr
    );
    // The other six still counted.
    assert_eq!(field(&r, "predictions_written"), "12");
    assert_eq!(field(&r, "models_failed"), odd);
}

/// No address, no run: a localhost default is how this batch spent a
/// week feeding a retired database. The refusal is EX_CONFIG, like the
/// packet helpers', and no model is asked anything.
#[test]
fn without_an_ml_api_url_the_batch_refuses_and_asks_nothing() {
    let r = run(Fake {
        written: 2,
        no_url: true,
        ..Fake::default()
    });
    assert_eq!(r.code, Some(78), "stderr: {}", r.stderr);
    assert!(
        r.invoked.is_empty(),
        "a model was asked with no address: {:?}",
        r.invoked
    );
    assert!(r.stderr.contains("BOSS_ML_API_URL"), "{}", r.stderr);
    assert_eq!(field(&r, "exit_status"), "78");
}

/// The unit, read as text: it names no ML API of its own (the address
/// comes from /etc/boss/sor.env, rendered from the estate source), it no
/// longer pulls the retired local ML API up with `Requires=` — a
/// dependency systemd honours whether or not the required unit is
/// enabled, so the retirement's stop lasted until the next 02:30 — and
/// it declares the summary file both Exec lines share.
#[test]
fn the_unit_takes_its_address_from_the_rendered_env_and_hands_its_packet_a_summary() {
    let unit =
        std::fs::read_to_string(repo_root().join("infra/ml/boss-ml-inference-batch.service"))
            .expect("read the unit");
    let lines: Vec<&str> = unit
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    assert!(
        !lines.iter().any(|l| l.contains("127.0.0.1")),
        "the unit still spells a loopback address:\n{unit}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("boss-ml-api.service")),
        "the unit still depends on the retired local boss-ml-api.service:\n{unit}"
    );
    assert!(
        lines.contains(&"EnvironmentFile=/etc/boss/sor.env"),
        "{unit}"
    );
    let summary = lines
        .iter()
        .find_map(|l| l.strip_prefix("Environment=BOSS_RUN_SUMMARY_FILE="))
        .unwrap_or_else(|| panic!("the unit declares no BOSS_RUN_SUMMARY_FILE:\n{unit}"));
    // User=boss cannot write /run itself: the file lives in the unit's
    // own RuntimeDirectory, which systemd removes only after ExecStopPost.
    let rundir = lines
        .iter()
        .find_map(|l| l.strip_prefix("RuntimeDirectory="))
        .unwrap_or_else(|| panic!("the unit declares no RuntimeDirectory:\n{unit}"));
    assert!(
        summary.starts_with(&format!("/run/{rundir}/")),
        "the summary file {summary} is not inside the unit's RuntimeDirectory {rundir}"
    );
}

// ---------------------------------------------------------------------
// THE MACHINE TOKEN (design 6805c764; backlog 44b2087e, the owed roster)
// ---------------------------------------------------------------------
//
// The seven infer-batch POSTs go to the system of record's `ml` port,
// which is gated, and went out with nothing at all: measured on the live
// record, `ml | - | POST /api/ml/models/{id}/infer-batch`, tokenless at
// 02:30Z on 2026-10-06, -07 and -08. The script now asks the one shell
// reader for a header file and hands it to every POST. PRESENTED, never
// required: the unit runs as `boss`, which cannot read the token
// directory today, and that run must be exactly the run it was.

const TOKEN: &str = "synthetic-machine-fixture";

struct TokenRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// `mode=<octal>` and the file's one line, per header file handed over.
    headers: String,
    /// The path of each header file handed over, one line each.
    paths: String,
    /// Every argv the fake curl was run with, one line each.
    argv: String,
    summary: serde_json::Value,
    /// What is left under TMPDIR and the runtime directory afterwards.
    left: Vec<String>,
    run_dir: String,
    /// The case's scratch, for a run that follows it.
    dir: std::path::PathBuf,
}

/// A fake curl that also reads the header FILE it is handed, while the
/// file exists. `FAKE_CURL_KILL`: on the first POST, SIGKILL the batch
/// itself — with the shell's own builtin, the gate image has no `kill`
/// binary — so no trap of any kind runs.
const HEADER_READING_CURL: &str = r#"#!/usr/bin/env bash
echo "$*" >> "$FAKE_ARGV"
url="" prev=""
for a in "$@"; do
    if [ "$prev" = -H ]; then
        case "$a" in
            @*)
                { stat -c 'mode=%a' "${a#@}"; cat "${a#@}"; } >> "$FAKE_HEADERS"
                echo "${a#@}" >> "$FAKE_PATHS"
                ;;
        esac
    fi
    prev="$a" url="$a"
done
if [ -n "${FAKE_CURL_KILL:-}" ]; then
    builtin kill -KILL "$(cat "$FAKE_PIDFILE")"
    exit 0
fi
id="${url##*/models/}"
id="${id%%/*}"
for f in $FAKE_CURL_FAIL; do
    if [ "$f" = "$id" ]; then exit 22; fi
done
printf '{"model_id":"%s","written":2,"skipped":0,"errors":[]}' "$id"
"#;

struct TokenCase<'a> {
    tag: &'a str,
    slot: Option<&'a str>,
    hosts: &'a str,
    fail: &'a str,
    runtime: bool,
    kill: bool,
    /// Run in the scratch a `kill` case left, as the next night does.
    reuse: Option<std::path::PathBuf>,
}

impl Default for TokenCase<'_> {
    fn default() -> Self {
        TokenCase {
            tag: "",
            slot: None,
            hosts: "ml.test",
            fail: "",
            runtime: false,
            kill: false,
            reuse: None,
        }
    }
}

fn run_token(case: TokenCase) -> TokenRun {
    let root = repo_root();
    let fresh = case.reuse.is_none();
    let dir = case.reuse.unwrap_or_else(|| {
        boss_testing::scratch_dir(&format!("boss-infer-batch-token-{}", case.tag))
    });
    let (mount, tmp, run) = (dir.join("mount"), dir.join("tmp"), dir.join("run"));
    if fresh {
        for d in [&mount, &tmp, &run] {
            boss_testing::create_dir(d);
        }
        if let Some(value) = case.slot {
            boss_testing::write_file(&mount.join("current"), value);
        }
        boss_testing::write_exec(&dir.join("curl"), HEADER_READING_CURL);
    }
    for f in ["headers", "paths", "argv"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let summary_file = dir.join("summary.json");
    // `exec` keeps the pid the fake curl is told to kill.
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c")
        .arg("echo $$ > \"$FAKE_PIDFILE\"; exec bash \"$0\"")
        .arg(root.join(SCRIPT))
        .env("BOSS_ML_CURL", dir.join("curl"))
        .env("BOSS_ML_API_URL", "http://ml.test")
        .env("BOSS_RUN_SUMMARY_FILE", &summary_file)
        .env("BOSS_MACHINE_TOKEN_DIR", &mount)
        .env("BOSS_MACHINE_TOKEN_HOSTS", case.hosts)
        .env("BOSS_SOR_ENV", dir.join("no-sor.env"))
        .env("TMPDIR", &tmp)
        .env_remove("RUNTIME_DIRECTORY")
        .env("FAKE_ARGV", dir.join("argv"))
        .env("FAKE_HEADERS", dir.join("headers"))
        .env("FAKE_PATHS", dir.join("paths"))
        .env("FAKE_PIDFILE", dir.join("pid"))
        .env("FAKE_CURL_FAIL", case.fail)
        .env_remove("FAKE_CURL_KILL")
        .current_dir(&root);
    if case.runtime {
        cmd.env("RUNTIME_DIRECTORY", &run);
    }
    if case.kill {
        cmd.env("FAKE_CURL_KILL", "1");
    }
    let out = cmd.output().expect("run run-inference-batch.sh");
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    TokenRun {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        headers: read("headers"),
        paths: read("paths"),
        argv: read("argv"),
        summary: std::fs::read_to_string(&summary_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(serde_json::Value::Null),
        left: [&tmp, &run]
            .iter()
            .flat_map(|d| std::fs::read_dir(d).unwrap())
            .filter_map(Result::ok)
            .map(|e| e.path().display().to_string())
            .collect(),
        run_dir: run.display().to_string(),
        dir,
    }
}

/// Every POST carries the token when the mount holds one and the ML API
/// is on the host list, and in every other state of the mount the run is
/// the run a host with no token makes: the same seven POSTs, the same
/// output, the same exit, the same summary.
#[test]
fn every_infer_batch_presents_the_machine_token_and_is_never_stopped_by_it() {
    let absent = run_token(TokenCase {
        tag: "absent",
        ..TokenCase::default()
    });
    assert_eq!(absent.code, Some(0), "{}", absent.stderr);
    assert_eq!(absent.headers, "", "no token, and a header file was sent");
    assert_eq!(absent.argv.lines().count(), ALL_MODELS.len());
    assert_eq!(absent.summary["exit_status"], "0");

    let fixture = format!("{TOKEN}\n");
    let broken = format!("{TOKEN}\nsecond-line\n");
    for (tag, slot, hosts, carries) in [
        ("present", fixture.as_str(), "ml.test", true),
        ("off-host", fixture.as_str(), "elsewhere.test", false),
        ("line-break", broken.as_str(), "ml.test", false),
    ] {
        let r = run_token(TokenCase {
            tag,
            slot: Some(slot),
            hosts,
            ..TokenCase::default()
        });
        assert_eq!(r.code, Some(0), "{tag}: {}", r.stderr);
        assert_eq!(r.stdout, absent.stdout, "{tag}: the run's own output");
        assert_eq!(r.summary, absent.summary, "{tag}: what the packet is told");
        assert_eq!(
            r.argv.lines().count(),
            ALL_MODELS.len(),
            "{tag}: every model is still asked, once: {}",
            r.argv
        );
        assert!(
            !r.argv.contains(TOKEN) && !r.stdout.contains(TOKEN) && !r.stderr.contains(TOKEN),
            "{tag}: the token is in curl's argv or on the run's output"
        );
        if carries {
            assert_eq!(
                r.headers,
                format!("mode=600\nx-boss-machine-token: {TOKEN}\n").repeat(ALL_MODELS.len()),
                "{tag}: every POST carries the token, as a 0600 header file"
            );
        } else {
            assert_eq!(r.headers, "", "{tag}: these POSTs go out without the token");
            assert!(
                !r.stderr.is_empty(),
                "{tag}: a token that is held and not sent is said on stderr"
            );
        }
        assert!(
            r.left.is_empty(),
            "{tag}: the header directory outlived the run: {:?}",
            r.left
        );
    }
}

/// The reader chains its cleanup IN FRONT OF the script's own EXIT trap,
/// which records `exit_status` for the packet. A failed run must still
/// record the exit it had, and still remove the header directory.
#[test]
fn a_failed_run_with_a_token_records_its_exit_and_leaves_no_header_file() {
    let r = run_token(TokenCase {
        tag: "failed",
        slot: Some(TOKEN),
        fail: "mdl-next-action-past-due-invoice-v1",
        ..TokenCase::default()
    });
    assert_eq!(r.code, Some(1), "{}", r.stderr);
    assert_eq!(
        r.summary["exit_status"], "1",
        "the script's own trap still sees the exit: {}",
        r.summary
    );
    assert_eq!(
        r.summary["models_failed"],
        "mdl-next-action-past-due-invoice-v1"
    );
    assert_eq!(r.headers.lines().count(), 2 * ALL_MODELS.len());
    assert!(r.left.is_empty(), "{:?}", r.left);
}

/// A KILLED BATCH. A SIGKILL runs no trap, so nothing the script does can
/// remove the file; what holds is where it was made and who finds it.
/// Under the unit (`RuntimeDirectory=boss-ml-inference-batch`) it is made
/// in the unit's own tmpfs directory, which systemd removes when the unit
/// stops. With no such directory it is left under TMPDIR — and the next
/// night's run removes it before it makes its own, and says so.
#[test]
fn a_killed_batch_leaves_its_header_file_where_the_unit_or_the_next_run_removes_it() {
    let under_unit = run_token(TokenCase {
        tag: "killed-under-unit",
        slot: Some(TOKEN),
        runtime: true,
        kill: true,
        ..TokenCase::default()
    });
    assert_eq!(under_unit.code, None, "the batch died of the signal");
    let path = under_unit.paths.lines().next().unwrap_or_default();
    assert!(
        path.starts_with(&format!("{}/boss-secret-header.", under_unit.run_dir)),
        "the header file was made at {path:?}, not in the unit's runtime directory"
    );
    assert_eq!(
        under_unit.left.len(),
        1,
        "one directory, in the runtime directory and nowhere else: {:?}",
        under_unit.left
    );
    assert!(under_unit.left[0].starts_with(&under_unit.run_dir));

    let killed = run_token(TokenCase {
        tag: "killed",
        slot: Some(TOKEN),
        kill: true,
        ..TokenCase::default()
    });
    assert_eq!(killed.code, None, "the batch died of the signal");
    assert_eq!(
        killed.left.len(),
        1,
        "a SIGKILL leaves the one directory: {:?}",
        killed.left
    );
    let next = run_token(TokenCase {
        tag: "killed",
        slot: Some(TOKEN),
        reuse: Some(killed.dir.clone()),
        ..TokenCase::default()
    });
    assert_eq!(next.code, Some(0), "{}", next.stderr);
    assert!(
        next.stderr
            .contains("removed 1 header directory a killed earlier run left"),
        "the next run says what it removed: {}",
        next.stderr
    );
    assert!(
        next.left.is_empty(),
        "and leaves nothing of its own: {:?}",
        next.left
    );
}

/// The unit still declares the runtime directory the kill case leans on.
#[test]
fn the_batch_unit_declares_a_runtime_directory() {
    let unit =
        std::fs::read_to_string(repo_root().join("infra/ml/boss-ml-inference-batch.service"))
            .unwrap();
    assert!(
        unit.lines()
            .any(|l| l.trim() == "RuntimeDirectory=boss-ml-inference-batch"),
        "without RuntimeDirectory= a killed batch's header file is left in /tmp until the next run"
    );
}
