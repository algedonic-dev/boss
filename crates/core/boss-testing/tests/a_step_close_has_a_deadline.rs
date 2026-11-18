//! Every jobs-API call `infra/boss-step.sh` makes has a deadline
//! (backlog 81bc8c90, also 05bb33bc).
//!
//! MEASURED by review run ffb8efa0 (car b16bb27c) and again by triage on
//! origin/main bf2034ae0: boss-step's three calls — the open-job GET, the
//! step metadata PATCH and the status PUT — went through
//! `infra/boss-api-curl.sh` with no `--max-time`. That helper's
//! BOSS_API_RETRY_DEADLINE bounds the RE-SENDS of a refused connect, not
//! one call that connects and is never answered, so against a listener
//! that accepts and never answers the call never returned (killed at
//! 90 s). Safe in ExecStopPost; any caller inside a unit's ExecStart
//! inherits the wait until TimeoutStartSec — 90 minutes on the converge —
//! and a oneshot then blocks the next run, including the one that would
//! ship the fix. The wrap had the same defect and is bounded by
//! BOSS_WRAP_MAX_TIME (re-review of 324597eb, 2026-09-27); this is its
//! twin, BOSS_STEP_MAX_TIME.
//!
//! WHAT THIS PINS, through the REAL boss-step.sh, the REAL curl helper
//! and the REAL curl, against a real socket that accepts and never
//! answers:
//!   * each of the three calls carries the deadline, and the call the
//!     socket never answers ends on it, NON-ZERO, saying it was not
//!     answered within that many seconds — a timeout is never a silent
//!     success and never a no-op exit 0;
//!   * the default is 30 s (the wrap's), and a deadline that is not a
//!     positive whole number is refused before anything is sent.
//!
//! NO ANSWER RACES THE DEADLINE (backlog dac8cf01). boss-step has one
//! deadline for all three calls, so the call the fixture ANSWERS carried
//! it too, and every budget these tests gave that answer lost to a slow
//! gate bay: a 2 s deadline to gate 1a55a341 (backlog 10184588), then a
//! 10 s budget over a 3 s stall to a 280 s io.full stall on gate
//! 3082de78. On that drive a wall-clock budget of any size is the wrong
//! shape, so whether a call is answered is the fixture's decision, not a
//! race: a stub `curl` first on PATH logs every call as sent, hands the
//! calls the fixture answers to the real curl WITHOUT their deadline
//! flags, and hands the call it never answers to the real curl exactly as
//! sent, where only that call's own `--max-time` can end it — any budget
//! is safe there, because nothing answers. A call the fixture never
//! answers that carries no `--max-time` fails at once, in words, rather
//! than hanging the test. The answered calls wait ANSWER_STALL, longer
//! than the 10 s budget gate 3082de78 lost to. That stub, its log and
//! the handoff to the real curl are `boss_testing::path_stub`'s, shared
//! with every_converge_tick_observes_the_site.rs (backlog 59f4b65d).

use boss_testing::path_stub::{self, PathStub};
use boss_testing::{repo_root, scratch_dir, without_proxy};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

const STEP: &str = "infra/boss-step.sh";
const KIND: &str = "maintenance-deadline-test";

/// One open packet with an active `run` step, no host — the GET's answer
/// when the test wants the WRITE to be the call that hangs.
fn open_packet() -> String {
    serde_json::json!({
        "data": [{
            "id": "job-1", "kind": KIND, "status": "open", "metadata": {},
            "steps": [{"id": "step-1", "spec_slug": "run", "title": "run",
                       "status": "active", "metadata": {}}]
        }],
        "total": 1
    })
    .to_string()
}

/// Read one request's head off the socket (enough to have taken it).
fn take_request(conn: &mut TcpStream) {
    let mut buf = [0u8; 4096];
    let mut head = Vec::new();
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
}

/// How late the fixture's ANSWERED calls arrive: longer than the 10 s
/// budget an answered GET exceeded on gate 3082de78, and six times the
/// deadline under test — so an answer still raced against that deadline
/// would lose here every time, not only on a stalled drive.
const ANSWER_STALL: Duration = Duration::from_secs(12);

/// BOSS_STEP_MAX_TIME every test here sets: the deadline under test,
/// which only the call the fixture never answers is held to.
const MAX_TIME: &str = "2";

/// A jobs API that answers the first `answered` requests with the open
/// packet, each after ANSWER_STALL, then accepts every connection after
/// and never answers — the shape of a wedged server behind a live socket.
fn sor_that_goes_silent_after(answered: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for (n, conn) in listener.incoming().flatten().enumerate() {
            let mut conn = conn;
            if n < answered {
                take_request(&mut conn);
                std::thread::sleep(ANSWER_STALL);
                let body = open_packet();
                let _ = write!(
                    conn,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            } else {
                held.push(conn);
            }
        }
    });
    url
}

struct Out {
    rc: i32,
    text: String,
    /// Every curl call boss-step made, one line each, argv as sent.
    calls: Vec<String>,
}

/// Run the real boss-step as a unit would, with no machine token on
/// hand (the token directory is an empty scratch dir) and no pod, and
/// the stub curl (`path_stub::CURL_UNBOUNDED_WHEN_ANSWERED`, module note)
/// told the fixture answers the first `answered` calls.
fn step(url: &str, answered: usize, args: &[&str], env: &[(&str, &str)]) -> Out {
    let home = scratch_dir("step-deadline");
    let stubs = PathStub::new(&home);
    stubs.stub("curl", path_stub::CURL_UNBOUNDED_WHEN_ANSWERED);
    let mut cmd = Command::new("bash");
    without_proxy(&mut cmd);
    stubs.wire(&mut cmd);
    cmd.arg(repo_root().join(STEP))
        .args([KIND, "run"])
        .args(args)
        .env("STUB_ANSWERED", answered.to_string())
        .env("BOSS_JOBS_URL", url)
        .env("HOME", &home)
        .env("BOSS_MACHINE_TOKEN_DIR", home.join("no-token"))
        .env("BOSS_MACHINE_TOKEN_HOSTS", "")
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env_remove("HOST_ID")
        .env_remove("BOSS_NODE_ID")
        .env_remove("SERVICE_RESULT")
        .env_remove("EXIT_STATUS")
        .env_remove("BOSS_RUN_SUMMARY_FILE")
        .env_remove("BOSS_STEP_DRY_RUN")
        .env_remove("BOSS_STEP_METADATA_ONLY")
        .env_remove("BOSS_STEP_OUTPUT_FILE")
        .env_remove("BOSS_STEP_MAX_TIME");
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Output goes to files, and a run nothing in the fixture can end is
    // KILLED at the watchdog: an unbounded boss-step is the defect, and a
    // test that hangs with it reports nothing.
    let log = home.join("out.log");
    let file = std::fs::File::create(&log).unwrap();
    cmd.stdout(file.try_clone().unwrap()).stderr(file);
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap_or_else(|e| panic!("run {STEP}: {e}"));
    let (rc, killed) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status.code().unwrap_or(-1), false);
        }
        if started.elapsed() > WATCHDOG {
            let _ = child.kill();
            let _ = child.wait();
            break (-1, true);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut text = std::fs::read_to_string(&log).unwrap_or_default();
    if killed {
        text.push_str(&format!(
            "\n[killed by the test's watchdog at {WATCHDOG:?}]"
        ));
    }
    Out {
        rc,
        text,
        calls: stubs.calls().lines().map(str::to_string).collect(),
    }
}

/// Never the verdict: a correct run ends on the deadline of the one call
/// nothing answers, and a call missing its deadline fails at the stub.
/// This only turns a hang nothing else can end into a failure, so it is
/// sized far past any stall, where it cannot race an answer.
const WATCHDOG: Duration = Duration::from_secs(600);

/// The call the fixture never answers is call `answered + 1`, the last
/// one boss-step made; every call carried the deadline; and the run ended
/// non-zero naming it.
fn assert_bounded(out: &Out, answered: usize, what: &str, hung: &str) {
    assert_ne!(out.rc, 0, "{what}: a timeout must not exit 0\n{}", out.text);
    let named = format!("not answered within {MAX_TIME}s");
    assert!(
        out.text.contains(&named),
        "{what}: the timeout must be named ({named})\n{}",
        out.text
    );
    assert_eq!(
        out.calls.len(),
        answered + 1,
        "{what}: the answered calls, then the one that hung, and nothing after: {:#?}\n{}",
        out.calls,
        out.text
    );
    for call in &out.calls {
        assert!(
            call.contains(&format!("--max-time {MAX_TIME}")),
            "{what}: every call carries the deadline: {call}"
        );
    }
    let last = out.calls.last().map_or("", String::as_str);
    assert!(
        last.contains(hung),
        "{what}: the call that hung is the {what} ({hung}): {last}"
    );
}

#[test]
fn the_open_job_read_has_a_deadline() {
    let url = sor_that_goes_silent_after(0);
    let out = step(&url, 0, &[], &[("BOSS_STEP_MAX_TIME", MAX_TIME)]);
    assert_bounded(&out, 0, "GET", "/api/jobs?kind=");
}

/// The read is answered; the metadata merge (a key=value pair) hangs.
#[test]
fn the_metadata_merge_has_a_deadline() {
    let url = sor_that_goes_silent_after(1);
    let out = step(
        &url,
        1,
        &["result=ok"],
        &[
            ("BOSS_STEP_MAX_TIME", MAX_TIME),
            ("KUBERNETES_SERVICE_HOST", "10.96.0.1"),
        ],
    );
    assert_bounded(&out, 1, "PATCH", "-X PATCH");
    assert!(out.text.contains("metadata merge"), "{}", out.text);
}

/// The read is answered and there are no keys (a pod names no host), so
/// the status PUT is the first write — and it hangs.
#[test]
fn the_status_put_has_a_deadline() {
    let url = sor_that_goes_silent_after(1);
    let out = step(
        &url,
        1,
        &[],
        &[
            ("BOSS_STEP_MAX_TIME", MAX_TIME),
            ("KUBERNETES_SERVICE_HOST", "10.96.0.1"),
        ],
    );
    assert_bounded(&out, 1, "PUT", "-X PUT");
    assert!(out.text.contains("PUT"), "{}", out.text);
}

/// The default is the wrap's 30 s, read off the script's own line.
#[test]
fn the_default_deadline_is_thirty_seconds() {
    let text = std::fs::read_to_string(repo_root().join(STEP)).unwrap();
    let default = text
        .lines()
        .find_map(|l| l.strip_prefix("MAX_TIME=\"${BOSS_STEP_MAX_TIME:-"))
        .and_then(|v| v.split('}').next()?.parse::<u32>().ok());
    assert_eq!(
        default,
        Some(30),
        "boss-step's deadline is 30 s unless a caller shortens it"
    );
}

/// A deadline that is not a positive whole number is refused before any
/// request: read as no bound it would be the defect, read as zero curl
/// would treat it as no bound too.
#[test]
fn a_deadline_that_is_not_a_number_is_refused() {
    for bad in ["0", "abc", "1.5", "-3"] {
        let out = step(
            &format!("http://127.0.0.1:{}", unused_port()),
            0,
            &[],
            &[("BOSS_STEP_MAX_TIME", bad)],
        );
        assert_eq!(out.rc, 2, "BOSS_STEP_MAX_TIME={bad}\n{}", out.text);
        assert!(out.text.contains("BOSS_STEP_MAX_TIME"), "{}", out.text);
        assert!(out.calls.is_empty(), "nothing is sent: {:#?}", out.calls);
    }
}

fn unused_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}
