//! The playground crawl does not start while the cluster converge is
//! rolling what it crawls (backlog c8c6b9a8).
//!
//! MEASURED 2026-09-28. Crawl 55bcf7fb ran 04:45:18-04:45:59Z. Converge
//! a10cbaa4 — the `maintenance-cluster-converge` packet the deploy
//! runner's unit opens in ExecStartPre and closes in ExecStopPost —
//! opened 04:41:01, recorded `build_head: 4f9a588` and
//! `roll_boss-playground: 4f9a588` on its run step, and closed 04:47:18.
//! The crawl sat inside that window, every one of its 43 routes read
//! `no-shell`, and the judge filed 43 items. The roll window was on the
//! record the whole time; nothing read it before the browser started.
//!
//! `infra/wait-out-a-converge.sh` reads it: a converge packet still open
//! (and younger than the unit's own 90-minute TimeoutStartSec, so a
//! packet whose close never landed does not hold the crawl forever), or
//! one whose run step BUILT (`build_head`) and completed inside the
//! settle window, is a decline — re-read every poll, for a bounded wait.
//! Clear, it returns at once; past the bound it says so and lets the
//! crawl run, because a crawl that never runs is silence and the judge
//! files a roll-shaped red as ONE item anyway. It never fails the check.
//!
//! The script runs against a stub API door (`BOSS_API_CURL`) that
//! serves one fixture per call and a stub `sleep` that records the wait
//! and returns at once, so the bounded wait is read without being spent.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GUARD: &str = "infra/wait-out-a-converge.sh";
const CRAWL: &str = "infra/cluster/manifests/boss-playground-crawl.yaml";

/// `date -u -d @<now - ago_s>` as the jobs API spells a timestamp —
/// fractional seconds and a `Z`.
fn ago(ago_s: i64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("-{ago_s} seconds"),
            "+%Y-%m-%dT%H:%M:%S.123456Z",
        ])
        .output()
        .expect("date runs");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A converge packet as `GET /api/jobs?...&full=true` lists it.
fn converge(
    id: &str,
    status: &str,
    opened_ago_s: i64,
    run: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "kind": "maintenance-cluster-converge",
        "status": status,
        "opened_at": ago(opened_ago_s),
        "steps": [
            { "spec_slug": "scheduled", "status": "completed", "metadata": {} },
            run,
        ],
    })
}

/// A run step that completed `ago_s` seconds ago; `built` = the tick
/// built and rolled a head (a deploy), else an `unchanged` tick.
fn run_step(ago_s: i64, built: bool) -> serde_json::Value {
    let metadata = if built {
        serde_json::json!({ "result": "ok", "build_head": "4f9a588",
                            "roll_boss-playground": "4f9a588", "observed_on": "deploy" })
    } else {
        serde_json::json!({ "result": "ok", "observed_on": "unchanged" })
    };
    serde_json::json!({ "spec_slug": "run", "status": "completed",
                        "completed_at": ago(ago_s), "metadata": metadata })
}

fn run_open() -> serde_json::Value {
    serde_json::json!({ "spec_slug": "run", "status": "ready", "metadata": {} })
}

fn page(rows: Vec<serde_json::Value>) -> String {
    let n = rows.len();
    serde_json::json!({ "data": rows, "total": n }).to_string()
}

struct Run {
    rc: i32,
    stdout: String,
    stderr: String,
    calls: Vec<String>,
    sleeps: Vec<String>,
}

/// Run the guard with the stub door answering `answers[i]` on call i (the
/// last answer repeats); an answer of `None` is a door that exits 7, the
/// jobs API dark through a roll.
fn run_guard(tag: &str, answers: &[Option<String>], env: &[(&str, &str)]) -> Run {
    let root = scratch_dir(&format!("wait-out-a-converge-{tag}"));
    let bin = root.join("bin");
    create_dir(&bin);
    let calls = root.join("calls.txt");
    let sleeps = root.join("sleeps.txt");
    write_file(&calls, "");
    write_file(&sleeps, "");
    for (i, a) in answers.iter().enumerate() {
        match a {
            Some(body) => write_file(&root.join(format!("answer-{}", i + 1)), body),
            None => write_file(&root.join(format!("dark-{}", i + 1)), ""),
        }
    }
    let door = root.join("door.sh");
    write_exec(
        &door,
        &format!(
            "#!/usr/bin/env bash\n\
             echo \"$*\" >> '{calls}'\n\
             n=$(wc -l < '{calls}')\n\
             last={last}\n\
             [ \"$n\" -le \"$last\" ] || n=$last\n\
             if [ -e '{root}/dark-'\"$n\" ]; then echo 'curl: (7) stub refused' >&2; exit 7; fi\n\
             cat '{root}/answer-'\"$n\"\n",
            calls = calls.display(),
            root = root.display(),
            last = answers.len(),
        ),
    );
    write_exec(
        &bin.join("sleep"),
        &format!(
            "#!/usr/bin/env bash\necho \"$1\" >> '{}'\n",
            sleeps.display()
        ),
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(GUARD))
        .env("PATH", path)
        .env("BOSS_JOBS_URL", "http://jobs.test:7900")
        .env("BOSS_API_CURL", &door)
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("the guard runs");
    let read_lines = |p: &Path| -> Vec<String> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    Run {
        rc: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        calls: read_lines(&calls),
        sleeps: read_lines(&sleeps),
    }
}

fn quiet_night() -> String {
    page(vec![
        converge(
            "cccccccc-0000-0000-0000-000000000003",
            "closed",
            60,
            run_step(55, false),
        ),
        converge(
            "bbbbbbbb-0000-0000-0000-000000000002",
            "closed",
            5 * 3600,
            run_step(5 * 3600 - 300, true),
        ),
    ])
}

/// THE CONVERGE READ PRESENTS THE MACHINE TOKEN, AND IS NEVER STOPPED BY
/// IT (design 6805c764; backlog 44b2087e). The read went out with
/// `x-boss-user` alone. The guard now asks the one shell reader for a
/// header file and hands it to its door — and, because it "never fails
/// the check", every state of the mount must leave the verdict, the exit
/// and the request exactly what they were:
///
///   absent     no slot (the playground crawl's pod, today)
///   present    a slot, the system of record on the host list
///   off-host   a slot, the system of record NOT on the host list
///   line-break a slot no header can carry
///
/// The door here records the header FILE it was handed (the guard's own
/// door logs argv, and the token must never be in argv).
#[test]
fn the_converge_read_presents_the_machine_token_and_is_never_stopped_by_it() {
    const TOKEN: &str = "synthetic-machine-fixture";
    let go = |tag: &str, slot: Option<&str>, hosts: &str| -> (Run, String, String, Vec<String>) {
        let root = scratch_dir(&format!("wait-out-a-converge-token-{tag}"));
        let mount = root.join("mount");
        create_dir(&mount);
        if let Some(value) = slot {
            write_file(&mount.join("current"), value);
        }
        let tmp = root.join("tmp");
        create_dir(&tmp);
        let headers = root.join("headers.txt");
        let argv = root.join("argv.txt");
        write_file(&root.join("answer.json"), &quiet_night());
        let door = root.join("header-door.sh");
        write_exec(
            &door,
            &format!(
                "#!/usr/bin/env bash\n\
                 echo \"$*\" >> '{argv}'\n\
                 prev=\n\
                 for a in \"$@\"; do\n\
                 if [ \"$prev\" = -H ]; then case \"$a\" in @*) {{ stat -c 'mode=%a' \"${{a#@}}\"; cat \"${{a#@}}\"; }} >> '{headers}' ;; esac; fi\n\
                 prev=\"$a\"\n\
                 done\n\
                 cat '{root}/answer.json'\n",
                argv = argv.display(),
                headers = headers.display(),
                root = root.display(),
            ),
        );
        let (mount_s, tmp_s, door_s, no_env) = (
            mount.display().to_string(),
            tmp.display().to_string(),
            door.display().to_string(),
            root.join("no-sor.env").display().to_string(),
        );
        let r = run_guard(
            &format!("token-{tag}"),
            &[Some(quiet_night())],
            &[
                ("BOSS_API_CURL", door_s.as_str()),
                ("BOSS_MACHINE_TOKEN_DIR", mount_s.as_str()),
                ("BOSS_MACHINE_TOKEN_HOSTS", hosts),
                ("BOSS_SOR_ENV", no_env.as_str()),
                ("TMPDIR", tmp_s.as_str()),
            ],
        );
        let left = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        (
            r,
            std::fs::read_to_string(&headers).unwrap_or_default(),
            std::fs::read_to_string(&argv).unwrap_or_default(),
            left,
        )
    };

    let (absent, absent_headers, absent_argv, _) = go("absent", None, "jobs.test");
    assert_eq!(absent.rc, 0, "{}\n{}", absent.stdout, absent.stderr);
    assert!(absent.stdout.contains("clear"), "{}", absent.stdout);
    assert_eq!(
        absent_headers, "",
        "no token, and a header file was handed over"
    );
    assert_eq!(absent_argv.lines().count(), 1, "one read: {absent_argv}");

    let fixture = format!("{TOKEN}\n");
    let broken = format!("{TOKEN}\nsecond-line\n");
    for (tag, slot, hosts, carries) in [
        ("present", fixture.as_str(), "jobs.test", true),
        ("off-host", fixture.as_str(), "elsewhere.test", false),
        ("line-break", broken.as_str(), "jobs.test", false),
    ] {
        let (r, headers, argv, left) = go(tag, Some(slot), hosts);
        assert_eq!(
            r.rc, 0,
            "{tag}: the guard exits 0 on every path\n{}\n{}",
            r.stdout, r.stderr
        );
        assert_eq!(
            r.stdout, absent.stdout,
            "{tag}: the verdict is the one a host with no token reads"
        );
        assert_eq!(
            argv.lines().count(),
            1,
            "{tag}: the read still goes out, once: {argv}"
        );
        assert!(
            !argv.contains(TOKEN) && !r.stdout.contains(TOKEN) && !r.stderr.contains(TOKEN),
            "{tag}: the token is in argv or on the guard's output:\n{argv}\n{}\n{}",
            r.stdout,
            r.stderr
        );
        if carries {
            assert_eq!(
                headers,
                format!("mode=600\nx-boss-machine-token: {TOKEN}\n"),
                "{tag}: the read carries the token, as a 0600 header file holding the slot's value"
            );
        } else {
            assert_eq!(
                headers, "",
                "{tag}: this request must go out without the token"
            );
            assert!(
                !r.stderr.is_empty(),
                "{tag}: a token that is held and not sent is said on stderr, never silent"
            );
        }
        assert!(
            left.is_empty(),
            "{tag}: the header directory outlived the guard: {left:?}"
        );
    }
}

#[test]
fn a_quiet_cluster_is_clear_at_once() {
    let r = run_guard("quiet", &[Some(quiet_night())], &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.calls.len(), 1, "one read, no wait: {:?}", r.calls);
    assert!(r.sleeps.is_empty(), "{:?}", r.sleeps);
    assert!(
        r.stdout.contains("clear"),
        "an unchanged tick a minute ago and a deploy five hours ago are not a roll:\n{}",
        r.stdout
    );
    let call = &r.calls[0];
    assert!(
        call.contains("http://jobs.test:7900/api/jobs?kind=maintenance-cluster-converge")
            && call.contains("full=true"),
        "it reads the converge packets, with their steps, off the system of record: {call}"
    );
    assert!(
        call.contains("x-boss-user: {\"id\":\"automation:"),
        "the read is SIGNED — an unidentified read answers total 0, a denied scope that would \
         read as no converge at all: {call}"
    );
}

/// The 2026-09-28 shape: the converge is open when the crawl is due.
#[test]
fn an_open_converge_is_waited_out_then_the_crawl_runs() {
    let rolling = page(vec![converge(
        "a10cbaa4-0000-0000-0000-000000000001",
        "open",
        4 * 60,
        run_open(),
    )]);
    // Closed, but it BUILT and rolled 3 min ago: still settling.
    let settling = page(vec![converge(
        "a10cbaa4-0000-0000-0000-000000000001",
        "closed",
        10 * 60,
        run_step(3 * 60, true),
    )]);
    let settled = page(vec![converge(
        "a10cbaa4-0000-0000-0000-000000000001",
        "closed",
        30 * 60,
        run_step(20 * 60, true),
    )]);
    let r = run_guard(
        "open",
        &[
            Some(rolling.clone()),
            Some(rolling),
            Some(settling),
            Some(settled),
        ],
        &[],
    );
    assert_eq!(r.rc, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.calls.len(), 4, "{:?}\n{}", r.calls, r.stdout);
    assert_eq!(
        r.sleeps,
        ["60", "60", "60"],
        "one poll per decline: {}",
        r.stdout
    );
    assert!(
        r.stdout.contains("converge a10cbaa4 is in progress"),
        "the decline names the packet it read:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("converge a10cbaa4 rolled 4f9a588"),
        "and the head the roll put on the instance:\n{}",
        r.stdout
    );
    assert_eq!(
        r.stdout.matches("is in progress").count(),
        1,
        "a decline is said once while it holds, not every poll:\n{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("clear after 180s"),
        "the wait it cost is on the record:\n{}",
        r.stdout
    );
}

/// A packet whose close never landed is not a roll in progress forever:
/// past the unit's own 90-minute bound it is not read as one.
#[test]
fn an_open_converge_older_than_the_units_timeout_does_not_hold_the_crawl() {
    let stuck = page(vec![converge(
        "dddddddd-0000-0000-0000-000000000004",
        "open",
        3 * 3600,
        run_open(),
    )]);
    let r = run_guard("stuck", &[Some(stuck)], &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.sleeps.is_empty(), "{}", r.stdout);
    assert!(r.stdout.contains("clear"), "{}", r.stdout);
}

/// The jobs API dark (a roll takes it away too) is a decline, not a pass
/// and not a failure: re-read, bounded.
#[test]
fn a_dark_system_of_record_is_a_decline_that_is_re_read() {
    let r = run_guard("dark", &[None, Some(quiet_night())], &[]);
    assert_eq!(r.rc, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.calls.len(), 2, "{}", r.stdout);
    assert!(
        r.stdout.contains("did not answer"),
        "an unread record is said as unread:\n{}",
        r.stdout
    );
    assert!(r.stdout.contains("clear after 60s"), "{}", r.stdout);
}

/// Past the bound the crawl runs anyway, and the line says it was NOT
/// clear — a crawl that never runs is silence, and the judge files a
/// roll-shaped red as one shared-cause item.
#[test]
fn past_the_bound_it_says_not_clear_and_lets_the_crawl_run() {
    let rolling = page(vec![converge(
        "eeeeeeee-0000-0000-0000-000000000005",
        "open",
        60,
        run_open(),
    )]);
    let r = run_guard("bound", &[Some(rolling)], &[("CONVERGE_WAIT_MAX_MIN", "3")]);
    assert_eq!(r.rc, 0, "never fails the check: {}\n{}", r.stdout, r.stderr);
    assert_eq!(r.sleeps, ["60", "60", "60"], "{}", r.stdout);
    assert_eq!(r.calls.len(), 4, "{}", r.stdout);
    assert!(
        r.stdout.contains("NOT CLEAR after 180s")
            && r.stdout.contains("converge eeeeeeee is in progress"),
        "{}",
        r.stdout
    );
}

/// No system of record named: nothing to read, said so, and the crawl
/// runs — the guard is a courtesy to the judge, never a gate on the check.
#[test]
fn no_system_of_record_named_is_said_and_not_waited_on() {
    let r = run_guard("unnamed", &[Some(quiet_night())], &[("BOSS_JOBS_URL", "")]);
    assert_eq!(r.rc, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.calls.is_empty(), "{:?}", r.calls);
    assert!(
        r.stdout.contains("BOSS_JOBS_URL is not set"),
        "{}",
        r.stdout
    );
}

/// The guard runs before the browser — in the pod's `record` container,
/// which holds the door, and the crawl's container waits for its word
/// (backlog 37742794, 2026-10-07: the guard reads the record, so it left
/// the container that runs bun and Playwright, which holds no token and
/// names no jobs door). The order across the two containers: the guard,
/// then the `go` file; and in the crawl, the clone, then the wait for
/// `go`, then the suite.
#[test]
fn the_crawl_waits_out_a_converge_before_the_browser_starts() {
    let yaml = std::fs::read_to_string(repo_root().join(CRAWL)).expect("the crawl manifest");
    let code: Vec<&str> = yaml
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect();
    let at = |needle: &str| {
        code.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{CRAWL}: no code line carries {needle:?}"))
    };
    let clone = at("git clone --depth 1");
    let waits = at("while [ ! -e /go/go ]");
    let crawl = at("bun run test:live");
    assert!(
        clone < waits && waits < crawl,
        "{CRAWL}: in the crawl container — clone, then wait for go, then crawl (lines {clone}, {waits}, {crawl})"
    );
    let record = at("- name: record");
    assert!(
        crawl < record,
        "{CRAWL}: the crawl container, then the record container"
    );
    let guard = at("\"$GUARD\" ||");
    let go = at(": > \"$GO_DIR/go\"");
    assert!(
        record < guard && guard < go,
        "{CRAWL}: in the record container — wait out a converge, THEN say go (lines {guard}, {go})"
    );
    assert!(
        code.contains(
            &"export GUARD=\"${BOSS_CRAWL_GUARD:-/usr/local/bin/wait-out-a-converge.sh}\""
        ),
        "{CRAWL}: the guard is the image's copy of {GUARD}"
    );
    assert!(
        code[record..].contains(&"- {name: BOSS_API_CURL, value: /usr/local/bin/boss-api-curl.sh}"),
        "{CRAWL}: the guard reads through the image's own door — the roll posture every chore shares"
    );
    let path: PathBuf = repo_root().join(GUARD);
    assert!(path.is_file(), "{GUARD} exists");
}
