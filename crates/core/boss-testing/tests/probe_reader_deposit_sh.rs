//! `infra/forge/probe-reader-deposit.sh` — the forge host takes the
//! probe-reader credential's `current` slot, and nothing else, from the
//! broker's Secret into the root-only file `boss prove` opens its reader
//! door with, on every forge-converge tick, and only after every gated
//! port has named that value `reader.current` (design b35c22b4; backlog
//! d26515c5, unit 7; adversarial review f09db7f0, F4).
//!
//! HOW THIS IS MEASURED. The script runs for real against the tree's own
//! reader rule. The shared stub `kubectl` answers the Secret from files in
//! scratch, wrapped so every argv it was handed is on record; one tiny
//! HTTP server per service port on 127.0.0.1 answers
//! `GET /api/machine-gate/accepts` the way a gate does — the NAME it
//! gives the presented value — and records every header name it was
//! sent; and `curl` is the real one behind a wrapper that records its
//! argv. Fixture values are fake; every case also checks that none of
//! them reaches the output, the run summary, a request line or an argv.
//!
//! WHAT NO TEST HERE HOLDS. Nothing here ran on the forge, read a real
//! Secret, or wrote under /etc/boss. Ownership by root is held only where
//! the suite itself runs as root (`another_account_cannot_read_the_file`
//! says NOT RUN elsewhere). The exclusive create of the temp file is held
//! by the script's source (`the_write_is_exclusive_and_renamed`), because
//! its name is random and cannot be planted from a test — the control
//! that can be run is the one on the directory: a parent anyone else can
//! write is refused before a byte is written.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const SCRIPT: &str = "infra/forge/probe-reader-deposit.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-probe-reader.toml";
const ESTATE_RULE: &str = "infra/dispatcher/rules/broker-rotates-the-machine-token.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";
const PORTS: &str = "infra/forge/sor-ports.env";
const DOOR: &str = "crates/orchestrators/boss-cli/src/probe_reader.rs";

// 43-character base64url fixtures, fake — the shape the broker mints.
const OLDER: &str = "fakePREVfakePREVfakePREVfakePREVfakePREV701";
const LIVE: &str = "fakeCURRfakeCURRfakeCURRfakeCURRfakeCURR702";
const STAGED: &str = "fakeNEXTfakeNEXTfakeNEXTfakeNEXTfakeNEXT703";
const FIXTURES: [&str; 3] = [OLDER, LIVE, STAGED];

/// The shared stub, with every argv it was handed appended to
/// `$STUB_ARGV` first — what the deposit ASKED the API server for.
const LOGGING_KUBECTL: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_ARGV"
exec "$STUB_REAL" "$@"
"#;

/// The real curl, with its argv on record.
const LOGGING_CURL: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CURL_ARGV"
exec "$REAL_CURL" "$@"
"#;

/// How one port answers the accepts read.
#[derive(Clone)]
enum Answer {
    /// A gate: the name it gives each value it knows, `none` otherwise.
    Gate(Vec<(String, String)>),
    /// An HTTP status and nothing a gate would say.
    Status(u16),
    /// 200 with a body that is not the gate's JSON.
    Garbage,
    /// The connection is taken and never answered.
    Silent,
}

#[derive(Default)]
struct Seen {
    /// Request line, the lowercased names of every header sent, and the
    /// machine-token values presented (compared, never printed).
    requests: Vec<(String, Vec<String>, Vec<String>)>,
}

struct Port {
    port: u16,
    seen: Arc<Mutex<Seen>>,
    answer: Arc<Mutex<Answer>>,
}

impl Port {
    fn start(answer: Answer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Seen>> = Default::default();
        let answer = Arc::new(Mutex::new(answer));
        let (s, a) = (seen.clone(), answer.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let request = line.trim().to_string();
                let (mut names, mut tokens) = (Vec::new(), Vec::new());
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        let k = k.trim().to_ascii_lowercase();
                        if k == "x-boss-machine-token" {
                            tokens.push(v.trim().to_string());
                        }
                        names.push(k);
                    }
                }
                s.lock()
                    .unwrap()
                    .requests
                    .push((request.clone(), names, tokens.clone()));
                let how = a.lock().unwrap().clone();
                let (status, body) = match how {
                    Answer::Silent => {
                        // Held past any bound under test, then dropped, so
                        // a script that lost its bound fails instead of
                        // hanging the suite.
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(45));
                            drop(conn);
                        });
                        continue;
                    }
                    Answer::Status(code) => (format!("{code} Refused"), "{}".to_string()),
                    Answer::Garbage => ("200 OK".to_string(), "<html>not a gate</html>".into()),
                    Answer::Gate(slots) => {
                        if request.starts_with("GET /api/machine-gate/accepts ") {
                            let matched = match tokens.as_slice() {
                                [one] => slots
                                    .iter()
                                    .find(|(v, _)| v == one)
                                    .map(|(_, n)| n.clone())
                                    .unwrap_or_else(|| "none".into()),
                                _ => "none".into(),
                            };
                            (
                                "200 OK".to_string(),
                                format!(
                                    r#"{{"service":"fixture","mode":"report","matched":"{matched}","degraded":false}}"#
                                ),
                            )
                        } else {
                            ("404 Not Found".to_string(), "{}".to_string())
                        }
                    }
                };
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.flush();
                let mut sink = [0u8; 1];
                let _ = conn.read(&mut sink);
            }
        });
        Self { port, seen, answer }
    }

    fn answers(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }

    fn requests(&self) -> Vec<(String, Vec<String>, Vec<String>)> {
        self.seen.lock().unwrap().requests.clone()
    }
}

/// A gate that names `value` as `name`.
fn names(value: &str, name: &str) -> Answer {
    Answer::Gate(vec![(value.to_string(), name.to_string())])
}

struct Case {
    root: PathBuf,
    secrets: PathBuf,
    dest: PathBuf,
    /// jobs, events, people — three gated ports; jobs is the base.
    gates: Vec<Port>,
    /// The edge, which mounts no gate and must never be sent the value.
    gateway: Port,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("probe-reader-deposit-{name}"));
        let secrets = root.join("secrets");
        for d in ["secrets", "tmp", "etc-boss", "bin"] {
            create_dir(&root.join(d));
        }
        // mode-bits-ok: a directory, the deposit's parent, as /etc/boss is — its owner's alone to write
        let owners = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(root.join("etc-boss"), owners).unwrap();
        write_exec(
            &root.join("kubectl-real"),
            boss_testing::kubectl_secret_stub::SECRET_KUBECTL,
        );
        write_exec(&root.join("kubectl"), LOGGING_KUBECTL);
        write_exec(&root.join("bin/curl"), LOGGING_CURL);
        write_file(&root.join("kubectl.argv"), "");
        write_file(&root.join("curl.argv"), "");
        let gates: Vec<Port> = (0..3)
            .map(|_| Port::start(names(LIVE, "reader.current")))
            .collect();
        let gateway = Port::start(Answer::Status(404));
        let case = Self {
            dest: root.join("etc-boss").join("probe-reader.credential"),
            root,
            secrets,
            gates,
            gateway,
        };
        case.ports_file(&format!(
            "# the fixture's port table\n\njobs={}\nevents={}\ngateway={}\npeople={}\n",
            case.gates[0].port, case.gates[1].port, case.gateway.port, case.gates[2].port
        ));
        case
    }

    fn ports_file(&self, text: &str) {
        let table = self.root.join("sor-ports.env");
        write_file(&table, text);
        // mode-bits-ok: a data file, its owner's alone to write whatever this process's umask
        let own = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(&table, own).unwrap();
    }

    fn secret_dir(&self) -> PathBuf {
        self.secrets.join("boss").join("boss-probe-reader")
    }

    /// The Secret as the broker leaves it: the object exists and holds
    /// each slot given.
    fn secret(&self, slots: &[(&str, &str)]) {
        create_dir(&self.secret_dir());
        for (k, v) in slots {
            std::fs::write(self.secret_dir().join(k), v).unwrap();
        }
    }

    /// The credential file an earlier pass left on the host.
    fn held(&self, value: &str) {
        std::fs::write(&self.dest, value).unwrap();
        // mode-bits-ok: a data file, as the deposit leaves it
        std::fs::set_permissions(&self.dest, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn file(&self) -> Option<String> {
        std::fs::read_to_string(&self.dest).ok()
    }

    fn every_gate(&self, answer: Answer) {
        for g in &self.gates {
            g.answers(answer.clone());
        }
    }

    fn asked(&self) -> usize {
        self.gates.iter().map(|g| g.requests().len()).sum()
    }

    fn real_curl() -> PathBuf {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|d| Path::new(d).join("curl"))
            .find(|p| p.is_file())
            .expect("curl is on PATH")
    }

    fn cmd(&self) -> Command {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(["--rule", &repo_root().join(RULE).display().to_string()])
            .args([
                "--ports",
                &self.root.join("sor-ports.env").display().to_string(),
            ])
            .args(["--dest", &self.dest.display().to_string()])
            .env("PATH", path)
            .env("REAL_CURL", Self::real_curl())
            .env("CURL_ARGV", self.root.join("curl.argv"))
            .env("BOSS_DEPOSIT_KUBECTL", self.root.join("kubectl"))
            .env("STUB_REAL", self.root.join("kubectl-real"))
            .env("STUB_ARGV", self.root.join("kubectl.argv"))
            .env("STUB_SECRETS", &self.secrets)
            .env("TMPDIR", self.root.join("tmp"))
            .env(
                "BOSS_JOBS_URL",
                format!("http://127.0.0.1:{}", self.gates[0].port),
            )
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env_remove("BOSS_MACHINE_TOKEN_DIR")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env_remove("RUNTIME_DIRECTORY");
        c
    }

    fn argv_of(&self, file: &str) -> String {
        std::fs::read_to_string(self.root.join(file)).unwrap_or_default()
    }

    fn run_cmd(&self, mut c: Command) -> (i32, String) {
        let out = c.output().expect("run probe-reader-deposit.sh");
        let summary = std::fs::read_to_string(self.root.join("summary.json")).unwrap_or_default();
        let text = format!(
            "{}{}\n--- summary\n{summary}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let argv = format!(
            "{}{}",
            self.argv_of("curl.argv"),
            self.argv_of("kubectl.argv")
        );
        for t in FIXTURES {
            assert!(!text.contains(t), "a value reached the output:\n{text}");
            // Not even an end of one: this deposit names lengths only.
            assert!(
                !text.contains(&t[t.len() - 8..]) && !text.contains(&t[..8]),
                "eight characters of a value reached the output:\n{text}"
            );
            assert!(!argv.contains(t), "a value rode in an argv");
        }
        for port in self.gates.iter().chain([&self.gateway]) {
            for (line, _, _) in port.requests() {
                for t in FIXTURES {
                    assert!(!line.contains(t), "a value rode in a request line: {line}");
                }
            }
        }
        // Nothing of the run outlives it in the temp directory: the 0600
        // header file's directory is removed on exit.
        let left: Vec<_> = std::fs::read_dir(self.root.join("tmp"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            left.is_empty(),
            "left in the temp directory: {left:?}\n{text}"
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn run(&self) -> (i32, String) {
        self.run_cmd(self.cmd())
    }

    fn summary(&self, key: &str) -> String {
        let text = std::fs::read_to_string(self.root.join("summary.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        v[key].as_str().unwrap_or_default().to_string()
    }

    /// The names beside the credential file, the file included.
    fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root.join("etc-boss"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

/// THE DEPOSIT: with a `current` every gated port names `reader.current`,
/// the file holds exactly that value, owner-only, with nothing left
/// beside it; the effect line says how many gates were asked; each gated
/// port was asked once with the value ALONE (one token header, no
/// asserted identity); the edge was never sent it; and the API server
/// was never asked for another slot.
#[test]
fn current_is_deposited_when_every_gate_names_it_reader_current() {
    let c = Case::new("deposit");
    c.secret(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.file().as_deref(), Some(LIVE));
    assert_eq!(mode(&c.dest), 0o600, "{text}");
    assert_eq!(c.beside(), ["probe-reader.credential"], "{text}");
    assert!(
        c.summary("probe_reader_effect")
            .starts_with("reader.current at 3 of 3 gates"),
        "the effect is the gates' own word, counted: {text}"
    );
    assert!(
        c.summary("probe_reader_action").starts_with("wrote")
            && c.summary("probe_reader_action").contains("43 bytes"),
        "the write is named by its length: {text}"
    );
    assert!(
        c.summary("probe_reader_secret")
            .contains("current 43 bytes"),
        "{text}"
    );
    for g in &c.gates {
        let requests = g.requests();
        assert_eq!(requests.len(), 1, "each gate is asked once: {requests:?}");
        let (line, names, tokens) = &requests[0];
        assert!(line.starts_with("GET /api/machine-gate/accepts "), "{line}");
        assert_eq!(tokens.len(), 1, "one token value, alone");
        assert!(
            tokens[0] == LIVE,
            "the value asked about is the Secret's current"
        );
        assert!(
            !names
                .iter()
                .any(|n| n == "x-boss-user" || n == "authorization"),
            "the value is presented alone, with no asserted identity: {names:?}"
        );
    }
    assert!(
        c.gateway.requests().is_empty(),
        "the edge mounts no gate and is never sent the credential"
    );
    let asked = c.argv_of("kubectl.argv");
    assert!(asked.contains("{.data.current}"), "{asked}");
    assert!(
        !asked.contains("next") && !asked.contains("previous"),
        "the API server is never asked for another slot: {asked}"
    );
    assert!(
        c.argv_of("curl.argv").contains("-H @"),
        "the header reaches curl as a file: {}",
        c.argv_of("curl.argv")
    );

    // A second pass changes nothing and says so; a looser mode an earlier
    // hand left is tightened.
    // mode-bits-ok: a data file, loosened so the pass must tighten it
    std::fs::set_permissions(&c.dest, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("probe_reader_action").starts_with("unchanged"),
        "{text}"
    );
    assert_eq!(mode(&c.dest), 0o600, "{text}");
    assert_eq!(c.file().as_deref(), Some(LIVE));
}

/// A PROMOTION IS FOLLOWED: the file an earlier pass left is replaced by
/// the Secret's new `current` once every gate names that `reader.current`.
#[test]
fn a_promoted_current_replaces_the_held_file() {
    let c = Case::new("promotion");
    c.held(OLDER);
    c.secret(&[("current", LIVE), ("previous", OLDER)]);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.file().as_deref(), Some(LIVE));
    assert_eq!(c.beside(), ["probe-reader.credential"]);
}

/// NEVER NEXT, NEVER PREVIOUS, NEVER A FALLBACK (review f09db7f0, F4).
/// With no `current`, a `next` and a `previous` that every gate would
/// name `reader.current` are still not read, not asked about and not
/// deposited: it is a not-yet, exit 0, and no gate hears from this host.
/// A file already held is kept exactly, and that is a fault said aloud —
/// a Secret that had a current and has none is no state a rotation
/// passes through.
#[test]
fn an_empty_current_deposits_nothing_and_never_falls_back() {
    for (leg, slots) in [
        ("empty", vec![]),
        ("blank", vec![("current", "  \n")]),
        ("staged", vec![("next", STAGED), ("previous", OLDER)]),
    ] {
        let c = Case::new(&format!("empty-{leg}"));
        c.secret(&slots);
        c.every_gate(Answer::Gate(vec![
            (STAGED.into(), "reader.current".into()),
            (OLDER.into(), "reader.current".into()),
        ]));
        let (rc, text) = c.run();
        assert_eq!(
            rc, 0,
            "[{leg}] nothing to deposit yet is not a failure: {text}"
        );
        assert_eq!(c.file(), None, "[{leg}] {text}");
        assert!(c.beside().is_empty(), "[{leg}] nothing was created: {text}");
        assert_eq!(c.asked(), 0, "[{leg}] no gate is asked about nothing");
        assert!(
            c.summary("probe_reader_effect").starts_with("not yet:"),
            "[{leg}] a not-yet, on the packet: {text}"
        );
        assert!(
            c.summary("probe_reader_action").starts_with("untouched"),
            "[{leg}] {text}"
        );
        assert!(
            c.summary("probe_reader_secret").starts_with("empty"),
            "[{leg}] {text}"
        );

        // The same Secret with a file already held: kept, and loud.
        c.held(LIVE);
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{leg}] {text}");
        assert_eq!(c.file().as_deref(), Some(LIVE), "[{leg}] removed nothing");
        assert_eq!(c.asked(), 0, "[{leg}]");
        assert!(
            c.summary("probe_reader_action")
                .contains("kept, not removed"),
            "[{leg}] {text}"
        );
        assert!(
            !c.summary("probe_reader_effect")
                .starts_with("reader.current"),
            "[{leg}] an unverified file is never recorded as the deposit's effect: {text}"
        );
        says_the_cost_and_the_remedy(&c.summary("probe_reader_effect"), leg);
    }
}

/// A STALE FILE IS KEPT, AND THE PACKET SAYS WHAT THAT COSTS (review
/// 4d39f4dc, N7): once the value a kept file holds has left every reader
/// slot, the door refuses every probe on the host — and what ends it is
/// a fresh rotation, or root removing the file.
fn says_the_cost_and_the_remedy(effect: &str, leg: &str) {
    assert!(
        effect.contains("every probe on this host is refused at the door"),
        "[{leg}] the line names the cost of the kept file: {effect}"
    );
    assert!(
        effect.contains("a fresh rotation of boss-probe-reader"),
        "[{leg}] the line names the remedy: {effect}"
    );
}

/// EVERY GATE, OR NOTHING. One gated port that does not say
/// `reader.current` — it names the value nothing, it is a refresh behind
/// or ahead, it is dark, it refuses, it is not a gate — and nothing is
/// written: no file where there was none, and the held file untouched
/// where there was one. Each reason is its own word on the packet.
#[test]
fn one_gate_that_does_not_say_reader_current_stops_the_deposit() {
    let legs: Vec<(&str, Answer, i32, &str)> = vec![
        ("none", names(OLDER, "reader.current"), 1, "NOT ACCEPTED"),
        ("lag-next", names(LIVE, "reader.next"), 0, "not yet:"),
        (
            "lag-previous",
            names(LIVE, "reader.previous"),
            0,
            "not yet:",
        ),
        ("unauthorized", Answer::Status(401), 1, "UNVERIFIED"),
        ("error", Answer::Status(500), 1, "UNVERIFIED"),
        ("garbage", Answer::Garbage, 1, "UNVERIFIED"),
        (
            "unknown-word",
            names(LIVE, "reader.currentish"),
            1,
            "UNVERIFIED",
        ),
        // The word, and then a line break INSIDE the JSON string: a
        // command substitution drops a trailing newline, so an answer
        // read that way was `reader.current` (review 4d39f4dc, N8).
        (
            "trailing-newline",
            names(LIVE, "reader.current\\n"),
            1,
            "UNVERIFIED",
        ),
        (
            "two-trailing-newlines",
            names(LIVE, "reader.current\\n\\n"),
            1,
            "UNVERIFIED",
        ),
        (
            "bare-with-newline",
            names(LIVE, "current\\n"),
            1,
            "UNVERIFIED",
        ),
        (
            "leading-space",
            names(LIVE, " reader.current"),
            1,
            "UNVERIFIED",
        ),
    ];
    for (leg, answer, want_rc, word) in legs {
        for which in 0..3 {
            let c = Case::new(&format!("stop-{leg}-{which}"));
            c.secret(&[("current", LIVE)]);
            c.gates[which].answers(answer.clone());
            let (rc, text) = c.run();
            assert_eq!(rc, want_rc, "[{leg} at gate {which}] {text}");
            assert_eq!(c.file(), None, "[{leg} at gate {which}] {text}");
            assert!(c.beside().is_empty(), "[{leg} at gate {which}] {text}");
            let effect = c.summary("probe_reader_effect");
            assert!(effect.starts_with(word), "[{leg} at gate {which}] {text}");
            assert!(
                effect.contains(&format!(":{}", c.gates[which].port)),
                "[{leg} at gate {which}] the gate is named by its port: {text}"
            );
            assert!(
                c.summary("probe_reader_action").starts_with("untouched"),
                "[{leg} at gate {which}] {text}"
            );

            // And a file already held is left exactly as it was.
            c.held(OLDER);
            let (rc, text) = c.run();
            assert_eq!(rc, want_rc, "[{leg} at gate {which}, held] {text}");
            assert_eq!(c.file().as_deref(), Some(OLDER), "[{leg} at gate {which}]");
            assert_eq!(c.beside(), ["probe-reader.credential"]);
        }
    }

    // A port nothing listens on.
    let c = Case::new("stop-dark");
    c.secret(&[("current", LIVE)]);
    // Held for the whole run, never bound and released (backlog ec131700).
    let held = boss_testing::dark_port();
    let dark = held.port;
    c.ports_file(&format!(
        "jobs={}\nevents={}\npeople={dark}\n",
        c.gates[0].port, c.gates[1].port
    ));
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "a dark port is the next tick's to ask again: {text}");
    assert_eq!(c.file(), None, "{text}");
    assert!(
        c.summary("probe_reader_effect").starts_with("UNVERIFIED"),
        "{text}"
    );
}

/// A BARE SLOT NAME IS A COLLISION, REFUSED OUTRIGHT (review f09db7f0,
/// F4; review 177b4976, N2). A gate that names this credential's value
/// `current`, `next` or `previous` — the estate token's names — is saying
/// the reader value EQUALS an estate machine token, which would hand
/// probe text every method under any identity. Never deposited, whatever
/// every other gate says; a fault, in capitals, on stderr and the packet.
#[test]
fn a_bare_slot_name_is_a_collision_and_is_never_deposited() {
    for bare in ["current", "next", "previous"] {
        for which in 0..3 {
            let c = Case::new(&format!("collision-{bare}-{which}"));
            c.secret(&[("current", LIVE)]);
            c.gates[which].answers(names(LIVE, bare));
            let out = c.cmd().output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            assert_eq!(out.status.code(), Some(1), "[{bare} at {which}] {stderr}");
            assert_eq!(c.file(), None, "[{bare} at {which}]");
            assert!(c.beside().is_empty(), "[{bare} at {which}]");
            let effect = c.summary("probe_reader_effect");
            assert!(
                effect.starts_with("REFUSED: COLLISION"),
                "[{bare} at {which}] {effect}"
            );
            assert!(
                effect.contains(&format!(":{} `{bare}`", c.gates[which].port)),
                "[{bare} at {which}] the gate and its word are named: {effect}"
            );
            assert!(
                stderr.contains("COLLISION"),
                "[{bare} at {which}] loud on stderr: {stderr}"
            );

            // With a different, older file held: kept exactly.
            c.held(OLDER);
            let (rc, text) = c.run();
            assert_eq!(rc, 1, "{text}");
            assert_eq!(c.file().as_deref(), Some(OLDER));
            says_the_cost_and_the_remedy(&c.summary("probe_reader_effect"), bare);
        }
    }
    // Every gate naming it a bare slot at once, and a collision beside a
    // gate that is merely behind: the collision is the word recorded.
    let c = Case::new("collision-all");
    c.secret(&[("current", LIVE)]);
    c.every_gate(names(LIVE, "current"));
    c.gates[0].answers(names(LIVE, "reader.next"));
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.file(), None);
    assert!(
        c.summary("probe_reader_effect")
            .starts_with("REFUSED: COLLISION"),
        "{text}"
    );
}

/// THE FILE IS NEVER WRITTEN THROUGH A NAME SOMEONE ELSE COULD PLANT.
/// A symlink where the file belongs, a directory there, a parent that
/// another account may write, a parent that is itself a link or absent:
/// each is refused before the Secret is read, names its reason, and
/// writes nothing — and what a planted link points at is untouched.
#[test]
fn a_destination_another_account_could_have_planted_is_refused() {
    // A symlink at the path, to a file that must not change.
    let c = Case::new("dest-symlink");
    c.secret(&[("current", LIVE)]);
    let victim = c.root.join("victim");
    write_file(&victim, "untouched");
    std::os::unix::fs::symlink(&victim, &c.dest).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert!(
        std::fs::symlink_metadata(&c.dest).unwrap().is_symlink(),
        "left as found"
    );
    assert!(
        c.summary("probe_reader_action").starts_with("REFUSED"),
        "{text}"
    );
    assert!(
        c.argv_of("kubectl.argv").is_empty(),
        "nothing was read: {text}"
    );
    assert_eq!(c.asked(), 0);

    // A directory at the path.
    let c = Case::new("dest-directory");
    c.secret(&[("current", LIVE)]);
    create_dir(&c.dest);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(c.dest.is_dir());
    assert!(c.argv_of("kubectl.argv").is_empty(), "{text}");

    // A parent that group or other may write.
    for bits in [0o775, 0o757, 0o777, 0o1777] {
        let c = Case::new(&format!("parent-{bits:o}"));
        c.secret(&[("current", LIVE)]);
        // mode-bits-ok: the parent directory, opened to others so the deposit must refuse it
        std::fs::set_permissions(
            c.root.join("etc-boss"),
            std::fs::Permissions::from_mode(bits),
        )
        .unwrap();
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{bits:o}] {text}");
        assert_eq!(c.file(), None, "[{bits:o}] {text}");
        assert!(c.beside().is_empty(), "[{bits:o}]");
        assert!(
            c.summary("probe_reader_action").starts_with("REFUSED")
                && c.summary("probe_reader_action")
                    .contains("not this account's alone"),
            "[{bits:o}] {text}"
        );
        assert!(c.argv_of("kubectl.argv").is_empty(), "[{bits:o}] {text}");
        assert_eq!(c.asked(), 0, "[{bits:o}]");
    }

    // A parent that is a link to a directory, and one that is not there.
    let c = Case::new("parent-link");
    c.secret(&[("current", LIVE)]);
    let linked = c.root.join("linked");
    std::os::unix::fs::symlink(c.root.join("etc-boss"), &linked).unwrap();
    let mut cmd = c.cmd();
    cmd.args([
        "--dest",
        &linked.join("probe-reader.credential").display().to_string(),
    ]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(c.beside().is_empty(), "{text}");
    let mut cmd = c.cmd();
    cmd.args([
        "--dest",
        &c.root
            .join("absent/probe-reader.credential")
            .display()
            .to_string(),
    ]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        !c.root.join("absent").exists(),
        "the parent is never made: {text}"
    );
}

/// A TEMP FILE A KILLED PASS LEFT — or a link planted under its name —
/// is removed by name, never written through, and the deposit goes on.
#[test]
fn a_leftover_temp_name_is_removed_never_written_through() {
    let c = Case::new("leftover");
    c.secret(&[("current", LIVE)]);
    let victim = c.root.join("victim");
    write_file(&victim, "untouched");
    let beside = c.root.join("etc-boss");
    std::os::unix::fs::symlink(&victim, beside.join(".probe-reader.credential.tmp.1.2")).unwrap();
    write_file(
        &beside.join(".probe-reader.credential.tmp.3.4"),
        "half a write",
    );
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert_eq!(c.file().as_deref(), Some(LIVE));
    assert_eq!(c.beside(), ["probe-reader.credential"], "{text}");
}

/// A LEFTOVER IS SWEPT ON EVERY PASS, NOT ONLY ONE THAT WRITES (review
/// 4d39f4dc, F4). A pass KILLed between its create and its rename leaves
/// a 0600 temp file holding a value; while the file stayed current the
/// next passes said `unchanged` and never looked. Now a pass that finds
/// the file current, one that finds the Secret empty and one that a gate
/// stops each remove it — by name, the planted link's target untouched.
#[test]
fn a_leftover_is_swept_on_every_pass_that_accepts_the_destination() {
    let plant = |c: &Case| -> PathBuf {
        let victim = c.root.join("victim");
        write_file(&victim, "untouched");
        let beside = c.root.join("etc-boss");
        write_file(
            &beside.join(".probe-reader.credential.tmp.77.4242"),
            "a value a killed pass left",
        );
        std::os::unix::fs::symlink(&victim, beside.join(".probe-reader.credential.tmp.78.1"))
            .unwrap();
        victim
    };
    // The file is already current: `unchanged`, and the leftovers go.
    let c = Case::new("sweep-unchanged");
    c.secret(&[("current", LIVE)]);
    c.held(LIVE);
    let victim = plant(&c);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("probe_reader_action").starts_with("unchanged"),
        "{text}"
    );
    assert_eq!(c.beside(), ["probe-reader.credential"], "{text}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert!(
        c.summary("probe_reader_action")
            .contains("2 leftover temp file(s) of an earlier pass removed"),
        "what was swept is said, by count: {text}"
    );

    // Nothing to deposit yet.
    let c = Case::new("sweep-empty");
    c.secret(&[]);
    let victim = plant(&c);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(c.beside().is_empty(), "{text}");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");

    // A gate says no: nothing written, the held file kept, leftovers gone.
    let c = Case::new("sweep-refused");
    c.secret(&[("current", LIVE)]);
    c.held(OLDER);
    c.gates[1].answers(names(LIVE, "current"));
    let victim = plant(&c);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.beside(), ["probe-reader.credential"], "{text}");
    assert_eq!(c.file().as_deref(), Some(OLDER));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");

    // A destination that is REFUSED is not swept: a directory someone
    // else can write is not this script's to tidy.
    let c = Case::new("sweep-not-refused-parent");
    c.secret(&[("current", LIVE)]);
    plant(&c);
    // mode-bits-ok: the parent directory, opened to others so the deposit must refuse it
    let open = std::fs::Permissions::from_mode(0o777);
    std::fs::set_permissions(c.root.join("etc-boss"), open).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.beside().len(), 2, "nothing is removed there: {text}");
}

/// THE CREATE IS EXCLUSIVE, BY EFFECT (review 4d39f4dc, F5). The one
/// line of the script that puts the value in the temp file is lifted out
/// and run as it stands against a name that is already there. The temp
/// name is random in the script, so no test can plant it there; this
/// runs the shipped line itself, so a spelling that would write through
/// a planted name — the reviewer's appending `>>`, which noclobber does
/// not guard — is red here, where a pin on the words was green.
#[test]
fn the_create_refuses_a_name_that_is_already_there() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let writers: Vec<&str> = script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter(|l| l.contains("\"$TMP\"") && l.contains('>') && l.contains("printf"))
        .collect();
    assert_eq!(
        writers.len(),
        1,
        "exactly one line writes the value into the temp file: {writers:?}"
    );
    let create = writers[0]
        .trim()
        .split(" 2>\"$KERR\"")
        .next()
        .unwrap()
        .to_string();
    assert!(
        create.starts_with('(') && create.ends_with(')'),
        "the create is one subshell, so its umask and noclobber leak nowhere: {create}"
    );
    let root = scratch_dir("probe-reader-deposit-create");
    let run = |tmp: &Path| -> bool {
        Command::new("bash")
            .args(["-c", &format!("set -euo pipefail\n{create}\n")])
            .env("V", LIVE)
            .env("TMP", tmp)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    // Control: a name that is not there is created, owner-only, holding
    // the value and nothing more.
    let fresh = root.join("fresh");
    assert!(run(&fresh), "the shipped line creates a fresh name");
    assert!(std::fs::read_to_string(&fresh).unwrap() == LIVE);
    assert_eq!(mode(&fresh), 0o600);

    // A link to a file that exists, a link to nothing, a standing file.
    let victim = root.join("victim");
    write_file(&victim, "untouched");
    let link = root.join("link");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    assert!(!run(&link), "a planted link is refused");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");

    let nowhere = root.join("nowhere");
    let dangling = root.join("dangling");
    std::os::unix::fs::symlink(&nowhere, &dangling).unwrap();
    assert!(!run(&dangling), "a dangling link is refused");
    assert!(!nowhere.exists(), "and its target is not created");

    let standing = root.join("standing");
    write_file(&standing, "untouched");
    assert!(!run(&standing), "a file that is already there is refused");
    assert_eq!(std::fs::read_to_string(&standing).unwrap(), "untouched");
}

/// The rest of the write, read off the script: the temp file is made in
/// the destination's own directory, and reaches its name by a rename
/// that never descends into what stands there.
#[test]
fn the_write_is_beside_the_file_and_renamed() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let live: Vec<&str> = script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        live.iter().any(|l| l.contains("TMP=\"$PARENT/.$BASE.tmp.")),
        "the temp file is made in the destination's own directory"
    );
    assert!(
        live.iter().any(|l| l.contains("mv -fT \"$TMP\" \"$DEST\"")),
        "the file reaches its name by rename"
    );
    assert!(
        !live
            .iter()
            .any(|l| l.contains("> \"$DEST\"") || l.contains(">\"$DEST\"")),
        "nothing ever writes through the destination's name"
    );
    assert!(script.starts_with("#!/usr/bin/env bash\n"));
    assert!(
        live.iter().position(|l| *l == "set +x").unwrap() < 3,
        "tracing is turned off before anything is read"
    );
}

/// AN ABSENT, UNREADABLE OR MALFORMED SECRET leaves the host exactly as
/// it was and names no value; a kubeconfig David has not placed is not
/// ready, and not a fault.
#[test]
fn a_secret_that_cannot_be_read_whole_leaves_the_host_untouched() {
    let c = Case::new("absent");
    c.held(OLDER);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("probe_reader_secret").starts_with("absent"),
        "{text}"
    );
    assert_eq!(c.file().as_deref(), Some(OLDER));
    assert_eq!(c.asked(), 0);

    let long = "A".repeat(4097);
    for (leg, value) in [
        ("two-words", "fake fake"),
        ("not-base64url", "fake+fake/fake="),
        ("line-break", "fake\nfake"),
        ("too-long", long.as_str()),
    ] {
        let c = Case::new(&format!("malformed-{leg}"));
        c.secret(&[("current", value)]);
        c.every_gate(names(value, "reader.current"));
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{leg}] {text}");
        assert!(
            c.summary("probe_reader_secret").starts_with("malformed"),
            "[{leg}] {text}"
        );
        assert_eq!(c.file(), None, "[{leg}]");
        assert_eq!(c.asked(), 0, "[{leg}] a malformed value is sent to no gate");
    }

    // No kubeconfig: root material David places. Not ready, exit 0.
    let c = Case::new("not-ready");
    let mut cmd = c.cmd();
    cmd.env_remove("BOSS_DEPOSIT_KUBECTL")
        .env("BOSS_OPS_DIR", c.root.join("no-ops-dir"));
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("probe_reader_secret").starts_with("not ready"),
        "{text}"
    );
    assert!(
        c.summary("probe_reader_effect").starts_with("not yet:"),
        "{text}"
    );
    assert_eq!(c.file(), None);
}

/// ONLY THE READER'S RULE. Handed the ESTATE machine token's rule — the
/// same handler, one argument apart — the script refuses before it reads
/// anything: the estate token must never land in the reader's file. So
/// is an invocation that names no absolute destination or no port list.
#[test]
fn the_estate_rule_and_a_bad_invocation_are_refused() {
    let c = Case::new("refused");
    create_dir(&c.secrets.join("boss").join("boss-machine-token"));
    std::fs::write(c.secrets.join("boss/boss-machine-token/current"), LIVE).unwrap();
    let mut cmd = c.cmd();
    cmd.args([
        "--rule",
        &repo_root().join(ESTATE_RULE).display().to_string(),
    ]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 78, "{text}");
    assert!(text.contains("gate_slots"), "{text}");
    assert_eq!(c.file(), None);
    assert!(
        c.argv_of("kubectl.argv").is_empty(),
        "nothing was read: {text}"
    );
    assert_eq!(c.asked(), 0);

    for args in [
        vec!["--dest", "relative/path"],
        vec!["--ports", ""],
        vec!["--rule", "/nonexistent/rule.toml"],
        vec!["--surprise", "x"],
    ] {
        let mut cmd = c.cmd();
        cmd.args(&args);
        let (rc, text) = c.run_cmd(cmd);
        assert_eq!(rc, 78, "{args:?}: {text}");
    }
    assert!(c.argv_of("kubectl.argv").is_empty());
}

/// A PORT LIST THAT CANNOT BE READ AS ONE, or that names no gate, sends
/// the credential nowhere and deposits nothing.
#[test]
fn a_port_list_that_is_not_one_sends_the_value_nowhere() {
    for (leg, table) in [
        ("word", "jobs=7900\nnot a port line\n".to_string()),
        ("zero", "jobs=0\n".to_string()),
        ("huge", "jobs=70000\n".to_string()),
        ("host", "jobs=example.invalid:80\n".to_string()),
    ] {
        let c = Case::new(&format!("ports-{leg}"));
        c.secret(&[("current", LIVE)]);
        c.ports_file(&table);
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{leg}] {text}");
        assert_eq!(c.file(), None, "[{leg}]");
        assert_eq!(c.asked(), 0, "[{leg}] {text}");
        assert!(
            c.argv_of("curl.argv").is_empty(),
            "[{leg}] nothing was sent"
        );
        assert!(
            c.summary("probe_reader_effect").starts_with("UNVERIFIED"),
            "[{leg}] {text}"
        );
    }
    // The base port is a gate whether or not the table lists it: a table
    // of the edge alone still asks the system of record's own port.
    let c = Case::new("ports-base-only");
    c.secret(&[("current", LIVE)]);
    c.ports_file(&format!("gateway={}\n", c.gateway.port));
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("probe_reader_effect")
            .starts_with("reader.current at 1 of 1 gates"),
        "{text}"
    );
    assert!(c.gateway.requests().is_empty());
}

/// AN UNGATED PORT IS NEVER SENT THE VALUE — NOT EVEN AS THE BASE
/// (review 4d39f4dc, F3). With BOSS_JOBS_URL on the gateway's own port
/// the first cut skipped the gateway's NAME and then appended its PORT
/// as the system of record's: the fixture gateway received the value. A
/// base port the table gives to an ungated service is refused before
/// anything is sent; so is a table that gives one port to a gated name
/// and an ungated one.
#[test]
fn an_ungated_port_is_never_sent_the_value_even_as_the_base() {
    let c = Case::new("ungated-base");
    c.secret(&[("current", LIVE)]);
    let mut cmd = c.cmd();
    cmd.env(
        "BOSS_JOBS_URL",
        format!("http://127.0.0.1:{}", c.gateway.port),
    );
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.gateway.requests().is_empty(),
        "the edge was sent the credential: {text}"
    );
    assert_eq!(c.asked(), 0, "nothing is sent anywhere: {text}");
    assert!(c.argv_of("curl.argv").is_empty(), "{text}");
    assert_eq!(c.file(), None);
    let effect = c.summary("probe_reader_effect");
    assert!(
        effect.starts_with("UNVERIFIED")
            && effect.contains(&format!("port {} is the ungated gateway's", c.gateway.port)),
        "{text}"
    );

    // One port under two names, one of them ungated: in either order.
    for (leg, table) in [
        ("gated-first", "jobs={j}\npeople={g}\ngateway={g}\n"),
        ("ungated-first", "jobs={j}\ngateway={g}\npeople={g}\n"),
    ] {
        let c = Case::new(&format!("ungated-shared-{leg}"));
        c.secret(&[("current", LIVE)]);
        c.ports_file(
            &table
                .replace("{j}", &c.gates[0].port.to_string())
                .replace("{g}", &c.gateway.port.to_string()),
        );
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{leg}] {text}");
        assert!(c.gateway.requests().is_empty(), "[{leg}] {text}");
        assert!(c.argv_of("curl.argv").is_empty(), "[{leg}] {text}");
        assert_eq!(c.file(), None, "[{leg}]");
    }
}

/// THE PORT TABLE IS THIS ACCOUNT'S ALONE, OR IT LISTS NOTHING (review
/// 4d39f4dc, N6; the door's own rule, review 991bb439 N3). "Every gate"
/// is as strong as the table: one shortened to a single gate deposited
/// at 1 of 1 and hid a gate that said no. So the table is taken only
/// from a file its reader owns and nobody else may write — judged on the
/// open file — which on the forge is the root-owned probe view's, never
/// the checkout owner's.
#[test]
fn a_port_table_another_account_can_write_lists_nothing() {
    for bits in [0o664, 0o646, 0o666] {
        let c = Case::new(&format!("table-{bits:o}"));
        c.secret(&[("current", LIVE)]);
        // mode-bits-ok: a data file, opened to others so the deposit must refuse it
        let loose = std::fs::Permissions::from_mode(bits);
        std::fs::set_permissions(c.root.join("sor-ports.env"), loose).unwrap();
        let (rc, text) = c.run();
        assert_eq!(rc, 1, "[{bits:o}] {text}");
        assert_eq!(c.file(), None, "[{bits:o}]");
        assert_eq!(c.asked(), 0, "[{bits:o}] {text}");
        assert!(c.argv_of("curl.argv").is_empty(), "[{bits:o}]");
        let effect = c.summary("probe_reader_effect");
        assert!(
            effect.starts_with("UNVERIFIED") && effect.contains("not this account's alone"),
            "[{bits:o}] {text}"
        );
    }
    // Reached through a link, the file judged is the one opened.
    let c = Case::new("table-linked-loose");
    c.secret(&[("current", LIVE)]);
    let loose_table = c.root.join("loose.env");
    write_file(&loose_table, &format!("jobs={}\n", c.gates[0].port));
    // mode-bits-ok: a data file, opened to others so the deposit must refuse it
    let loose = std::fs::Permissions::from_mode(0o666);
    std::fs::set_permissions(&loose_table, loose).unwrap();
    std::fs::remove_file(c.root.join("sor-ports.env")).unwrap();
    std::os::unix::fs::symlink(&loose_table, c.root.join("sor-ports.env")).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.asked(), 0, "{text}");
    // An absent table: nothing listed, nothing sent.
    let c = Case::new("table-absent");
    c.secret(&[("current", LIVE)]);
    std::fs::remove_file(c.root.join("sor-ports.env")).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.asked(), 0, "{text}");
    assert_eq!(c.file(), None);
    // A SHORT TABLE IS VISIBLE: the effect counts the gates it asked, and
    // names the table they came from.
    let c = Case::new("table-short");
    c.secret(&[("current", LIVE)]);
    c.ports_file(&format!("jobs={}\n", c.gates[0].port));
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    let effect = c.summary("probe_reader_effect");
    assert!(
        effect.starts_with("reader.current at 1 of 1 gates")
            && effect.contains(&c.root.join("sor-ports.env").display().to_string()),
        "{text}"
    );
}

/// THE VALUE GOES ONLY TO THE ESTATE'S OWN HOST. With the system of
/// record's address pointed at a host that is neither loopback nor named
/// in BOSS_MACHINE_TOKEN_HOSTS — a hand run with the public edge in
/// BOSS_JOBS_URL — nothing is sent at all; and an address that is not a
/// plain origin (userinfo, a path, a query) is refused the same way.
#[test]
fn the_value_is_withheld_from_a_host_that_is_not_the_estates() {
    for url in [
        "http://example.invalid:7900",
        "http://127.0.0.1.example.invalid:7900",
        "http://user@127.0.0.1:7900",
        "http://127.0.0.1:7900/path",
        "http://127.0.0.1:7900?x=1",
        "ftp://127.0.0.1:7900",
        "",
    ] {
        let c = Case::new("withheld");
        c.secret(&[("current", LIVE)]);
        let mut cmd = c.cmd();
        cmd.env("BOSS_JOBS_URL", url);
        let (rc, text) = c.run_cmd(cmd);
        assert_eq!(c.file(), None, "[{url}] {text}");
        assert!(
            c.argv_of("curl.argv").is_empty(),
            "[{url}] nothing may be sent: {}",
            c.argv_of("curl.argv")
        );
        assert!(
            c.summary("probe_reader_effect").starts_with("UNVERIFIED"),
            "[{url}] {text}"
        );
        assert!(rc == 0 || rc == 1, "[{url}] {text}");
    }
    // Named in the list, the same host IS asked (and is dark here).
    let c = Case::new("listed");
    c.secret(&[("current", LIVE)]);
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", "http://localhost.example.invalid:7900")
        .env("BOSS_MACHINE_TOKEN_HOSTS", "localhost.example.invalid")
        .env("BOSS_READER_GATE_BOUND_S", "1");
    let (_, text) = c.run_cmd(cmd);
    assert!(
        c.argv_of("curl.argv").contains("localhost.example.invalid"),
        "the control: a listed host is asked: {text}"
    );

    // The list's two sources, in the estate token's order: the rendered
    // sor.env names the host and the variable is unset — asked; the
    // variable SET and empty overrides the file — withheld.
    let c = Case::new("listed-in-sor-env");
    c.secret(&[("current", LIVE)]);
    write_file(
        &c.root.join("sor.env"),
        "BOSS_JOBS_URL=unused\nBOSS_MACHINE_TOKEN_HOSTS=localhost.example.invalid\n",
    );
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", "http://localhost.example.invalid:7900")
        .env("BOSS_SOR_ENV", c.root.join("sor.env"))
        .env("BOSS_READER_GATE_BOUND_S", "1");
    let (_, text) = c.run_cmd(cmd);
    assert!(
        c.argv_of("curl.argv").contains("localhost.example.invalid"),
        "a host the rendered sor.env lists is asked: {text}"
    );
    let c = Case::new("emptied-list");
    c.secret(&[("current", LIVE)]);
    write_file(
        &c.root.join("sor.env"),
        "BOSS_MACHINE_TOKEN_HOSTS=localhost.example.invalid\n",
    );
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", "http://localhost.example.invalid:7900")
        .env("BOSS_SOR_ENV", c.root.join("sor.env"))
        .env("BOSS_MACHINE_TOKEN_HOSTS", "");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.argv_of("curl.argv").is_empty(),
        "a list set empty withholds: {text}"
    );
}

/// EVERY WAIT IS BOUNDED. A read of the Secret that never answers and a
/// gate that takes the connection and says nothing each cost their bound,
/// write nothing, and leave the summary written; and the bound the
/// converge puts on the whole script is longer than the script's own
/// worst case and well under a tick, beside the machine token's.
#[test]
fn a_wait_that_never_ends_is_stopped_at_its_bound() {
    let c = Case::new("stalled-read");
    c.held(OLDER);
    let sleepy = c.root.join("sleepy-kubectl");
    write_exec(&sleepy, "#!/bin/sh\nsleep 45\n");
    let mut cmd = c.cmd();
    cmd.env("BOSS_DEPOSIT_KUBECTL", &sleepy)
        .env("BOSS_DEPOSIT_READ_BOUND_S", "1");
    let started = std::time::Instant::now();
    let (rc, text) = c.run_cmd(cmd);
    assert!(started.elapsed().as_secs() < 20, "{text}");
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("probe_reader_secret")
            .contains("did not answer inside 1 seconds"),
        "{text}"
    );
    assert_eq!(c.file().as_deref(), Some(OLDER));

    let c = Case::new("stalled-gate");
    c.secret(&[("current", LIVE)]);
    c.gates[1].answers(Answer::Silent);
    let mut cmd = c.cmd();
    cmd.env("BOSS_READER_GATE_BOUND_S", "2");
    let started = std::time::Instant::now();
    let (rc, text) = c.run_cmd(cmd);
    assert!(started.elapsed().as_secs() < 20, "{text}");
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.file(), None, "{text}");
    assert!(
        c.summary("probe_reader_effect").starts_with("UNVERIFIED"),
        "{text}"
    );

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
    let gate = default_of(&script, "BOSS_READER_GATE_BOUND_S");
    let whole = default_of(&converge, "BOSS_FORGE_READER_DEPOSIT_BOUND_S");
    let estate = default_of(&converge, "BOSS_FORGE_DEPOSIT_BOUND_S");
    // One read with timeout's five-second kill, and every listed port
    // (the edge too, were it ever gated) at its bound.
    let ports = std::fs::read_to_string(repo_root().join(PORTS))
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .count() as u64;
    let worst = (read + 5) + ports * gate;
    assert!(
        whole > worst,
        "forge-converge.sh bounds the reader deposit at {whole}s, inside its worst case of {worst}s"
    );
    assert!(
        (whole + estate) * 2 <= 600,
        "the two deposits at the converge's head ({whole}s + {estate}s) are not well under a tick"
    );
    assert!(script.contains("\"--request-timeout=${REQUEST_TIMEOUT_S}s\""));
}

/// A SHELL STARTED WITH TRACING ON PRINTS NOTHING OF THE VALUE: the
/// script turns xtrace off before it reads anything. `run_cmd` refuses
/// any fixture in the output.
#[test]
fn an_inherited_xtrace_prints_no_value() {
    let c = Case::new("xtrace");
    c.secret(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    let mut cmd = c.cmd();
    cmd.env("SHELLOPTS", "xtrace");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.file().as_deref(), Some(LIVE));
}

/// THE ISOLATION, BY EFFECT: deposited as root, the file gives another
/// account nothing. That other account is what a car's probe runs as
/// (boss-probe on the forge). Needs root to make a root-owned file; under
/// any other uid it says it did not run rather than pass.
#[test]
fn another_account_cannot_read_the_file() {
    let me = std::fs::metadata("/proc/self").unwrap().uid();
    if me != 0 || Command::new("setpriv").arg("--version").output().is_err() {
        eprintln!(
            "NOT RUN: uid {me} cannot make a root-owned file and read it as another account \
             (needs root and setpriv). The 0600 mode is held by \
             current_is_deposited_when_every_gate_names_it_reader_current."
        );
        return;
    }
    let c = Case::new("isolation");
    for d in [c.root.parent().unwrap().to_path_buf(), c.root.clone()] {
        // mode-bits-ok: scratch directories, opened so the file's own mode is what refuses
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    c.secret(&[("current", LIVE)]);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    let meta = std::fs::metadata(&c.dest).unwrap();
    assert_eq!((meta.uid(), meta.gid()), (0, 0), "root:root");
    let as_other = |script: &str| {
        Command::new("setpriv")
            .args([
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "bash",
                "-c",
                script,
            ])
            .env("F", &c.dest)
            .env("BESIDE", c.root.join("etc-boss"))
            .output()
            .unwrap()
    };
    let open = c.root.join("etc-boss").join("open");
    write_file(&open, "control");
    // mode-bits-ok: a data file, opened so the control read is allowed
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = as_other("cat \"$BESIDE/open\"");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "control",
        "the control read works"
    );
    let out = as_other("cat \"$F\"");
    assert!(
        !out.status.success() && out.stdout.is_empty(),
        "the file was read"
    );
    let out = as_other("echo planted > \"$BESIDE/.probe-reader.credential.tmp.9.9\"");
    assert!(
        !out.status.success(),
        "another account planted a name beside the file"
    );

    // A PARENT THAT IS ANOTHER ACCOUNT'S, closed to group and other: its
    // owner can still plant any name in it, so root deposits nothing
    // there. The directory is made BY that account, since this root may
    // hold no CAP_CHOWN.
    let open_dir = c.root.join("open-dir");
    create_dir(&open_dir);
    // mode-bits-ok: a scratch directory, opened so the other account can make its own inside
    std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let theirs = open_dir.join("theirs");
    let out = Command::new("setpriv")
        .args([
            "--reuid=65534",
            "--regid=65534",
            "--clear-groups",
            "mkdir",
            "-m",
            "755",
        ])
        .arg(&theirs)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the other account makes its directory"
    );
    assert_eq!(std::fs::metadata(&theirs).unwrap().uid(), 65534);
    let mut cmd = c.cmd();
    cmd.args([
        "--dest",
        &theirs.join("probe-reader.credential").display().to_string(),
    ]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("probe_reader_action")
            .contains("not this account's alone"),
        "{text}"
    );
    assert_eq!(std::fs::read_dir(&theirs).unwrap().count(), 0, "{text}");

    // A PORT TABLE THAT IS ANOTHER ACCOUNT'S, closed to group and other:
    // its owner can shorten it, so root lists no gate from it.
    let their_table = theirs.join("sor-ports.env");
    let out = Command::new("setpriv")
        .args([
            "--reuid=65534",
            "--regid=65534",
            "--clear-groups",
            "bash",
            "-c",
            "umask 022 && printf 'jobs=%s\\n' \"$P\" > \"$T\"",
        ])
        .env("P", c.gates[0].port.to_string())
        .env("T", &their_table)
        .output()
        .unwrap();
    assert!(out.status.success(), "the other account writes its table");
    assert_eq!(std::fs::metadata(&their_table).unwrap().uid(), 65534);
    let asked_before = c.asked();
    let mut cmd = c.cmd();
    cmd.args(["--ports", &their_table.display().to_string()]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.asked(), asked_before, "no gate is asked: {text}");
    assert!(
        c.summary("probe_reader_effect")
            .contains("not this account's alone"),
        "{text}"
    );
    assert_eq!(c.file().as_deref(), Some(LIVE), "the file held is kept");
}

/// FACTS THAT LIVE TWICE, HELD EQUAL (CLAUDE.md §9a): the path is the one
/// `boss prove` opens; the slot bound is boss-core's; the ports the
/// script skips are exactly the services boss-core says mount no gate;
/// the header is boss-core's; and the answer it waits for is the name
/// boss-core gives a reader's current slot.
#[test]
fn the_path_the_bound_the_ungated_roster_and_the_answer_are_cores() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
    let door = std::fs::read_to_string(repo_root().join(DOOR)).unwrap();
    assert!(
        door.contains(
            "pub(crate) const DEFAULT_CREDENTIAL: &str = \"/etc/boss/probe-reader.credential\";"
        ),
        "{DOOR} no longer opens the file the forge deposits"
    );
    assert!(
        door.contains("pub(crate) const CREDENTIAL_ENV: &str = \"BOSS_PROBE_READER_CREDENTIAL\";")
    );
    assert!(
        converge.contains(
            "--dest \"${BOSS_PROBE_READER_CREDENTIAL:-/etc/boss/probe-reader.credential}\""
        ),
        "the converge deposits where the door reads"
    );
    // THE PORT TABLE IS THE ROOT-OWNED PROBE VIEW'S, the one the door
    // itself reads (BOSS_PROBE_DIR in the ops runner's drop-in is that
    // view), by the view's own variable and default — never the checkout
    // owner's copy.
    let account =
        std::fs::read_to_string(repo_root().join("infra/forge/probe-account.sh")).unwrap();
    assert!(
        account.contains("VIEW=\"${BOSS_PROBE_VIEW:-/var/lib/boss/probe-view}\"")
            && account.contains("echo \"Environment=BOSS_PROBE_DIR=$VIEW\""),
        "infra/forge/probe-account.sh no longer names the view the converge reads its table from"
    );
    assert!(
        converge.contains(
            "--ports \"${BOSS_PROBE_VIEW:-/var/lib/boss/probe-view}/infra/forge/sor-ports.env\""
        ),
        "the converge takes the port table from the root-owned probe view"
    );
    assert!(
        !converge.contains("--ports \"$INFRA"),
        "never from the checkout the converge itself runs out of"
    );
    // THE DOOR'S RULE FOR A PORT LIST, AND THE SCRIPT'S ONE RULE FOR A
    // PATH THAT IS ITS ACCOUNT'S ALONE: the same two facts — the owner,
    // and no group or other write bit. The door names uid 0; the script
    // names the uid it runs as, which on the forge is 0 (the unit is
    // User=root) and in this suite is the suite's own. One function in
    // the script judges both the table and the file's directory.
    assert!(
        door.contains("(source.uid == 0 && source.mode & 0o022 == 0)"),
        "{DOOR}: the door's port-list rule moved; hold the deposit's to it"
    );
    assert_eq!(
        script.matches("8#022").count(),
        1,
        "the script judges `alone to write` in ONE place"
    );
    assert!(
        script.contains("if [ \"$owner\" != \"$me\" ] || [ $((8#$bits & 8#022)) -ne 0 ]; then")
    );
    assert!(script.contains(&format!(
        "MAX_SLOT_BYTES={}",
        boss_core::machine_token::MAX_SLOT_BYTES
    )));
    let ungated: Vec<&str> = boss_core::machine_gate::UNGATED
        .iter()
        .filter_map(|u| u.service)
        .collect();
    assert!(
        script.contains(&format!("UNGATED=\"{}\"", ungated.join(" "))),
        "{SCRIPT} must skip exactly the services boss-core says mount no gate: {ungated:?}"
    );
    // The header is the lib's to write, under the estate token's host
    // rule; the script hands it the value and the host it judged.
    assert!(
        script.contains("machine_gate_value_header READER_HDR \"$SCHEME://$HOST\" \"$V\""),
        "the value reaches curl through the lib's header writer"
    );
    assert!(
        !script
            .to_ascii_lowercase()
            .contains("x-boss-machine-token:"),
        "the script spells no header of its own"
    );
    let lib = std::fs::read_to_string(repo_root().join("infra/lib/secret-header.sh")).unwrap();
    let writer = lib
        .split("\nmachine_gate_value_header() {\n")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("infra/lib/secret-header.sh defines machine_gate_value_header");
    assert!(writer.contains(&format!(
        "secret_header \"$1\" \"{}: $3\"",
        boss_core::machine_token::HEADER
    )));
    assert!(
        writer.contains("if ! _secret_header_mt_allowed \"$_secret_header_gv_host\""),
        "the lib's writer applies the estate token's host rule before it writes"
    );
    assert!(script.contains(&format!("{}\"", boss_core::machine_gate::ACCEPTS_PATH)));
    assert!(
        script.contains("'\"reader.current\"') N_OK=$((N_OK + 1)) ;;"),
        "the one answer that deposits"
    );
    // Every name in the tree's own table is a name the script can read.
    for line in std::fs::read_to_string(repo_root().join(PORTS))
        .unwrap()
        .lines()
    {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, port) = line.split_once('=').expect("name=port");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{name}"
        );
        assert!(port.parse::<u16>().is_ok_and(|p| p > 0), "{line}");
    }
}

/// THE CONVERGE CALLS IT BEFORE ITS FIRST STEP THAT CAN FAIL, after the
/// estate token's own deposit, hands it the reader's rule and the tree's
/// port table, bounds it, and never carries its exit into its own.
#[test]
fn the_converge_deposits_before_the_fetch_and_owes_it_nothing() {
    let converge = std::fs::read_to_string(repo_root().join(CONVERGE)).unwrap();
    let live: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    let at = |needle: &str| {
        live.iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{CONVERGE} has no live line containing `{needle}`"))
    };
    let deposit = at("forge/probe-reader-deposit.sh");
    assert!(at("forge/machine-token-deposit.sh") < deposit);
    assert!(deposit < at("checkout_git '$REPO' fetch"));
    assert!(
        live[deposit].starts_with(
            "timeout -k 5 \"$FORGE_READER_DEPOSIT_BOUND_S\" \"$INFRA/forge/probe-reader-deposit.sh\""
        ),
        "the converge runs the reader deposit under a bound: {}",
        live[deposit]
    );
    let call = live[deposit..deposit + 4].join("\n");
    assert!(
        call.contains("--rule \"$INFRA/dispatcher/rules/broker-rotates-the-probe-reader.toml\"")
    );
    assert!(
        call.contains(
            "--ports \"${BOSS_PROBE_VIEW:-/var/lib/boss/probe-view}/infra/forge/sor-ports.env\""
        ),
        "{call}"
    );
    assert!(call.contains("|| probe_reader_rc=$?"), "{call}");
    for l in &live {
        assert!(
            !(l.contains("exit") && l.contains("probe_reader_rc")),
            "the converge must never exit on the reader's deposit: {l}"
        );
    }
    // The estate token's call is untouched by this car.
    let estate = at("forge/machine-token-deposit.sh");
    let call = live[estate..estate + 3].join("\n");
    assert!(call.contains("broker-rotates-the-machine-token.toml"));
    assert!(call.contains("${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}"));
}
