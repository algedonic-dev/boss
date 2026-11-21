//! `infra/forge/read-github-job-log.sh` is RUN, not read — against the
//! job log GitHub serves for a failed mirror Gate
//! (`tests/fixtures/github/gate-job-log.txt`, BOM, a timestamp per line
//! and a `##[group]Run …` header per step, as GitHub writes it), with
//! every outside party a stub: a `curl` that serves fixture files and
//! records every request, and the one stub `kubectl` the forge's REAL
//! credential render is driven through. Never the network.
//!
//! THE GAP (backlog 8f266452, David 2026-11-20). PR #261's mirror Gate
//! failed (job 135639960947) and nothing in BOSS could name the test: its
//! annotations quote a prose line (54278a24), its job log answers 403 to
//! an anonymous reader, and the one reader holding a token —
//! read-publish-checks — reads only the OPEN publish packet's failing
//! checks. A closed packet's failure, or any other run, had no door.
//! This verb is that door, with no new credential: the broker's existing
//! read-only checks token (actions:read + metadata:read on the mirror),
//! minted by the same rule when this verb is filed. These cases pin it:
//!
//!   * `job <id>` reads one log, and the answer names every failed test
//!     with its binary and cargo's rerun selector, the receipt's failed
//!     checks, and a bounded tail around the FIRST failure;
//!   * `run <id>` lists the run's jobs anonymously and reads the logs of
//!     at most two that did not pass, naming the rest as not read;
//!   * the repository is never a parameter: a mirror that is not the one
//!     the checks token is minted for is REFUSED before anything is read,
//!     and so is an id that is not a number;
//!   * the token reaches only the log read, through a header file, and is
//!     nowhere in what the verb prints or in any process's argv; the slot
//!     it was rendered into is gone when the run ends;
//!   * no live token, or a refused read, is stated with why — never a
//!     silent empty answer.

use boss_testing::{repo_root, scratch_dir, with_default_signals, write_exec, write_file};
use std::path::{Path, PathBuf};

const SCRIPT: &str = "infra/forge/read-github-job-log.sh";
const VERB: &str = "infra/ops/verbs/read-github-job-log.json";
const CHECKS_RULE: &str = "infra/dispatcher/rules/broker-mints-the-algedonic-dev-checks-token-when-a-checks-read-is-filed.toml";
const CHECKS_TOKEN: &str = "ghs_fixture0000000000000000000000joblog1";
/// PR #261's failed mirror Gate (backlog 8f266452), the job the packet
/// names; its run.
const JOB: &str = "135639960947";
const RUN: &str = "44546952203";
const GATE: &str = "Gate (infra/gate.sh, full)";

fn fixture(name: &str) -> PathBuf {
    repo_root()
        .join("crates/core/boss-testing/tests/fixtures/github")
        .join(name)
}

/// The repository the checks token is minted for: the rule's own
/// `verify_repo`, which boss-dispatcher's github_app_installation_token_rules
/// holds equal to the estate's declared mirror.
fn tokens_repository() -> String {
    let doc: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo_root().join(CHECKS_RULE)).unwrap()).unwrap();
    let raw = doc["rule"][0]["do"][0]["args"]["verify_repo"]
        .as_str()
        .expect("the checks rule declares verify_repo")
        .to_string();
    raw.trim_matches('"').to_string()
}

struct Run {
    root: PathBuf,
    routes: PathBuf,
    log: PathBuf,
}

impl Run {
    fn new(case: &str) -> Run {
        let root = scratch_dir(&format!("read-github-job-log-{case}"));
        let routes = root.join("routes");
        let log = root.join("curl.log");
        write_file(&routes, "");
        let stubs = root.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        write_exec(
            &stubs.join("curl"),
            &format!(
                r#"#!/bin/sh
echo "$*" >> '{log}'
url=""; out=""; wfmt=""; prev=""
for a in "$@"; do
    case "$prev" in -o) out="$a" ;; -w) wfmt="$a" ;; esac
    case "$a" in http://*|https://*) url="$a" ;; esac
    prev="$a"
done
# Whether a `-H @file` holds a GitHub credential; the line is kept, so a
# case can say WHICH token went out.
auth=no; prev=""
for a in "$@"; do
    if [ "$prev" = -H ]; then
        case "$a" in @*) if grep -q '^Authorization: ' "${{a#@}}" 2>/dev/null; then
            auth=yes; grep '^Authorization: ' "${{a#@}}" >> '{log}.auth'
        fi ;; esac
    fi
    prev="$a"
done
printf '%s auth=%s\n' "$url" "$auth" >> '{log}.calls'
# First route whose needle the URL contains wins. A route may name the
# answer's status (200 when it does not): a `-w` caller is told it and
# `-o` gets the body; a caller without `-w` gets what `curl -f` gives on
# a 4xx, exit 22.
while IFS='	' read -r needle file status; do
    [ -f "$file" ] || continue
    case "$url" in *"$needle"*)
        status="${{status:-200}}"
        if [ -n "$out" ]; then cat "$file" > "$out"; elif [ -n "$wfmt" ] || [ "$status" -lt 400 ]; then cat "$file"; fi
        if [ -n "$wfmt" ]; then printf '%s' "$status"; exit 0; fi
        if [ "$status" -ge 400 ]; then
            echo "curl: (22) The requested URL returned error: $status" >&2
            exit 22
        fi
        exit 0 ;;
    esac
done < '{routes}'
if [ -n "$wfmt" ]; then
    if [ -n "$out" ]; then : > "$out"; fi
    printf '404'
    exit 0
fi
echo "curl: (22) The requested URL returned error: 404 for $url" >&2
exit 22
"#,
                log = log.display(),
                routes = routes.display(),
            ),
        );
        write_exec(
            &root.join("kubectl"),
            boss_testing::kubectl_secret_stub::SECRET_KUBECTL,
        );
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        Run { root, routes, log }
    }

    /// The broker's Secret, holding a live checks token.
    fn live_token(&self) {
        let dir = self.root.join("secrets/boss/github-app-algedonic-dev");
        std::fs::create_dir_all(&dir).unwrap();
        for (k, v) in [
            ("checks-token", CHECKS_TOKEN.to_string()),
            ("checks-token.expires-at", minutes_from_now(50)),
            ("checks-token.minted-for", "refresh".to_string()),
        ] {
            std::fs::write(dir.join(k), v).unwrap();
        }
    }

    /// The broker's Secret as the converge created it: no key yet.
    fn empty_secret(&self) {
        std::fs::create_dir_all(self.root.join("secrets/boss/github-app-algedonic-dev")).unwrap();
    }

    fn checks_slot(&self) -> PathBuf {
        self.root.join("github-app/algedonic-dev-checks.token")
    }

    fn route(&self, needle: &str, file: &Path) {
        self.route_status(needle, file, 200);
    }

    fn route_status(&self, needle: &str, file: &Path, status: u16) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.routes)
            .unwrap();
        writeln!(f, "{needle}\t{}\t{status}", file.display()).unwrap();
    }

    fn json(&self, name: &str, v: serde_json::Value) -> PathBuf {
        let p = self.root.join(name);
        write_file(&p, &v.to_string());
        p
    }

    /// The job's own record, as GET /actions/jobs/<id> answers it.
    fn job_record(&self, id: &str, name: &str, conclusion: &str) -> PathBuf {
        self.json(
            &format!("job-{id}.json"),
            serde_json::json!({"id": id.parse::<u64>().unwrap(), "run_id": RUN.parse::<u64>().unwrap(),
                               "name": name, "status": "completed", "conclusion": conclusion}),
        )
    }

    fn go_as(&self, slug: &str, args: &[&str]) -> (i32, String) {
        use std::os::unix::fs::MetadataExt;
        let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let uid = std::fs::metadata(&self.root).unwrap().uid().to_string();
        let out = with_default_signals("bash")
            .arg(repo_root().join(SCRIPT))
            .args(args)
            .env_clear()
            .env(
                "PATH",
                format!("{}:{outer}", self.root.join("stubs").display()),
            )
            .env("BOSS_GITHUB_API", "https://api.github.invalid")
            .env("BOSS_MIRROR_SLUG", slug)
            .env("BOSS_RENDER_KUBECTL", self.root.join("kubectl"))
            .env("STUB_SECRETS", self.root.join("secrets"))
            .env("BOSS_CHECKS_TOKEN_FILE", self.checks_slot())
            .env("BOSS_CHECKS_TOKEN_OWNER_UID", &uid)
            .output()
            .expect("the verb runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    fn go(&self, args: &[&str]) -> (i32, String) {
        self.go_as(&tokens_repository(), args)
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(format!("{}.calls", self.log.display()))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn auth_lines(&self) -> Vec<String> {
        std::fs::read_to_string(format!("{}.auth", self.log.display()))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Nothing the token is, anywhere it could leak: no line the verb
    /// printed, and no process's argv. The slot is gone.
    fn the_token_is_nowhere(&self, text: &str) {
        let carrying: Vec<&str> = text.lines().filter(|l| l.contains(CHECKS_TOKEN)).collect();
        assert!(
            carrying.is_empty(),
            "an output line carries the token: {carrying:?}"
        );
        let argv = std::fs::read_to_string(&self.log).unwrap_or_default();
        assert!(
            !argv.contains(CHECKS_TOKEN),
            "the token is in curl's argv:\n{argv}"
        );
        assert!(
            !self.checks_slot().exists(),
            "the slot rendered for the read is removed when the run ends"
        );
    }
}

fn minutes_from_now(minutes: i64) -> String {
    let out = std::process::Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("{minutes} minutes"),
            "+%Y-%m-%dT%H:%M:%S.123456789+00:00",
        ])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn logs_url(id: &str) -> String {
    format!("/actions/jobs/{id}/logs")
}

/// The answer's tail block: the lines after `tail:` that start `  | `.
fn tail_of(text: &str) -> Vec<String> {
    text.lines()
        .skip_while(|l| !l.trim_start().starts_with("tail:"))
        .skip(1)
        .take_while(|l| l.starts_with("  | "))
        .map(|l| l["  | ".len()..].to_string())
        .collect()
}

/// ONE JOB: the answer names every test the log says FAILED, with its
/// binary and the selector that reruns it, the checks the gate's receipt
/// names failed, and the log around the first failure. The log is read
/// with the broker's token; the job's own record is read anonymously.
#[test]
fn a_job_log_names_its_failed_tests_the_receipt_and_the_tail_around_the_first_failure() {
    let run = Run::new("job");
    run.live_token();
    run.route(&logs_url(JOB), &fixture("gate-job-log.txt"));
    run.route(
        &format!("/actions/jobs/{JOB}"),
        &run.job_record(JOB, GATE, "failure"),
    );

    let (code, text) = run.go(&["job", JOB]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!("job {JOB}: {GATE} — failure (run {RUN})")),
        "the job's id, name and conclusion:\n{text}"
    );
    assert!(
        text.contains(
            "failed test: the_door_refuses_a_path_it_cannot_resolve — in \
             tests/shared_checkout_guard.rs — rerun: cargo test -p boss-testing --test \
             shared_checkout_guard the_door_refuses_a_path_it_cannot_resolve"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "failed test: train::forge::tests::a_job_log_is_read_by_its_job_id — in \
             unittests src/main.rs — rerun: cargo test -p boss-cli --bin boss \
             train::forge::tests::a_job_log_is_read_by_its_job_id"
        ),
        "{text}"
    );
    assert!(
        text.contains("receipt: failed checks: test"),
        "the receipt's failed checks:\n{text}"
    );
    let tail = tail_of(&text);
    assert!(
        tail.iter()
            .any(|l| l == "test the_door_refuses_a_path_it_cannot_resolve ... FAILED"),
        "the tail holds the first failure:\n{text}"
    );
    assert!(
        tail.iter().any(|l| l.contains("panicked at")),
        "and what follows it:\n{text}"
    );
    assert!(
        tail.len() <= 61,
        "40 lines before the first failure, it, and 20 after: {}",
        tail.len()
    );
    assert!(
        !tail.iter().any(|l| l.starts_with("2026-11-19T")),
        "GitHub's timestamps are stripped:\n{text}"
    );
    assert!(
        !tail.iter().any(|l| l.contains('\u{1b}')),
        "no terminal escape reaches the record:\n{text}"
    );
    let last = text.lines().last().unwrap_or_default();
    assert!(
        last.starts_with(&format!(
            "read-github-job-log: job {JOB} read — 2 failed test(s): "
        )) && last.contains("the_door_refuses_a_path_it_cannot_resolve"),
        "the answer line names the failing test: {last}"
    );

    let calls = run.calls();
    let logs: Vec<&String> = calls
        .iter()
        .filter(|c| c.contains(&logs_url(JOB)))
        .collect();
    assert_eq!(logs.len(), 1, "{calls:?}");
    assert!(logs[0].ends_with(" auth=yes"), "{calls:?}");
    assert!(
        calls
            .iter()
            .filter(|c| !c.contains("/logs"))
            .all(|c| c.ends_with(" auth=no")),
        "every other GitHub read stays anonymous:\n{calls:?}"
    );
    assert!(
        calls.iter().all(|c| c.contains(&format!(
            "api.github.invalid/repos/{}/",
            tokens_repository()
        ))),
        "every read is of the mirror:\n{calls:?}"
    );
    assert_eq!(
        run.auth_lines(),
        vec![format!("Authorization: Bearer {CHECKS_TOKEN}")]
    );
    run.the_token_is_nowhere(&text);
}

/// THE TAIL IS BOUNDED IN BYTES, and keeps the failure. Sixty-one lines
/// of two kilobytes each are far past 8 KiB; the leading context goes
/// first, then the trailing, and the first failure line stays.
#[test]
fn the_tail_is_held_to_eight_kibibytes_and_keeps_the_first_failure() {
    let run = Run::new("tail-cap");
    run.live_token();
    let wide = "x".repeat(2048);
    let mut log = String::from("\u{feff}");
    for i in 0..45 {
        log.push_str(&format!("2026-11-20T00:00:00.0000000Z before {i} {wide}\n"));
    }
    log.push_str("2026-11-20T00:00:01.0000000Z test a::wide::case ... FAILED\n");
    for i in 0..30 {
        log.push_str(&format!("2026-11-20T00:00:02.0000000Z after {i} {wide}\n"));
    }
    let file = run.root.join("wide.log");
    write_file(&file, &log);
    run.route(&logs_url(JOB), &file);
    run.route(
        &format!("/actions/jobs/{JOB}"),
        &run.job_record(JOB, GATE, "failure"),
    );

    let (code, text) = run.go(&["job", JOB]);
    assert_eq!(code, 0, "{text}");
    let tail = tail_of(&text);
    let bytes: usize = tail.iter().map(|l| l.len() + 1).sum();
    assert!(bytes <= 8192, "the tail is {bytes} bytes:\n{text}");
    assert!(
        tail.iter().any(|l| l == "test a::wide::case ... FAILED"),
        "the first failure survives the cap:\n{tail:?}"
    );
    assert!(
        text.contains("receipt: unread — the log holds no step headed Run "),
        "a log with no receipt says so:\n{text}"
    );
    run.the_token_is_nowhere(&text);
}

/// ONE RUN: its jobs are listed anonymously (the mirror is public), and
/// the logs of at most two that did not pass are read; a third is named
/// as not read, with where GitHub holds it. A job that passed is read for
/// nothing.
#[test]
fn a_run_reads_at_most_two_failed_jobs_and_names_the_rest() {
    let run = Run::new("run");
    run.live_token();
    let ids = ["135639960001", "135639960002", "135639960003", JOB];
    let jobs = run.json(
        "jobs.json",
        serde_json::json!({"total_count": 4, "jobs": [
            {"id": 135639960001u64, "name": "web", "status": "completed", "conclusion": "success"},
            {"id": 135639960002u64, "name": "image", "status": "completed", "conclusion": "failure"},
            {"id": 135639960003u64, "name": "locomotive", "status": "completed", "conclusion": "timed_out"},
            {"id": 135639960947u64, "name": GATE, "status": "completed", "conclusion": "failure"}
        ]}),
    );
    run.route(&format!("/actions/runs/{RUN}/jobs"), &jobs);
    for id in &ids[1..] {
        run.route(&logs_url(id), &fixture("gate-job-log.txt"));
    }

    let (code, text) = run.go(&["run", RUN]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!("run {RUN}: 4 job(s), 3 not passed")),
        "{text}"
    );
    assert!(text.contains("job 135639960002: image — failure"), "{text}");
    assert!(
        text.contains("job 135639960003: locomotive — timed_out"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "job {JOB}: {GATE} — failure\n  not read: this run reads at most 2 job logs"
        )),
        "the third is named, not read:\n{text}"
    );
    assert!(
        !text.contains("job 135639960001"),
        "a job that passed: {text}"
    );
    let last = text.lines().last().unwrap_or_default();
    assert!(
        last.starts_with(&format!(
            "read-github-job-log: run {RUN} — 3 job(s) not passed, 2 log(s) read — 2 failed test(s): "
        )),
        "the answer line counts a test two jobs name once: {last}"
    );
    let calls = run.calls();
    assert!(
        !calls.iter().any(|c| c.contains(&logs_url(ids[0]))),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.contains(&logs_url(JOB))),
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| c.contains("/logs")).count(),
        2,
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .filter(|c| c.contains(&format!("/actions/runs/{RUN}/jobs")))
            .all(|c| c.ends_with(" auth=no")),
        "the job list is read anonymously:\n{calls:?}"
    );
    run.the_token_is_nowhere(&text);
}

/// A RUN WITH NOTHING FAILING TOUCHES NO CREDENTIAL: no log to read, so
/// the token is never rendered onto the host.
#[test]
fn a_run_with_nothing_failing_renders_no_token() {
    let run = Run::new("run-green");
    run.live_token();
    let jobs = run.json(
        "jobs.json",
        serde_json::json!({"total_count": 1, "jobs": [
            {"id": 135639960001u64, "name": "web", "status": "completed", "conclusion": "success"}
        ]}),
    );
    run.route(&format!("/actions/runs/{RUN}/jobs"), &jobs);
    let (code, text) = run.go(&["run", RUN]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains(&format!("run {RUN}: 1 job(s), 0 not passed")),
        "{text}"
    );
    assert!(!text.contains("checks token"), "{text}");
    assert!(run.auth_lines().is_empty());
    assert!(!run.checks_slot().exists());
}

/// THE REPOSITORY IS NOT A PARAMETER, AND NO OTHER IS READ. The verb
/// reads the mirror the estate declares — and refuses, before any read or
/// render, when that is not the repository the checks token is minted
/// for: a sibling (the private DR copy), a fixture, another owner's
/// repository of the same name, or the same name in another case.
#[test]
fn a_mirror_that_is_not_the_tokens_repository_is_refused() {
    let repo = tokens_repository();
    let name = repo.split('/').nth(1).expect("owner/repo");
    for slug in [
        "fixture-upstream/mirror".to_string(),
        format!("{repo}-dr"),
        format!("another-owner/{name}"),
        repo.to_uppercase(),
    ] {
        let run = Run::new("other-repo");
        run.live_token();
        let (code, text) = run.go_as(&slug, &["job", JOB]);
        assert_eq!(code, 2, "slug {slug:?}: {text}");
        assert!(text.contains("REFUSED"), "{text}");
        assert!(
            run.calls().is_empty(),
            "nothing was read: {:?}",
            run.calls()
        );
        assert!(!run.checks_slot().exists(), "nothing was rendered");
    }
}

/// AN ID IS A NUMBER, AND THE MODE ONE OF THREE WORDS. Anything else is
/// refused before a read — the runner's pattern is the first wall, this
/// is the second.
#[test]
fn an_id_that_is_not_a_number_is_refused() {
    for args in [
        vec!["job", "abc"],
        vec!["job", "13563996094x"],
        vec!["job", "-1"],
        vec!["job", "123456789012345678901"],
        vec!["job", "1/../../orgs"],
        vec!["job"],
        vec!["run", ""],
        vec!["logs", JOB],
        vec!["job", JOB, "extra"],
        vec![],
    ] {
        let run = Run::new("bad-id");
        run.live_token();
        let (code, text) = run.go(&args);
        assert_eq!(code, 2, "{args:?}: {text}");
        assert!(text.contains("REFUSED"), "{args:?}: {text}");
        assert!(run.calls().is_empty(), "{args:?}: {:?}", run.calls());
    }
}

/// NO LIVE TOKEN IS STATED, NOT SILENT. A filing that reaches the runner
/// before its mint lands finds the Secret's key absent: no log read is
/// made, and the answer says why and that a refile reads it.
#[test]
fn no_live_token_is_stated_and_no_log_is_read() {
    let run = Run::new("no-token");
    run.empty_secret();
    run.route(&logs_url(JOB), &fixture("gate-job-log.txt"));
    run.route(
        &format!("/actions/jobs/{JOB}"),
        &run.job_record(JOB, GATE, "failure"),
    );
    let (code, text) = run.go(&["job", JOB]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("not read: no live checks token"),
        "the job says why it was not read:\n{text}"
    );
    assert!(
        !run.calls().iter().any(|c| c.contains("/logs")),
        "{:?}",
        run.calls()
    );
    assert!(run.auth_lines().is_empty());
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.starts_with("read-github-job-log: FAILED — ")),
        "{text}"
    );
}

/// A REFUSED READ IS STATED IN GITHUB'S OWN WORDS.
#[test]
fn a_refused_log_read_is_stated_with_githubs_answer() {
    let run = Run::new("forbidden");
    run.live_token();
    let body = run.json(
        "forbidden.json",
        serde_json::json!({"message": "Must have admin rights to Repository.", "status": "403"}),
    );
    run.route_status(&logs_url(JOB), &body, 403);
    run.route(
        &format!("/actions/jobs/{JOB}"),
        &run.job_record(JOB, GATE, "failure"),
    );
    let (code, text) = run.go(&["job", JOB]);
    assert_eq!(code, 1, "{text}");
    assert!(
        text.contains("GitHub answered 403")
            && text.contains("Must have admin rights to Repository."),
        "{text}"
    );
    assert!(!text.contains("failed test:"), "nothing invented: {text}");
    run.the_token_is_nowhere(&text);
}

/// `--check` holds no token and opens no socket.
#[test]
fn check_reads_the_tools_and_the_mirror_and_touches_no_network() {
    let run = Run::new("check");
    let (code, text) = run.go(&["--check"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("--check ok"), "{text}");
    assert!(run.calls().is_empty(), "{:?}", run.calls());
    let (code, text) = run.go_as("fixture-upstream/mirror", &["--check"]);
    assert_ne!(
        code, 0,
        "a mirror that is not the token's repository: {text}"
    );
}

/// The verb file: serves the forge, runs this script, takes a mode of
/// three words and a numeric id — and NO repository parameter.
#[test]
fn the_verb_file_takes_a_mode_and_a_numeric_id_and_no_repository() {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo_root().join(VERB)).unwrap()).unwrap();
    assert_eq!(v["hosts"], serde_json::json!(["forge"]));
    assert_eq!(v["argv"], serde_json::json!([SCRIPT, "{1}", "{2}"]));
    let params = v["params"].as_array().expect("params");
    assert_eq!(params.len(), 2, "a mode and an id, nothing else: {v}");
    assert_eq!(params[0]["name"], "mode");
    assert_eq!(
        params[0]["one_of"],
        serde_json::json!(["job", "run", "--check"])
    );
    assert_eq!(params[1]["name"], "id");
    assert_eq!(params[1]["pattern"], "^[0-9]{1,20}$");
    assert_eq!(params[1]["optional"], true, "--check takes no id");
    assert!(
        v["timeout"].as_u64().is_some_and(|t| t <= 120),
        "the forge's runner is serial: {v}"
    );
    assert!(
        !v["about"].as_str().unwrap_or_default().contains("MUTATING"),
        "it reads"
    );
}

/// ONE FACT, TWO FILES (CLAUDE.md §9a). The verb finds the receipt by the
/// `Run …` header GitHub writes for the step, which is the step's own
/// `run:` line in the mirror's workflow.
#[test]
fn the_receipt_step_the_verb_reads_is_the_one_the_mirror_runs() {
    let ci = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap();
    let mut lines = ci
        .lines()
        .skip_while(|l| l.trim() != "- name: The gate's receipt");
    assert!(lines.next().is_some());
    let run_line = lines
        .map(str::trim)
        .find_map(|l| l.strip_prefix("run: "))
        .expect("the receipt step has a run: line");
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let declared = script
        .lines()
        .find_map(|l| l.strip_prefix("RECEIPT_STEP_RUN='"))
        .and_then(|l| l.strip_suffix('\''))
        .expect("the verb states RECEIPT_STEP_RUN='<the step's run line>'");
    assert_eq!(declared, run_line);
}
