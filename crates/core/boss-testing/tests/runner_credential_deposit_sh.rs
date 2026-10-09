//! `infra/forge/runner-credential-deposit.sh` — the forge takes its ops
//! runner credential from the broker's Secret into the root-only file its
//! runner presents, PROVED through the jobs API's credential door first,
//! and records delivery so the broker may promote it (design f623e425 Q1;
//! backlog 1e50e66b; review 3930a3eb's N1, N3, N4 and N5 folded in).
//!
//! HOW THIS IS MEASURED. The script runs for real against the tree's own
//! broker rule; the shared stub `kubectl` answering the Secret from files
//! in scratch (`boss_testing::kubectl_secret_stub`); and a tiny HTTP jobs
//! API on 127.0.0.1 whose `GET /api/jobs/runner-credential` resolves the
//! presented `x-boss-runner-credential` against a table the case sets —
//! the door's contract: a host and a slot, or `resolved: false` — or
//! answers 404, as a build without the door would; and whose
//! `GET /api/jobs/{id}` answers the rotation packets a case files. Fixture
//! values are fake; every case also checks that none of them reaches the
//! output, the run summary, or a request line or body.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

const SCRIPT: &str = "infra/forge/runner-credential-deposit.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-forge-ops-runner-credential.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";
const RUNNER: &str = "infra/ops/ops-runner.sh";
const CRED: &str = "ops-runner-credential-forge";
const JOB: &str = "1e50e66b-b501-4d45-8da7-c162a0f41d54";
const OTHER_JOB: &str = "6c9183de-0000-4000-8000-000000000001";
const ATTEMPT: &str = "00000000-0000-4000-8000-000000000104";
const SECRET_UID: &str = "00000000-0000-4000-8000-000000000105";

// 43-character base64url fixtures, fake.
const OLD: &str = "oldOLDoldOLDoldOLDoldOLDoldOLDoldOLDold0001";
const NEW: &str = "newNEWnewNEWnewNEWnewNEWnewNEWnewNEWnew0002";

fn last8(s: &str) -> &str {
    &s[s.len() - 8..]
}

/// An RFC 3339 instant `secs` ago, in the jobs API's spelling.
fn ago(secs: i64) -> String {
    let out = Command::new("date")
        .args([
            "-u",
            "-d",
            &format!("{secs} seconds ago"),
            "+%Y-%m-%dT%H:%M:%S.123456Z",
        ])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    body: String,
    actor: Option<String>,
    runner_credential_presented: bool,
}

#[derive(Default)]
struct State {
    /// value → (host, slot) the door resolves it to.
    known: Vec<(String, String, String)>,
    /// The rotation packets, by id.
    jobs: HashMap<String, serde_json::Value>,
    /// The jobs API has no credential door: whoami answers 404.
    no_door: bool,
    whoami_status: Option<u16>,
    packet_status: Option<u16>,
    requests: Vec<Req>,
    delivery_context: Option<serde_json::Value>,
    delivery_receipt: Option<serde_json::Value>,
    delivery_refusal: bool,
    foreign_holder: bool,
    malformed_delivery_receipt: bool,
    completion_refusal: bool,
    empty_reply_at: Option<String>,
    blank_reply: bool,
}

/// The jobs API: the credential door's whoami, a packet read, and the step
/// writes.
struct Api {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Api {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state: Arc<Mutex<State>> = Default::default();
        let s = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                serve(conn, &s);
            }
        });
        Self { port, state }
    }

    fn resolves(&self, value: &str, host: &str, slot: &str) {
        self.state
            .lock()
            .unwrap()
            .known
            .push((value.into(), host.into(), slot.into()));
    }

    /// An open rotation packet of this credential whose `delivered` step
    /// is ready, its install recorded `installed_secs_ago`.
    fn awaiting(&self, id: &str, installed_secs_ago: i64) {
        let job = serde_json::json!({
            "id": id, "status": "open", "opened_at": ago(installed_secs_ago), "subject": {"subject_kind": "custom", "id": CRED},
            "steps": [
                {"id": format!("step-install-{}", &id[..8]), "spec_slug": "install", "status": "completed",
                 "completed_at": ago(installed_secs_ago)},
                {"id": format!("step-delivered-{}", &id[..8]), "spec_slug": "delivered", "status": "ready"}
                ,{"id": format!("step-scope-{}", &id[..8]), "spec_slug": "scope", "status": "completed", "completed_at": ago(installed_secs_ago)}
            ]
        });
        self.state.lock().unwrap().jobs.insert(id.into(), job);
    }

    fn no_door(&self) {
        self.state.lock().unwrap().no_door = true;
    }

    fn writes(&self) -> Vec<Req> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.method != "GET")
            .cloned()
            .collect()
    }

    fn whoamis(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.path == "/api/jobs/runner-credential")
            .count()
    }
}

fn serve(conn: std::net::TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let (mut presented, mut len) = (None::<String>, 0usize);
    let mut actor = None;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let v = v.trim().to_string();
            match k.to_ascii_lowercase().as_str() {
                "x-boss-runner-credential" => presented = Some(v),
                "x-boss-user" => {
                    actor = serde_json::from_str::<serde_json::Value>(&v).unwrap()["id"]
                        .as_str()
                        .map(str::to_string);
                }
                "content-length" => len = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; len];
    let _ = reader.read_exact(&mut body);
    let body = String::from_utf8_lossy(&body).to_string();
    let mut st = state.lock().unwrap();
    st.requests.push(Req {
        method: method.clone(),
        path: path.clone(),
        body,
        actor,
        runner_credential_presented: presented.is_some(),
    });
    let (status, text) = if path == "/api/jobs/runner-credential" {
        if let Some(code) = st.whoami_status {
            (
                if code == 401 {
                    "401 Unauthorized"
                } else {
                    "503 Unavailable"
                },
                "{}".to_string(),
            )
        } else if st.no_door {
            ("404 Not Found", "Not Found".to_string())
        } else {
            let hit = presented.and_then(|p| st.known.iter().find(|(v, _, _)| *v == p).cloned());
            (
                "200 OK",
                match hit {
                    Some((_, host, slot)) => serde_json::json!({
                        "resolved": true, "principal": "runner:ops",
                        "actor_id": "automation:ops-runner", "host": host, "slot": slot,
                        "delivery":st.delivery_context
                    })
                    .to_string(),
                    None => r#"{"resolved":false,"header":"x-boss-runner-credential"}"#.to_string(),
                },
            )
        }
    } else if method == "POST" && path == format!("/api/credentials/{CRED}/delivery") {
        let authenticated = presented.as_ref().is_some_and(|presented| {
            st.known
                .iter()
                .any(|(value, host, _)| value == presented && host == "forge")
        });
        if authenticated && !st.delivery_refusal {
            let context = st.delivery_context.clone().unwrap();
            let evidence = serde_json::json!({"purpose":"authenticated-runner-delivery",
                "credential_id":CRED,"host":"forge","job_id":context["job_id"],
                "attempt":ATTEMPT,"secret_uid":SECRET_UID});
            let mut receipt = serde_json::json!({"version":1,"credential_id":CRED,
                "phase":"verified","observation_id":format!("{ATTEMPT}:delivered"),
                "actor":"automation:ops-runner","timestamp":ago(0),
                "event_id":"00000000-0000-4000-8000-000000000106",
                "evidence_json":evidence.to_string(),
                "canonical":boss_core::job::canonical_json_bytes(&evidence)});
            if st.malformed_delivery_receipt {
                receipt["actor"] = serde_json::json!("forged-caller");
            }
            st.delivery_receipt = Some(receipt.clone());
            ("202 Accepted", serde_json::json!({"recorded":true,
                "kind":"credential.verified","observation":{"outcome":"recorded","receipt":receipt}}).to_string())
        } else {
            ("403 Forbidden", "{}".into())
        }
    } else if method == "GET" && path.ends_with("/version") {
        ("200 OK", serde_json::json!({"step":{"status":"ready","assignee_id":if st.foreign_holder { Some("fake-other-holder") } else { None }},
            "version":"fake-scoped-version"}).to_string())
    } else if method == "POST" && path.ends_with("/complete-if") {
        if st.completion_refusal {
            ("409 Conflict", "{}".into())
        } else {
            ("200 OK", "{\"outcome\":\"completed\"}".into())
        }
    } else if method == "GET" && path.starts_with("/api/jobs/") {
        let id = path.trim_start_matches("/api/jobs/");
        match (st.packet_status, st.jobs.get(id)) {
            (Some(_), _) => ("401 Unauthorized", "{}".to_string()),
            (_, Some(j)) => ("200 OK", j.to_string()),
            (_, None) => ("404 Not Found", "{}".to_string()),
        }
    } else {
        ("200 OK", "{}".to_string())
    };
    let text = if st.empty_reply_at.as_deref() == Some(path.as_str()) {
        if st.blank_reply {
            " \n\t".into()
        } else {
            String::new()
        }
    } else {
        text
    };
    drop(st);
    let mut conn = conn;
    let _ = write!(
        conn,
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

struct Case {
    root: PathBuf,
    secrets: PathBuf,
    dest: PathBuf,
    summary: PathBuf,
    kubectl: PathBuf,
    api: Api,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("runner-credential-deposit-{name}"));
        let secrets = root.join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let kubectl = root.join("kubectl");
        write_exec(&kubectl, boss_testing::kubectl_secret_stub::SECRET_KUBECTL);
        Self {
            dest: root.join("etc-boss").join("ops-runner.credential"),
            summary: root.join("summary.json"),
            secrets,
            kubectl,
            api: Api::start(),
            root,
        }
    }

    fn secret_dir(&self) -> PathBuf {
        self.secrets.join("boss").join("ops-runner-credential")
    }

    fn secret(&self, entries: &[(&str, &str)]) {
        let dir = self.secret_dir();
        std::fs::create_dir_all(&dir).unwrap();
        for (k, v) in entries {
            std::fs::write(dir.join(k), v).unwrap();
        }
    }

    /// The broker's stage: `next` = NEW for `job`, `current` = OLD.
    fn staged_for(&self, job: &str) {
        self.secret(&[
            ("forge.current", OLD),
            ("forge.next", NEW),
            ("forge.next.minted-for", job),
        ]);
        self.api.state.lock().unwrap().delivery_context = Some(serde_json::json!({
            "credential_id":CRED,"job_id":job,"attempt":ATTEMPT,"secret_uid":SECRET_UID
        }));
    }

    fn held(&self, value: &str) {
        std::fs::create_dir_all(self.dest.parent().unwrap()).unwrap();
        write_file(&self.dest, value);
        // mode-bits-ok: a file as a hand placement might leave it.
        std::fs::set_permissions(&self.dest, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    fn file(&self) -> Option<String> {
        std::fs::read_to_string(&self.dest).ok()
    }

    fn mode(&self) -> u32 {
        std::fs::metadata(&self.dest).unwrap().permissions().mode() & 0o777
    }

    fn run_as(&self, node: &str) -> (i32, String) {
        self.run_at(node, &format!("http://127.0.0.1:{}", self.api.port))
    }

    /// The pass with `BOSS_JOBS_URL` spelled as the case gives it, and no
    /// host list anywhere: loopback is the only host the rule admits.
    fn run_at(&self, node: &str, jobs_url: &str) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .args(["--rule", &repo_root().join(RULE).display().to_string()])
            .args(["--dest", &self.dest.display().to_string()])
            .env("BOSS_DEPOSIT_KUBECTL", &self.kubectl)
            .env("STUB_SECRETS", &self.secrets)
            .env("BOSS_JOBS_URL", jobs_url)
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("no-machine-token"))
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary)
            .env("BOSS_NODE_ID", node)
            .env("BOSS_API_RETRY_DEADLINE", "0")
            .env("TMPDIR", &self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env_remove("http_proxy")
            .env_remove("HTTP_PROXY")
            .env_remove("RUNTIME_DIRECTORY")
            .env_remove("BOSS_MACHINE_TOKEN")
            .output()
            .expect("run runner-credential-deposit.sh");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        self.assert_nothing_leaked(&text);
        (out.status.code().unwrap_or(-1), text)
    }

    fn run(&self) -> (i32, String) {
        self.run_as("forge")
    }

    fn summary(&self, key: &str) -> String {
        let text = std::fs::read_to_string(&self.summary).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        v[key].as_str().unwrap_or("").to_string()
    }

    fn assert_nothing_leaked(&self, out: &str) {
        let summary = std::fs::read_to_string(&self.summary).unwrap_or_default();
        let requests: String = self
            .api
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .map(|r| format!("{} {} {}\n", r.method, r.path, r.body))
            .collect();
        for v in [OLD, NEW] {
            for (what, text) in [
                ("output", out),
                ("summary", &summary),
                ("requests", &requests),
            ] {
                assert!(!text.contains(v), "a value reached the {what}:\n{text}");
            }
        }
    }
}

#[test]
fn credential_http_refusals_are_red_with_their_status_and_no_delivery() {
    for status in [401, 503] {
        let c = Case::new(&format!("http-refusal-{status}"));
        c.staged_for(JOB);
        c.held(OLD);
        c.api.state.lock().unwrap().whoami_status = Some(status);
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{out}");
        assert!(
            c.summary("runner_credential_action")
                .contains(&format!("HTTP {status}")),
            "{out}"
        );
        assert_eq!(c.file().as_deref(), Some(OLD));
        assert!(c.api.writes().is_empty());
    }
}

#[test]
fn a_resolving_held_value_does_not_hide_unbounded_mount_lag() {
    let c = Case::new("held-too-old");
    c.staged_for(JOB);
    c.held(OLD);
    c.api.resolves(OLD, "forge", "current");
    c.api.awaiting(JOB, 7201);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_action").contains("lag bound"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(OLD));
    assert!(c.api.writes().is_empty());
}

#[test]
fn a_rotation_packet_http_refusal_is_red_and_names_its_status() {
    let c = Case::new("packet-refused");
    c.staged_for(JOB);
    c.held(NEW);
    c.api.state.lock().unwrap().packet_status = Some(401);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_delivery").contains("HTTP 401"),
        "{out}"
    );
    assert!(c.api.writes().is_empty());
}

#[test]
fn packet_selection_jq_failures_always_reach_the_durable_summary() {
    let real = Command::new("sh")
        .args(["-c", "command -v jq"])
        .output()
        .unwrap();
    assert!(real.status.success());
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    for expression in ["].id // empty", "].completed_at // empty"] {
        let c = Case::new(&format!("jq-selection-{}", expression.len()));
        c.staged_for(JOB);
        c.held(NEW);
        c.api.resolves(NEW, "forge", "next");
        c.api.awaiting(JOB, 30);
        std::fs::create_dir_all(c.root.join("bin")).unwrap();
        write_exec(
            &c.root.join("bin/jq"),
            &format!(
                "#!/bin/bash\nif [[ \"$*\" == *'{expression}'* ]]; then exit 7; fi\nexec '{real}' \"$@\"\n"
            ),
        );
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{out}");
        assert!(
            c.summary("runner_credential_delivery")
                .contains("did not parse"),
            "{out}"
        );
        assert!(c.api.writes().is_empty());
        assert_eq!(c.file().as_deref(), Some(NEW));
    }
}

#[test]
fn an_existing_delivery_record_does_not_claim_unobserved_promotion() {
    let c = Case::new("recorded-not-promoted");
    c.staged_for(JOB);
    c.held(NEW);
    c.api.awaiting(JOB, 30);
    c.api.state.lock().unwrap().jobs.get_mut(JOB).unwrap()["steps"][1]["status"] =
        "completed".into();
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("already recorded"), "{out}");
    assert!(!out.contains("broker promotes"), "{out}");
    assert!(
        c.summary("runner_credential_delivery")
            .contains("promotion not observed"),
        "{out}"
    );
    assert!(c.api.writes().is_empty());
}

#[test]
fn a_held_value_lag_without_a_valid_past_install_has_no_grace() {
    for timestamp in [
        serde_json::Value::Null,
        serde_json::json!(ago(-3600)),
        serde_json::json!("invalid"),
    ] {
        let c = Case::new("held-invalid-age");
        c.staged_for(JOB);
        c.held(OLD);
        c.api.resolves(OLD, "forge", "current");
        c.api.awaiting(JOB, 30);
        c.api.state.lock().unwrap().jobs.get_mut(JOB).unwrap()["steps"][0]["completed_at"] =
            timestamp;
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{out}");
        assert!(
            c.summary("runner_credential_action").contains("lag bound"),
            "{out}"
        );
        assert_eq!(c.file().as_deref(), Some(OLD));
        assert!(c.api.writes().is_empty());
    }
}

#[test]
fn a_staged_value_before_install_completion_is_diagnosed_pending() {
    let c = Case::new("install-not-completed");
    c.staged_for(JOB);
    c.api.awaiting(JOB, 30);
    {
        let mut state = c.api.state.lock().unwrap();
        let job = state.jobs.get_mut(JOB).unwrap();
        job["steps"][0]["status"] = "active".into();
        job["steps"][0]["completed_at"] = serde_json::Value::Null;
        job["steps"][1]["status"] = "pending".into();
    }
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        c.summary("runner_credential_action")
            .contains("install is not completed"),
        "{out}"
    );
    assert_eq!(c.file(), None);
    assert!(c.api.writes().is_empty());
}

#[test]
fn pending_install_without_a_known_young_scope_is_not_an_unbounded_retry() {
    for opened in [
        serde_json::Value::Null,
        serde_json::json!(ago(7201)),
        serde_json::json!(ago(-3600)),
    ] {
        let c = Case::new("pending-install-no-grace");
        c.staged_for(JOB);
        c.api.awaiting(JOB, 30);
        {
            let mut state = c.api.state.lock().unwrap();
            let job = state.jobs.get_mut(JOB).unwrap();
            job["steps"][2]["completed_at"] = opened;
            job["steps"][0]["status"] = "active".into();
            job["steps"][0]["completed_at"] = serde_json::Value::Null;
            job["steps"][1]["status"] = "pending".into();
        }
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{out}");
        assert!(!c.summary("runner_credential_action").is_empty(), "{out}");
        assert_eq!(c.file(), None);
        assert!(c.api.writes().is_empty());
    }
}

#[test]
fn old_admission_with_a_newly_completed_scope_can_wait_for_install() {
    let c = Case::new("old-admission-new-scope");
    c.staged_for(JOB);
    c.api.awaiting(JOB, 30);
    {
        let mut state = c.api.state.lock().unwrap();
        let job = state.jobs.get_mut(JOB).unwrap();
        job["opened_at"] = ago(86400).into();
        job["steps"][0]["status"] = "active".into();
        job["steps"][0]["completed_at"] = serde_json::Value::Null;
        job["steps"][1]["status"] = "pending".into();
    }
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        c.summary("runner_credential_action")
            .contains("install is not completed"),
        "{out}"
    );
    assert_eq!(c.file(), None);
    assert!(c.api.writes().is_empty());
}

#[test]
fn an_installed_value_http_refusal_cannot_record_delivery() {
    let c = Case::new("held-http-refusal");
    c.staged_for(JOB);
    c.held(NEW);
    c.api.awaiting(JOB, 30);
    c.api.state.lock().unwrap().whoami_status = Some(401);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_delivery").contains("HTTP 401"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(NEW));
    assert!(c.api.writes().is_empty());
}

#[test]
fn a_delivery_body_jq_failure_is_diagnosed_without_writes() {
    let real = Command::new("sh")
        .args(["-c", "command -v jq"])
        .output()
        .unwrap();
    assert!(real.status.success());
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let c = Case::new("delivery-body-jq-failure");
    c.staged_for(JOB);
    c.held(NEW);
    c.api.resolves(NEW, "forge", "next");
    c.api.awaiting(JOB, 30);
    std::fs::create_dir_all(c.root.join("bin")).unwrap();
    write_exec(
        &c.root.join("bin/jq"),
        &format!(
            "#!/bin/bash\nif [[ \"$*\" == *'secret_uid: .secret_uid'* ]]; then exit 7; fi\nexec '{real}' \"$@\"\n"
        ),
    );
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_delivery")
            .contains("did not serialize"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(NEW));
    assert!(c.api.writes().is_empty());
}

#[test]
fn a_delivery_attempt_read_failure_is_diagnosed_without_writes() {
    let real = Command::new("sh")
        .args(["-c", "command -v jq"])
        .output()
        .unwrap();
    assert!(real.status.success());
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let c = Case::new("delivery-attempt-jq-failure");
    c.staged_for(JOB);
    c.held(NEW);
    c.api.resolves(NEW, "forge", "next");
    c.api.awaiting(JOB, 30);
    std::fs::create_dir_all(c.root.join("bin")).unwrap();
    write_exec(
        &c.root.join("bin/jq"),
        &format!(
            "#!/bin/bash\nif [[ \"$*\" == *'-r .attempt'* ]]; then exit 7; fi\nexec '{real}' \"$@\"\n"
        ),
    );
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_delivery")
            .contains("did not serialize"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(NEW));
    assert!(c.api.writes().is_empty());
}

#[test]
fn a_staged_value_the_door_resolves_is_installed_root_only_and_its_delivery_recorded() {
    let c = Case::new("staged");
    c.staged_for(JOB);
    c.held(OLD);
    c.api.resolves(OLD, "forge", "current");
    c.api.resolves(NEW, "forge", "next");
    c.api.awaiting(JOB, 3600);

    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.file().as_deref(), Some(NEW));
    assert_eq!(c.mode(), 0o600, "root-only: 0600");
    assert!(
        out.contains(&format!("installed forge.next (…{})", last8(NEW))),
        "{out}"
    );
    let writes = c.api.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(writes.iter().all(|request|
        request.actor.as_deref() == Some("automation:runner-credential-deposit")),
        "client attribution is preserved; the real server independently resolves the credential actor");
    assert!(
        writes
            .iter()
            .all(|request| request.runner_credential_presented),
        "delivery writes present the installed host credential: {writes:?}"
    );
    assert_eq!(writes[0].method, "POST");
    assert_eq!(writes[0].path, format!("/api/credentials/{CRED}/delivery"));
    let body: serde_json::Value = serde_json::from_str(&writes[0].body).unwrap();
    assert_eq!(body["job_id"], JOB);
    assert_eq!(body["attempt"], ATTEMPT);
    assert_eq!(body["secret_uid"], SECRET_UID);
    assert_eq!(writes[1].method, "POST");
    assert!(writes[1].path.ends_with("/complete-if"));
    let body: serde_json::Value = serde_json::from_str(&writes[1].body).unwrap();
    assert_eq!(body["evidence"]["delivered_last_eight"], last8(NEW));
    assert_eq!(
        body["evidence"]["delivered_to"],
        format!("forge:{}", c.dest.display())
    );
    assert_eq!(
        body["evidence"]["delivered_receipt"],
        c.api
            .state
            .lock()
            .unwrap()
            .delivery_receipt
            .clone()
            .unwrap()
    );
    assert!(
        c.summary("runner_credential_delivery")
            .starts_with(&format!("recorded on {}", &JOB[..8])),
        "{}",
        c.summary("runner_credential_delivery")
    );
    assert_eq!(c.summary("runner_credential_last_eight"), last8(NEW));
}

/// N1. A mount that is only BEHIND is not a failure: the file the runner
/// presents still resolves, so the staged value's absence is kubelet's
/// refresh, and the pass says "not yet" and stays green.
#[test]
fn a_staged_value_the_mount_has_not_refreshed_is_not_yet_while_the_held_one_resolves() {
    let c = Case::new("lagging");
    c.staged_for(JOB);
    c.held(OLD);
    c.api.resolves(OLD, "forge", "current");
    c.api.awaiting(JOB, 30);

    let (rc, out) = c.run();
    assert_eq!(rc, 0, "a lagging mount is not a red: {out}");
    assert_eq!(c.file().as_deref(), Some(OLD), "the working value is kept");
    assert!(
        c.summary("runner_credential_action").starts_with("not yet"),
        "{out}"
    );
    assert!(c.api.writes().is_empty(), "no delivery is recorded");
}

/// N1's other half: when the held value no longer resolves either, the
/// mount is not behind — it is broken or missing — and the pass reds.
#[test]
fn a_staged_value_unresolved_while_the_held_one_is_dead_too_reds_the_pass() {
    let c = Case::new("dead");
    c.staged_for(JOB);
    c.held(OLD);
    c.api.awaiting(JOB, 30);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file().as_deref(), Some(OLD));
    assert!(out.contains("does not resolve forge.next"), "{out}");
    assert!(c.api.writes().is_empty());
}

/// With no file yet (a host's first rotation) the staged value's age is
/// the only witness: inside kubelet's bound it is not yet, past it a red.
#[test]
fn a_first_value_the_door_has_not_seen_is_not_yet_only_inside_kubelets_bound() {
    let c = Case::new("first-young");
    c.staged_for(JOB);
    c.api.awaiting(JOB, 30);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.file(), None);
    assert!(
        c.summary("runner_credential_action").starts_with("not yet"),
        "{out}"
    );

    let c = Case::new("first-old");
    c.staged_for(JOB);
    c.api.awaiting(JOB, 3600);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file(), None);
}

/// THE CREDENTIAL IS PRESENTED ONLY TO A HOST THE MACHINE TOKEN'S RULE
/// ADMITS (backlog 50708d76, F2 of review 927f8602). `whoami` made the
/// credential's header with plain `secret_header`, which judges no host,
/// while the machine token four lines above it was judged — the shape car
/// 025640e3 repaired in the ops runner. It asks `machine_token_admits`
/// first now.
///
/// Read by EFFECT, with the real curl, from what the stub API received.
/// Both spellings reach the stub on loopback and neither is a host the
/// rule reads as one: `127.1` is the short IPv4 curl expands and the rule
/// withholds, and the bare `127.0.0.1:<port>` has no leading scheme (F1 —
/// curl guesses http). THE CONTROL IS IN THE SAME PASS: the rotation
/// packet's read, which carries no credential, arrives — so "no whoami"
/// is the credential withheld, not a URL that reached nothing.
#[test]
fn the_credential_is_never_presented_to_a_host_the_machine_token_rule_does_not_admit() {
    for (tag, spelled) in [("short-quad", "http://127.1"), ("no-scheme", "127.0.0.1")] {
        // Staged and not yet held: the install's proof is the first
        // presentation, and it does not happen.
        let c = Case::new(&format!("off-estate-install-{tag}"));
        c.staged_for(JOB);
        c.held(OLD);
        c.api.resolves(OLD, "forge", "current");
        c.api.resolves(NEW, "forge", "next");
        c.api.awaiting(JOB, 3600);
        let url = format!("{spelled}:{}", c.api.port);
        let (rc, out) = c.run_at("forge", &url);
        let requests = c.api.state.lock().unwrap().requests.clone();
        assert!(
            requests
                .iter()
                .any(|r| r.path == format!("/api/jobs/{JOB}")),
            "[{tag}] the control: the packet read reaches the stub at {url}: {requests:?}\n{out}"
        );
        assert!(
            c.api.whoamis() == 0 && requests.iter().all(|r| !r.runner_credential_presented),
            "[{tag}] the runner credential left for a host the machine token's rule does not \
             admit: {requests:?}\n{out}"
        );
        assert_eq!(rc, 1, "[{tag}] a value that cannot be proved is red: {out}");
        assert_eq!(
            c.file().as_deref(),
            Some(OLD),
            "[{tag}] the held file is kept"
        );
        let action = c.summary("runner_credential_action");
        assert!(
            action.starts_with("not installed")
                && action.contains("NOT presented")
                && action.contains("BOSS_MACHINE_TOKEN_HOSTS"),
            "[{tag}] the summary says what was withheld and by which rule: {action}"
        );
        assert!(c.api.writes().is_empty(), "[{tag}]");

        // Already held: the delivery record's presentation does not
        // happen either, and nothing is recorded.
        let c = Case::new(&format!("off-estate-delivery-{tag}"));
        c.staged_for(JOB);
        c.held(NEW);
        c.api.resolves(NEW, "forge", "next");
        c.api.awaiting(JOB, 3600);
        let url = format!("{spelled}:{}", c.api.port);
        let (rc, out) = c.run_at("forge", &url);
        let requests = c.api.state.lock().unwrap().requests.clone();
        assert!(
            requests
                .iter()
                .any(|r| r.path == format!("/api/jobs/{JOB}")),
            "[{tag}] the control: {requests:?}\n{out}"
        );
        assert!(
            c.api.whoamis() == 0 && requests.iter().all(|r| !r.runner_credential_presented),
            "[{tag}] the held credential left for a host the rule does not admit: \
             {requests:?}\n{out}"
        );
        assert_eq!(rc, 1, "[{tag}] {out}");
        let delivery = c.summary("runner_credential_delivery");
        assert!(
            delivery.starts_with("not recorded") && delivery.contains("NOT presented"),
            "[{tag}] {delivery}"
        );
        assert!(c.api.writes().is_empty(), "[{tag}]");
    }
}

#[test]
fn a_value_the_door_resolves_to_another_host_is_never_installed() {
    let c = Case::new("other-host");
    c.secret(&[("forge.next", NEW), ("forge.next.minted-for", JOB)]);
    c.api.resolves(NEW, "boss-gcp", "current");
    c.api.awaiting(JOB, 3600);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file(), None);
    assert!(out.contains("boss-gcp/current"), "{out}");
}

/// N5. A jobs API that answers 404 for the door is a build without it — a
/// permanent condition, named as such and red, never "the next pass
/// retries".
#[test]
fn a_jobs_api_without_the_door_is_a_loud_distinct_failure() {
    let c = Case::new("no-door");
    c.staged_for(JOB);
    c.held(OLD);
    c.api.awaiting(JOB, 3600);
    c.api.no_door();
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(c.file().as_deref(), Some(OLD));
    assert!(
        c.summary("runner_credential_action")
            .contains("has no credential door"),
        "{}",
        c.summary("runner_credential_action")
    );
    assert!(!out.contains("next pass retries"), "{out}");
}

#[test]
fn the_first_value_is_installed_from_current_and_there_is_nothing_to_record() {
    let c = Case::new("current");
    c.secret(&[("forge.current", NEW), ("forge.current.minted-for", JOB)]);
    c.api.resolves(NEW, "forge", "current");
    c.api.awaiting(JOB, 3600);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.file().as_deref(), Some(NEW));
    assert_eq!(c.mode(), 0o600);
    assert!(
        c.api.writes().is_empty(),
        "delivery is recorded only for a STAGED value — the promotion's input"
    );
    assert_eq!(c.summary("runner_credential_delivery"), "nothing to record");
}

#[test]
fn a_held_value_is_left_alone_and_its_mode_repaired() {
    let c = Case::new("held");
    c.secret(&[("forge.current", OLD)]);
    c.held(OLD);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert_eq!(c.file().as_deref(), Some(OLD));
    assert_eq!(c.mode(), 0o600, "the file is held at 0600 whatever set it");
    assert_eq!(c.api.whoamis(), 0, "nothing to prove");
    assert_eq!(c.summary("runner_credential_action"), "unchanged");
}

/// N3. The staged value names the packet it was minted for, so the
/// delivery is recorded THERE — never on whichever packet happens to wait,
/// which the broker would refuse (its staged value is not that packet's)
/// and could never be recorded again.
#[test]
fn delivery_is_recorded_on_the_packet_the_staged_value_names() {
    let c = Case::new("named");
    c.staged_for(JOB);
    c.api.resolves(NEW, "forge", "next");
    c.api.awaiting(JOB, 3600);
    c.api.awaiting(OTHER_JOB, 3600);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    let writes = c.api.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert_eq!(writes[0].path, format!("/api/credentials/{CRED}/delivery"));
    let acknowledged: serde_json::Value = serde_json::from_str(&writes[0].body).unwrap();
    assert_eq!(
        acknowledged["job_id"], JOB,
        "the actual mounted generation names the packet"
    );
    assert!(
        writes[1].path.starts_with(&format!("/api/jobs/{JOB}/")),
        "only that named packet is conditionally completed: {writes:?}"
    );

    // Minted for a packet that is not awaiting delivery: nothing recorded.
    let c = Case::new("named-closed");
    c.staged_for(OTHER_JOB);
    c.api.resolves(NEW, "forge", "next");
    c.api.awaiting(JOB, 3600);
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(c.api.writes().is_empty(), "{:?}", c.api.writes());
    assert!(
        c.summary("runner_credential_delivery")
            .contains(&OTHER_JOB[..8]),
        "{}",
        c.summary("runner_credential_delivery")
    );
}

#[test]
fn an_absent_or_empty_secret_changes_nothing() {
    let c = Case::new("absent");
    c.held(OLD);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        c.summary("runner_credential_secret").starts_with("absent"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(OLD));

    c.secret(&[]);
    let (rc, out) = c.run();
    assert_eq!(rc, 0, "{out}");
    assert!(
        c.summary("runner_credential_secret").starts_with("empty"),
        "{out}"
    );
    assert_eq!(c.file().as_deref(), Some(OLD));
}

#[test]
fn another_hosts_rule_and_a_symlinked_file_are_refused() {
    let c = Case::new("refused");
    c.secret(&[("forge.next", NEW)]);
    c.api.resolves(NEW, "forge", "next");
    let (rc, out) = c.run_as("boss-gcp");
    assert_eq!(rc, 78, "a host holds its own credential only: {out}");
    assert_eq!(c.file(), None);

    let target = c.root.join("elsewhere");
    write_file(&target, "not the deposit's");
    std::fs::create_dir_all(c.dest.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &c.dest).unwrap();
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "not the deposit's",
        "never written through"
    );
    assert!(out.contains("is a symlink"), "{out}");
}

/// N4. A destination that cannot be read or written ends the pass red WITH
/// its cause on the converge packet — `set -e` used to exit before the run
/// summary was written, so the packet said it failed and not why.
#[test]
fn a_destination_that_cannot_be_read_or_written_names_its_cause() {
    // Unreadable: the destination is a directory.
    let c = Case::new("unreadable");
    c.staged_for(JOB);
    c.api.resolves(NEW, "forge", "next");
    std::fs::create_dir_all(&c.dest).unwrap();
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_action")
            .contains("cannot read"),
        "{}",
        c.summary("runner_credential_action")
    );

    // Unwritable: the destination's directory cannot be made (its parent
    // is a file) — the shape a full or read-only disk takes for the write.
    let c = Case::new("unwritable");
    c.staged_for(JOB);
    c.api.resolves(NEW, "forge", "next");
    let blocker = c.root.join("etc-boss");
    write_file(&blocker, "a file where the directory belongs");
    let (rc, out) = c.run();
    assert_eq!(rc, 1, "{out}");
    assert!(
        c.summary("runner_credential_action")
            .contains("could not be written"),
        "{}",
        c.summary("runner_credential_action")
    );
    assert!(c.api.writes().is_empty(), "no delivery without the file");
}

/// The forge's converge runs the deposit with the forge's rule, and the
/// file it writes is the file the runner reads: `BOSS_RUNNER_CREDENTIAL_FILE`
/// with ONE default in both scripts (CLAUDE.md §9a).
#[test]
fn the_converge_deposits_the_file_the_runner_presents() {
    let default_of = |path: &str| {
        let text = std::fs::read_to_string(repo_root().join(path)).unwrap();
        let marker = "${BOSS_RUNNER_CREDENTIAL_FILE:-";
        let start = text
            .find(marker)
            .unwrap_or_else(|| panic!("{path} names no BOSS_RUNNER_CREDENTIAL_FILE default"))
            + marker.len();
        text[start..start + text[start..].find('}').unwrap()].to_string()
    };
    assert_eq!(default_of(CONVERGE), "/etc/boss/ops-runner.credential");
    assert_eq!(default_of(CONVERGE), default_of(RUNNER));

    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
    let code: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let at = code
        .iter()
        .position(|l| l.contains("runner-credential-deposit.sh"))
        .expect("forge-converge.sh runs the runner credential deposit");
    assert!(
        !code[at].starts_with(' '),
        "the deposit runs on every tick, at the top level: {}",
        code[at]
    );
    assert!(
        code[at..at + 3]
            .join("\n")
            .contains(RULE.trim_start_matches("infra/")),
        "handed the forge's broker rule"
    );
    assert!(
        code.iter()
            .any(|l| l.contains("runner_rc") && l.contains("exit")),
        "its verdict decides the converge's exit"
    );
}

#[test]
fn delivery_owner_holder_receipt_and_completion_refusals_preserve_the_installed_file() {
    for (case, writes, cause) in [
        ("holder", 0, "holder refused"),
        ("owner", 1, "HTTP 403"),
        ("receipt", 1, "original receipt is unavailable"),
        ("completion", 2, "conditional completion refused (HTTP 409)"),
    ] {
        let c = Case::new(case);
        c.staged_for(JOB);
        c.held(NEW);
        c.api.resolves(NEW, "forge", "next");
        c.api.awaiting(JOB, 30);
        {
            let mut state = c.api.state.lock().unwrap();
            state.foreign_holder = case == "holder";
            state.delivery_refusal = case == "owner";
            state.malformed_delivery_receipt = case == "receipt";
            state.completion_refusal = case == "completion";
        }
        let (rc, out) = c.run();
        assert_eq!(rc, 1, "{case}: {out}");
        assert!(
            c.summary("runner_credential_delivery").contains(cause),
            "{case}: {out}"
        );
        assert_eq!(c.file().as_deref(), Some(NEW));
        assert_eq!(c.mode(), 0o600);
        assert_eq!(c.api.writes().len(), writes, "{case}: {:?}", c.api.writes());
        if case == "completion" {
            assert!(
                c.api.state.lock().unwrap().delivery_receipt.is_some(),
                "a Jobs CAS refusal does not discard the original owner receipt"
            );
        }
    }
}

#[test]
fn silent_version_owner_and_completion_replies_never_record_delivery() {
    for (stage, writes) in [("version", 0), ("owner", 1), ("completion", 2)] {
        for blank in [false, true] {
            let c = Case::new(&format!("silent-{stage}-{blank}"));
            c.staged_for(JOB);
            c.api.resolves(NEW, "forge", "next");
            c.api.awaiting(JOB, 3600);
            {
                let mut state = c.api.state.lock().unwrap();
                state.empty_reply_at = Some(match stage {
                    "version" => {
                        format!("/api/jobs/{JOB}/steps/step-delivered-{}/version", &JOB[..8])
                    }
                    "owner" => format!("/api/credentials/{CRED}/delivery"),
                    _ => format!(
                        "/api/jobs/{JOB}/steps/step-delivered-{}/complete-if",
                        &JOB[..8]
                    ),
                });
                state.blank_reply = blank;
            }
            let (rc, out) = c.run();
            assert_eq!(rc, 1, "{stage}/{blank}: {out}");
            assert_eq!(c.file().as_deref(), Some(NEW));
            assert!(
                c.summary("runner_credential_delivery")
                    .starts_with("not recorded:")
            );
            assert_eq!(
                c.api.writes().len(),
                writes,
                "{stage}/{blank}: {:?}",
                c.api.writes()
            );
            assert!(!out.contains(NEW));
        }
    }
}
