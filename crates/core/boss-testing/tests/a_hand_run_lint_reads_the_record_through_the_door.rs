//! A lint run by hand where a door is on PATH reads the system of record
//! THROUGH THE DOOR, and sends no tokenless request of its own.
//!
//! MEASURED (backlog 83bff88b, from 0914f277; the machine gate's window,
//! 2026-10-09 17:17-20:01Z). Three would-refuse facts signed
//! `automation:gate-runner` came from the dev pod, presenting no token:
//! an agent or operator running `the-live-protocols-are-the-authored-
//! protocols` or `the-live-rules-are-the-authored-rules` by hand. Both
//! read through `infra/lint/lib/sor-read.sh`, whose `lint_sor_read` had
//! two ways to answer — a read a stamping launcher handed in (backlog
//! 44b2087e), or its own bare curl — and a hand run has no launcher. The
//! dev container holds the machine token for its doors, so the read goes
//! through `boss-api`, which stamps it, the way R3 of backlog 37742794
//! closed the pod's `boss-sor-read` and R2 closed the pre-flight's
//! edit-level read.
//!
//! WHAT THIS PINS. The door is asked only when nothing was handed in, the
//! run names no instance of its own (neither the lint's own URL variable
//! nor `BOSS_JOBS_URL`, the door's base — the door stamps estate hosts
//! only, but a lint aimed at a named instance must read THAT instance),
//! and a `boss-api` is on PATH. Then it is asked once, for the path of
//! the lint's URL, with the lint's own roll window, and never with a
//! service name the caller's shell carried (the path decides). A door
//! that cannot answer is answered `000` — never a curl in its place,
//! which would be the tokenless read this removes. Anything else reads
//! exactly as it did: the gate's runner has no door on PATH.
//!
//! The lints run from the repo root against a stub `boss-api` and a stub
//! `curl` that each log their calls, and a stub `sleep`. Every PATH
//! directory carrying a real `boss-api` is left out, so the pod's own
//! door never answers a test.
//!
//! tree-wide pin — it runs lints under infra/lint/ that no changed-file
//! map attributes to this crate, so every scoped gate runs it whatever
//! its scope (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Each lint that reads through `lint_sor_read`, the env var its header
/// names for its read surface, and the path of the URL it asks.
const LINTS: &[(&str, &str, &str)] = &[
    (
        "infra/lint/the-live-protocols-are-the-authored-protocols.sh",
        "BOSS_JOBS_URL",
        "/api/workflows",
    ),
    (
        "infra/lint/the-live-rules-are-the-authored-rules.sh",
        "BOSS_DISPATCHER_URL",
        "/api/dispatcher/rules",
    ),
    (
        "infra/lint/a-car-stays-under-the-edit-level.sh",
        "BOSS_JOBS_URL",
        "/api/tenant/edit-level",
    ),
];

const EDIT_LEVEL: &str = "infra/lint/a-car-stays-under-the-edit-level.sh";

struct Planted {
    bin: PathBuf,
    door_calls: PathBuf,
    curl_calls: PathBuf,
}

/// How the planted door answers.
#[derive(Clone, Copy)]
enum Door {
    /// No `boss-api` anywhere on PATH.
    Absent,
    /// `body` on stdout, `HTTP:<code>` on stderr, exit `rc` — the door's
    /// own shape (infra/dev/boss-api: 0 on a 2xx, 1 on any other status).
    Answers(&'static str, &'static str, i32),
    /// A refusal before any request: its words on stderr, no status
    /// line, exit 2 — what the door says for a usage or routing refusal.
    Refuses,
}

/// A stub door and curl that log `<argv> wait=<window> service=<name>`
/// and `<url>`; the curl prints `STUB_CODE` (default 503). The protocols
/// lint's self-test child aims at `[::1]:9` with BOSS_JOBS_URL named on
/// every run and is refused and not logged, as in
/// a_lint_that_reads_the_api_waits_out_a_roll.rs.
fn plant(tag: &str, door: Door) -> Planted {
    let root = scratch_dir(&format!("lint-door-{tag}"));
    let bin = root.join("bin");
    create_dir(&bin);
    let door_calls = root.join("door-calls.txt");
    let curl_calls = root.join("curl-calls.txt");
    write_exec(
        &bin.join("curl"),
        &format!(
            "#!/usr/bin/env bash\n\
             case \"$*\" in *'[::1]:9'*) printf '000'; exit 7 ;; esac\n\
             echo \"${{@: -1}}\" >> '{}'\n\
             printf '%s' \"${{STUB_CODE:-503}}\"\n",
            curl_calls.display()
        ),
    );
    write_exec(&bin.join("sleep"), "#!/usr/bin/env bash\nexit 0\n");
    let log = format!(
        "echo \"$* wait=${{BOSS_SOR_WAIT_SECONDS:-unset}} service=${{BOSS_SOR_SERVICE:-unset}}\" >> '{}'\n",
        door_calls.display()
    );
    match door {
        Door::Absent => {}
        Door::Answers(body, code, rc) => write_exec(
            &bin.join("boss-api"),
            &format!(
                "#!/usr/bin/env bash\n{log}\
                 printf '%s' '{body}'\n\
                 echo 'HTTP:{code}' >&2\n\
                 exit {rc}\n"
            ),
        ),
        Door::Refuses => write_exec(
            &bin.join("boss-api"),
            &format!(
                "#!/usr/bin/env bash\n{log}\
                 echo 'boss-api: refusing GET: the planted refusal' >&2\n\
                 exit 2\n"
            ),
        ),
    }
    Planted {
        bin,
        door_calls,
        curl_calls,
    }
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// The lint, from the repo root, with every read override and hand-over
/// removed from the environment and `env` added.
fn run(b: &Planted, lint: &str, env: &[(&str, &str)]) -> Run {
    let root = repo_root();
    let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let path = std::iter::once(b.bin.display().to_string())
        .chain(
            outer
                .split(':')
                .filter(|d| !d.is_empty() && !Path::new(d).join("boss-api").exists())
                .map(str::to_string),
        )
        .collect::<Vec<_>>()
        .join(":");
    let mut cmd = Command::new("bash");
    cmd.arg(root.join(lint))
        .current_dir(&root)
        .stdin(Stdio::null())
        .env("PATH", path);
    for var in [
        "BOSS_JOBS_URL",
        "BOSS_DISPATCHER_URL",
        "BOSS_ESTATE",
        "BOSS_SOR_WAIT_SECONDS",
        "BOSS_SOR_SERVICE",
        "BOSS_EDIT_LEVEL_ANSWER",
        "BOSS_LINT_SOR_HANDED_URL",
        "BOSS_LINT_SOR_HANDED_CODE",
        "BOSS_LINT_SOR_HANDED_BODY",
    ] {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("run {lint}: {e}"));
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn tag(lint: &str) -> &str {
    Path::new(lint)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("lint")
}

/// THE DEFECT. Run by hand with a door on PATH and nothing named, each
/// lint asks the door once — for its URL's path, with the lint's sixty
/// second window, and without the service name the caller's shell
/// carried — and curl never runs. The door's 503 is judged as the
/// lint's answer (a refusal, exit 3), and the lint says which reader
/// answered.
#[test]
fn a_hand_run_lint_reads_through_the_door() {
    for (lint, _, path) in LINTS {
        let b = plant(&format!("first-{}", tag(lint)), Door::Answers("", "503", 1));
        let r = run(&b, lint, &[("BOSS_SOR_SERVICE", "gateway")]);
        assert_eq!(
            lines(&b.door_calls),
            vec![format!("GET {path} wait=60 service=unset")],
            "{lint}: the door is asked once, for the path, with the lint's window and no \
             service name:\n{}",
            r.stderr
        );
        assert!(
            lines(&b.curl_calls).is_empty(),
            "{lint}: with a door on PATH the lint still sent its own tokenless request: {:?}",
            lines(&b.curl_calls)
        );
        assert_eq!(r.code, 3, "{lint}:\n{}\n{}", r.stdout, r.stderr);
        assert!(
            r.stderr.contains("HTTP 503") && r.stderr.contains("through the door (boss-api"),
            "{lint}: the door's status is the lint's, and the lint says which reader \
             answered:\n{}",
            r.stderr
        );
    }
}

/// The door's body is the lint's body: an edit level the door answers is
/// judged exactly as a read one. The trunk is named (`HEAD`) because the
/// lint's tree half diffs against one and a checkout may carry none of
/// the refs it tries (the dev worker's carries none, 2026-10-18); `core`
/// admits every path whatever that diff holds.
#[test]
fn the_doors_answer_is_judged_as_the_lints_own() {
    let b = plant(
        "answer",
        Door::Answers(r#"{"edit_level":"core"}"#, "200", 0),
    );
    let r = run(&b, EDIT_LEVEL, &[("BOSS_TRUNK_REF", "HEAD")]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(lines(&b.door_calls).len(), 1, "{}", r.stderr);
    assert!(
        lines(&b.curl_calls).is_empty(),
        "{:?}",
        lines(&b.curl_calls)
    );
    assert!(
        format!("{}{}", r.stdout, r.stderr).contains("edit level `core` admits"),
        "{}\n{}",
        r.stdout,
        r.stderr
    );
}

/// An operator's own roll window is the door's too.
#[test]
fn the_operators_window_reaches_the_door() {
    let b = plant("window", Door::Answers("", "503", 1));
    let r = run(&b, EDIT_LEVEL, &[("BOSS_SOR_WAIT_SECONDS", "0")]);
    assert_eq!(
        lines(&b.door_calls),
        vec!["GET /api/tenant/edit-level wait=0 service=unset".to_string()],
        "{}",
        r.stderr
    );
}

/// A door that refuses before any request is NO ANSWER (`000`), and its
/// own words reach the reader — never a curl in its place, which would
/// be the tokenless read the door exists to remove.
#[test]
fn a_door_that_cannot_answer_is_no_answer_and_never_a_curl() {
    for (lint, _, _) in LINTS {
        let b = plant(&format!("refused-{}", tag(lint)), Door::Refuses);
        let r = run(&b, lint, &[]);
        assert_eq!(lines(&b.door_calls).len(), 1, "{lint}:\n{}", r.stderr);
        assert!(
            lines(&b.curl_calls).is_empty(),
            "{lint}: a refused door fell back to a tokenless curl: {:?}",
            lines(&b.curl_calls)
        );
        assert_eq!(r.code, 3, "{lint}:\n{}\n{}", r.stdout, r.stderr);
        assert!(
            r.stderr.contains("HTTP 000") && r.stderr.contains("the planted refusal"),
            "{lint}: no answer, and the door's own words:\n{}",
            r.stderr
        );
    }
}

/// Where the door is NOT asked, each lint reads exactly as it did: a run
/// that names the lint's own instance, a dispatcher read in a run that
/// names the door's base, a box with no door, and a read handed in by a
/// launcher (which sends nothing at all).
#[test]
fn the_door_is_asked_only_when_nothing_is_named_or_handed() {
    for (lint, var, path) in LINTS {
        let t = tag(lint);
        let named = plant(&format!("named-{t}"), Door::Answers("", "200", 0));
        let r = run(&named, lint, &[(var, "http://registry.test:7900")]);
        assert!(
            lines(&named.door_calls).is_empty(),
            "{lint}: {var} named an instance and the door was asked:\n{}",
            r.stderr
        );
        assert_eq!(
            lines(&named.curl_calls),
            vec![format!("http://registry.test:7900{path}")],
            "{lint}: the named instance is read, as before:\n{}",
            r.stderr
        );

        let absent = plant(&format!("absent-{t}"), Door::Absent);
        let r = run(&absent, lint, &[]);
        assert_eq!(
            lines(&absent.curl_calls).len(),
            1,
            "{lint}: with no door on PATH the lint reads as it always did:\n{}",
            r.stderr
        );

        let handed = plant(&format!("handed-{t}"), Door::Answers("", "200", 0));
        let url = format!("http://registry.test:7900{path}");
        let r = run(
            &handed,
            lint,
            &[
                (var, "http://registry.test:7900"),
                ("BOSS_LINT_SOR_HANDED_URL", &url),
                ("BOSS_LINT_SOR_HANDED_CODE", "503"),
                ("BOSS_LINT_SOR_HANDED_BODY", "/dev/null"),
            ],
        );
        assert!(
            lines(&handed.door_calls).is_empty() && lines(&handed.curl_calls).is_empty(),
            "{lint}: handed its read, the lint asked something anyway:\n{}",
            r.stderr
        );
    }
    let b = plant("jobs-base-named", Door::Answers("", "200", 0));
    let r = run(
        &b,
        "infra/lint/the-live-rules-are-the-authored-rules.sh",
        &[("BOSS_JOBS_URL", "http://registry.test:7900")],
    );
    assert!(
        lines(&b.door_calls).is_empty(),
        "BOSS_JOBS_URL names the door's base, so the door would read another instance:\n{}",
        r.stderr
    );
    assert_eq!(lines(&b.curl_calls).len(), 1, "{}", r.stderr);
}
