//! boss-gcp's ops runner credential leaves the forge only after the
//! receiver speaks first (backlog 1e50e66b; backlog dea2236f).
//!
//! `infra/gcp/runner-credential-send.sh` is the transport and
//! `infra/forge/runner-credential-push.sh` its deployment on the forge.
//! Each pass does one of three things — DELIVER a staged `boss-gcp.next`,
//! VERIFY the enrolled key reaches the receiver (and record that effect on
//! the deposit key's verify step), or NOTHING — and in every case the far
//! side's first line decides whether a credential byte may go: a key whose
//! line reaches a shell is handed nothing and is RED on its own route.
//!
//! The far side is a stub `ssh` on PATH acting out one of the shapes a real
//! sshd + forced command produces; the jobs API is a stub server that
//! serves the enrollment packet and records every request.
use boss_testing::{repo_root, scratch_dir, without_proxy, write_exec, write_file};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const SENDER: &str = "infra/gcp/runner-credential-send.sh";
const RECEIVER: &str = "infra/gcp/runner-credential-recv.sh";
const PUSH: &str = "infra/forge/runner-credential-push.sh";
const ENROLLMENT: &str = "00000000-0000-4000-8000-0000000000e1";
const VERIFY_STEP: &str = "00000000-0000-4000-8000-0000000000e5";
const JOB: &str = "00000000-0000-4000-8000-000000000001";
const ATTEMPT: &str = "00000000-0000-4000-8000-000000000002";
const UID: &str = "00000000-0000-4000-8000-000000000003";
const CREDENTIAL: &str = "ops-runner-credential-boss-gcp";
const VALUE: &str = "fake-sensitive-input-000000000000000000";

/// A `NAME='…'` constant of a script, read from the file.
fn script_constant(path: &str, name: &str) -> String {
    let body = std::fs::read_to_string(repo_root().join(path)).unwrap();
    let prefix = format!("{name}='");
    let line = body
        .lines()
        .find(|l| l.trim_start().starts_with(&prefix))
        .unwrap_or_else(|| panic!("{path} declares no {name}"));
    line.trim_start()
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or_else(|| panic!("{path}: {name} is not one single-quoted line: {line}"))
        .to_string()
}

fn greeting() -> String {
    script_constant(RECEIVER, "GREETING")
}

/// The runner deposit key's forced command, from its rule row.
fn forced_command() -> String {
    let rule: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            repo_root().join("infra/dispatcher/rules/broker-prepares-the-runner-deposit-key.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    rule["rule"][0]["do"][0]["args"]["forced_command"]
        .as_str()
        .unwrap()
        .trim_matches('"')
        .to_string()
}

fn estate_address() -> String {
    let estate: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/estate/estate.toml")).unwrap(),
    )
    .unwrap();
    estate["node"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"].as_str() == Some("boss-gcp"))
        .unwrap()["address"]
        .as_str()
        .unwrap()
        .to_string()
}

fn b64(text: &str) -> String {
    use std::process::Stdio;
    let mut child = Command::new("base64")
        .arg("-w0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    boss_testing::feed_stdin(&mut child, text.as_bytes());
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

struct Fixture {
    root: PathBuf,
    key: PathBuf,
    known: PathBuf,
    bin: PathBuf,
    public: String,
}

/// A fresh scratch with a deposit key stored the way the broker stores
/// one — WITHOUT its last newline, which an ed25519 key's parser refuses
/// unless the sender gives it back.
fn fixture(name: &str) -> Fixture {
    let root = scratch_dir(name);
    let generated = root.join("generated");
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&generated)
            .output()
            .unwrap()
            .status
            .success()
    );
    let public = Command::new("ssh-keygen")
        .args(["-y", "-f"])
        .arg(&generated)
        .output()
        .unwrap();
    assert!(public.status.success());
    let public = String::from_utf8(public.stdout).unwrap().trim().to_string();
    let pem = std::fs::read_to_string(&generated).unwrap();
    let key = root.join("private_key");
    write_file(&key, pem.trim_end_matches('\n'));
    let known = root.join("known_hosts");
    write_file(&known, "fixture-pinned-host-key");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    Fixture {
        root,
        key,
        known,
        bin,
        public,
    }
}

/// The deposit key's prepare-a-deposit-key packet: install names `public`,
/// enroll is in `enroll_status` and signed with the exact line, and verify
/// is in `verify_status`, carrying `context` when given.
fn enrollment(
    public: &str,
    enroll_status: &str,
    verify_status: &str,
    context: Option<&str>,
) -> serde_json::Value {
    let line = format!("command=\"{}\",restrict {public}", forced_command());
    let mut verify = serde_json::json!({"id":VERIFY_STEP,"spec_slug":"verify","status":verify_status,
        "metadata":{"human_only":true,"procedure":"Record a machine-run effect"}});
    if let Some(c) = context {
        verify["metadata"]["context_md"] = serde_json::json!(c);
    }
    serde_json::json!({"id":ENROLLMENT,"kind":"prepare-a-deposit-key","status":"open",
        "subject":{"id":"runner-deposit-key"},"steps":[
        {"id":"00000000-0000-4000-8000-0000000000e3","spec_slug":"install","status":"completed",
         "metadata":{"installed":"read back","public_key":public}},
        {"id":"00000000-0000-4000-8000-0000000000e4","spec_slug":"enroll","status":enroll_status,
         "metadata":{"authorized_keys_line":line,"receiver_host":"boss-gcp",
            "purpose":"ops-runner credential deposit","decision":"approved",
            "enrollment_record":"converge records runner_credential_receiver installed"}},
        verify]})
}

/// Secret boss/ops-runner-credential: `staged` stages boss-gcp.next with
/// its packet and the broker's original witness; otherwise the Secret
/// holds only the forge's slots.
fn runner_secret(staged: bool) -> serde_json::Value {
    let mut data = serde_json::json!({"forge.current": b64("forge-value-not-this-hosts")});
    if staged {
        let witness = serde_json::json!({"version":1,"host":"boss-gcp","credential_id":CREDENTIAL,
            "job_id":JOB,"attempt":ATTEMPT,"uid":UID,"promoted":false});
        data["boss-gcp.next"] = serde_json::json!(b64(VALUE));
        data["boss-gcp.next.minted-for"] = serde_json::json!(b64(JOB));
        data[format!("boss-gcp.recovery.{JOB}.witness")] =
            serde_json::json!(b64(&witness.to_string()));
    }
    serde_json::json!({"metadata":{"uid":UID},"data":data})
}

fn receipt() -> serde_json::Value {
    serde_json::json!({"version":1,"recorded":true,"host":"boss-gcp","credential_id":CREDENTIAL,
        "job_id":JOB,"attempt":ATTEMPT,"secret_uid":UID,"original_delivery_receipt":{
        "version":1,"credential_id":CREDENTIAL,"phase":"verified","actor":"automation:ops-runner",
        "observation_id":format!("{ATTEMPT}:delivered"),"event_id":"original-event",
        "timestamp":"original-time","evidence_json":serde_json::json!({
            "purpose":"authenticated-runner-delivery","job_id":JOB,"host":"boss-gcp",
            "attempt":ATTEMPT}).to_string()}})
}

/// The far side, as one of the shapes it can take. Every shape records its
/// argv to `ssh-args` and whatever it was sent to `wire`.
fn ssh_stub(f: &Fixture, shape: &str) {
    let greeting = greeting();
    let empty = script_constant(SENDER, "EMPTY_REFUSAL");
    let old = script_constant(SENDER, "OLD_REFUSAL");
    let body = match shape {
        // The receiver this tree installs, handed nothing: it greets, reads
        // to the end, and refuses the empty envelope in its own words.
        "receiver" => format!(
            "printf '%s\\n' '{greeting}'\ncat > \"$FIXTURE_ROOT/wire\"\nif [ -s \"$FIXTURE_ROOT/wire\" ]; then cat \"$FIXTURE_ROOT/receipt\"; exit 0; fi\necho '{empty}' >&2\nexit 65\n"
        ),
        // A key placed bare: sshd runs the account's shell, which prints
        // its banner and reads what it is sent as commands.
        "banner" => "echo 'Welcome to Ubuntu 22.04.4 LTS'\ncat > \"$FIXTURE_ROOT/wire\"\nexit 0\n"
            .to_string(),
        // The same shell, with no banner: silent, then a clean end.
        "silent-shell" => "cat > \"$FIXTURE_ROOT/wire\"\nexit 0\n".to_string(),
        // The receiver as boss-gcp holds it before this tree's lands: it
        // reads its whole stdin before it says anything.
        "old-receiver" => format!("cat > \"$FIXTURE_ROOT/wire\"\necho '{old}' >&2\nexit 65\n"),
        // sshd refusing the key.
        "refused-key" => {
            "echo 'fixture@host: Permission denied (publickey).' >&2\nexit 255\n".to_string()
        }
        other => panic!("no ssh shape {other}"),
    };
    write_exec(
        &f.bin.join("ssh"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$FIXTURE_ROOT/ssh-args\"\n{body}"),
    );
}

/// `kubectl`, answering from the fixture's files: `secret` for
/// ops-runner-credential, `deposit-secret` for runner-deposit-key, `pin`
/// for the known-hosts ConfigMap. A file that does not exist is NotFound.
fn kubectl_stub(f: &Fixture) -> PathBuf {
    let path = f.bin.join("kubectl");
    write_exec(
        &path,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FIXTURE_ROOT/kubectl-args\"\ncase \"$*\" in\n  *'get secret ops-runner-credential '*) f=secret ;;\n  *'get secret runner-deposit-key '*) f=deposit-secret ;;\n  *'get configmap break-glass-deposit-known-hosts '*) f=pin ;;\n  *) echo \"unexpected kubectl $*\" >&2; exit 2 ;;\nesac\n[ -f \"$FIXTURE_ROOT/$f\" ] || { echo 'Error from server (NotFound): not found' >&2; exit 1; }\ncat \"$FIXTURE_ROOT/$f\"\n",
    );
    path
}

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    user: serde_json::Value,
    body: String,
}

struct Server {
    address: String,
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<Vec<Req>>,
}

impl Server {
    fn requests(self) -> Vec<Req> {
        self.stop.store(true, Ordering::SeqCst);
        self.handle.join().unwrap()
    }
}

/// The jobs API: GET of the enrollment serves `packet`; a PATCH answers
/// 200 with `{}`; everything is recorded.
fn serve(packet: serde_json::Value) -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (stop2, seen2) = (stop.clone(), seen.clone());
    let handle = std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    reader.read_line(&mut request).unwrap();
                    let mut parts = request.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("").to_string();
                    let (mut length, mut user) = (0usize, serde_json::Value::Null);
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                        if let Some((name, raw)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                length = raw.trim().parse().unwrap();
                            }
                            if name.eq_ignore_ascii_case("x-boss-user") {
                                user = serde_json::from_str(raw.trim()).unwrap();
                            }
                        }
                    }
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).unwrap();
                    let answer = if method == "GET" && path == format!("/api/jobs/{ENROLLMENT}") {
                        packet.to_string()
                    } else if method == "PATCH" {
                        "{}".to_string()
                    } else {
                        String::new()
                    };
                    let status = if answer.is_empty() {
                        "404 Not Found"
                    } else {
                        "200 OK"
                    };
                    let mut stream = stream;
                    write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    )
                    .unwrap();
                    seen2.lock().unwrap().push(Req {
                        method,
                        path,
                        user,
                        body: String::from_utf8(body).unwrap(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => panic!("accept: {e}"),
            }
        }
        seen2.lock().unwrap().clone()
    });
    Server {
        address,
        stop,
        handle,
    }
}

fn path_with(bin: &Path) -> String {
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

/// The sender, as the forge's push hands it its transport.
fn send(f: &Fixture, server: &Server, extra: &[(&str, &str)]) -> Output {
    let kubectl = kubectl_stub(f);
    let mut cmd = Command::new("bash");
    without_proxy(&mut cmd);
    cmd.arg(repo_root().join(SENDER))
        .env("BOSS_RUNNER_DEPOSIT_ENROLLMENT", ENROLLMENT)
        .env("BOSS_RUNNER_DEPOSIT_KEY", &f.key)
        .env("BOSS_RUNNER_DEPOSIT_KNOWN_HOSTS", &f.known)
        .env(
            "BOSS_RUNNER_DEPOSIT_TARGET",
            format!("fixture@{}", estate_address()),
        )
        .env("BOSS_RUNNER_SEND_KUBECTL", &kubectl)
        .env("BOSS_RUNNER_SEND_GREETING_TIMEOUT_S", "2")
        .env("BOSS_RUNNER_SEND_TIMEOUT_S", "20")
        .env("BOSS_JOBS_URL", &server.address)
        .env("BOSS_MACHINE_TOKEN_DIR", f.root.join("absent-token"))
        .env("FIXTURE_ROOT", &f.root)
        .env("PATH", path_with(&f.bin));
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn wire(f: &Fixture) -> String {
    std::fs::read_to_string(f.root.join("wire")).unwrap_or_default()
}

fn stage(f: &Fixture, staged: bool) {
    write_file(&f.root.join("secret"), &runner_secret(staged).to_string());
    write_file(&f.root.join("receipt"), &receipt().to_string());
}

// ---------------------------------------------------------------------------
// The two halves speak one greeting.
// ---------------------------------------------------------------------------

/// The receiver's first line and the line the sender waits for are one
/// fact in two files (CLAUDE.md §9a).
#[test]
fn the_sender_waits_for_the_line_the_receiver_says_first() {
    assert_eq!(script_constant(SENDER, "GREETING"), greeting());
    let receiver = std::fs::read_to_string(repo_root().join(RECEIVER)).unwrap();
    assert!(
        receiver.contains(&format!(
            "echo '{}' >&2; exit 65",
            script_constant(SENDER, "EMPTY_REFUSAL")
        )),
        "the receiver refuses an empty envelope in the words the sender reads as evidence"
    );
}

/// The receiver says its greeting BEFORE it reads a byte: the line is
/// read here while nothing has been written to it, and only then is its
/// stdin closed.
#[test]
fn the_receiver_speaks_before_it_reads() {
    use std::process::Stdio;
    let root = scratch_dir("runner-recv-speaks-first");
    let destination = root.join("ops-runner.credential");
    write_file(&destination, "fake-existing-host-value");
    let mut child = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join(RECEIVER))
        .env("BOSS_RUNNER_CREDENTIAL_FILE", &destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let first = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the receiver said nothing while its stdin was still open");
    assert_eq!(first.trim_end_matches('\n'), greeting());
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(65));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&script_constant(SENDER, "EMPTY_REFUSAL")),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        "fake-existing-host-value"
    );
}

// ---------------------------------------------------------------------------
// VERIFY: nothing staged, and the deposit key's verify step owes a record.
// ---------------------------------------------------------------------------

/// The machine-run effect the verify step's procedure asks for: the
/// enrolled key reaches the receiver, which greets, is handed no byte, and
/// refuses the empty envelope — recorded on the step as context_md, by the
/// sender's own automation id, and never as the step's answer.
#[test]
fn a_pass_with_nothing_staged_verifies_the_receiver_and_records_the_effect_on_the_step() {
    let f = fixture("runner-send-verify");
    stage(&f, false);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let output = send(&f, &server, &[]);
    let requests = server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("verified:"));
    assert_eq!(wire(&f), "", "a verify pass sends nothing");
    let patch: Vec<&Req> = requests.iter().filter(|r| r.method == "PATCH").collect();
    assert_eq!(patch.len(), 1, "{requests:?}");
    assert_eq!(
        patch[0].path,
        format!("/api/jobs/{ENROLLMENT}/steps/{VERIFY_STEP}/metadata")
    );
    let body: serde_json::Value = serde_json::from_str(&patch[0].body).unwrap();
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec!["context_md"],
        "a machine writes only context on a human-only step"
    );
    let context = body["context_md"].as_str().unwrap();
    assert!(context.starts_with("Verified by effect at "), "{context}");
    assert!(context.contains(&greeting()), "{context}");
    assert_eq!(patch[0].user["id"], "automation:runner-credential-send");
    assert_eq!(patch[0].user["role"], "platform-admin");
    for r in requests.iter().filter(|r| r.method == "GET") {
        assert_eq!(r.user["role"], "audit-readonly", "reads sign read-only");
    }
}

/// Nothing staged and nothing owed: no connection is made at all.
#[test]
fn a_pass_with_nothing_staged_and_nothing_owed_makes_no_connection() {
    for (status, context) in [
        ("completed", None),
        ("ready", Some("Verified by effect at an earlier pass")),
    ] {
        let f = fixture("runner-send-nothing");
        stage(&f, false);
        ssh_stub(&f, "receiver");
        let server = serve(enrollment(&f.public, "completed", status, context));
        let output = send(&f, &server, &[]);
        let requests = server.requests();
        assert_eq!(output.status.code(), Some(0), "{}", text(&output));
        assert!(String::from_utf8_lossy(&output.stdout).contains("nothing to deliver"));
        assert!(!f.root.join("ssh-args").exists(), "no ssh was run");
        assert!(requests.iter().all(|r| r.method == "GET"), "{requests:?}");
    }
}

/// Before David signs enroll the key is not authorized to reach the
/// receiver: not yet, and neither the cluster nor boss-gcp is touched.
#[test]
fn an_unsigned_enrollment_is_not_yet_and_touches_nothing() {
    let f = fixture("runner-send-unsigned");
    stage(&f, true);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "ready", "pending", None));
    let output = send(&f, &server, &[]);
    server.requests();
    assert_eq!(output.status.code(), Some(75), "{}", text(&output));
    assert!(!f.root.join("kubectl-args").exists());
    assert!(!f.root.join("ssh-args").exists());
}

// ---------------------------------------------------------------------------
// DELIVER: a staged boss-gcp.next goes only to a far side that greeted.
// ---------------------------------------------------------------------------

#[test]
fn a_staged_value_reaches_a_receiver_that_greeted_and_its_receipt_comes_back() {
    let f = fixture("runner-send-deliver");
    stage(&f, true);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let output = send(&f, &server, &[]);
    let requests = server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let sent: serde_json::Value = serde_json::from_str(&wire(&f)).unwrap();
    assert_eq!(sent["value"], VALUE);
    assert_eq!(sent["job_id"], JOB);
    let returned: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        returned,
        receipt(),
        "stdout is the receiver's receipt, whole"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(VALUE));
    let args = std::fs::read_to_string(f.root.join("ssh-args")).unwrap();
    for flag in [
        "-F /dev/null",
        "-T",
        "StrictHostKeyChecking=yes",
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "GlobalKnownHostsFile=/dev/null",
    ] {
        assert!(args.contains(flag), "{flag}: {args}");
    }
    assert!(!args.contains(VALUE));
    // A delivery is also an effect against the receiver: recorded once.
    let patch: Vec<&Req> = requests.iter().filter(|r| r.method == "PATCH").collect();
    assert_eq!(patch.len(), 1, "{requests:?}");
    assert!(
        patch[0]
            .body
            .contains("received the ops-runner-credential-boss-gcp envelope")
    );
    assert!(!patch[0].body.contains(VALUE));
}

/// THE FINDING THIS EXISTS FOR (backlog dea2236f): a key placed without
/// its forced command reaches a shell. Whatever shape the shell takes,
/// not one byte of the staged value is written to it, and the pass is RED
/// on the key's own route, naming the line to remove.
#[test]
fn a_key_that_reaches_a_shell_is_handed_no_byte_and_is_red() {
    for shape in ["banner", "silent-shell"] {
        let f = fixture(&format!("runner-send-{shape}"));
        stage(&f, true);
        ssh_stub(&f, shape);
        let server = serve(enrollment(&f.public, "completed", "ready", None));
        let output = send(&f, &server, &[]);
        let requests = server.requests();
        assert_eq!(output.status.code(), Some(1), "{shape}: {}", text(&output));
        assert_eq!(wire(&f), "", "{shape}: the shell was handed a byte");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("RED runner-credential-send/key-unconfined refused:"),
            "{shape}: {stderr}"
        );
        assert!(stderr.contains("REMOVE THAT LINE"), "{shape}: {stderr}");
        assert!(!stderr.contains(VALUE));
        assert!(
            requests.iter().all(|r| r.method == "GET"),
            "{shape}: nothing is recorded as verified: {requests:?}"
        );
    }
}

/// An older receiver, which reads before it speaks, refuses the empty
/// stream in its own words: not yet while the tree's receiver is younger
/// than boss-gcp's converge can have installed, RED after — and RED when
/// the age is unknown. It is handed nothing either way.
#[test]
fn an_older_receiver_is_not_yet_inside_the_bound_and_red_after() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (since, code) in [
        ((now - 600).to_string(), 75),
        ((now - 3 * 3600).to_string(), 1),
        (String::new(), 1),
    ] {
        let f = fixture("runner-send-old-receiver");
        stage(&f, true);
        ssh_stub(&f, "old-receiver");
        let server = serve(enrollment(&f.public, "completed", "ready", None));
        let output = send(&f, &server, &[("BOSS_RUNNER_RECV_SINCE", &since)]);
        server.requests();
        assert_eq!(
            output.status.code(),
            Some(code),
            "since {since:?}: {}",
            text(&output)
        );
        assert_eq!(wire(&f), "", "an older receiver was handed a byte");
        if code == 1 {
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("RED runner-credential-send/receiver-stale refused:")
            );
        }
    }
}

/// The enroll step is signed, so its line was placed first: sshd refusing
/// the key is a line missing from boss-gcp, and a person must look.
#[test]
fn a_signed_key_that_boss_gcp_refuses_is_red() {
    let f = fixture("runner-send-refused");
    stage(&f, true);
    ssh_stub(&f, "refused-key");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let output = send(&f, &server, &[]);
    server.requests();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("RED runner-credential-send/key-refused refused:")
    );
}

// ---------------------------------------------------------------------------
// The forge's push: the deployment that reads the transport and runs it.
// ---------------------------------------------------------------------------

fn push(f: &Fixture, server: &Server, summary: &Path, extra: &[(&str, &str)]) -> Output {
    let mut cmd = push_cmd(f, server, summary, &repo_root().join(PUSH));
    cmd.env("BOSS_RUNNER_RECV_SINCE", "");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// The push at `script`, with the fixture's cluster, far side and jobs
/// API — and NO receiver age handed in: the script finds its own.
fn push_cmd(f: &Fixture, server: &Server, summary: &Path, script: &Path) -> Command {
    let kubectl = kubectl_stub(f);
    let mut cmd = Command::new("bash");
    without_proxy(&mut cmd);
    cmd.arg(script)
        .env("BOSS_DEPOSIT_KUBECTL", &kubectl)
        .env("BOSS_RUN_SUMMARY_FILE", summary)
        .env_remove("BOSS_RUNNER_RECV_SINCE")
        .env("BOSS_RUNNER_SEND_GREETING_TIMEOUT_S", "2")
        .env("BOSS_RUNNER_SEND_TIMEOUT_S", "20")
        .env("BOSS_JOBS_URL", &server.address)
        .env("BOSS_MACHINE_TOKEN_DIR", f.root.join("absent-token"))
        .env_remove("RUNTIME_DIRECTORY")
        .env("TMPDIR", &f.root)
        .env("FIXTURE_ROOT", &f.root)
        .env("PATH", path_with(&f.bin));
    cmd
}

/// The cluster's runner-deposit-key Secret and known-hosts ConfigMap, as
/// the forge's kubectl reads them.
fn deploy(f: &Fixture, with_pin: bool) {
    let pem = std::fs::read_to_string(&f.key).unwrap();
    write_file(
        &f.root.join("deposit-secret"),
        &serde_json::json!({"data":{"private_key":b64(&pem),"public_key":b64(&f.public),
            "minted_for":b64(ENROLLMENT)}})
        .to_string(),
    );
    let mut data = serde_json::json!({"target":format!("fixture@{}", estate_address())});
    if with_pin {
        data["known_hosts"] = serde_json::json!("fixture-pinned-host-key\n");
    }
    write_file(
        &f.root.join("pin"),
        &serde_json::json!({ "data": data }).to_string(),
    );
}

fn summary_line(summary: &Path) -> String {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(summary).unwrap()).unwrap();
    v["runner_credential_boss_gcp"]
        .as_str()
        .unwrap_or_else(|| panic!("no runner_credential_boss_gcp in {v}"))
        .to_string()
}

#[test]
fn the_forge_push_hands_the_sender_its_transport_and_records_what_happened() {
    let f = fixture("runner-push-verify");
    stage(&f, false);
    deploy(&f, true);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = push(&f, &server, &summary, &[]);
    let requests = server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let line = summary_line(&summary);
    assert!(line.starts_with("verified:"), "{line}");
    assert!(requests.iter().any(|r| r.method == "PATCH"), "{requests:?}");
    // The private copy lived in a scratch directory of the pass's own,
    // gone when it ended.
    let left: Vec<_> = std::fs::read_dir(&f.root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with("runner-push."))
        .collect();
    assert!(left.is_empty(), "scratch left behind: {left:?}");
    let kubectl = std::fs::read_to_string(f.root.join("kubectl-args")).unwrap();
    assert!(kubectl.contains("-n boss get secret runner-deposit-key -o json"));
    assert!(kubectl.contains("-n boss get configmap break-glass-deposit-known-hosts -o json"));
    assert!(kubectl.contains("-n boss get secret ops-runner-credential -o json"));
}

#[test]
fn the_forge_push_carries_a_red_and_names_it_on_the_packet() {
    let f = fixture("runner-push-red");
    stage(&f, true);
    deploy(&f, true);
    ssh_stub(&f, "banner");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = push(&f, &server, &summary, &[]);
    server.requests();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        summary_line(&summary).starts_with("RED runner-credential-send/key-unconfined"),
        "{}",
        summary_line(&summary)
    );
    assert_eq!(wire(&f), "");
}

#[test]
fn the_forge_push_without_its_inputs_says_which_and_sends_nothing() {
    // No transport Secret yet: not yet, green.
    let f = fixture("runner-push-no-key");
    stage(&f, true);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = push(&f, &server, &summary, &[]);
    server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(summary_line(&summary).starts_with("not yet:"));
    assert!(!f.root.join("ssh-args").exists());

    // No pinned host key: red — a key that may reach the receiver never
    // reaches a host this pass cannot authenticate.
    let f = fixture("runner-push-no-pin");
    stage(&f, true);
    deploy(&f, false);
    ssh_stub(&f, "receiver");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = push(&f, &server, &summary, &[]);
    server.requests();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(summary_line(&summary).starts_with("not configured:"));
    assert!(!f.root.join("ssh-args").exists());

    // No admin kubeconfig on the host: not ready, green, nothing read.
    let f = fixture("runner-push-no-kubeconfig");
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join(PUSH))
        .env_remove("BOSS_DEPOSIT_KUBECTL")
        .env("BOSS_OPS_DIR", f.root.join("no-ops-dir"))
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("BOSS_JOBS_URL", &server.address)
        .env("PATH", path_with(&f.bin))
        .output()
        .unwrap();
    server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(summary_line(&summary).starts_with("not ready:"));
}

/// The files the push runs, as a generation of root's tree carries them:
/// the push, the sender, and everything either sources or reads.
const PUSH_CLOSURE: &[&str] = &[
    PUSH,
    SENDER,
    "infra/run-summary.sh",
    "infra/lib/jq.sh",
    "infra/lib/sor.sh",
    "infra/lib/sor-reader.sh",
    "infra/lib/secret-header.sh",
    "infra/lib/curl-through-a-roll.sh",
    "infra/boss-api-curl.sh",
    "infra/estate/estate.toml",
];

/// Root's tree on the forge (infra/forge/root-tree.sh), as the push meets
/// it: a bare store, `repo.git`, holding a commit that changed the
/// receiver `receiver_age_s` seconds ago and a NEWER one that changed
/// something else; and `gen/<sha>/`, the newer commit exported with no
/// `.git`, saying which commit it is in `.boss-tree-sha`. Returns the tree
/// and the generation's push.
fn roots_tree(f: &Fixture, receiver_age_s: u64) -> (PathBuf, PathBuf) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let src = f.root.join("src");
    let tree = f.root.join("tree");
    boss_testing::create_dir(&src.join("infra/gcp"));
    boss_testing::create_dir(&tree);
    let git = |when: u64, args: &[&str]| {
        let at = format!("@{when} +0000");
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", &at)
            .env("GIT_COMMITTER_DATE", &at)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .arg("-C")
            .arg(&src)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(now, &["init", "-q"]);
    write_file(
        &src.join(RECEIVER),
        "the receiver, as this commit left it\n",
    );
    git(now, &["add", "-A"]);
    git(
        now - receiver_age_s,
        &["commit", "-q", "-m", "change the receiver"],
    );
    write_file(
        &src.join("infra/gcp/other.txt"),
        "a later change elsewhere\n",
    );
    git(now, &["add", "-A"]);
    git(now, &["commit", "-q", "-m", "change something else"]);
    let sha = git(now, &["rev-parse", "HEAD"]);
    let store = tree.join("repo.git");
    git(
        now,
        &["clone", "-q", "--bare", ".", store.to_str().unwrap()],
    );
    let generation = tree.join("gen").join(&sha);
    for path in PUSH_CLOSURE {
        let to = generation.join(path);
        boss_testing::create_dir(to.parent().unwrap());
        boss_testing::copy_exec(&repo_root().join(path), &to);
    }
    write_file(&generation.join(".boss-tree-sha"), &sha);
    assert!(!generation.join(".git").exists());
    (tree, generation.join(PUSH))
}

/// THE FORGE RUNS THE PUSH FROM A GENERATION OF ROOT'S TREE, which has no
/// `.git` (review f15070d2 of car 2c628718, B2). The push read the
/// receiver's age with `git -C` on its own tree, which there answers "not
/// a git repository": no age, no grace, so the first verify after landing
/// — meeting boss-gcp's older receiver, whose converge runs half-hourly —
/// was RED. The age is read from root's store at the generation's own
/// commit, by the receiver's path: ten minutes old is `not yet`, three
/// hours old is RED, though the generation's commit itself is new.
#[test]
fn the_push_from_a_generation_of_roots_tree_reads_the_receivers_age_from_roots_store() {
    for (age, code, says) in [
        (600, 0, "not yet"),
        (
            3 * 3600,
            1,
            "RED runner-credential-send/receiver-stale refused:",
        ),
    ] {
        let f = fixture("runner-push-generation");
        stage(&f, true);
        deploy(&f, true);
        ssh_stub(&f, "old-receiver");
        let (tree, script) = roots_tree(&f, age);
        let server = serve(enrollment(&f.public, "completed", "ready", None));
        let summary = f.root.join("summary.json");
        let output = push_cmd(&f, &server, &summary, &script)
            .env("BOSS_ROOT_TREE", &tree)
            .env("GIT_CEILING_DIRECTORIES", &f.root)
            .output()
            .unwrap();
        server.requests();
        assert_eq!(
            output.status.code(),
            Some(code),
            "receiver {age}s old: {}",
            text(&output)
        );
        let line = summary_line(&summary);
        assert!(line.starts_with(says), "receiver {age}s old: {line}");
        assert_eq!(wire(&f), "", "an older receiver was handed a byte");
    }
}

/// THE SENDER'S SCRATCH IS THE UNIT'S RUNTIME DIRECTORY TOO (review
/// f15070d2, N1). Its private copy of the deposit key and the whole
/// ops-runner-credential Secret it reads went to `mktemp -d` under the
/// host's /tmp, which a SIGKILL leaves behind; the push hands it the
/// directory systemd removes whatever the exit.
#[test]
fn the_sender_keeps_its_scratch_in_the_pushs_private_directory() {
    let f = fixture("runner-push-runtime-dir");
    stage(&f, false);
    deploy(&f, true);
    ssh_stub(&f, "receiver");
    let run = f.root.join("run");
    let tmp = f.root.join("tmp");
    boss_testing::create_dir(&run);
    boss_testing::create_dir(&tmp);
    let server = serve(enrollment(&f.public, "completed", "ready", None));
    let summary = f.root.join("summary.json");
    let output = push_cmd(&f, &server, &summary, &repo_root().join(PUSH))
        .env("BOSS_RUNNER_RECV_SINCE", "")
        .env("RUNTIME_DIRECTORY", &run)
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    server.requests();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let args = std::fs::read_to_string(f.root.join("ssh-args")).unwrap();
    let key = args
        .split_whitespace()
        .skip_while(|a| *a != "-i")
        .nth(1)
        .unwrap_or_else(|| panic!("ssh was handed no -i: {args}"));
    assert!(
        key.starts_with(run.to_str().unwrap()),
        "the sender's key copy was at {key}, not under {}",
        run.display()
    );
}

/// forge-converge runs the push on every tick and carries its exit, like
/// the forge's own deposit.
#[test]
fn forge_converge_runs_the_push_and_carries_its_exit() {
    let converge =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh")).unwrap();
    assert!(
        converge.contains(
            "timeout -k 5 \"$FORGE_RUNNER_PUSH_BOUND_S\" \"$INFRA/forge/runner-credential-push.sh\" || gcp_runner_rc=$?"
        ),
        "forge-converge does not run {PUSH} under its bound"
    );
    assert!(converge.contains("[ \"$gcp_runner_rc\" -eq 0 ] || exit \"$gcp_runner_rc\""));
    let mode = std::fs::metadata(repo_root().join(PUSH)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert!(
        mode.permissions().mode() & 0o111 != 0,
        "{PUSH} is executable"
    );
}

/// THE BUDGET THE CONVERGE'S COMMENT STATES IS THE ONE ITS NUMBERS MAKE
/// (review f15070d2, N2). It said the push sat with the deposits inside
/// the unit's ten minutes at a bound of 150 s; with the third deposit and
/// each `timeout -k 5` that was 620 s of a 600 s unit. The four bounded
/// steps of a tick, each with its kill margin, now fit the unit's
/// TimeoutStartSec on their own — and the push's bound still covers the
/// sender's own transfer bound and its kill margin.
#[test]
fn the_bounded_steps_of_a_converge_tick_fit_its_unit_together() {
    let converge =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.sh")).unwrap();
    let sender = std::fs::read_to_string(repo_root().join(SENDER)).unwrap();
    let unit =
        std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.service")).unwrap();
    let default_of = |text: &str, name: &str| -> u64 {
        let marker = format!("{name}:-");
        let at = text
            .find(&marker)
            .unwrap_or_else(|| panic!("no default for {name}"));
        text[at + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    };
    let unit_s: u64 = unit
        .lines()
        .find_map(|l| l.trim().strip_prefix("TimeoutStartSec="))
        .and_then(|v| v.strip_suffix("min"))
        .and_then(|m| m.parse::<u64>().ok())
        .map(|m| m * 60)
        .expect("forge-converge.service states TimeoutStartSec in minutes");
    let bounds = [
        "BOSS_FORGE_DEPOSIT_BOUND_S",
        "BOSS_FORGE_READER_DEPOSIT_BOUND_S",
        "BOSS_FORGE_REGISTRY_DEPOSIT_BOUND_S",
        "BOSS_FORGE_RUNNER_PUSH_BOUND_S",
    ];
    let together: u64 = bounds.iter().map(|b| default_of(&converge, b) + 5).sum();
    assert!(
        together < unit_s,
        "the four bounded steps of a tick may take {together}s, kill margins included, of a {unit_s}s unit"
    );
    let push = default_of(&converge, "BOSS_FORGE_RUNNER_PUSH_BOUND_S");
    let transfer = default_of(&sender, "BOSS_RUNNER_SEND_TIMEOUT_S");
    assert!(
        push > transfer + 5,
        "the push is bounded at {push}s, inside the sender's own {transfer}s transfer and its kill margin"
    );
}

// ---------------------------------------------------------------------------
// The rules that stage and promote boss-gcp's credential.
// ---------------------------------------------------------------------------

/// The declaration names the live pair; the pair hands the handler one
/// declaration, boss-gcp's, the one the receiver and sender are built for.
#[test]
fn boss_gcps_rules_are_live_and_hand_the_handler_one_declaration() {
    let declaration: toml::Value = toml::from_str(
        &std::fs::read_to_string(repo_root().join("infra/gcp/runner-credential-declaration.toml"))
            .unwrap(),
    )
    .unwrap();
    assert!(
        declaration.get("rule").is_none(),
        "the rules live in infra/dispatcher/rules, once (CLAUDE.md §9a)"
    );
    let id = declaration["credential"]["id"].as_str().unwrap();
    assert_eq!(id, CREDENTIAL);
    let names: Vec<&str> = declaration["activation"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2);
    let rules: Vec<toml::Value> = names
        .iter()
        .map(|n| {
            let file = repo_root().join(format!("infra/dispatcher/rules/{n}.toml"));
            let v: toml::Value = toml::from_str(
                &std::fs::read_to_string(&file)
                    .unwrap_or_else(|e| panic!("{}: {e}", file.display())),
            )
            .unwrap();
            v["rule"][0].clone()
        })
        .collect();
    assert_eq!(
        rules[0]["on_event"].as_str(),
        Some("step.done.credential-rotation")
    );
    assert_eq!(
        rules[1]["on_event"].as_str(),
        Some("step.done.credential-delivery")
    );
    for (rule, name) in rules.iter().zip(&names) {
        assert_eq!(rule["name"].as_str(), Some(*name));
        assert_eq!(
            rule["when"].as_str(),
            Some(format!("subject_id = \"{id}\"").as_str())
        );
        assert_eq!(
            rule["do"][0]["handler"].as_str(),
            declaration["credential"]["issuer_handler"].as_str()
        );
    }
    assert_eq!(rules[0]["do"][0]["args"], rules[1]["do"][0]["args"]);
    let args = &rules[0]["do"][0]["args"];
    for (key, want) in [
        ("credential_id", id),
        ("host", declaration["credential"]["host"].as_str().unwrap()),
        (
            "secret_name",
            declaration["credential"]["secret_name"].as_str().unwrap(),
        ),
        (
            "secret_namespace",
            declaration["credential"]["secret_namespace"]
                .as_str()
                .unwrap(),
        ),
    ] {
        assert_eq!(
            args[key].as_str(),
            Some(format!("\"{want}\"").as_str()),
            "{key}"
        );
    }
    for path in [RECEIVER, SENDER] {
        let body = std::fs::read_to_string(repo_root().join(path)).unwrap();
        assert!(body.contains(id), "{path} is not built for {id}");
    }
}
