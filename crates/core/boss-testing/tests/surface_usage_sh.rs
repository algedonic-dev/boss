//! `infra/surface-usage.sh` — which surfaces each operator opened is
//! ROLLED UP daily and FILED, with its coverage stated (backlog
//! 628f182b; David, 2026-09-16: "let's measure which surfaces I open
//! for a week").
//!
//! The stub below is the jobs API as the script sees it: it serves the
//! roll-up (`/api/surface-opens/rollup`), the sweep, the open packet
//! (`/api/jobs?kind=…`), and records the PATCH — so the test reads the
//! filed row, not the script's account of it. The roll-up fixture names
//! two actors and three routes, one of them a catalogued path; the
//! never-opened list must then be the catalog minus that one path, read
//! off THIS tree's nav catalog.

use boss_testing::announce::{await_announced_port, with_announce};
use boss_testing::{repo_root, without_proxy};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const JOB_ID: &str = "0123456789abcdef";

/// "I could not answer." `infra/lint/lib/git-answer.sh` carries the
/// argument; here it means the roll-up or the catalog was not read.
const CANNOT_ANSWER: i32 = 3;

fn traced_curl_script() -> &'static str {
    // No argv, URL, headers or environment dump: even a query can carry a secret.
    r#"#!/bin/bash
printf 'curl_start utc=%s pid=%s port=%s\n' "$EPOCHREALTIME" "$$" "$FIXTURE_CURL_PORT" >> "$FIXTURE_CURL_TRACE"
"$FIXTURE_REAL_CURL" "$@"
status=$?
printf 'curl_exit utc=%s pid=%s port=%s status=%s\n' "$EPOCHREALTIME" "$$" "$FIXTURE_CURL_PORT" "$status" >> "$FIXTURE_CURL_TRACE"
exit "$status"
"#
}

fn admission_trace(began: std::time::Instant, port: u16, stage: &str, status: Option<u16>) {
    eprintln!(
        "{}",
        serde_json::json!({
            "fixture": "lost-ack-native-admission", "port": port, "pid": std::process::id(),
            "thread": format!("{:?}", std::thread::current().id()), "stage": stage,
            "elapsed_ns": began.elapsed().as_nanos(), "utc": format!("{:?}", std::time::SystemTime::now()),
            "status": status,
        })
    );
}

#[test]
fn curl_diagnostics_preserve_raw_bytes_exit_and_hide_arguments() {
    let directory = boss_testing::scratch_dir("lost-ack-curl-trace-control");
    let real = directory.join("real-curl");
    boss_testing::write_exec(
        &real,
        r#"#!/bin/bash
[[ $# == 3 && "$1" == 'http://user:password@fixture/private?token=do-not-record' && "$2" == '-H' && "$3" == 'Authorization: secret' ]] || exit 91
read -r input
[[ "$input" == 'unchanged stdin' && "$FIXTURE_PASSTHROUGH" == 'unchanged environment' ]] || exit 92
printf '\000\377out'
printf '\000\376err' >&2
exit 23
"#,
    );
    let wrapper = directory.join("curl");
    boss_testing::write_exec(&wrapper, traced_curl_script());
    let trace = directory.join("curl.trace");
    let mut child = Command::new(&wrapper)
        .args([
            "http://user:password@fixture/private?token=do-not-record",
            "-H",
            "Authorization: secret",
        ])
        .env("FIXTURE_REAL_CURL", real)
        .env("FIXTURE_CURL_TRACE", &trace)
        .env("FIXTURE_CURL_PORT", "12345")
        .env("FIXTURE_PASSTHROUGH", "unchanged environment")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(&mut child, b"unchanged stdin\n");
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"\x00\xffout");
    assert_eq!(output.stderr, b"\x00\xfeerr");
    let events = std::fs::read_to_string(trace).unwrap_or_default();
    assert!(
        events.contains("curl_start"),
        "missing curl start: {events}"
    );
    assert!(events.contains("curl_exit"), "missing curl exit: {events}");
    assert!(
        events.contains("status=23"),
        "missing actual exit: {events}"
    );
    for forbidden in ["password", "token", "Authorization", "secret", "private"] {
        assert!(!events.contains(forbidden), "argument leaked: {events}");
    }
}

/// What a loaded gate bay may add to a call the stub ANSWERS. It rides
/// on the open-packet read of every `packet-*` mode, the call the wrap
/// bounds with BOSS_WRAP_MAX_TIME; longer than the 1 s that bound once
/// was (backlog 39f5bf22).
const ANSWER_STALL: Duration = Duration::from_millis(1500);

/// What one answered stub call may take on a loaded gate bay, and so the
/// BOSS_WRAP_MAX_TIME the packet lifecycle runs under. No stub mode
/// leaves a call unanswered (a lost ack closes the socket), so the bound
/// only keeps a wedged run finite and costs no test its wait.
const FIXTURE_BUDGET: Duration = Duration::from_secs(10);
const _: () = assert!(ANSWER_STALL.as_millis() < FIXTURE_BUDGET.as_millis());

const STUB: &str = r#"
import http.server, json, os, sys, socket, time
log, started, mode = sys.argv[1], sys.argv[2], sys.argv[3]
answer_stall = float(os.environ["ANSWER_STALL_SECS"])
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.bind(("127.0.0.1", 0))
port = sock.getsockname()[1]
OPEN = {"data": [{"id": "0123456789abcdef", "kind": "maintenance-surface-usage", "status": "open", "created_at": "2026-09-17T05:30:35Z", "metadata": {}}], "total": 1}
ROLLUP = {"since": "2026-09-16T05:30:00Z", "until": "2026-09-17T05:30:00Z", "rows": [
  {"actor_id": "emp-david", "route": "/it", "opens": 7, "last_at": "2026-09-17T01:00:00Z"},
  {"actor_id": "emp-david", "route": "/ux/jobs/:jobId", "opens": 3, "last_at": "2026-09-17T00:30:00Z"},
  {"actor_id": "emp-032", "route": "/ux/me", "opens": 2, "last_at": "2026-09-16T09:00:00Z"}
]}
if mode.startswith("packet-") and mode != "packet-rerun-existing":
    OPEN = {"data": [], "total": 0}
if mode == "packet-rerun-existing":
    OPEN["data"][0]["metadata"]["unit_invocation_id"] = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
class H(http.server.BaseHTTPRequestHandler):
    packet_reads = 0
    packet_posts = 0
    def log_message(self, *a): pass
    def _reply(self, code, body=b"{}"):
        self.send_response(code); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def do_GET(self):
        with open(log, "ab") as f:
            f.write(b"GET " + self.path.encode() + b"\n")
        if self.path.startswith("/api/surface-opens/rollup"):
            if mode == "rollup-down":
                self._reply(503, b'{"error":"upstream unavailable"}')
            elif "since=" not in self.path or "until=" not in self.path:
                self._reply(400, b'{"error":"since and until are required"}')
            else:
                self._reply(200, json.dumps(ROLLUP).encode())
        elif self.path.startswith("/api/jobs/"):
            if mode == "packet-empty-after-ack":
                self._reply(200, b"")
            elif mode == "packet-unread-after-ack":
                self._reply(503)
            else:
                found = next((j for j in OPEN["data"] if j["id"] == self.path.rsplit("/", 1)[-1]), None)
                if found and mode == "packet-mismatch-after-ack":
                    found = dict(found, title="a different admission body")
                if found and mode == "packet-foreign-id-after-ack":
                    found = dict(found, id="ffffffff-ffff-4fff-afff-ffffffffffff")
                if found and mode == "packet-foreign-subject-after-ack":
                    found = dict(found, subject={"subject_kind":"custom", "id":"infra/foreign"})
                if found and mode == "packet-foreign-kind-after-ack":
                    found = dict(found, kind="maintenance-foreign")
                if found and mode == "packet-foreign-metadata-after-ack":
                    found = dict(found, metadata=dict(found["metadata"], chore="foreign"))
                if found and mode == "packet-missing-owner-after-ack":
                    found = {k:v for k,v in found.items() if k != "owner_id"}
                if found and mode == "packet-missing-status-after-ack":
                    found = {k:v for k,v in found.items() if k != "status"}
                self._reply(200 if found else 404, json.dumps(found or {}).encode())
        elif self.path.startswith("/api/jobs"):
            type(self).packet_reads += 1
            if mode.startswith("packet-"):
                time.sleep(answer_stall)
            if mode == "packet-permanent" or (mode == "packet-recover" and type(self).packet_reads == 1) or (mode == "packet-unread-after-ack" and type(self).packet_posts):
                self._reply(503)
            else:
                self._reply(200, json.dumps(OPEN).encode())
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_POST(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        with open(log, "ab") as f:
            f.write(b"POST " + self.path.encode() + b"\n")
        if self.path == "/api/jobs":
            type(self).packet_posts += 1
            sent = json.loads(body)
            with open(log, "ab") as f:
                f.write(b"ADMIT " + json.dumps(sent, separators=(",", ":")).encode() + b"\n")
            if mode == "packet-post-permanent" or (mode == "packet-post-recover" and type(self).packet_posts == 1):
                self._reply(503)
                return
            key = sent.get("id", "0123456789abcdef")
            old = next((j for j in OPEN["data"] if j["id"] == key), None)
            if old:
                same = all(old.get(k) == v for k, v in sent.items())
                self._reply(200 if same else 409, json.dumps({"id": key, "already_admitted": same}).encode())
                return
            stored = dict(sent, id=key)
            # Admission resolves a non-roster owner, and the protocol may
            # move status before a lost ACK is read back. Echoing the
            # request here would conceal the real admission contract.
            if mode in ("packet-normalized-owner-after-ack", "packet-normalized-both-after-ack"):
                stored["owner_id"] = "emp-resolved-admin"
            if mode in ("packet-normalized-status-after-ack", "packet-normalized-both-after-ack"):
                stored["status"] = "closed"
            OPEN["data"].append(stored)
            OPEN["total"] = len(OPEN["data"])
            if mode in ("packet-lost-ack", "packet-unread-after-ack", "packet-mismatch-after-ack", "packet-empty-after-ack", "packet-normalized-owner-after-ack", "packet-normalized-status-after-ack", "packet-normalized-both-after-ack", "packet-foreign-id-after-ack", "packet-foreign-subject-after-ack", "packet-foreign-kind-after-ack", "packet-foreign-metadata-after-ack", "packet-missing-owner-after-ack", "packet-missing-status-after-ack"):
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                return
            self._reply(201, json.dumps({"id": key}).encode())
        elif self.path == "/api/surface-opens/sweep":
            if mode == "sweep-down":
                self._reply(500, b'{"error":"storage: boom"}')
            else:
                self._reply(200, b'{"deleted": 4, "before": "2026-08-18T05:30:00Z", "retention_days": 30}')
        else:
            self._reply(404, b'{"error":"no such route"}')
    def do_PATCH(self):
        n = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(n)
        try:
            line = json.dumps(json.loads(body), separators=(",", ":")).encode()
        except Exception as e:
            line = b"NOT JSON: " + repr(e).encode()
        with open(log, "ab") as f:
            f.write(b"PATCH " + self.path.encode() + b"\n" + line + b"\n")
        for packet in OPEN["data"]:
            if self.path == "/api/jobs/" + packet["id"] + "/metadata":
                packet.setdefault("metadata", {}).update(json.loads(body))
        self._reply(204, b"")
srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H, bind_and_activate=False)
srv.socket.close(); srv.socket = sock; srv.server_address = sock.getsockname(); srv.server_activate()
# Announced once listening, and through boss_testing's announce (prepended),
# which renames the file into place: the test reads it the moment it
# exists (backlog 0d1e557e).
announce(started, str(port))
srv.serve_forever()
"#;

fn script() -> PathBuf {
    repo_root().join("infra/surface-usage.sh")
}

/// Every `path:` the tree's nav catalog declares, once each — the
/// roster the script's never-opened list is judged against, read here
/// the same way the script reads it so the two cannot drift.
fn catalog_paths() -> Vec<String> {
    let src = std::fs::read_to_string(repo_root().join("apps/web/src/shell/nav-catalog.ts"))
        .expect("apps/web/src/shell/nav-catalog.ts");
    let mut out: Vec<String> = Vec::new();
    for line in src.lines() {
        if let Some(rest) = line.split("path: '").nth(1)
            && let Some(p) = rest.split('\'').next()
            && !out.iter().any(|x| x == p)
        {
            out.push(p.to_string());
        }
    }
    out
}

struct Stub {
    child: Child,
    port: u16,
    log: PathBuf,
}
impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Stub {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn patches(&self) -> Vec<(String, serde_json::Value)> {
        let log = self.log();
        let mut lines = log.lines();
        let mut out = Vec::new();
        while let Some(l) = lines.next() {
            if let Some(path) = l.strip_prefix("PATCH ") {
                let body = lines.next().expect("a PATCH line is followed by its body");
                out.push((
                    path.to_string(),
                    serde_json::from_str(body).expect("the PATCH body is JSON"),
                ));
            }
        }
        out
    }
}

fn start_stub(case: &str, mode: &str) -> Stub {
    let dir = boss_testing::scratch_dir(&format!("surface-usage-stub-{case}"));
    let script = dir.join("stub.py");
    let started = dir.join("started");
    let log = dir.join("calls.log");
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_file(&started);
    boss_testing::write_exec(&script, &with_announce(STUB));
    let mut child = without_proxy(&mut Command::new("python3"))
        .arg(&script)
        .arg(&log)
        .arg(&started)
        .arg(mode)
        .env("ANSWER_STALL_SECS", ANSWER_STALL.as_secs_f64().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3");
    let port = await_announced_port(&mut child, &started, Duration::from_secs(20));
    Stub { child, port, log }
}

fn run(stub: &Stub, cmd: &str, repo: &std::path::Path) -> (Option<i32>, String, String) {
    run_under(stub, cmd, repo, &[])
}

/// What a GitHub-hosted runner hands every step: the step runs inside
/// the runner's systemd service, so the test process inherits that
/// service's INVOCATION_ID — 32 lowercase hex, the shape the script
/// accepts as its own collector's identity (backlog 70e5bbc7: publish
/// PR 259's Gate panicked at the key assertion below while every forge
/// gate, a Kubernetes pod with no systemd above it, passed the tree).
const RUNNER_INVOCATION_ID: &str = "5f1d2c3b4a5968778695a4b3c2d1e0f9";

/// `run`, with `inherited` set on the command FIRST, so the fixture's
/// own settings apply over it exactly as they apply over whatever the
/// test process inherited from the host it runs on.
fn run_under(
    stub: &Stub,
    cmd: &str,
    repo: &std::path::Path,
    inherited: &[(&str, &str)],
) -> (Option<i32>, String, String) {
    let mut command = Command::new("bash");
    command.envs(inherited.iter().copied());
    let out = without_proxy(&mut command)
        .arg(script())
        .arg(cmd)
        .arg("--repo")
        .arg(repo)
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{}", stub.port))
        .env("BOSS_API_RETRY_DEADLINE", "5")
        // The script is right to bind its PATCH to INVOCATION_ID: under
        // its unit that IS the collector's systemd invocation. A plain
        // run is not under one, so the fixture says so rather than
        // inheriting whatever the host's service manager exported —
        // `packet_lifecycle` below pins the same name for the wrap.
        .env_remove("INVOCATION_ID")
        .output()
        .expect("bash");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn has_python3() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

/// The plain `file` run below asserts the PATCH carries exactly the
/// chore contract's two keys. A host that runs the suite under systemd
/// must not change that answer: the reading-invocation binding is
/// proven where the test sets INVOCATION_ID itself
/// (`an_existing_packet_keeps_its_creation_identity_…`), never by what
/// the host happened to export (backlog 70e5bbc7).
#[test]
fn a_host_systemd_invocation_does_not_reach_the_plain_file_run() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("file-under-systemd", "serve");
    let (rc, out, err) = run_under(
        &stub,
        "file",
        &repo_root(),
        &[("INVOCATION_ID", RUNNER_INVOCATION_ID)],
    );
    assert_eq!(rc, Some(0), "file did not exit 0:\n{out}\n{err}");
    let patches = stub.patches();
    assert_eq!(patches.len(), 1, "{}", stub.log());
    let keys: Vec<&str> = patches[0]
        .1
        .as_object()
        .expect("the body is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["coverage", "measured"],
        "the host's INVOCATION_ID rode into the fixture: {}",
        patches[0].1
    );
}

/// `file` reads the roll-up over a 24-hour window, sweeps retention,
/// and PATCHes ONE row — `measured` + `coverage`, the chore contract's
/// two required keys — onto the open packet: opens per actor per
/// route, the top routes, and the catalogued surfaces nobody opened.
#[test]
fn file_patches_measured_and_coverage_onto_the_open_packet() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("file", "serve");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(0), "file did not exit 0:\n{out}\n{err}");

    let patches = stub.patches();
    assert_eq!(
        patches.len(),
        1,
        "exactly one PATCH is the record; got {} in:\n{}",
        patches.len(),
        stub.log()
    );
    let (path, body) = &patches[0];
    assert_eq!(path, &format!("/api/jobs/{JOB_ID}/metadata"));
    let keys: Vec<&str> = body
        .as_object()
        .expect("the body is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["coverage", "measured"],
        "the chore contract's two keys (14c135f5), merged server-side: {body}"
    );

    let m = &body["measured"];
    assert_eq!(m["opens"].as_i64(), Some(12), "7 + 3 + 2: {m}");
    assert_eq!(m["actors"].as_i64(), Some(2), "{m}");
    assert_eq!(m["distinct_routes"].as_i64(), Some(3), "{m}");
    assert!(m["at"].as_str().is_some_and(|s| s.ends_with('Z')), "{m}");
    assert!(m["method"].is_object(), "the method rides on the row: {m}");

    // Per actor, busiest first; routes most-opened first.
    let per = m["per_actor"].as_array().expect("per_actor is an array");
    assert_eq!(per.len(), 2, "{m}");
    assert_eq!(per[0]["actor_id"], "emp-david");
    assert_eq!(per[0]["opens"].as_i64(), Some(10));
    assert_eq!(per[0]["distinct_routes"].as_i64(), Some(2));
    assert_eq!(per[0]["routes"][0]["route"], "/it");
    assert_eq!(per[0]["routes"][0]["opens"].as_i64(), Some(7));
    assert_eq!(per[0]["routes"][1]["route"], "/ux/jobs/:jobId");
    assert_eq!(per[1]["actor_id"], "emp-032");

    let top = m["top_routes"].as_array().expect("top_routes is an array");
    assert_eq!(top[0]["route"], "/it");
    assert_eq!(top[0]["opens"].as_i64(), Some(7));

    // The deletion candidates: this tree's catalog minus whatever the
    // fixture opened that the catalog lists — `/it` today; `/ux/me` is
    // not a catalog entry (My Day is the personal fallback, not a
    // sidebar row) and `/ux/jobs/:jobId` is a pattern, not a path. The
    // filter names both anyway so the test holds if either joins.
    let catalog = catalog_paths();
    assert!(
        catalog.len() > 20,
        "the catalog read {} paths",
        catalog.len()
    );
    let expected: Vec<&str> = catalog
        .iter()
        .map(String::as_str)
        .filter(|p| *p != "/it" && *p != "/ux/me")
        .collect();
    let never: Vec<&str> = m["never_opened"]
        .as_array()
        .expect("never_opened is an array")
        .iter()
        .map(|v| v.as_str().expect("a path"))
        .collect();
    assert_eq!(
        never, expected,
        "never_opened is the catalog minus what was opened: {m}"
    );
    assert_eq!(
        m["never_opened_count"].as_u64(),
        Some(expected.len() as u64),
        "{m}"
    );
    assert_eq!(
        m["catalog_routes"].as_u64(),
        Some(catalog.len() as u64),
        "{m}"
    );

    // The sweep's facts, not that one happened.
    assert_eq!(m["retention"]["swept"]["deleted"].as_i64(), Some(4), "{m}");
    assert_eq!(
        m["retention"]["swept"]["retention_days"].as_i64(),
        Some(30),
        "{m}"
    );
    assert!(m["retention"]["error"].is_null(), "{m}");

    // COVERAGE: what was examined, and that it was all of it.
    let c = &body["coverage"];
    assert_eq!(c["window"]["hours"].as_i64(), Some(24), "{c}");
    assert!(
        c["window"]["since"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "{c}"
    );
    assert!(
        c["window"]["until"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "{c}"
    );
    assert_eq!(
        c["rollup"]["source"],
        format!("http://127.0.0.1:{}/api/surface-opens/rollup", stub.port),
        "the row names the surface it read: {c}"
    );
    assert_eq!(c["rollup"]["rows"].as_i64(), Some(3), "{c}");
    assert_eq!(
        c["catalog"]["file"], "apps/web/src/shell/nav-catalog.ts",
        "{c}"
    );
    assert_eq!(
        c["catalog"]["routes"].as_u64(),
        Some(catalog.len() as u64),
        "{c}"
    );
    assert_eq!(c["complete"], true, "{c}");

    // The roll-up was asked for a bounded window and the sweep ran.
    let log = stub.log();
    assert!(
        log.contains("GET /api/surface-opens/rollup?since=") && log.contains("&until="),
        "the roll-up was not read over a stated window:\n{log}"
    );
    assert!(
        log.contains("POST /api/surface-opens/sweep"),
        "retention was not swept:\n{log}"
    );

    // THE JOURNAL GETS THE HEADLINE, not a digest that hides the counts.
    assert!(
        out.contains(&format!("filed on {}", &JOB_ID[..8])),
        "stdout does not name the packet:\n{out}"
    );
    assert!(
        out.contains("12 open(s) by 2 actor(s) across 3 route(s)"),
        "the headline does not carry the totals:\n{out}"
    );
    assert!(
        out.contains("emp-david: 10 open(s), 2 route(s)"),
        "each actor gets a line:\n{out}"
    );
    assert!(
        out.contains("swept 4 row(s)"),
        "the sweep's count is in the headline:\n{out}"
    );
}

/// `row` is the same reading printed, for an operator asking by hand —
/// it reads the roll-up, touches no packet, and SWEEPS NOTHING.
#[test]
fn row_prints_the_reading_and_neither_files_nor_sweeps() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("row", "serve");
    let (rc, out, err) = run(&stub, "row", &repo_root());
    assert_eq!(rc, Some(0), "row did not exit 0:\n{err}");
    let row: serde_json::Value = serde_json::from_str(&out).expect("row prints JSON");
    assert_eq!(row["measured"]["opens"].as_i64(), Some(12));
    assert!(
        row["measured"]["retention"].is_null(),
        "row does not sweep: {row}"
    );
    assert!(row["coverage"]["complete"] == true);
    assert!(
        stub.patches().is_empty(),
        "row filed something:\n{}",
        stub.log()
    );
    let log = stub.log();
    assert!(
        !log.contains("/api/jobs"),
        "row read the packet queue:\n{log}"
    );
    assert!(!log.contains("sweep"), "row swept rows:\n{log}");
}

/// A roll-up that cannot be read is an infrastructure refusal (exit 3)
/// and NOTHING is filed: a row from no read would say "nobody opened
/// anything", which is the confident wrong answer the chore contract
/// exists to refuse (14c135f5: a run that cannot state its coverage
/// FAILS).
#[test]
fn an_unreadable_rollup_files_nothing_and_exits_cannot_answer() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("rollup-down", "rollup-down");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(CANNOT_ANSWER), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("CANNOT ANSWER"), "{err}");
    assert!(
        err.contains("rollup"),
        "the refusal names what it could not read:\n{err}"
    );
    assert!(
        stub.patches().is_empty(),
        "something was filed:\n{}",
        stub.log()
    );
    assert!(
        !stub.log().contains("sweep"),
        "a run that read nothing must not sweep:\n{}",
        stub.log()
    );
}

/// A tree whose nav catalog yields no paths has no roster to judge
/// against — refused the same way, never filed as "every surface was
/// opened".
#[test]
fn an_unreadable_catalog_is_refused_not_an_empty_never_opened_list() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("no-catalog", "serve");
    let repo = boss_testing::scratch_dir("surface-usage-no-catalog");
    std::fs::create_dir_all(repo.join("apps/web/src/shell")).unwrap();
    std::fs::write(
        repo.join("apps/web/src/shell/nav-catalog.ts"),
        "// a catalog with no entries\nexport const ROUTE_CATALOG = {};\n",
    )
    .unwrap();
    let (rc, out, err) = run(&stub, "file", &repo);
    assert_eq!(rc, Some(CANNOT_ANSWER), "stdout:\n{out}\nstderr:\n{err}");
    assert!(err.contains("no nav paths"), "{err}");
    assert!(
        stub.patches().is_empty(),
        "something was filed:\n{}",
        stub.log()
    );
}

/// A sweep that fails is loud AFTER the reading is on the packet: the
/// row carries the reason under `measured.retention.error`, the run
/// exits 1, and the measurement is never lost to the housekeeping.
#[test]
fn a_failed_sweep_still_files_the_reading_and_exits_nonzero() {
    if !has_python3() {
        eprintln!("skipping: no python3");
        return;
    }
    let stub = start_stub("sweep-down", "sweep-down");
    let (rc, out, err) = run(&stub, "file", &repo_root());
    assert_eq!(rc, Some(1), "stdout:\n{out}\nstderr:\n{err}");
    let patches = stub.patches();
    assert_eq!(
        patches.len(),
        1,
        "the reading was not filed:\n{}",
        stub.log()
    );
    let m = &patches[0].1["measured"];
    assert_eq!(m["opens"].as_i64(), Some(12), "{m}");
    assert!(m["retention"]["swept"].is_null(), "{m}");
    assert!(
        m["retention"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("sweep")),
        "the row says why retention was not swept: {m}"
    );
    assert!(
        out.contains("NOT swept"),
        "the headline says so too:\n{out}"
    );
}

// Drive both actual unit programs. A wrapper's deliberate exit0 on a
// visibility miss is not success when the collector then loses its record.
fn packet_lifecycle(stub: &Stub, opt_in: bool) -> (std::process::Output, String) {
    packet_lifecycle_with_ledger(stub, opt_in, false)
}

fn packet_lifecycle_with_ledger(
    stub: &Stub,
    opt_in: bool,
    ledger: bool,
) -> (std::process::Output, String) {
    let launch = stub.log.with_extension("lifecycle.sh");
    boss_testing::write_file(
        &launch,
        &format!(
            "#!/usr/bin/env bash\nbash '{}' maintenance-surface-usage 'Surface usage'\nbash '{}' file --repo '{}'\n",
            repo_root().join("infra/boss-maintenance-wrap.sh").display(),
            script().display(),
            repo_root().display()
        ),
    );
    let home = stub.log.with_extension("home");
    boss_testing::create_dir(&home);
    let mut command = Command::new("bash");
    command
        .arg(&launch)
        .env("BOSS_JOBS_URL", format!("http://127.0.0.1:{}", stub.port))
        .env("BOSS_WRAP_MAX_TIME", FIXTURE_BUDGET.as_secs().to_string())
        .env("BOSS_API_RETRY_DEADLINE", "0")
        .env("HOME", &home)
        .env("KUBERNETES_SERVICE_HOST", "fixture")
        .env_remove("BOSS_NODE_ID")
        .env_remove("HOST_ID")
        .env_remove("INVOCATION_ID");
    if ledger {
        command
            .env_remove("KUBERNETES_SERVICE_HOST")
            .env("HOST_ID", "fixture");
        let ledger_dir = home.join(".boss-maintenance-unrecorded");
        boss_testing::create_dir(&ledger_dir);
        boss_testing::write_file(
            &ledger_dir.join("maintenance-surface-usage@fixture.jsonl"),
            "{\"id\":\"older-unrecorded\",\"at\":\"2026-10-24T00:00:00Z\",\"why\":\"original visibility failure\"}\n",
        );
    }
    if opt_in {
        command
            .env("BOSS_WRAP_OPEN_RETRY", "3")
            .env("INVOCATION_ID", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .env("BOSS_WRAP_OPEN_RETRY_WAIT", "0");
    } else {
        command
            .env_remove("BOSS_WRAP_OPEN_RETRY")
            .env_remove("BOSS_WRAP_OPEN_RETRY_WAIT");
    }
    let out = without_proxy(&mut command).output().unwrap();
    let calls = std::fs::read_to_string(&stub.log).unwrap();
    (out, calls)
}

#[test]
fn a_packet_open_read_failure_does_not_lose_the_actual_surface_reading() {
    let stub = start_stub("packet-recover", "packet-recover");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(
        out.status.success(),
        "actual collector failed after wrapper: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        calls.lines().filter(|l| *l == "POST /api/jobs").count(),
        1,
        "{calls}"
    );
    assert!(
        calls.contains("PATCH /api/jobs/"),
        "reading was never filed: {calls}"
    );
    assert!(
        calls.contains("\"opens\":12"),
        "whole measured row missing: {calls}"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("GET /api/surface-opens/rollup"))
            .count(),
        1,
        "collector recomputed during packet retry: {calls}"
    );
}

#[test]
fn a_permanent_packet_open_failure_is_bounded_and_keeps_the_whole_reading() {
    let stub = start_stub("packet-permanent", "packet-permanent");
    let started = std::time::Instant::now();
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(!out.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "fixture retry never ended"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("GET /api/jobs?"))
            .count(),
        4,
        "three bounded wrapper reads plus collector read: {calls}"
    );
    assert!(!calls.contains("POST /api/jobs"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("NOT filed") && err.contains("\"complete\": true"),
        "full refusal/row missing: {err}"
    );
}

#[test]
fn a_lost_packet_create_ack_reads_back_the_same_id_without_a_duplicate() {
    let stub = start_stub("packet-lost-ack", "packet-lost-ack");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sent: Vec<serde_json::Value> = calls
        .lines()
        .filter_map(|l| l.strip_prefix("ADMIT "))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        sent.len(),
        1,
        "unknown ACK must not mint another packet: {calls}"
    );
    let id = sent[0]["id"]
        .as_str()
        .expect("opt-in creates under one stable caller UUID");
    assert!(
        calls.contains(&format!("GET /api/jobs/{id}\n")),
        "unknown ACK never read its exact identity: {calls}"
    );
    assert!(
        calls.contains(&format!("PATCH /api/jobs/{id}/metadata\n")),
        "collector did not file original reading: {calls}"
    );
}

#[test]
fn a_lost_ack_confirms_the_server_resolved_owner_and_protocol_status() {
    for mode in [
        "packet-lost-ack",
        "packet-normalized-owner-after-ack",
        "packet-normalized-status-after-ack",
        "packet-normalized-both-after-ack",
    ] {
        let stub = start_stub(mode, mode);
        let (out, calls) = packet_lifecycle(&stub, true);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{mode}: {stdout}\n{stderr}\n{calls}");
        assert!(
            stdout.contains("spawned today's"),
            "{mode}: an admitted packet was falsely refused: {stdout}\n{stderr}\n{calls}"
        );
        assert!(
            !stderr.contains("packet create acknowledgement is unread or differs"),
            "{mode}: {stderr}"
        );
        let admissions: Vec<serde_json::Value> = calls
            .lines()
            .filter_map(|line| line.strip_prefix("ADMIT "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(admissions.len(), 1, "no duplicate POST: {mode}: {calls}");
        let id = admissions[0]["id"].as_str().unwrap();
        assert!(calls.contains(&format!("GET /api/jobs/{id}\n")), "{calls}");
    }
}

#[test]
fn a_lost_ack_never_confirms_a_foreign_identity_subject_or_metadata() {
    for mode in [
        "packet-foreign-id-after-ack",
        "packet-foreign-subject-after-ack",
        "packet-foreign-kind-after-ack",
        "packet-foreign-metadata-after-ack",
        "packet-missing-owner-after-ack",
        "packet-missing-status-after-ack",
    ] {
        let stub = start_stub(mode, mode);
        let (out, calls) = packet_lifecycle(&stub, true);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            calls
                .lines()
                .filter(|line| *line == "POST /api/jobs")
                .count(),
            1,
            "{mode}: {calls}"
        );
        assert!(
            stderr.contains(
                "packet create acknowledgement is unread or differs; no duplicate was sent"
            ),
            "{mode}: {stderr}"
        );
        assert!(!stdout.contains("spawned today's"), "{mode}: {stdout}");
    }
}

/// Use the real admission handler and the shipped Workflow, not an echo
/// server: the POST commits with a resolved owner, its ACK is lost, and
/// the wrapper must judge the native GET representation without a resend.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_ack_confirms_actual_native_admission_with_a_resolved_timer_owner() {
    use axum::response::IntoResponse;
    use axum::serve::ListenerExt;
    use boss_core::{port::EventBus, publisher::DomainPublisher};
    use boss_jobs::{
        InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry,
        http::{JobsApiState, router},
        owner_resolution::RosterLookup,
    };
    use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
    use std::sync::{
        Arc,
        atomic::{AtomicU16, AtomicUsize, Ordering},
    };

    struct Roster;
    #[async_trait::async_trait]
    impl RosterLookup for Roster {
        async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
            Ok(if role == "platform-admin" {
                vec!["emp-resolved-admin".into()]
            } else {
                vec![]
            })
        }
        async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
            // A tenant CAN have an active bootstrap employee. The
            // opt-in request must still use its explicit machine owner.
            Ok(matches!(id, "emp-bootstrap-admin" | "emp-resolved-admin"))
        }
    }

    let registry = Arc::new(InMemoryWorkflows::for_fixture());
    let specifications = boss_jobs::seed_loader::load_workflows(
        repo_root().join("infra/platform/workflows/maintenance-surface-usage.toml"),
    )
    .unwrap();
    for spec in specifications {
        registry.seed(spec).unwrap();
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = boss_testing::RecordingEventBus::new();
    let events: Arc<dyn EventBus> = bus.clone();
    let policy = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        kind_registry: Some(registry as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(events, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let began = std::time::Instant::now();
    let trace_port = Arc::new(AtomicU16::new(0));
    let posts = Arc::new(AtomicUsize::new(0));
    let requests = posts.clone();
    let native = router(state);
    let middleware_port = trace_port.clone();
    let app = native.clone().layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let posts = posts.clone();
            let trace_port = middleware_port.clone();
            async move {
                let port = trace_port.load(Ordering::Relaxed);
                admission_trace(began, port, "middleware_entry", None);
                let method = match request.method().as_str() { "GET" => "GET", "POST" => "POST", _ => "other" };
                let route = match request.uri().path() {
                    "/api/jobs" => "/api/jobs",
                    path if path.starts_with("/api/jobs/") => "/api/jobs/:id",
                    _ => "other",
                };
                eprintln!("{}", serde_json::json!({"fixture": "lost-ack-native-admission", "port": port, "method": method, "route": route}));
                let admission = request.method() == axum::http::Method::POST
                    && request.uri().path() == "/api/jobs";
                let response = next.run(request).await;
                admission_trace(began, port, "native_response_decision", Some(response.status().as_u16()));
                if admission {
                    posts.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
                    admission_trace(began, port, "injected_lost_ack", Some(503));
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "ACK lost after admission",
                    )
                        .into_response()
                } else {
                    response
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    // Label observations without moving bind/spawn or adding a readiness barrier.
    trace_port.store(port, Ordering::Relaxed);
    admission_trace(began, port, "listener_bound", None);
    eprintln!(
        "lost-ack fixture port={port} runtime_workers={}",
        tokio::runtime::Handle::current().metrics().num_workers()
    );
    let listener = listener.tap_io(move |_| admission_trace(began, port, "accept", None));
    let server = tokio::spawn(async move {
        admission_trace(began, port, "server_task_entry", None);
        axum::serve(listener, app).await.unwrap()
    });
    let output = tokio::task::spawn_blocking(move || {
        let directory = boss_testing::scratch_dir("lost-ack-native-admission-trace");
        let real = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("curl")).find(|path| path.is_file()).unwrap();
        boss_testing::write_exec(&directory.join("curl"), traced_curl_script());
        let trace = directory.join("curl.trace");
        let mut paths = vec![directory.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let result = without_proxy(&mut Command::new("bash"))
            .arg(repo_root().join("infra/boss-maintenance-wrap.sh"))
            .args(["maintenance-surface-usage", "Surface usage"])
            .env("BOSS_JOBS_URL", base)
            .env("BOSS_WRAP_OPEN_RETRY", "3")
            .env("BOSS_WRAP_OPEN_RETRY_WAIT", "0")
            .env("BOSS_API_RETRY_DEADLINE", "0")
            .env("KUBERNETES_SERVICE_HOST", "fixture")
            .env("INVOCATION_ID", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("FIXTURE_REAL_CURL", real)
            .env("FIXTURE_CURL_TRACE", &trace)
            .env("FIXTURE_CURL_PORT", port.to_string())
            .env_remove("BOSS_NODE_ID")
            .env_remove("HOST_ID")
            .output()
            .unwrap();
        // Preserve native bytes before ANY jobs/owner/status assertion can panic.
        std::fs::write(directory.join("child.stdout"), &result.stdout).unwrap();
        std::fs::write(directory.join("child.stderr"), &result.stderr).unwrap();
        admission_trace(began, port, "wrapper_exit", result.status.code().and_then(|n| n.try_into().ok()));
        eprintln!("lost-ack fixture port={port} raw_directory={directory:?} status={} stdout={:?} stderr={:?} curl_trace={:?}", result.status, result.stdout, result.stderr, std::fs::read_to_string(trace));
        result
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    admission_trace(began, port, "server_joined", None);
    let (packets, total) = jobs
        .list_jobs(&boss_jobs::port::JobFilter::default(), 10, 0)
        .await
        .unwrap();
    assert_eq!(total, 1);
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].owner_id, "emp-resolved-admin");
    assert_eq!(requests.load(Ordering::SeqCst), 1, "no second POST");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("spawned today's"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("UNRECORDED RUN"), "{stderr}");
    println!(
        "NATIVE_ADMISSION posts=1 packets=1 resolved_owner={} confirmed=true",
        packets[0].owner_id
    );

    // The canonical exception is conditional: a requested active person
    // stays caller-owned, so a different active owner is a collision.
    use tower::ServiceExt;
    let mut body = serde_json::json!({
        "id": "18181818-1818-4818-a818-181818181818",
        "kind": "maintenance-surface-usage",
        "subject": {"subject_kind": "custom", "id": "infra/maintenance-surface-usage"},
        "title": "Active human owner control",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard", "status": "open",
        "metadata": {"chore": "maintenance-surface-usage"}, "tags": ["maintenance"]
    });
    for expected in [201, 200, 409, 422] {
        if expected == 409 {
            body["owner_id"] = "emp-resolved-admin".into();
        }
        if expected == 422 {
            body.as_object_mut().unwrap().remove("owner_id");
        }
        let response = native.clone().oneshot(
            axum::http::Request::builder()
                .method("POST").uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", r#"{"id":"automation:maintenance-timer","role":"platform-admin","access_tier":"operator"}"#)
                .body(axum::body::Body::from(body.to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(response.status().as_u16(), expected);
    }
}

#[test]
fn an_unread_create_ack_is_not_reposted_or_called_recorded() {
    let stub = start_stub("packet-unread-after-ack", "packet-unread-after-ack");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(!out.status.success());
    assert_eq!(
        calls.lines().filter(|l| *l == "POST /api/jobs").count(),
        1,
        "uncertain readback must not mint another UUID: {calls}"
    );
    assert!(!calls.contains("PATCH /api/jobs/"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("NOT filed"));
}

#[test]
fn other_maintenance_callers_keep_the_existing_single_attempt_behavior() {
    let stub = start_stub("packet-default", "packet-recover");
    let (out, calls) = packet_lifecycle(&stub, false);
    assert!(!out.status.success());
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("GET /api/jobs?"))
            .count(),
        2,
        "default wrapper changed: {calls}"
    );
    assert!(!calls.contains("POST /api/jobs"));
}

#[test]
fn a_default_packet_keeps_its_legacy_owner_and_server_allocated_identity() {
    let stub = start_stub("packet-default-open", "packet-default-open");
    let (out, calls) = packet_lifecycle(&stub, false);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let admissions: Vec<serde_json::Value> = calls
        .lines()
        .filter_map(|line| line.strip_prefix("ADMIT "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(admissions.len(), 1);
    assert_eq!(admissions[0]["owner_id"], "emp-bootstrap-admin");
    assert!(admissions[0].get("id").is_none());
}

#[test]
fn an_opt_in_unknown_ack_preserves_the_original_carried_rows_as_possibly_recorded() {
    let stub = start_stub("ledger-unknown-ack", "packet-unread-after-ack");
    let (out, calls) = packet_lifecycle_with_ledger(&stub, true, true);
    assert!(!out.status.success());
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "POST /api/jobs")
            .count(),
        1
    );
    let ledger = stub
        .log
        .with_extension("home")
        .join(".boss-maintenance-unrecorded/maintenance-surface-usage@fixture.jsonl");
    let rows = std::fs::read_to_string(ledger).unwrap();
    let original: serde_json::Value = rows
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["id"] == "older-unrecorded")
        .unwrap();
    assert_eq!(original["why"], "original visibility failure");
    assert!(
        original["possibly_recorded_at"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "a committed-but-unread create cannot return the carried row as never sent: {original}"
    );
}

#[test]
fn a_transient_create_failure_retries_the_exact_same_caller_packet_after_absence_readback() {
    let stub = start_stub("post-recovery", "packet-post-recover");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(
        out.status.success(),
        "{}\n{calls}",
        String::from_utf8_lossy(&out.stderr)
    );
    let admitted: Vec<serde_json::Value> = calls
        .lines()
        .filter_map(|line| line.strip_prefix("ADMIT "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(admitted.len(), 2);
    assert_eq!(admitted[0], admitted[1], "never change ID or body on retry");
    let id = admitted[0]["id"].as_str().unwrap();
    assert!(calls.contains(&format!("GET /api/jobs/{id}\n")));
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("GET /api/surface-opens/rollup?"))
            .count(),
        1
    );
}

#[test]
fn a_permanent_create_failure_stops_at_the_declared_bound_and_retains_the_reading() {
    let stub = start_stub("post-permanent", "packet-post-permanent");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(!out.status.success());
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "POST /api/jobs")
            .count(),
        3,
        "bounded identical admission attempts: {calls}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("recorded nothing"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("\"complete\": true"));
}

#[test]
fn a_mismatched_create_readback_is_not_reposted_or_called_a_confirmed_admission() {
    let stub = start_stub("mismatch-ack", "packet-mismatch-after-ack");
    let (out, calls) = packet_lifecycle(&stub, true);
    // Visibility refusal must not stop the actual collector's work. The
    // wrapper cannot call the mismatched admission confirmed, even when
    // the collector can subsequently file a real reading on the open row.
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "POST /api/jobs")
            .count(),
        1
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("packet create acknowledgement is unread or differs; no duplicate was sent")
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("spawned today's"));
}

#[test]
fn an_empty_successful_create_readback_is_not_a_confirmed_admission() {
    let stub = start_stub("empty-ack", "packet-empty-after-ack");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert_eq!(
        calls
            .lines()
            .filter(|line| *line == "POST /api/jobs")
            .count(),
        1
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("packet create acknowledgement is unread or differs; no duplicate was sent")
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("spawned today's"));
}

#[test]
fn an_existing_packet_keeps_its_creation_identity_when_a_fresh_reading_is_filed() {
    let stub = start_stub("existing-reading", "packet-rerun-existing");
    let (out, calls) = packet_lifecycle(&stub, true);
    assert!(
        out.status.success(),
        "{}\n{calls}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!calls.contains("POST /api/jobs\n"));
    let patches = stub.patches();
    assert_eq!(patches.len(), 1);
    assert_eq!(
        patches[0].1["reading_invocation_id"],
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    );
    assert!(
        patches[0].1.get("unit_invocation_id").is_none(),
        "creation identity is not overwritten"
    );
    assert_eq!(patches[0].1["coverage"]["complete"], true);
    assert_eq!(patches[0].1["measured"]["opens"], 12);
}
