//! A maintenance chore runs whatever the system of record answers, and
//! the run it could not record is replayed onto the next packet (backlog
//! 9fd7f51e, 2026-09-27).
//!
//! MEASURED by the review of the packet-open policy car, already true on
//! main: `infra/boss-maintenance-wrap.sh` reads `/api/jobs` before it
//! opens a packet, and that read asks policy (`list_jobs` ->
//! `scope_predicate`), which answers 503 during a policy outage. The wrap
//! let the chore run only on a curl TRANSPORT failure (6, 7, 28, ...); an
//! HTTP answer — curl exit 22 — aborted it, and four forge units open
//! their packet from a HARD `ExecStartPre=`, so systemd never started the
//! chore. One of them is `cluster-deploy-runner.service`, the converge:
//! a policy outage stopped the loop that would deploy the policy fix.
//! CLAUDE.md §Diagnosis, "an arm that needs the patient is not an arm" —
//! the 2026-09-05 fix for the same loop covered only a DARK API.
//!
//! WHAT THIS PINS, through the REAL wrap and the REAL curl helper against
//! a real HTTP listener (no stubbed curl: the answer is an HTTP status on
//! a socket, the shape the outage has):
//!   * a 503 on the read lets the chore run — the wrap exits 0, and the
//!     converge unit's own ExecStartPre line, read from the unit file,
//!     would start its ExecStart;
//!   * so does every other bad answer: a 500, a 403, a 200 whose body
//!     lost `.data`, a refused connect, and a 503 on the spawn POST;
//!   * each says so loudly, naming the status curl was handed, and keeps
//!     the miss in a ledger under `$HOME` — and the next packet the host
//!     opens for that kind carries it as `unrecorded_runs`, so the gap is
//!     on the record, not only in a journal (CLAUDE.md §Diagnosis, "an
//!     alarm that reports through its subject dies with it ... retain and
//!     replay");
//!   * every unit that opens a packet through the wrap does so from an
//!     `ExecStartPre=-` line, so not even a wrap defect can hold a chore.
//!
//! tree-wide pin — it walks every `.service` under infra/ and runs
//! infra/boss-maintenance-wrap.sh, files no changed-file map attributes
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::{dark_port, repo_root, scratch_dir};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const WRAP: &str = "infra/boss-maintenance-wrap.sh";
const CONVERGE_UNIT: &str = "infra/forge/cluster-deploy-runner.service";
const LEDGER_DIR: &str = ".boss-maintenance-unrecorded";

/// One request the listener saw: method, path, body.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    body: String,
}

type Answer = Box<dyn Fn(&str, &str) -> (u16, String) + Send + Sync>;

/// A jobs API on a real socket. `answer` maps a method and a request
/// body to a status and a reply body; every request is recorded.
struct Sor {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Sor {
    fn start(answer: Answer) -> Sor {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let answer = Arc::new(answer);
        std::thread::spawn(move || {
            // One thread per connection, so two clients really overlap;
            // each request is logged BEFORE it is answered, so one still
            // being answered (or never answered in time) is seen.
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let (log, answer) = (Arc::clone(&log), Arc::clone(&answer));
                std::thread::spawn(move || {
                    let req = read_request(&mut conn);
                    log.lock().unwrap().push(req.clone());
                    let (status, body) = answer(&req.method, &req.body);
                    let reason = match status {
                        200 => "OK",
                        400 => "Bad Request",
                        403 => "Forbidden",
                        500 => "Internal Server Error",
                        503 => "Service Unavailable",
                        _ => "Status",
                    };
                    let _ = write!(
                        conn,
                        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        Sor { url, seen }
    }

    /// The same answer to every request.
    fn always(status: u16, body: &str) -> Sor {
        let body = body.to_string();
        Sor::start(Box::new(move |_: &str, _: &str| (status, body.clone())))
    }

    /// A healthy API with no open packet: the read answers an empty
    /// list, a spawn answers the filed job.
    fn healthy() -> Sor {
        Sor::start(Box::new(|method: &str, _: &str| match method {
            "POST" => (200, r#"{"id":"job-1"}"#.to_string()),
            _ => (200, r#"{"data":[],"total":0}"#.to_string()),
        }))
    }

    /// An API that accepts every connection and never answers — the
    /// shape of a wedged server behind a live socket.
    fn hanging() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in listener.incoming().flatten() {
                held.push(conn);
            }
        });
        url
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn read_request(conn: &mut std::net::TcpStream) -> Seen {
    let mut req = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => break req.len(),
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
    };
    let head = String::from_utf8_lossy(&req[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while req.len() < head_end + length {
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
    }
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    Seen {
        method: first.next().unwrap_or("").to_string(),
        path: first.next().unwrap_or("").to_string(),
        body: String::from_utf8_lossy(&req[head_end..]).to_string(),
    }
}

struct Out {
    rc: i32,
    text: String,
}

/// Run the real wrap as a forge unit does: HOST_ID=forge, a HOME of its
/// own (the ledger's home), no pod.
fn wrap(url: &str, home: &Path, kind: &str) -> Out {
    wrap_with(Some(url), home, kind, &[])
}

/// `wrap`, with the SoR optional (None: BOSS_JOBS_URL unset) and extra
/// environment laid over the unit's.
fn wrap_with(url: Option<&str>, home: &Path, kind: &str, env: &[(&str, &str)]) -> Out {
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join(WRAP))
        .args([kind, "Cluster converge on forge main"])
        .env("HOME", home)
        .env("HOST_ID", "forge")
        // A refused connect is waited out for 150 s by default; the
        // test is about what happens past that window.
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env_remove("BOSS_JOBS_URL")
        .env_remove("BOSS_NODE_ID")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("BOSS_MACHINE_TOKEN");
    if let Some(url) = url {
        cmd.env("BOSS_JOBS_URL", url);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap_or_else(|e| panic!("run {WRAP}: {e}"));
    Out {
        rc: out.status.code().unwrap_or(-1),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn ledger(home: &Path, kind: &str) -> PathBuf {
    home.join(LEDGER_DIR).join(format!("{kind}@forge.jsonl"))
}

fn ledger_rows(home: &Path, kind: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(ledger(home, kind)).unwrap_or_default();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("ledger line {l:?}: {e}")))
        .collect()
}

/// The ExecStartPre line of a unit that runs the wrap, without the key.
fn wrap_pre_line(unit_text: &str) -> Option<&str> {
    unit_text
        .lines()
        .filter_map(|l| l.strip_prefix("ExecStartPre="))
        .find(|l| l.contains("boss-maintenance-wrap.sh"))
}

#[test]
fn a_503_from_the_sor_does_not_stop_the_converge() {
    let unit = std::fs::read_to_string(repo_root().join(CONVERGE_UNIT)).unwrap();
    let pre = wrap_pre_line(&unit)
        .unwrap_or_else(|| panic!("{CONVERGE_UNIT} no longer opens a packet through the wrap"));
    let hard = !pre.starts_with('-');
    let kind = pre
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("no kind on {pre:?}"))
        .to_string();

    let sor = Sor::always(503, r#"{"error":"policy service unavailable"}"#);
    let home = scratch_dir("sor-503-converge");
    let out = wrap(&sor.url, &home, &kind);

    // What systemd does with that exit: a hard ExecStartPre that fails
    // skips ExecStart; a `-` one does not. The chore here is a marker.
    let chore_ran = home.join("chore-ran");
    if !hard || out.rc == 0 {
        std::fs::write(&chore_ran, "converged").unwrap();
    }
    assert!(
        chore_ran.exists(),
        "a 503 from the system of record stopped the converge ({CONVERGE_UNIT}: \
         ExecStartPre={pre}, wrap exit {}):\n{}",
        out.rc,
        out.text
    );
    assert_eq!(
        out.rc, 0,
        "the wrap itself must never refuse a chore over its visibility:\n{}",
        out.text
    );
    assert!(
        out.text.contains("UNRECORDED") && out.text.contains("503"),
        "the lost visibility is said loudly, naming the status:\n{}",
        out.text
    );
    let seen = sor.seen();
    assert_eq!(
        seen.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
        ["GET"],
        "a read that answered badly is not followed by a spawn (it could be a duplicate): {seen:?}"
    );
    let rows = ledger_rows(&home, &kind);
    assert_eq!(rows.len(), 1, "the miss is kept for replay: {rows:?}");
    assert!(
        rows[0].to_string().contains("503"),
        "the kept miss names the status: {rows:?}"
    );
    assert!(
        rows[0]["id"].as_str().is_some_and(|id| !id.is_empty()),
        "the kept miss carries an id, so a reader can dedupe it: {rows:?}"
    );
    assert!(
        rows[0]["at"].as_str().is_some_and(|t| t.ends_with('Z')),
        "the kept miss is dated in UTC: {rows:?}"
    );
}

#[test]
fn every_bad_answer_lets_the_chore_run_and_is_kept() {
    let kind = "maintenance-disk-floor-sweep";
    // A refused connect: a port HELD dark for the whole test (backlog
    // ec131700). It was bound, read and released, and a released port is
    // the next bind's to take.
    let dark = dark_port();
    let cases: Vec<(&str, String, Option<Sor>)> = vec![
        ("a 500", String::new(), Some(Sor::always(500, "boom"))),
        (
            "a 403",
            String::new(),
            Some(Sor::always(403, r#"{"error":"denied"}"#)),
        ),
        (
            "a 200 with no .data",
            String::new(),
            Some(Sor::always(200, r#"{"rows":[]}"#)),
        ),
        (
            "a 503 on the spawn",
            String::new(),
            Some(Sor::start(Box::new(|method: &str, _: &str| match method {
                "POST" => (503, r#"{"error":"policy service unavailable"}"#.to_string()),
                _ => (200, r#"{"data":[]}"#.to_string()),
            }))),
        ),
        ("a refused connect", dark.base.clone(), None),
    ];
    for (what, dark_url, sor) in cases {
        let url = sor.as_ref().map_or(dark_url, |s| s.url.clone());
        let home = scratch_dir("sor-bad-answer");
        let out = wrap(&url, &home, kind);
        assert_eq!(
            out.rc, 0,
            "{what} from the system of record stopped the chore:\n{}",
            out.text
        );
        assert!(
            out.text.contains("UNRECORDED"),
            "{what}: the lost visibility is said loudly:\n{}",
            out.text
        );
        assert_eq!(
            ledger_rows(&home, kind).len(),
            1,
            "{what}: the miss is kept for replay:\n{}",
            out.text
        );
    }
}

#[test]
fn a_run_the_sor_did_not_record_rides_the_next_packet() {
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-replay");

    let outage = Sor::always(503, r#"{"error":"policy service unavailable"}"#);
    assert_eq!(wrap(&outage.url, &home, kind).rc, 0);
    assert_eq!(wrap(&outage.url, &home, kind).rc, 0);
    assert_eq!(ledger_rows(&home, kind).len(), 2, "two misses kept");

    let back = Sor::healthy();
    let out = wrap(&back.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let posts: Vec<Seen> = back
        .seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .collect();
    assert_eq!(posts.len(), 1, "one packet spawned: {posts:?}");
    assert_eq!(posts[0].path, "/api/jobs");
    let body: Value = serde_json::from_str(&posts[0].body)
        .unwrap_or_else(|e| panic!("spawn body {:?}: {e}", posts[0].body));
    let carried = body["metadata"]["unrecorded_runs"]
        .as_array()
        .unwrap_or_else(|| panic!("the spawn carries no unrecorded_runs: {body}"));
    assert_eq!(carried.len(), 2, "both misses ride the packet: {body}");
    assert!(
        carried.iter().all(|r| r.to_string().contains("503")),
        "each carried miss says why: {body}"
    );
    let ids: std::collections::BTreeSet<&str> =
        carried.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids.len(), 2, "each carried miss has its own id: {body}");
    assert!(
        !ledger(&home, kind).exists(),
        "a miss carried onto a packet leaves the ledger, so it rides once:\n{}",
        out.text
    );

    let again = Sor::healthy();
    assert_eq!(wrap(&again.url, &home, kind).rc, 0);
    let post = again
        .seen()
        .into_iter()
        .find(|s| s.method == "POST")
        .expect("a second spawn");
    let body: Value = serde_json::from_str(&post.body).unwrap();
    assert!(
        body["metadata"].get("unrecorded_runs").is_none(),
        "a run after a clean one carries nothing: {body}"
    );
}

/// How many kept misses one packet carries, read from the wrap itself
/// (`UNRECORDED_CAP=<n>`), so this test and the script cannot disagree.
fn carried_cap() -> usize {
    let text = std::fs::read_to_string(repo_root().join(WRAP)).unwrap();
    text.lines()
        .find_map(|l| l.strip_prefix("UNRECORDED_CAP="))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or_else(|| panic!("{WRAP} declares no UNRECORDED_CAP=<n>"))
}

/// The adversarial review of ce69cc05 (2026-09-27): the ledger rode as
/// ONE argv string, twice (`jq --argjson`, `curl -d`). Past Linux's
/// MAX_ARG_STRLEN (128 KiB) jq died with 126 inside a command
/// substitution `set -e` does not see, curl POSTed an EMPTY body, the
/// API answered 400, that miss was kept too — and the ledger never
/// emptied again, even with a healthy SoR. ~30 misses of the old size
/// reached it: five hours of converge ticks.
#[test]
fn an_oversized_ledger_still_opens_a_packet_and_empties() {
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-oversized-ledger");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let filler = "x".repeat(1000);
    let mut text = String::new();
    for i in 0..300 {
        let row = serde_json::json!({
            "id": format!("row-{i}"),
            "at": format!("2026-09-27T{:02}:{:02}:00Z", i / 60, i % 60),
            "why": format!("the jobs API answered 503 {filler}"),
        });
        text.push_str(&row.to_string());
        text.push('\n');
    }
    // A write cut short, among the newest rows: it rides as raw text.
    text.push_str("{\"id\":\"row-cut\",\"at\":\"2026-09-27T05:00\n");
    std::fs::write(&path, &text).unwrap();
    assert!(
        text.len() > 128 * 1024,
        "the fixture must pass MAX_ARG_STRLEN"
    );

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let posts: Vec<Seen> = sor
        .seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .collect();
    assert_eq!(
        posts.len(),
        1,
        "one packet spawned: {posts:?}\n{}",
        out.text
    );
    let body: Value = serde_json::from_str(&posts[0].body).unwrap_or_else(|e| {
        panic!(
            "the spawn body is not the packet ({e}) — {} bytes:\n{}",
            posts[0].body.len(),
            out.text
        )
    });
    let cap = carried_cap();
    let carried = body["metadata"]["unrecorded_runs"]
        .as_array()
        .unwrap_or_else(|| panic!("no unrecorded_runs: {}", out.text));
    assert_eq!(carried.len(), cap, "the packet carries the newest {cap}");
    assert!(
        carried
            .iter()
            .any(|r| r["raw"].as_str().is_some_and(|s| s.contains("row-cut"))),
        "a line that is not JSON still rides, as its raw text"
    );
    assert_eq!(
        carried[cap - 2]["id"],
        "row-299",
        "the newest rows are the ones carried"
    );
    let dropped = &body["metadata"]["unrecorded_runs_dropped"];
    assert_eq!(
        dropped["count"].as_u64(),
        Some((301 - cap) as u64),
        "what the cap left off is counted: {dropped}"
    );
    assert_eq!(dropped["first_at"], "2026-09-27T00:00:00Z", "{dropped}");
    assert!(
        !path.exists(),
        "a ledger carried onto a packet is emptied, whatever its size:\n{}",
        out.text
    );
}

#[test]
fn a_kept_row_is_small_whatever_the_retries_printed() {
    let kind = "maintenance-disk-floor-sweep";
    let home = scratch_dir("sor-row-small");
    // HELD dark, not bound and released (backlog ec131700): on gate-run
    // 8b71013e (2026-10-08) another test's server was handed the released
    // port and answered this read with a 422, so no wait was printed and
    // the assertion below failed on a car that touches neither file.
    // Reproduced by binding a listener that answers 422 on the released
    // port: the gate's own line, every time.
    let dark = dark_port();
    // More than one attempt inside a three-second window: the roll's wait
    // lines and one curl line per attempt reach the journal, not the row.
    let out = wrap_with(
        Some(&dark.base),
        &home,
        kind,
        &[("BOSS_API_RETRY_DEADLINE", "3")],
    );
    assert_eq!(out.rc, 0, "{}", out.text);
    assert!(
        out.text.contains("retrying"),
        "the waits still reach the journal:\n{}",
        out.text
    );
    let rows = ledger_rows(&home, kind);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    // The bound the wrap actually sets: its own phrase and curl's final
    // line are each cut to 500 characters; beside them sit only an id,
    // a timestamp and a count. (The first cut of this test asserted "under
    // 800 bytes", which held for this fixture and was no bound at all.)
    for field in ["why", "curl"] {
        let len = row[field].as_str().map_or(0, |s| s.chars().count());
        assert!(
            len <= 500,
            "{field} is {len} characters, past the 500 cut: {row}"
        );
    }
    assert_eq!(
        row.as_object().map(|o| {
            let mut k: Vec<&str> = o.keys().map(String::as_str).collect();
            k.sort_unstable();
            k
        }),
        Some(vec!["at", "attempts", "curl", "id", "why"]),
        "a kept row is curl's final word plus the attempt count, not the whole log: {row}"
    );
    assert!(
        row["attempts"].as_u64().is_some_and(|n| n >= 2),
        "the row counts the attempts: {row}"
    );
    assert!(
        !row.to_string().contains("retrying"),
        "no wait lines in the row: {row}"
    );
}

/// Every unit's wrap line carries `-`, so the 78 a unit with no SoR
/// named used to fail the unit with is now swallowed by systemd. The
/// wrap still exits 78 — the journal records it — and keeps the miss.
#[test]
fn a_unit_with_no_sor_named_keeps_the_miss() {
    let kind = "maintenance-reap-ci-jobs";
    let home = scratch_dir("sor-unnamed");
    let out = wrap_with(None, &home, kind, &[]);
    assert_eq!(out.rc, 78, "{}", out.text);
    assert!(out.text.contains("UNRECORDED"), "{}", out.text);
    let rows = ledger_rows(&home, kind);
    assert_eq!(rows.len(), 1, "the miss is kept:\n{}", out.text);
    assert!(rows[0].to_string().contains("BOSS_JOBS_URL"), "{rows:?}");
}

/// A CronJob pod's filesystem ends with the run, so a ledger written
/// there would be a promise nothing keeps.
#[test]
fn a_pod_keeps_no_ledger_and_says_so() {
    let kind = "maintenance-views-catchup";
    let home = scratch_dir("sor-pod");
    let sor = Sor::always(503, r#"{"error":"policy service unavailable"}"#);
    let out = wrap_with(
        Some(&sor.url),
        &home,
        kind,
        &[("KUBERNETES_SERVICE_HOST", "10.96.0.1")],
    );
    assert_eq!(out.rc, 0, "{}", out.text);
    assert!(
        !home.join(LEDGER_DIR).exists(),
        "a pod wrote a ledger it cannot keep:\n{}",
        out.text
    );
    assert!(
        out.text.contains("NOT kept") && out.text.contains("pod"),
        "the line says the miss could not be kept, and why:\n{}",
        out.text
    );
}

/// Plant ledger lines as a run of the wrap would have left them.
fn plant(home: &Path, kind: &str, lines: &[String]) {
    let path = ledger(home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&path, text).unwrap();
}

/// Everything in the ledger directory beside the ledger itself.
/// Everything in the ledger directory beside the ledger itself, less the
/// lock file: a lock file is never deleted (a second run could then lock
/// a different inode than the first holds).
fn ledger_dir_names(home: &Path) -> Vec<String> {
    std::fs::read_dir(home.join(LEDGER_DIR))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.ends_with(".lock"))
                .collect()
        })
        .unwrap_or_default()
}

fn posts(sor: &Sor) -> Vec<Value> {
    sor.seen()
        .into_iter()
        .filter(|s| s.method == "POST")
        .map(|s| {
            serde_json::from_str(&s.body).unwrap_or_else(|e| panic!("POST body {:?}: {e}", s.body))
        })
        .collect()
}

fn carried_ids(body: &Value) -> Vec<String> {
    body["metadata"]["unrecorded_runs"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A `jq` ahead of the real one on PATH that fails the way a full TMPDIR
/// fails it — exit 2, ENOSPC — when its stdout is the packet file, and
/// is the real jq everywhere else (the ledger lives in HOME, a different
/// filesystem from TMPDIR on the forge).
fn full_tmpdir_path(dir: &Path) -> String {
    let real = Command::new("bash")
        .args(["-c", "command -v jq"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    assert!(!real.is_empty(), "jq is not on PATH");
    let shim = dir.join("full-tmpdir-bin");
    std::fs::create_dir_all(&shim).unwrap();
    boss_testing::write_exec(
        &shim.join("jq"),
        &format!(
            "#!/usr/bin/env bash\n\
             case \"$(readlink /proc/$$/fd/1 2>/dev/null)\" in\n\
             \x20 */packet.json) echo 'jq: error: No space left on device' >&2; exit 2 ;;\n\
             esac\n\
             exec {real} \"$@\"\n"
        ),
    );
    format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// The re-review of 324597eb (2026-09-27): ANY failure to build the
/// packet set the ledger aside as unreadable — and a full TMPDIR is a
/// recurring forge incident, the very hour disk-floor-sweep runs through
/// this wrap. The set-aside file was named only on that run's packet,
/// lost when it was not sent, and never named again. A build that fails
/// must lose no row, however many times it fails.
#[test]
fn a_full_tmpdir_loses_no_kept_row() {
    let kind = "maintenance-disk-floor-sweep";
    let home = scratch_dir("sor-full-tmpdir");
    let planted: Vec<String> = (1..=3)
        .map(|i| {
            serde_json::json!({"id": format!("r{i}"), "at": format!("2026-09-27T00:00:0{i}Z"),
                               "why": "the jobs API answered 503"})
            .to_string()
        })
        .collect();
    plant(&home, kind, &planted);
    let path = full_tmpdir_path(&home);

    for (run, expect) in [(1, 4), (2, 5)] {
        let sor = Sor::healthy();
        let out = wrap_with(Some(&sor.url), &home, kind, &[("PATH", &path)]);
        assert_eq!(out.rc, 0, "run {run}: {}", out.text);
        assert!(
            posts(&sor).is_empty(),
            "run {run}: nothing is sent when the packet cannot be built"
        );
        let rows = ledger_rows(&home, kind);
        assert_eq!(
            rows.len(),
            expect,
            "run {run}: every kept row stays, plus this run's miss:\n{}",
            out.text
        );
        for id in ["r1", "r2", "r3"] {
            assert!(
                rows.iter().any(|r| r["id"] == id),
                "run {run}: {id} lost:\n{}",
                out.text
            );
        }
        let names = ledger_dir_names(&home);
        assert!(
            names
                .iter()
                .all(|n| !n.contains(".unreadable-") && !n.contains(".claim-")),
            "run {run}: a readable ledger was set aside or a claim left behind: {names:?}\n{}",
            out.text
        );
    }

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let bodies = posts(&sor);
    assert_eq!(bodies.len(), 1, "{}", out.text);
    let ids = carried_ids(&bodies[0]);
    assert_eq!(
        ids.len(),
        5,
        "all five rows ride once the disk has room: {ids:?}"
    );
    for id in ["r1", "r2", "r3"] {
        assert!(ids.iter().any(|i| i == id), "{id} did not ride: {ids:?}");
    }
    assert!(
        ledger_dir_names(&home).is_empty(),
        "a landed packet leaves no ledger and no claim: {:?}",
        ledger_dir_names(&home)
    );
}

/// Fourth review of fe1309fe, finding 1: after a 4xx on the POST — even
/// one the retry without rows also got — the rows were turned into file
/// names that never reached the record, and enough names overflowed
/// jq's argv and wedged every packet for good. Now a refused packet goes
/// once more WITHOUT the rows, the rows stay in the ledger unchanged, and
/// nothing is ever named by path; a row the API never accepts is bounded
/// by the on-disk cap alone. Four runs against a SoR refusing every POST,
/// then a healthy one: every row reaches the record. A cut-short line
/// rides raw, capped at 500 characters without NULs, and so does a JSON
/// row holding \u0000.
#[test]
fn a_post_only_4xx_wedges_nothing_and_every_row_lands_later() {
    let kind = "maintenance-reap-ci-jobs";
    let home = scratch_dir("sor-post-4xx");
    let good =
        serde_json::json!({"id": "good-1", "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string();
    let cut = format!("cut\u{0}{}", "y".repeat(5000));
    let nul_row = r#"{"id":"nul-1","at":"2026-09-27T00:00:01Z","why":"a\u0000b"}"#.to_string();
    plant(&home, kind, &[good, cut, nul_row]);

    for run in 1..=4 {
        let sor = Sor::start(Box::new(|method: &str, _: &str| match method {
            "POST" => (422, r#"{"error":"unprocessable"}"#.to_string()),
            _ => (200, r#"{"data":[]}"#.to_string()),
        }));
        let out = wrap(&sor.url, &home, kind);
        assert_eq!(out.rc, 0, "run {run}: {}", out.text);
        let bodies = posts(&sor);
        assert_eq!(
            bodies.len(),
            2,
            "run {run}: sent with the rows, then once without them:\n{}",
            out.text
        );
        assert!(
            bodies[0]["metadata"]["unrecorded_runs"].is_array(),
            "run {run}"
        );
        assert!(
            bodies[1]["metadata"].get("unrecorded_runs").is_none(),
            "run {run}: {}",
            bodies[1]
        );
        assert!(
            bodies[1]["metadata"]["unrecorded_runs_held_back"]["rows"]
                .as_u64()
                .is_some_and(|n| n >= 3),
            "run {run}: the second send says what it held back: {}",
            bodies[1]
        );
        let text = std::fs::read_to_string(ledger(&home, kind)).unwrap_or_default();
        assert!(
            text.contains("good-1") && !text.contains("possibly_recorded_at"),
            "run {run}: the rows stay, unmarked — a 4xx is a definite answer:\n{}",
            out.text
        );
        assert!(
            ledger_dir_names(&home)
                .iter()
                .all(|n| n.ends_with(".jsonl")),
            "run {run}: nothing beside the ledger: {:?}",
            ledger_dir_names(&home)
        );
    }

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let bodies = posts(&sor);
    assert_eq!(bodies.len(), 1, "{}", out.text);
    let rows = bodies[0]["metadata"]["unrecorded_runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let ids = carried_ids(&bodies[0]);
    assert!(ids.iter().any(|i| i == "good-1"), "{ids:?}");
    assert_eq!(
        ids.len(),
        5,
        "good-1 and the four runs' own misses: {ids:?}"
    );
    let raws: Vec<&str> = rows.iter().filter_map(|r| r["raw"].as_str()).collect();
    assert_eq!(
        raws.len(),
        2,
        "the cut-short line and the NUL row ride raw: {rows:?}"
    );
    for raw in &raws {
        assert!(
            raw.chars().count() <= 500 && !raw.contains('\0'),
            "a raw row is capped at 500 characters with no NUL: {} chars",
            raw.chars().count()
        );
    }
    assert!(
        ledger_dir_names(&home).is_empty(),
        "{:?}",
        ledger_dir_names(&home)
    );
}

/// Finding 2: only 28, 52 and 56 marked rows as possibly recorded, but
/// any exit but the two where nothing was sent (6, 7) and a definite
/// HTTP answer can follow a committed POST — and a SIGTERM (systemd's
/// stop, which reaches every process in the unit) left through the EXIT
/// trap with the rows unmarked. The whole process group is signalled
/// mid-POST, as systemd does.
#[test]
fn a_sigterm_mid_post_marks_the_rows() {
    use std::os::unix::process::CommandExt;
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-sigterm");
    let row = |id: &str| {
        serde_json::json!({"id": id, "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string()
    };
    plant(&home, kind, &[row("a"), row("b")]);
    let sor = Sor::start(Box::new(|method: &str, _: &str| match method {
        "POST" => {
            std::thread::sleep(std::time::Duration::from_secs(30));
            (200, r#"{"id":"job-1"}"#.to_string())
        }
        _ => (200, r#"{"data":[]}"#.to_string()),
    }));
    let mut child = Command::new("bash")
        .arg(repo_root().join(WRAP))
        .args([kind, "x"])
        .env("BOSS_JOBS_URL", &sor.url)
        .env("HOME", &home)
        .env("HOST_ID", "forge")
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env("BOSS_WRAP_MAX_TIME", "20")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("BOSS_NODE_ID")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while posts_seen(&sor) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the POST never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let group = format!("-{}", child.id());
    // bash's builtin kill: the gate's image has no /bin/kill (the first
    // gate of this test died on NotFound).
    let killed = Command::new("bash")
        .args(["-c", "kill -TERM -- \"$1\"", "kill", &group])
        .status()
        .unwrap();
    assert!(killed.success(), "kill -TERM {group}");
    child.wait().unwrap();

    let rows = ledger_rows(&home, kind);
    for id in ["a", "b"] {
        let r = rows
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} lost: {rows:?}"));
        assert!(
            r["possibly_recorded_at"].as_str().is_some(),
            "a row whose POST was cut off may be on the record, and says so: {r}"
        );
    }
    assert!(
        ledger_dir_names(&home)
            .iter()
            .all(|n| n.ends_with(".jsonl")),
        "{:?}",
        ledger_dir_names(&home)
    );
}

fn posts_seen(sor: &Sor) -> usize {
    sor.seen().iter().filter(|s| s.method == "POST").count()
}

/// Own the lock process and release it when this scope ends, including panic.
struct HeldLedgerLock(std::process::Child);

impl Drop for HeldLedgerLock {
    fn drop(&mut self) {
        // EOF releases the owned reader even during assertion unwinding.
        // No sleeping descendant survives to hold the lock after this test.
        self.0.stdin.take();
        let _ = self.0.wait();
    }
}

/// Finding 3: a stray row was written in place under its final `.stray-`
/// name, so a sweep could read it half-written. It is written as
/// `.writing-*`, which no sweep reads, and only then renamed.
#[test]
fn a_stray_is_written_aside_and_only_then_named_for_the_sweep() {
    let kind = "maintenance-disk-floor-sweep";
    let home = scratch_dir("sor-writing-stray");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A writer caught mid-write: the sweep must leave it alone.
    let half = format!("{}.writing-Half01", path.display());
    std::fs::write(&half, "{\"id\":\"half").unwrap();

    // Another run holds the lock, so this run's miss becomes a stray.
    let ready = home.join("lock-held");
    // f08bcb9f: a fixed eight-second sleep released this lock while the
    // wrap was still doing HTTP work. Hold it until the actual wrap exits.
    let mut holder = HeldLedgerLock(
        Command::new("flock")
            .arg(format!("{}.lock", path.display()))
            .args(["bash", "-c", "touch \"$1\"; read -r release", "lock-holder"])
            .arg(&ready)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the lock holder never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // The race is inside the wrap, between creating the file and filling
    // it, so the protocol is pinned where it is decided: a mktemp that
    // logs the template it was asked for.
    let shim = home.join("mktemp-log-bin");
    std::fs::create_dir_all(&shim).unwrap();
    let real = Command::new("bash")
        .args(["-c", "command -v mktemp"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    let log = home.join("mktemp.log");
    boss_testing::write_exec(
        &shim.join("mktemp"),
        &format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> '{}'\nexec {real} \"$@\"\n",
            log.display()
        ),
    );
    let path_env = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let down = Sor::start(Box::new(|_, _| {
        // The original eight-second holder expires during this bounded
        // request. The fixture's ownership must survive until wrap exit.
        std::thread::sleep(std::time::Duration::from_secs(9));
        (503, "{}".to_string())
    }));
    let out = wrap_with(
        Some(&down.url),
        &home,
        kind,
        &[("BOSS_WRAP_LOCK_WAIT", "1"), ("PATH", &path_env)],
    );
    assert!(
        holder.0.try_wait().unwrap().is_none(),
        "the fixture lock owner must still be alive when the wrap exits"
    );
    drop(holder);
    assert_eq!(out.rc, 0, "{}", out.text);
    let asked = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        asked.contains(".writing-XXXXXX") && !asked.contains(".stray-XXXXXX"),
        "a stray is created under a .writing- name and only renamed to .stray- once \
         written, never created where a sweep could read it half-written:\n{asked}"
    );
    let names = ledger_dir_names(&home);
    let strays: Vec<&String> = names.iter().filter(|n| n.contains(".stray-")).collect();
    assert_eq!(strays.len(), 1, "{names:?}\n{}", out.text);
    assert_eq!(
        names.iter().filter(|n| n.contains(".writing-")).count(),
        1,
        "only the planted half-written file is left mid-write: {names:?}"
    );
    let stray_text = std::fs::read_to_string(path.parent().unwrap().join(strays[0])).unwrap();
    let stray: Value = serde_json::from_str(stray_text.trim()).unwrap();
    let stray_id = stray["id"].as_str().unwrap().to_string();

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    assert_eq!(carried_ids(&posts(&sor)[0]), vec![stray_id], "{}", out.text);
    assert!(
        std::path::Path::new(&half).exists(),
        "a file still being written is never swept"
    );
}

/// Finding 4: with no flock the wrap cannot know whether a `.claim-*`
/// belongs to a run still in flight, so unlocked it sweeps none.
#[test]
fn unlocked_there_is_no_claim_sweep() {
    let kind = "maintenance-estate-observe-units";
    let home = scratch_dir("sor-unlocked");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = |id: &str| {
        serde_json::json!({"id": id, "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string() + "\n"
    };
    std::fs::write(&path, row("in-ledger")).unwrap();
    let foreign = format!("{}.claim-Other1", path.display());
    std::fs::write(&foreign, row("foreign")).unwrap();

    // A PATH holding every tool the wrap uses, except flock.
    let bin = home.join("no-flock-bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in [
        "bash", "env", "cat", "cut", "date", "dirname", "basename", "find", "getent", "grep", "id",
        "jq", "mkdir", "mktemp", "mv", "readlink", "rm", "sleep", "tail", "touch", "uname", "wc",
        "curl", "tr", "sed", "head",
    ] {
        let found = Command::new("bash")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap();
        let at = String::from_utf8_lossy(&found.stdout).trim().to_string();
        if !at.is_empty() {
            std::os::unix::fs::symlink(&at, bin.join(tool)).unwrap();
        }
    }
    let sor = Sor::healthy();
    let out = wrap_with(
        Some(&sor.url),
        &home,
        kind,
        &[("PATH", &bin.display().to_string())],
    );
    assert_eq!(out.rc, 0, "{}", out.text);
    assert!(out.text.contains("unlocked"), "{}", out.text);
    assert_eq!(
        carried_ids(&posts(&sor)[0]),
        vec!["in-ledger"],
        "{}",
        out.text
    );
    assert!(
        std::path::Path::new(&foreign).exists(),
        "a claim that may be another run's is left alone"
    );
}

/// A claim a killed run left behind, and a stray row written while the
/// lock was busy, are swept in before this run claims, and carried.
#[test]
fn a_stray_claim_is_swept_and_carried() {
    let kind = "maintenance-disk-floor-sweep";
    let home = scratch_dir("sor-stray-claim");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = |id: &str| {
        serde_json::json!({"id": id, "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string() + "\n"
    };
    std::fs::write(format!("{}.claim-Zq81Lx", path.display()), row("stranded")).unwrap();
    std::fs::write(format!("{}.stray-P0aa1B", path.display()), row("stray")).unwrap();
    std::fs::write(&path, row("fresh")).unwrap();

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let mut ids = carried_ids(&posts(&sor)[0]);
    ids.sort();
    assert_eq!(ids, ["fresh", "stranded", "stray"], "{}", out.text);
    assert!(
        ledger_dir_names(&home).is_empty(),
        "{:?}",
        ledger_dir_names(&home)
    );
}

/// Fifth review of 5bb5f48d, H1: a SIGKILL, an OOM kill or a crash
/// mid-POST runs no trap, so the claim it leaves carries no
/// possibly_recorded_at — and the next locked sweep sent it as if new. A
/// claim left on disk exists only after an unclean finish, so its rows
/// are stamped when swept; a stray and the ledger never were in a POST.
#[test]
fn a_leftover_claim_is_carried_marked_possibly_recorded() {
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-leftover-claim");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = |id: &str| {
        serde_json::json!({"id": id, "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string() + "\n"
    };
    std::fs::write(format!("{}.claim-Kill9x", path.display()), row("killed")).unwrap();
    std::fs::write(format!("{}.stray-Busy01", path.display()), row("stray")).unwrap();
    std::fs::write(&path, row("fresh")).unwrap();

    let sor = Sor::healthy();
    let out = wrap(&sor.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    let body = &posts(&sor)[0];
    let rows = body["metadata"]["unrecorded_runs"].as_array().unwrap();
    let marked = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id} not carried: {body}"))["possibly_recorded_at"]
            .is_string()
    };
    assert!(
        marked("killed"),
        "a leftover claim's row may be on the record: {body}"
    );
    assert!(!marked("stray"), "a stray was never in a POST: {body}");
    assert!(!marked("fresh"), "the ledger was never in a POST: {body}");
    assert_eq!(
        body["metadata"]["unrecorded_runs_possibly_repeated"], 1,
        "{body}"
    );
}

/// H2: `cat "$f" >> "$CLAIM" && rm -f "$f"` — a copy cut short (ENOSPC)
/// left rows in the claim AND in the source; the claim was sent, then
/// deleted, and the rows rode again. The claim is cut back to its size
/// before that source, and gathering stops. Emulated with a `cat` that
/// writes part of the ledger into the claim and then fails.
#[test]
fn a_copy_cut_short_sends_nothing_twice() {
    let kind = "maintenance-disk-floor-sweep";
    let home = scratch_dir("sor-cut-copy");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = |id: String| {
        serde_json::json!({"id": id, "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string() + "\n"
    };
    std::fs::write(
        format!("{}.stray-Busy01", path.display()),
        row("stray".into()),
    )
    .unwrap();
    let ledger_rows: String = (0..10).map(|i| row(format!("r{i}"))).collect();
    std::fs::write(&path, ledger_rows).unwrap();

    let real = Command::new("bash")
        .args(["-c", "command -v cat"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    let shim = home.join("short-cat-bin");
    std::fs::create_dir_all(&shim).unwrap();
    boss_testing::write_exec(
        &shim.join("cat"),
        &format!(
            "#!/usr/bin/env bash\n\
             case \"$(readlink /proc/$$/fd/1 2>/dev/null)|${{1:-}}\" in\n\
             \x20 *.claim-*'|'*.jsonl) head -c 150 \"$1\"; echo 'cat: write error: No space left on device' >&2; exit 1 ;;\n\
             esac\n\
             exec {real} \"$@\"\n"
        ),
    );
    let path_env = format!(
        "{}:{}",
        shim.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let first = Sor::healthy();
    let out = wrap_with(Some(&first.url), &home, kind, &[("PATH", &path_env)]);
    assert_eq!(out.rc, 0, "{}", out.text);
    let second = Sor::healthy();
    let out2 = wrap(&second.url, &home, kind);
    assert_eq!(out2.rc, 0, "{}", out2.text);

    let mut ids: Vec<String> = posts(&first)
        .iter()
        .chain(posts(&second).iter())
        .flat_map(carried_ids)
        .collect();
    ids.sort();
    let mut want: Vec<String> = (0..10).map(|i| format!("r{i}")).collect();
    want.push("stray".into());
    want.sort();
    assert_eq!(
        ids, want,
        "every row sent exactly once across both runs:\n{}\n---\n{}",
        out.text, out2.text
    );
    assert!(
        out.text.contains("its rows wait for a later run"),
        "{}",
        out.text
    );
}

/// The old claim was `<ledger>.claim-$$`, renamed over with `mv -f`: a run
/// whose pid matched a stranded claim's destroyed it. Emulated exactly: a
/// shell writes a claim named with its own pid, then execs the wrap, which
/// keeps that pid.
#[test]
fn pid_reuse_loses_nothing() {
    let kind = "maintenance-estate-observe-host";
    let home = scratch_dir("sor-pid-reuse");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({"id": "in-ledger", "at": "2026-09-27T00:00:01Z", "why": "x"})
            .to_string()
            + "\n",
    )
    .unwrap();
    let stranded =
        serde_json::json!({"id": "stranded", "at": "2026-09-27T00:00:00Z", "why": "x"}).to_string();
    let sor = Sor::healthy();
    let script = format!(
        "printf '%s\\n' '{stranded}' > '{}'.claim-$$; exec bash '{}' {kind} 'x'",
        path.display(),
        repo_root().join(WRAP).display()
    );
    let out = Command::new("bash")
        .args(["-c", &script])
        .env("BOSS_JOBS_URL", &sor.url)
        .env("HOME", &home)
        .env("HOST_ID", "forge")
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env_remove("KUBERNETES_SERVICE_HOST")
        .env_remove("BOSS_NODE_ID")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{text}");
    let mut ids = carried_ids(&posts(&sor)[0]);
    ids.sort();
    assert_eq!(ids, ["in-ledger", "stranded"], "{text}");
}

/// curl 28, 52 or 56 on the POST means the SoR may have committed the
/// packet. Nothing on the SoR side dedupes (POST /api/jobs has no
/// idempotency key), so the rows come back marked `possibly_recorded_at`
/// and the next packet says they may be a repeat; within one build rows
/// are deduped by id.
#[test]
fn an_ambiguous_exit_marks_the_rows_and_the_build_dedupes() {
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-ambiguous");
    let row = |id: &str, at: &str| {
        serde_json::json!({"id": id, "at": at, "why": "the jobs API answered 503"}).to_string()
    };
    plant(
        &home,
        kind,
        &[
            row("a", "2026-09-27T00:00:00Z"),
            row("b", "2026-09-27T00:00:01Z"),
            // The same row twice, as a partial fold-back can leave it.
            row("b", "2026-09-27T00:00:01Z"),
        ],
    );
    // Commits the packet, then answers after the wrap's deadline.
    let committed = Sor::start(Box::new(|method: &str, _: &str| match method {
        "POST" => {
            std::thread::sleep(std::time::Duration::from_secs(4));
            (200, r#"{"id":"job-1"}"#.to_string())
        }
        _ => (200, r#"{"data":[]}"#.to_string()),
    }));
    let out = wrap_with(
        Some(&committed.url),
        &home,
        kind,
        &[("BOSS_WRAP_MAX_TIME", "2")],
    );
    assert_eq!(out.rc, 0, "{}", out.text);
    let first = posts(&committed);
    assert_eq!(first.len(), 1, "{}", out.text);
    let mut sent = carried_ids(&first[0]);
    sent.sort();
    assert_eq!(sent, ["a", "b"], "the build dedupes by id: {}", first[0]);

    let back = Sor::healthy();
    let out2 = wrap(&back.url, &home, kind);
    assert_eq!(out2.rc, 0, "{}", out2.text);
    let body = &posts(&back)[0];
    let rows = body["metadata"]["unrecorded_runs"].as_array().unwrap();
    let ids = carried_ids(body);
    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "no id twice: {body}");
    for id in ["a", "b"] {
        let r = rows
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {body}"));
        assert!(
            r["possibly_recorded_at"].as_str().is_some(),
            "a row that may already be on the record says so: {r}"
        );
    }
    assert_eq!(
        body["metadata"]["unrecorded_runs_possibly_repeated"], 2,
        "the packet counts the rows that may be repeats: {body}"
    );
    let _ = &out.text;
}

/// A hand run beside a timer run: the lock beside the ledger is held
/// across claim, build, POST and remove, so neither sweeps the other's
/// live claim, and nothing is lost or sent twice.
#[test]
fn two_wraps_at_once_lose_and_duplicate_nothing() {
    let kind = "maintenance-audit-integrity";
    let home = scratch_dir("sor-two-at-once");
    let planted: Vec<String> = (0..20)
        .map(|i| {
            serde_json::json!({"id": format!("r{i:02}"), "at": format!("2026-09-27T00:00:{i:02}Z"),
                               "why": "x"})
            .to_string()
        })
        .collect();
    plant(&home, kind, &planted);
    // A slow POST, so the second run arrives while the first holds the lock.
    let sor = Sor::start(Box::new(|method: &str, _: &str| match method {
        "POST" => {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            (200, r#"{"id":"job-1"}"#.to_string())
        }
        _ => (200, r#"{"data":[]}"#.to_string()),
    }));
    let spawn = |url: String, home: PathBuf| {
        std::thread::spawn(move || wrap(&url, &home, "maintenance-audit-integrity"))
    };
    let a = spawn(sor.url.clone(), home.clone());
    let b = spawn(sor.url.clone(), home.clone());
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    assert_eq!(a.rc, 0, "{}", a.text);
    assert_eq!(b.rc, 0, "{}", b.text);
    let mut ids: Vec<String> = posts(&sor).iter().flat_map(carried_ids).collect();
    ids.sort();
    let want: Vec<String> = (0..20).map(|i| format!("r{i:02}")).collect();
    assert_eq!(
        ids, want,
        "each row sent exactly once:\n{}\n---\n{}",
        a.text, b.text
    );
    assert!(
        ledger_dir_names(&home).is_empty(),
        "{:?}",
        ledger_dir_names(&home)
    );
}

/// The ledger on disk is capped too: past LEDGER_MAX rows the oldest go,
/// and the next packet counts them.
#[test]
fn the_ledger_on_disk_is_capped_and_the_drop_is_counted() {
    let kind = "maintenance-files-gc";
    let home = scratch_dir("sor-disk-cap");
    let text = std::fs::read_to_string(repo_root().join(WRAP)).unwrap();
    let max: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("LEDGER_MAX="))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or_else(|| panic!("{WRAP} declares no LEDGER_MAX=<n>"));
    let planted: Vec<String> = (0..max + 4)
        .map(|i| {
            serde_json::json!({"id": format!("r{i}"), "at": format!("2026-09-{:02}T00:00:00Z", 1 + i % 26),
                               "why": "x"})
            .to_string()
        })
        .collect();
    plant(&home, kind, &planted);
    let down = Sor::always(503, "{}");
    assert_eq!(wrap(&down.url, &home, kind).rc, 0);
    assert_eq!(
        ledger_rows(&home, kind).len(),
        max,
        "the ledger holds at most {max} rows"
    );
    let back = Sor::healthy();
    let out = wrap(&back.url, &home, kind);
    let body = &posts(&back)[0];
    assert_eq!(
        body["metadata"]["unrecorded_runs_dropped_on_disk"], 5,
        "the rows the disk cap dropped are counted: {}\n{}",
        body["metadata"], out.text
    );
    assert!(
        ledger_dir_names(&home).is_empty(),
        "{:?}",
        ledger_dir_names(&home)
    );
}

/// A SoR that accepts the connection and never answers held ExecStartPre
/// until the converge's 90-minute TimeoutStartSec killed the unit — the
/// converge never ran. Every request now has a deadline.
#[test]
fn a_hanging_sor_is_abandoned_and_the_chore_runs() {
    let kind = "maintenance-cluster-converge";
    let home = scratch_dir("sor-hanging");
    let url = Sor::hanging();
    let started = std::time::Instant::now();
    let out = wrap_with(Some(&url), &home, kind, &[("BOSS_WRAP_MAX_TIME", "2")]);
    let took = started.elapsed();
    assert_eq!(out.rc, 0, "{}", out.text);
    assert!(
        took < std::time::Duration::from_secs(20),
        "the wrap waited {took:?} on a SoR that never answers"
    );
    assert!(out.text.contains("UNRECORDED"), "{}", out.text);
    assert_eq!(ledger_rows(&home, kind).len(), 1, "{}", out.text);
    let text = std::fs::read_to_string(repo_root().join(WRAP)).unwrap();
    let default = text
        .lines()
        .find_map(|l| l.strip_prefix("MAX_TIME=\"${BOSS_WRAP_MAX_TIME:-"))
        .and_then(|v| v.split('}').next()?.parse::<u32>().ok());
    assert_eq!(
        default,
        Some(30),
        "the wrap's own deadline is 30 s unless a test shortens it"
    );
}

/// A ledger jq cannot read (here: a directory where the file should be)
/// is set aside, and every packet counts the set-aside files until one
/// counting them lands. A count, never a list of names: a list is what
/// wedged every packet in the fourth review of fe1309fe.
#[test]
fn an_unreadable_ledger_is_counted_until_a_packet_lands() {
    let kind = "maintenance-estate-observe-host";
    let home = scratch_dir("sor-unreadable");
    let path = ledger(&home, kind);
    std::fs::create_dir_all(path.join("not-a-file")).unwrap();
    let counted = |body: &Value| body["metadata"]["unrecorded_ledgers_unreadable"].as_u64();

    let refusing = Sor::start(Box::new(|method: &str, _: &str| match method {
        "POST" => (503, r#"{"error":"policy service unavailable"}"#.to_string()),
        _ => (200, r#"{"data":[]}"#.to_string()),
    }));
    let out = wrap(&refusing.url, &home, kind);
    assert_eq!(out.rc, 0, "{}", out.text);
    assert_eq!(counted(&posts(&refusing)[0]), Some(1), "{}", out.text);
    assert!(
        ledger_dir_names(&home)
            .iter()
            .any(|n| n.contains(".unreadable-")),
        "the set-aside file is kept as evidence: {:?}",
        ledger_dir_names(&home)
    );

    let back = Sor::healthy();
    assert_eq!(wrap(&back.url, &home, kind).rc, 0);
    assert_eq!(
        counted(&posts(&back)[0]),
        Some(1),
        "a packet that was not sent did not report it, so the next one counts it again"
    );

    let after = Sor::healthy();
    assert_eq!(wrap(&after.url, &home, kind).rc, 0);
    assert_eq!(
        counted(&posts(&after)[0]),
        None,
        "once a packet counting it lands, it is not counted again"
    );
}

fn units(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            units(&path, found);
        } else if path.extension().is_some_and(|e| e == "service") {
            found.push(path);
        }
    }
}

#[test]
fn every_unit_that_opens_a_packet_starts_its_chore_whatever_the_wrap_does() {
    let root = repo_root();
    let mut found = Vec::new();
    units(&root.join("infra"), &mut found);
    let mut openers = 0;
    let hard: Vec<String> = found
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            let pre = wrap_pre_line(&text)?.to_string();
            openers += 1;
            (!pre.starts_with('-')).then(|| {
                format!(
                    "{}: ExecStartPre={pre}",
                    p.strip_prefix(&root).unwrap_or(p).display()
                )
            })
        })
        .collect();
    assert!(
        openers >= 10,
        "found only {openers} units opening a packet through the wrap — the walk is broken"
    );
    assert!(
        hard.is_empty(),
        "these units open their packet from a HARD ExecStartPre, so a wrap that fails \
         for any reason skips the chore. The packet is visibility, never a precondition — \
         prefix the line with `-`:\n  {}",
        hard.join("\n  ")
    );
}
