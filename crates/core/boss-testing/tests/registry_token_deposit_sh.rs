//! `infra/forge/registry-token-deposit.sh` — the forge host takes the
//! broker-minted registry push token from its Secret into a ROOT-ONLY
//! docker config, proves it by a login the registry accepts, and records
//! delivery on the rotation packet (design c8502e17, question
//! image-build, David 2026-10-07; backlog 683d7fe9, its first car).
//!
//! HOW THIS IS MEASURED. The script runs for real against the tree's own
//! broker rule; the shared stub `kubectl` answers the Secret from files
//! in scratch (`boss_testing::kubectl_secret_stub`); and one tiny HTTP
//! server on 127.0.0.1 is both the registry — `GET /v2/token`, answered
//! 200 with a token for the logins a case admits and with the case's
//! refusal for any other — and the jobs API the delivery record reads
//! and writes. Fixture tokens are fake; every run checks that none of
//! them reaches the output, the run summary or a request line.
//!
//! WHAT IT DOES NOT MEASURE. Whether the live forge's `/v2/token`
//! answers a `write:package` token as this stub does: that is read on
//! the first real rotation, as `registry_token_effect` on the forge's
//! converge packet. And root ownership: the gate's uid cannot chown, so
//! the modes are held here and the owner only when run as root.

use boss_testing::{create_dir, repo_root, scratch_dir, without_proxy, write_exec};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const SCRIPT: &str = "infra/forge/registry-token-deposit.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-forge-registry-push-token.toml";
const DELIVERED_RULE: &str =
    "infra/dispatcher/rules/broker-revokes-the-forge-registry-push-token-on-delivery.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";
const CREDENTIAL: &str = "forge-registry-push-token";

// Token words, fake — one alphanumeric word, as the forge mints.
const OLD: &str = "0ldf0rgepusht0ken0ldf0rgepusht0ken0a1b2c3";
const NEW: &str = "newf0rgepusht0kennewf0rgepusht0ken4d5e6f7";
const FIXTURES: [&str; 2] = [OLD, NEW];

fn last8(s: &str) -> &str {
    &s[s.len() - 8..]
}

fn b64(input: &str) -> String {
    let out = Command::new("base64")
        .arg("-w0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(input.as_bytes())?;
            c.wait_with_output()
        })
        .expect("base64");
    String::from_utf8(out.stdout).unwrap()
}

#[derive(Default)]
struct State {
    /// `user:token` logins the registry answers with a token.
    admitted: Vec<String>,
    /// What any other login is answered with (401 unless a case says).
    refusal: u16,
    /// The open rotations the jobs API lists, as JSON rows.
    jobs: Vec<String>,
    /// Every request: its line, and its body.
    seen: Vec<(String, String)>,
}

/// The registry and the jobs API, as far as the deposit asks either.
struct Server {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state: Arc<Mutex<State>> = Arc::new(Mutex::new(State {
            refusal: 401,
            ..Default::default()
        }));
        let st = state.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let request = line.trim().to_string();
                let (mut basic, mut length) = (None::<String>, 0usize);
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    let Some((k, v)) = h.split_once(':') else {
                        continue;
                    };
                    if k.eq_ignore_ascii_case("authorization") {
                        basic = v.trim().strip_prefix("Basic ").map(str::to_string);
                    } else if k.eq_ignore_ascii_case("content-length") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let body = String::from_utf8_lossy(&body).into_owned();
                let mut s = st.lock().unwrap();
                s.seen.push((request.clone(), body));
                let (status, answer) = if request.starts_with("GET /v2/token?") {
                    let ok = basic
                        .as_deref()
                        .is_some_and(|b| s.admitted.iter().any(|a| b64(a) == b));
                    if ok {
                        ("200 OK".to_string(), r#"{"token":"bearer"}"#.to_string())
                    } else {
                        (format!("{} Refused", s.refusal), "{}".to_string())
                    }
                } else if request.starts_with("GET /api/jobs?") {
                    (
                        "200 OK".to_string(),
                        format!(
                            r#"{{"data":[{}],"total":{}}}"#,
                            s.jobs.join(","),
                            s.jobs.len()
                        ),
                    )
                } else if request.starts_with("PATCH /api/jobs/")
                    || request.starts_with("PUT /api/jobs/")
                {
                    ("200 OK".to_string(), r#"{"ok":true}"#.to_string())
                } else {
                    ("404 Not Found".to_string(), "{}".to_string())
                };
                drop(s);
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = conn.flush();
                let mut sink = [0u8; 1];
                let _ = conn.read(&mut sink);
            }
        });
        Self { port, state }
    }

    fn admit(&self, token: &str) {
        self.state
            .lock()
            .unwrap()
            .admitted
            .push(format!("david:{token}"));
    }

    fn refuse_with(&self, code: u16) {
        self.state.lock().unwrap().refusal = code;
    }

    /// An open rotation of `credential` whose `delivered` step is ready.
    fn open_rotation(&self, credential: &str) {
        self.state.lock().unwrap().jobs.push(format!(
            r#"{{"id":"c4fe0001-aaaa-bbbb-cccc-{credential:0>12.12}","kind":"rotate-a-credential","status":"open","subject":{{"subject_kind":"custom","id":"{credential}"}},"steps":[{{"id":"step-scope","spec_slug":"scope","status":"completed","metadata":{{}}}},{{"id":"step-delivered-{credential}","spec_slug":"delivered","status":"ready","metadata":{{}}}}]}}"#
        ));
    }

    fn seen(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().seen.clone()
    }

    fn writes(&self) -> Vec<(String, String)> {
        self.seen()
            .into_iter()
            .filter(|(l, _)| l.starts_with("PATCH ") || l.starts_with("PUT "))
            .collect()
    }

    fn logins(&self) -> usize {
        self.seen()
            .iter()
            .filter(|(l, _)| l.starts_with("GET /v2/token?"))
            .count()
    }
}

struct Case {
    root: PathBuf,
    secrets: PathBuf,
    dest: PathBuf,
    kubectl: PathBuf,
    server: Server,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("registry-token-deposit-{name}"));
        let secrets = root.join("secrets");
        create_dir(&secrets);
        create_dir(&root.join("tmp"));
        create_dir(&root.join("etc-boss"));
        let kubectl = root.join("kubectl");
        write_exec(&kubectl, boss_testing::kubectl_secret_stub::SECRET_KUBECTL);
        Self {
            dest: root.join("etc-boss").join("registry-push"),
            root,
            secrets,
            kubectl,
            server: Server::start(),
        }
    }

    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.server.port)
    }

    fn secret_dir(&self) -> PathBuf {
        self.secrets.join("boss").join(CREDENTIAL)
    }

    /// The Secret as the cluster converge creates it: there, and empty.
    fn empty_secret(&self) {
        create_dir(&self.secret_dir());
    }

    /// The Secret as the broker's install phase leaves it.
    fn secret(&self, token: &str) {
        create_dir(&self.secret_dir());
        std::fs::write(self.secret_dir().join("token"), token).unwrap();
    }

    fn file(&self) -> PathBuf {
        self.dest.join("config.json")
    }

    /// A login an earlier pass left on the host, in docker's own shape.
    fn held(&self, token: &str) {
        create_dir(&self.dest);
        std::fs::write(
            self.file(),
            format!(
                r#"{{"auths":{{"{}":{{"auth":"{}"}}}}}}"#,
                self.host(),
                b64(&format!("david:{token}"))
            ),
        )
        .unwrap();
    }

    /// The token the host's file holds for this registry, decoded.
    fn held_token(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.file()).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let auth = v["auths"][self.host()]["auth"].as_str()?.to_string();
        let out = without_proxy(&mut Command::new("bash"))
            .args(["-c", "printf '%s' \"$1\" | base64 -d", "sh", &auth])
            .output()
            .ok()?;
        String::from_utf8(out.stdout)
            .ok()?
            .strip_prefix("david:")
            .map(str::to_string)
    }

    fn cmd_for(&self, rule: &str) -> Command {
        let mut c = Command::new("bash");
        without_proxy(&mut c);
        c.arg(repo_root().join(SCRIPT))
            .args(["--rule", &repo_root().join(rule).display().to_string()])
            .args(["--dest", &self.dest.display().to_string()])
            .env("BOSS_DEPOSIT_KUBECTL", &self.kubectl)
            .env("STUB_SECRETS", &self.secrets)
            .env("TMPDIR", self.root.join("tmp"))
            .env("BOSS_FORGE_REGISTRY_HOST", self.host())
            .env("BOSS_JOBS_URL", format!("http://{}", self.host()))
            // A CEILING, not a wait: the stub answers every login at once
            // and no case here stalls it, so a passing run never reaches
            // the bound. It was 3, and gate-run d29f9551 (2026-10-09) went
            // red on two cases reading "did not answer inside 3 seconds"
            // from this stub while the gate ran test binaries side by side
            // under load; the same head passed 13 of 13 on the dev pod.
            // 30 is twice the script's own default (backlog 683d7fe9).
            .env("BOSS_DEPOSIT_VERIFY_BOUND_S", "30")
            .env("BOSS_DEPOSIT_RECORD_WAIT", "1")
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env_remove("BOSS_REGISTRY_DEPOSIT_URL")
            .env_remove("BOSS_MACHINE_TOKEN_DIR")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env_remove("RUNTIME_DIRECTORY");
        c
    }

    fn run_cmd(&self, mut c: Command) -> (i32, String) {
        let out = c.output().expect("run registry-token-deposit.sh");
        let summary = std::fs::read_to_string(self.root.join("summary.json")).unwrap_or_default();
        let text = format!(
            "{}{}\n--- summary\n{summary}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        for t in FIXTURES {
            assert!(!text.contains(t), "a token reached the output:\n{text}");
            assert!(
                !text.contains(&b64(&format!("david:{t}"))),
                "a login reached the output:\n{text}"
            );
        }
        for (line, body) in self.server.seen() {
            for t in FIXTURES {
                assert!(
                    !line.contains(t) && !body.contains(t),
                    "a token rode in a request: {line} {body}"
                );
            }
        }
        // Nothing of the proof is left behind: no curl config, no body.
        let left: Vec<String> = std::fs::read_dir(self.root.join("tmp"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "temp files outlived the run: {left:?}");
        (out.status.code().unwrap_or(-1), text)
    }

    fn run(&self) -> (i32, String) {
        self.run_cmd(self.cmd_for(RULE))
    }

    fn summary(&self, key: &str) -> String {
        let text = std::fs::read_to_string(self.root.join("summary.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        v[key].as_str().unwrap_or_default().to_string()
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

/// Until the first rotation is scoped the Secret is there and empty:
/// nothing is written, nothing is asked of the registry, and it is a
/// not-yet on the packet — never a red on a converge that owes it nothing.
#[test]
fn an_unminted_token_is_a_not_yet_and_writes_nothing() {
    let c = Case::new("unminted");
    c.empty_secret();
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(!c.dest.exists(), "no directory is made for nothing: {text}");
    assert!(
        c.summary("registry_token_secret").starts_with("empty:"),
        "{text}"
    );
    assert_eq!(
        c.summary("registry_token_action"),
        "untouched: nothing to take"
    );
    assert!(
        c.summary("registry_token_effect")
            .starts_with("nothing to present"),
        "{text}"
    );
    assert_eq!(c.summary("registry_token_last_eight"), "none");
    assert_eq!(c.server.logins(), 0, "nothing to prove: {text}");
}

/// A minted value is PROVED by a login the registry accepts and only
/// then written — one login, in docker's own shape, owner-only — and the
/// delivery is recorded on this credential's rotation packet with the
/// last eight, which is what the broker's revoke waits for.
#[test]
fn a_minted_token_is_proved_then_written_root_only_and_its_delivery_recorded() {
    let c = Case::new("minted");
    c.secret(NEW);
    c.server.admit(NEW);
    c.server.open_rotation(CREDENTIAL);
    // Another credential's rotation is not this deposit's to record.
    c.server.open_rotation("forge-host-checkout-token");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");

    assert_eq!(c.held_token().as_deref(), Some(NEW), "{text}");
    assert_eq!(mode(&c.dest), 0o700, "the directory is owner-only");
    assert_eq!(mode(&c.file()), 0o600, "the file is owner-only");
    let names: Vec<String> = std::fs::read_dir(&c.dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["config.json"], "no temp file is left beside it");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(c.file()).unwrap()).unwrap();
    assert_eq!(
        v["auths"].as_object().unwrap().len(),
        1,
        "ONE login, for the estate's registry and no other"
    );

    assert!(
        c.summary("registry_token_action")
            .starts_with("installed from the Secret"),
        "{text}"
    );
    assert!(
        c.summary("registry_token_effect").starts_with("accepted:"),
        "the effect is the registry's own answer: {text}"
    );
    assert_eq!(c.summary("registry_token_last_eight"), last8(NEW));
    assert_eq!(
        c.server.logins(),
        1,
        "proved once: the proof that admitted the write is the pass's effect"
    );

    let writes = c.server.writes();
    assert_eq!(writes.len(), 2, "{writes:?}");
    assert!(
        writes[0].0.starts_with(&format!(
            "PATCH /api/jobs/c4fe0001-aaaa-bbbb-cccc-forge-regist/steps/step-delivered-{CREDENTIAL}/metadata "
        )),
        "the delivered step of THIS credential's packet: {writes:?}"
    );
    let merged: serde_json::Value = serde_json::from_str(&writes[0].1).unwrap();
    assert_eq!(merged["delivered_last_eight"], last8(NEW));
    assert!(
        merged["delivered_to"]
            .as_str()
            .unwrap()
            .ends_with("registry-push/config.json"),
        "{merged}"
    );
    assert!(
        writes[1].0.starts_with(
            "PUT /api/jobs/c4fe0001-aaaa-bbbb-cccc-forge-regist/steps/step-delivered-"
        ) && writes[1].1 == r#"{"status":"completed"}"#,
        "then the status alone: {writes:?}"
    );
    assert!(
        c.summary("registry_token_delivery")
            .starts_with("recorded on c4fe0001"),
        "{text}"
    );
}

/// A value the registry REFUSES is never written: the login the host
/// holds stays, the run says which value was refused by its last eight,
/// and no delivery is recorded — so the broker revokes nothing.
#[test]
fn a_value_the_registry_refuses_is_not_written_and_nothing_is_recorded() {
    for refusal in [401, 403] {
        let c = Case::new(&format!("refused-{refusal}"));
        c.held(OLD);
        c.secret(NEW);
        c.server.admit(OLD);
        c.server.refuse_with(refusal);
        c.server.open_rotation(CREDENTIAL);
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "a refused value is a fault this pass names: {text}");
        assert_eq!(c.held_token().as_deref(), Some(OLD), "{text}");
        assert!(
            c.summary("registry_token_action")
                .starts_with("NOT WRITTEN: the registry refused the Secret's value"),
            "{text}"
        );
        assert!(
            c.summary("registry_token_effect").starts_with("accepted:"),
            "the login still held still works, and the packet says so: {text}"
        );
        assert_eq!(c.summary("registry_token_last_eight"), last8(OLD));
        assert!(c.server.writes().is_empty(), "no delivery: {text}");
    }
}

/// A registry that does not answer proves nothing either way: the value
/// is not written yet, the pass is not red, and the next one asks again.
#[test]
fn a_registry_that_cannot_answer_writes_nothing_and_is_not_a_fault() {
    let c = Case::new("unanswered");
    c.secret(NEW);
    c.server.refuse_with(503);
    c.server.open_rotation(CREDENTIAL);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(!c.file().exists(), "{text}");
    assert!(
        c.summary("registry_token_action")
            .starts_with("not written yet:"),
        "{text}"
    );
    assert!(c.server.writes().is_empty(), "no delivery: {text}");
}

/// A pass that finds the Secret's value already held installs nothing,
/// proves the held login again, and still records a delivery that an
/// earlier pass could not (the jobs API was dark, the packet not ready).
#[test]
fn a_held_value_is_proved_again_and_a_pending_delivery_is_still_recorded() {
    let c = Case::new("held");
    c.held(NEW);
    // mode-bits-ok: a directory loosened so the pass can be seen to hold it; nothing execs it
    std::fs::set_permissions(&c.dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(c.file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    c.secret(NEW);
    c.server.admit(NEW);
    c.server.open_rotation(CREDENTIAL);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("registry_token_action").starts_with("unchanged:"),
        "{text}"
    );
    assert_eq!(mode(&c.dest), 0o700, "the modes are held on every pass");
    assert_eq!(mode(&c.file()), 0o600);
    assert!(c.summary("registry_token_effect").starts_with("accepted:"));
    assert_eq!(c.server.writes().len(), 2, "{text}");
}

/// The login the host holds is proved on EVERY pass, so a token revoked
/// under it is a red on the packet the same tick — not a failed push
/// found later by the converge that builds the image.
#[test]
fn a_held_login_the_registry_no_longer_accepts_is_named() {
    let c = Case::new("dead");
    c.held(OLD);
    c.secret(OLD);
    c.server.open_rotation(CREDENTIAL);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("registry_token_effect")
            .starts_with("NOT ACCEPTED:"),
        "{text}"
    );
    assert_eq!(c.held_token().as_deref(), Some(OLD), "nothing is removed");
    assert!(
        c.server.writes().is_empty(),
        "a login that does not work is not a delivery: {text}"
    );
    assert!(
        c.summary("registry_token_delivery")
            .starts_with("not recorded: the installed value did not authenticate"),
        "{text}"
    );
}

/// Two open rotations of this credential are refused, never guessed
/// between — the shared record's rule, held for this caller too.
#[test]
fn two_rotations_awaiting_delivery_are_not_guessed_between() {
    let c = Case::new("two");
    c.secret(NEW);
    c.server.admit(NEW);
    c.server.open_rotation(CREDENTIAL);
    c.server.open_rotation(CREDENTIAL);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(
        c.held_token().as_deref(),
        Some(NEW),
        "the value is still installed"
    );
    assert!(
        c.summary("registry_token_delivery")
            .starts_with("refused: 2 open rotations"),
        "{text}"
    );
    assert!(c.server.writes().is_empty());
}

/// A Secret that held a token and now holds none is not a state a
/// rotation passes through: the host keeps the login that works.
#[test]
fn an_emptied_secret_never_removes_a_held_login() {
    let c = Case::new("emptied");
    c.held(OLD);
    c.empty_secret();
    c.server.admit(OLD);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.held_token().as_deref(), Some(OLD));
    assert!(
        c.summary("registry_token_action")
            .contains("kept, not removed"),
        "{text}"
    );
}

/// A Secret value that is not one token word is refused before it can
/// become half of a login — nothing is asked of the registry with it.
#[test]
fn a_secret_that_is_not_one_token_word_is_refused() {
    for bad in ["two words", "tok:en", "line\nbreak"] {
        let c = Case::new(&format!("malformed-{}", bad.len()));
        c.secret(bad);
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "{bad:?}: {text}");
        assert!(!c.file().exists(), "{bad:?}");
        assert!(
            c.summary("registry_token_secret")
                .starts_with("unreadable: key token"),
            "{bad:?}: {text}"
        );
        assert_eq!(c.server.logins(), 0, "{bad:?}");
    }
}

/// A link planted at the directory or at the file is refused before the
/// Secret is read: root never writes a credential through a path
/// somebody else chose.
#[test]
fn a_planted_link_is_refused_before_anything_is_read() {
    let c = Case::new("link-dir");
    c.secret(NEW);
    c.server.admit(NEW);
    let elsewhere = c.root.join("elsewhere");
    create_dir(&elsewhere);
    std::os::unix::fs::symlink(&elsewhere, &c.dest).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("registry_token_action").starts_with("REFUSED:"),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "{text}");
    assert_eq!(c.server.logins(), 0);

    let c = Case::new("link-file");
    c.secret(NEW);
    c.server.admit(NEW);
    create_dir(&c.dest);
    let target = c.root.join("target.json");
    std::fs::write(&target, "{}").unwrap();
    std::os::unix::fs::symlink(&target, c.file()).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}", "{text}");
    assert_eq!(c.server.logins(), 0);
}

/// Only the registry kind's rule is deposited: the checkout token's rule
/// (a repository token, another Secret) and an invocation with no
/// destination are refused before anything is read.
#[test]
fn a_rule_of_another_kind_or_a_bad_invocation_is_refused() {
    let c = Case::new("wrong-rule");
    c.secret(NEW);
    let out = c
        .cmd_for("infra/dispatcher/rules/broker-rotates-the-forge-host-checkout-token.toml")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(78), "{err}");
    assert!(err.contains("is not a registry push token's rule"), "{err}");
    assert!(!c.dest.exists());

    let out = without_proxy(&mut Command::new("bash"))
        .arg(repo_root().join(SCRIPT))
        .args(["--rule", &repo_root().join(RULE).display().to_string()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
}

/// A shell started with tracing on prints nothing of the token: the
/// script turns xtrace off before it reads anything (`run_cmd` refuses
/// any fixture, or its login, in the output).
#[test]
fn an_inherited_xtrace_prints_no_value() {
    let c = Case::new("xtrace");
    c.secret(NEW);
    c.server.admit(NEW);
    let mut cmd = c.cmd_for(RULE);
    cmd.env("SHELLOPTS", "xtrace");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.held_token().as_deref(), Some(NEW));
}

/// The converge runs the deposit before its fetch, under a bound, with
/// its exit recorded and never carried — and judges the destination as a
/// place this run would touch the machine. And the delivery twin is the
/// same kind of rule, so the deposit can be pointed at either file.
#[test]
fn the_forge_converge_deposits_it_bounded_and_never_carries_its_exit() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
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
    let read = default_of(&script, "BOSS_DEPOSIT_READ_BOUND_S");
    let verify = default_of(&script, "BOSS_DEPOSIT_VERIFY_BOUND_S");
    let record = default_of(&script, "BOSS_DEPOSIT_RECORD_WAIT");
    let whole = default_of(&converge, "BOSS_FORGE_REGISTRY_DEPOSIT_BOUND_S");
    // Two reads of the Secret (the pass's, and the record's re-read),
    // each with timeout's five-second kill; two logins; the record's
    // three calls to the jobs API.
    let worst = 2 * (read + 5) + 2 * verify + 3 * record;
    assert!(
        whole > worst,
        "forge-converge.sh bounds the deposit at {whole}s, inside its own worst case of {worst}s"
    );
    let all = whole
        + default_of(&converge, "BOSS_FORGE_DEPOSIT_BOUND_S")
        + default_of(&converge, "BOSS_FORGE_READER_DEPOSIT_BOUND_S");
    assert!(
        all <= 480,
        "the three root-only deposits may take {all}s of a ten-minute unit together"
    );

    let live: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let at = |needle: &str| {
        live.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("forge-converge.sh has no line with {needle}"))
    };
    let run = at(
        "timeout -k 5 \"$FORGE_REGISTRY_DEPOSIT_BOUND_S\" \"$INFRA/forge/registry-token-deposit.sh\"",
    );
    assert!(
        live[run + 1].contains(&format!(
            "--rule \"$INFRA/{}\"",
            RULE.trim_start_matches("infra/")
        )),
        "the deposit reads the tree's own declaration: {}",
        live[run + 1]
    );
    assert!(
        live[run + 2].contains("--dest \"${BOSS_REGISTRY_PUSH_DIR:-/etc/boss/registry-push}\"")
            && live[run + 2]
                .trim_end()
                .ends_with("|| registry_token_rc=$?"),
        "its exit is caught, not carried: {}",
        live[run + 2]
    );
    assert!(
        !live
            .iter()
            .any(|l| l.contains("exit \"$registry_token_rc\"")),
        "the converge never exits on this deposit's status"
    );
    assert!(
        run < at("fetch -q forgejo main"),
        "it runs before the fetch, the first step that can end the run"
    );
    assert!(
        live.contains(&"host_seam BOSS_REGISTRY_PUSH_DIR /etc/boss/registry-push"),
        "the destination is a seam the host check judges"
    );
    assert!(
        script.contains("host_path --dest \"$DEST\" /etc/boss/registry-push"),
        "and the script judges it again for a hand run"
    );

    let twin = std::fs::read_to_string(repo_root().join(DELIVERED_RULE)).unwrap();
    assert!(twin.contains("handler = \"credential.rotate.forgejo-registry\""));
}
