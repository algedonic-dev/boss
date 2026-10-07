//! `infra/forge/machine-token-deposit.sh` — the forge host takes the
//! estate machine token, all three slots, from the broker's Secret into
//! the ROOT-ONLY directory its shell callers already read, on every
//! forge-converge tick, and proves it by effect (design-doc 20058482,
//! question forge-token, David 2026-10-06; backlog 88379df3).
//!
//! HOW THIS IS MEASURED. The script runs for real against the tree's own
//! broker rule; the shared stub `kubectl` answers the Secret from files
//! in scratch (`boss_testing::kubectl_secret_stub`), wrapped so a case
//! can move the Secret between two reads; and a tiny HTTP jobs API on
//! 127.0.0.1 answers `GET /api/machine-gate/accepts` the way the gate
//! does — the NAME of the slot the presented header matches, or `none`.
//! Fixture values are fake; every case also checks that none of them
//! reaches the output, the run summary or a request line.
//!
//! WHAT THE ISOLATION LEG PROVES, AND WHAT IT DOES NOT. A car's recorded
//! probe runs on the forge as `BOSS_PROBE_USER` (default `david`,
//! boss-cli prove.rs). `a_non_root_account_cannot_read_the_token_path`
//! deposits as root and then reads as another uid — a plain read, a
//! directory listing and the shared reader itself — and every one comes
//! back empty-handed. It needs root to make a root-owned tree, so under
//! the gate's own uid it reports that it did not run and holds only the
//! mode bits (`the_tree_is_owner_only`); it was run as root on the dev
//! pod for the car's receipt.
//!
//! WHAT NO TEST HERE CAN HOLD — two roads, both older than this deposit,
//! by which a probe running as david reaches the root-only token on the
//! real host today: (1) david holds `sudo -n docker` there
//! (infra/estate/ops-credentials.sh reads /etc/boss-ops through it),
//! which reaches any root file; (2) forge-converge.service runs
//! /home/david/boss/infra/forge/forge-converge.sh AS ROOT out of david's
//! own checkout, and the deposit and the libraries it sources execute
//! from there before the converge's checkout -f, so any shell as david
//! can edit what root runs within ten minutes. The modes this file
//! holds keep the token from a direct read and from an accident; what
//! closes both roads is a probe account of its own (design-doc bdc60b65,
//! question probe-user), a separate car.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const SCRIPT: &str = "infra/forge/machine-token-deposit.sh";
const RULE: &str = "infra/dispatcher/rules/broker-rotates-the-machine-token.toml";
const CONVERGE: &str = "infra/forge/forge-converge.sh";
const READER: &str = "infra/lib/secret-header.sh";

// 43-character base64url fixtures, fake — the shape the broker mints.
const OLDER: &str = "prevPREVprevPREVprevPREVprevPREVprevPREV001";
const LIVE: &str = "currCURRcurrCURRcurrCURRcurrCURRcurrCURR002";
const STAGED: &str = "nextNEXTnextNEXTnextNEXTnextNEXTnextNEXT003";
const FIXTURES: [&str; 3] = [OLDER, LIVE, STAGED];

/// The shared stub, and after it a move of the Secret: when
/// `$STUB_SECRETS/then` exists, its files REPLACE the three slots and the
/// resourceVersion advances — once, the directory is consumed — so the
/// read that follows a deposit's writes sees a promoted Secret. With
/// `$STUB_SECRETS/always` the version advances on every read.
const MOVING_KUBECTL: &str = r#"#!/usr/bin/env bash
set -euo pipefail
"$STUB_REAL" "$@"
d="$STUB_SECRETS/$2/$5"
[ -d "$d" ] || exit 0
rv="$(cat "$d/.rv" 2>/dev/null || echo 1)"
if [ -d "$STUB_SECRETS/then" ]; then
    rm -f "$d/current" "$d/next" "$d/previous"
    for f in "$STUB_SECRETS/then"/*; do [ -f "$f" ] && cp "$f" "$d/"; done
    rm -rf "$STUB_SECRETS/then"
    echo $((rv + 1)) > "$d/.rv"
elif [ -e "$STUB_SECRETS/always" ]; then
    echo $((rv + 1)) > "$d/.rv"
fi
"#;

#[derive(Default)]
struct Gate {
    /// value → the slot name the gate answers for it.
    slots: Vec<(String, String)>,
    /// The request lines and whether each carried the header.
    seen: Vec<(String, bool)>,
}

/// The jobs API's machine gate, as far as the deposit asks it.
struct Api {
    port: u16,
    gate: Arc<Mutex<Gate>>,
}

impl Api {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let gate: Arc<Mutex<Gate>> = Default::default();
        let g = gate.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let mut reader = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let request = line.trim().to_string();
                let mut presented: Option<String> = None;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((_, v)) = h
                        .split_once(':')
                        .filter(|(k, _)| k.eq_ignore_ascii_case("x-boss-machine-token"))
                    {
                        presented = Some(v.trim().to_string());
                    }
                }
                let mut st = g.lock().unwrap();
                st.seen.push((request.clone(), presented.is_some()));
                let (status, body) = if request.starts_with("GET /api/machine-gate/accepts ") {
                    let matched = presented
                        .as_deref()
                        .and_then(|p| st.slots.iter().find(|(v, _)| v == p))
                        .map(|(_, s)| s.clone())
                        .unwrap_or_else(|| "none".into());
                    (
                        "200 OK",
                        format!(
                            r#"{{"service":"jobs","mode":"report","matched":"{matched}","degraded":false}}"#
                        ),
                    )
                } else {
                    ("404 Not Found", "{}".to_string())
                };
                drop(st);
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
        Self { port, gate }
    }

    fn accepts(&self, value: &str, slot: &str) {
        self.gate
            .lock()
            .unwrap()
            .slots
            .push((value.into(), slot.into()));
    }

    fn seen(&self) -> Vec<(String, bool)> {
        self.gate.lock().unwrap().seen.clone()
    }
}

struct Case {
    root: PathBuf,
    secrets: PathBuf,
    dest: PathBuf,
    kubectl: PathBuf,
    api: Api,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("machine-token-deposit-{name}"));
        let secrets = root.join("secrets");
        create_dir(&secrets);
        create_dir(&root.join("tmp"));
        create_dir(&root.join("etc-boss"));
        write_exec(
            &root.join("kubectl-real"),
            boss_testing::kubectl_secret_stub::SECRET_KUBECTL,
        );
        let kubectl = root.join("kubectl");
        write_exec(&kubectl, MOVING_KUBECTL);
        Self {
            dest: root.join("etc-boss").join("machine-token"),
            root,
            secrets,
            kubectl,
            api: Api::start(),
        }
    }

    fn secret_dir(&self) -> PathBuf {
        self.secrets.join("boss").join("boss-machine-token")
    }

    /// The Secret as the broker leaves it: the object exists and holds
    /// each slot given.
    fn secret(&self, slots: &[(&str, &str)]) {
        create_dir(&self.secret_dir());
        for (k, v) in slots {
            std::fs::write(self.secret_dir().join(k), v).unwrap();
        }
    }

    /// What the Secret becomes right after the deposit's next read.
    fn then(&self, slots: &[(&str, &str)]) {
        create_dir(&self.secrets.join("then"));
        for (k, v) in slots {
            std::fs::write(self.secrets.join("then").join(k), v).unwrap();
        }
    }

    /// Slots an earlier pass left on the host.
    fn held(&self, slots: &[(&str, &str)]) {
        create_dir(&self.dest);
        for (k, v) in slots {
            std::fs::write(self.dest.join(k), v).unwrap();
        }
    }

    fn slot(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dest.join(name)).ok()
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new("bash");
        c.arg(repo_root().join(SCRIPT))
            .args(["--rule", &repo_root().join(RULE).display().to_string()])
            .args(["--dest", &self.dest.display().to_string()])
            .env("BOSS_DEPOSIT_KUBECTL", &self.kubectl)
            .env("STUB_REAL", self.root.join("kubectl-real"))
            .env("STUB_SECRETS", &self.secrets)
            .env("TMPDIR", self.root.join("tmp"))
            .env(
                "BOSS_JOBS_URL",
                format!("http://127.0.0.1:{}", self.api.port),
            )
            .env("BOSS_DEPOSIT_VERIFY_WAIT", "1")
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env("BOSS_SOR_ENV", self.root.join("no-sor.env"))
            .env_remove("BOSS_MACHINE_TOKEN_DIR")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .env_remove("RUNTIME_DIRECTORY");
        c
    }

    fn run_cmd(&self, mut c: Command) -> (i32, String) {
        let out = c.output().expect("run machine-token-deposit.sh");
        let summary = std::fs::read_to_string(self.root.join("summary.json")).unwrap_or_default();
        let text = format!(
            "{}{}\n--- summary\n{summary}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        for t in FIXTURES {
            assert!(
                !text.contains(t),
                "a slot's value reached the output:\n{text}"
            );
            // Not even a suffix: this deposit names lengths, never an end.
            assert!(
                !text.contains(&t[t.len() - 8..]),
                "the last eight of a slot reached the output:\n{text}"
            );
        }
        for (line, _) in self.api.seen() {
            for t in FIXTURES {
                assert!(
                    !line.contains(t),
                    "a slot's value rode in a request line: {line}"
                );
            }
        }
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
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

/// The three slots land, each equal to the Secret's, the tree owner-only,
/// and the GATE says the host's stamped read matched `current` — by the
/// gate's answer, not by a file being there.
#[test]
fn all_three_slots_are_deposited_and_proved_by_effect() {
    let c = Case::new("three");
    c.secret(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    c.api.accepts(LIVE, "current");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
    assert_eq!(c.slot("next").as_deref(), Some(STAGED));
    assert_eq!(c.slot("previous").as_deref(), Some(OLDER));
    assert!(
        c.summary("machine_token_effect")
            .starts_with("matched current"),
        "the effect is the gate's own word: {text}"
    );
    assert!(
        c.summary("machine_token_action")
            .contains("current(43 bytes)"),
        "a slot is named by its length: {text}"
    );
    let seen = c.api.seen();
    assert!(
        seen.iter()
            .any(|(l, stamped)| l.starts_with("GET /api/machine-gate/accepts ") && *stamped),
        "the verify is a STAMPED read of the gate's accepts route: {seen:?}"
    );
    // Nothing but the three slots: no temp file is left beside them.
    let mut names: Vec<String> = std::fs::read_dir(&c.dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["current", "next", "previous"]);
}

/// THE TREE IS OWNER-ONLY: the directory 0700, each slot 0600, no group
/// or other bit anywhere — and an earlier, looser mode is tightened on a
/// pass that changes no value.
#[test]
fn the_tree_is_owner_only() {
    let c = Case::new("modes");
    c.secret(&[("current", LIVE), ("previous", OLDER)]);
    c.api.accepts(LIVE, "current");
    c.held(&[("current", LIVE)]);
    // mode-bits-ok: a directory and a data file, loosened to be re-tightened
    std::fs::set_permissions(&c.dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    // mode-bits-ok: a data file, never exec'd
    std::fs::set_permissions(
        c.dest.join("current"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(mode(&c.dest), 0o700, "{text}");
    for slot in ["current", "previous"] {
        assert_eq!(mode(&c.dest.join(slot)), 0o600, "{slot}: {text}");
        let m = std::fs::metadata(c.dest.join(slot)).unwrap();
        let me = std::fs::metadata(&c.root).unwrap();
        assert_eq!(m.uid(), me.uid(), "{slot} is the depositing account's own");
    }
}

/// An ABSENT Secret, a BLANK one and each MALFORMED one leave every file
/// on the host exactly as it was, say so, and name no value.
#[test]
fn an_absent_blank_or_malformed_secret_leaves_the_host_untouched() {
    let long = "A".repeat(4097);
    let legs: Vec<(&str, Option<Vec<(&str, &str)>>, &str)> = vec![
        ("absent", None, "absent"),
        ("blank", Some(vec![]), "empty"),
        (
            "two-words",
            Some(vec![("current", "two words")]),
            "malformed",
        ),
        (
            "line-break",
            Some(vec![("current", "abc\ndef")]),
            "malformed",
        ),
        ("oversize", Some(vec![("current", &long)]), "malformed"),
        (
            "bad-sibling",
            Some(vec![("current", STAGED), ("previous", "not/base64url!")]),
            "malformed",
        ),
    ];
    for (leg, secret, word) in legs {
        let c = Case::new(&format!("untouched-{leg}"));
        if let Some(slots) = &secret {
            c.secret(slots);
        }
        c.held(&[("current", LIVE), ("previous", OLDER)]);
        c.api.accepts(LIVE, "current");
        let (rc, text) = c.run();
        assert_eq!(
            rc, 1,
            "[{leg}] a Secret that cannot be followed is a named fault: {text}"
        );
        assert!(
            c.summary("machine_token_secret").starts_with(word),
            "[{leg}] the Secret's state leads with `{word}`: {text}"
        );
        assert!(
            c.summary("machine_token_action").starts_with("untouched"),
            "[{leg}] {text}"
        );
        assert_eq!(c.slot("current").as_deref(), Some(LIVE), "[{leg}]");
        assert_eq!(c.slot("previous").as_deref(), Some(OLDER), "[{leg}]");
        assert!(c.slot("next").is_none(), "[{leg}]");
        // What the host still holds is still proved by effect.
        assert!(
            c.summary("machine_token_effect")
                .starts_with("matched current"),
            "[{leg}] {text}"
        );
    }
}

/// Before the first mint — an empty Secret and an empty host, or a
/// kubeconfig David has not placed — there is nothing to take and
/// nothing is a fault.
#[test]
fn nothing_minted_or_not_ready_is_not_a_fault() {
    let c = Case::new("unminted");
    c.secret(&[]);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(!c.dest.exists(), "nothing is made for an empty Secret");
    assert!(
        c.summary("machine_token_effect")
            .starts_with("nothing to present")
    );

    // The first mint between install and promotion: only `next`.
    let c = Case::new("staged-only");
    c.secret(&[("next", STAGED)]);
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        !c.dest.exists(),
        "a next no caller may send is not deposited alone"
    );
    assert!(
        c.summary("machine_token_secret").starts_with("staged"),
        "{text}"
    );

    let c = Case::new("no-kubeconfig");
    c.held(&[("current", LIVE)]);
    c.api.accepts(LIVE, "current");
    let mut cmd = c.cmd();
    cmd.env_remove("BOSS_DEPOSIT_KUBECTL")
        .env("BOSS_OPS_DIR", c.root.join("no-ops-dir"));
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("machine_token_secret").starts_with("not ready"),
        "{text}"
    );
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
}

/// A PARTIAL SLOT SET is followed exactly: the slot the Secret holds is
/// written and the slots it holds blank are removed from the host.
#[test]
fn a_partial_slot_set_is_followed_exactly() {
    let c = Case::new("partial");
    c.secret(&[("current", LIVE), ("next", ""), ("previous", "  \n")]);
    c.held(&[("current", OLDER), ("next", LIVE), ("previous", STAGED)]);
    c.api.accepts(LIVE, "current");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
    assert!(c.slot("next").is_none(), "a blank next is removed: {text}");
    assert!(
        c.slot("previous").is_none(),
        "a blank previous is removed: {text}"
    );
    let action = c.summary("machine_token_action");
    assert!(action.contains("removed next previous"), "{action}");
}

/// AN UNWRITABLE DESTINATION — a file where the directory belongs, a
/// symlink, a parent that is not a directory — is a named fault that
/// changes nothing and still names no value.
#[test]
fn an_unwritable_destination_is_named_and_nothing_changes() {
    let c = Case::new("dest-file");
    c.secret(&[("current", LIVE)]);
    write_file(&c.dest, "not a directory");
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("machine_token_action").starts_with("REFUSED"),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&c.dest).unwrap(), "not a directory");

    let c = Case::new("dest-link");
    c.secret(&[("current", LIVE)]);
    let elsewhere = c.root.join("elsewhere");
    create_dir(&elsewhere);
    std::os::unix::fs::symlink(&elsewhere, &c.dest).unwrap();
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("machine_token_action").contains("symlink"),
        "{text}"
    );
    assert_eq!(
        std::fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "nothing was written through the link"
    );

    let c = Case::new("dest-parent");
    c.secret(&[("current", LIVE)]);
    write_file(&c.root.join("blocker"), "a file");
    let mut cmd = c.cmd();
    let dest = c.root.join("blocker").join("machine-token");
    // The two --dest arguments: the later one wins the script's parse.
    cmd.args(["--dest", &dest.display().to_string()]);
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("machine_token_action").starts_with("NOT WRITTEN"),
        "{text}"
    );

    // A slot path a directory was planted at: that slot is named, the
    // slots before it are whole, and the pass is a fault.
    let c = Case::new("slot-dir");
    c.secret(&[("current", LIVE), ("next", STAGED)]);
    create_dir(&c.dest.join("next").join("planted"));
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
    assert!(
        c.summary("machine_token_action").contains("slot next"),
        "{text}"
    );
}

/// ROTATION MID-DEPOSIT. The broker promotes between this pass's read
/// and its writes: the pass sees the resourceVersion moved and follows
/// the Secret again at once, so it never ends on a slot set the Secret
/// no longer holds.
#[test]
fn a_rotation_landing_mid_deposit_is_followed_in_the_same_pass() {
    let c = Case::new("mid-rotation");
    c.secret(&[("current", OLDER), ("next", LIVE)]);
    // The promotion: next → current, current → previous, next blank.
    c.then(&[("current", LIVE), ("previous", OLDER)]);
    c.api.accepts(LIVE, "current");
    c.api.accepts(OLDER, "previous");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE), "{text}");
    assert_eq!(c.slot("previous").as_deref(), Some(OLDER), "{text}");
    assert!(c.slot("next").is_none(), "{text}");
    assert!(
        c.summary("machine_token_action")
            .contains("moved mid-deposit"),
        "{text}"
    );
    assert!(
        c.summary("machine_token_effect")
            .starts_with("matched current"),
        "{text}"
    );

    // A Secret that moves under every pass ends the pass after three,
    // says so, and is not a fault: the next tick follows it.
    let c = Case::new("always-moving");
    c.secret(&[("current", LIVE)]);
    write_file(&c.secrets.join("always"), "");
    c.api.accepts(LIVE, "current");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
    assert!(
        c.summary("machine_token_action")
            .contains("each of 3 passes"),
        "{text}"
    );
}

/// THE EFFECT IS THE GATE'S ANSWER. A value the gate matches to no slot
/// is a fault said in capitals though every file is in place; a gate one
/// refresh behind (`next`) is accepted and named; a dark API is
/// UNVERIFIED — said, never read as a pass, and never a fault that could
/// be mistaken for a bad token.
#[test]
fn the_effect_is_the_gates_answer_not_the_file() {
    let c = Case::new("not-accepted");
    c.secret(&[("current", LIVE)]);
    let (rc, text) = c.run();
    assert_eq!(rc, 1, "{text}");
    assert_eq!(
        c.slot("current").as_deref(),
        Some(LIVE),
        "the file is in place"
    );
    assert!(
        c.summary("machine_token_effect")
            .starts_with("NOT ACCEPTED"),
        "a file in place is not the proof: {text}"
    );

    let c = Case::new("gate-behind");
    c.secret(&[("current", LIVE)]);
    c.api.accepts(LIVE, "next");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("machine_token_effect")
            .starts_with("matched next"),
        "{text}"
    );

    let c = Case::new("dark");
    c.secret(&[("current", LIVE)]);
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", "http://127.0.0.1:1");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("machine_token_effect").starts_with("UNVERIFIED"),
        "{text}"
    );

    // A jobs API that is not one of the estate's hosts gets no token,
    // and that is said rather than sent.
    let c = Case::new("off-estate");
    c.secret(&[("current", LIVE)]);
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", "http://jobs.example.invalid:7900")
        .env("BOSS_MACHINE_TOKEN_HOSTS", "");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 1, "{text}");
    assert!(
        c.summary("machine_token_effect").starts_with("NOT STAMPED"),
        "{text}"
    );
}

/// EVERY WAIT IS BOUNDED (adversarial review 9a1e289b, B1). This script
/// runs at the head of the converge, so its time is the converge's. A
/// read of the Secret that never answers is stopped at its bound, named
/// with the bound, with every held file untouched and the summary still
/// written; a jobs API that accepts the connection and never answers
/// costs the verify its bound and reads UNVERIFIED. And the bound the
/// converge puts on the whole script is longer than the script's own
/// worst case and far shorter than a tick.
#[test]
fn a_read_that_never_answers_is_stopped_at_its_bound() {
    let c = Case::new("stalled-read");
    c.held(&[("current", LIVE)]);
    c.api.accepts(LIVE, "current");
    let sleepy = c.root.join("sleepy-kubectl");
    write_exec(&sleepy, "#!/bin/sh\nsleep 45\n");
    let mut cmd = c.cmd();
    cmd.env("BOSS_DEPOSIT_KUBECTL", &sleepy)
        .env("BOSS_DEPOSIT_READ_BOUND_S", "1");
    let started = std::time::Instant::now();
    let (rc, text) = c.run_cmd(cmd);
    let took = started.elapsed().as_secs();
    assert!(
        took < 20,
        "the pass waited {took}s on a read that never answers: {text}"
    );
    assert_eq!(rc, 1, "{text}");
    let state = c.summary("machine_token_secret");
    assert!(
        state.starts_with("unreadable") && state.contains("did not answer inside 1 seconds"),
        "the stop is named with its bound, on the packet: {text}"
    );
    assert!(
        c.summary("machine_token_action").starts_with("untouched"),
        "{text}"
    );
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
    assert!(
        c.summary("machine_token_effect")
            .starts_with("matched current"),
        "what the host still holds is still proved: {text}"
    );

    // A jobs API that takes the connection and says nothing.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in silent.incoming().flatten() {
            // Held well past the bound under test, then dropped, so a
            // script that lost its bound fails this test instead of
            // hanging it.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(45));
                drop(conn);
            });
        }
    });
    let c = Case::new("stalled-verify");
    c.secret(&[("current", LIVE)]);
    let mut cmd = c.cmd();
    cmd.env("BOSS_JOBS_URL", format!("http://127.0.0.1:{port}"))
        .env("BOSS_DEPOSIT_VERIFY_BOUND_S", "2");
    let started = std::time::Instant::now();
    let (rc, text) = c.run_cmd(cmd);
    let took = started.elapsed().as_secs();
    assert!(
        took < 20,
        "the verify waited {took}s on an API that never answers: {text}"
    );
    assert_eq!(rc, 0, "{text}");
    assert!(
        c.summary("machine_token_effect").starts_with("UNVERIFIED"),
        "{text}"
    );
    assert_eq!(
        c.slot("current").as_deref(),
        Some(LIVE),
        "the slot was still written"
    );

    // The three bounds, read from the files that set them.
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
    let whole = default_of(&converge, "BOSS_FORGE_DEPOSIT_BOUND_S");
    // Up to four reads (three passes and the re-read of the last), each
    // with timeout's five-second kill; the verify, and its one retry
    // window.
    let worst = 4 * (read + 5) + 2 * verify;
    assert!(
        whole > worst,
        "forge-converge.sh bounds the deposit at {whole}s, inside its own worst case of {worst}s"
    );
    assert!(
        whole * 3 <= 600,
        "{whole}s is not well under a ten-minute tick"
    );
    assert!(
        script.contains("\"--request-timeout=${REQUEST_TIMEOUT_S}s\""),
        "kubectl ends its own request inside the container a killed client would leave running"
    );
    let live: Vec<&str> = converge
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(
        live.iter().any(|l| l.starts_with(
            "timeout -k 5 \"$FORGE_DEPOSIT_BOUND_S\" \"$INFRA/forge/machine-token-deposit.sh\""
        )),
        "the converge runs the deposit under a bound"
    );
}

/// A SHELL STARTED WITH TRACING ON PRINTS NOTHING OF A SLOT (review
/// 9a1e289b, F8). An exported SHELLOPTS=xtrace makes bash trace every
/// expansion from its first line; the script turns it off before it
/// reads anything. `run_cmd` refuses any fixture in the output.
#[test]
fn an_inherited_xtrace_prints_no_value() {
    let c = Case::new("xtrace");
    c.secret(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    c.api.accepts(LIVE, "current");
    let mut cmd = c.cmd();
    cmd.env("SHELLOPTS", "xtrace");
    let (rc, text) = c.run_cmd(cmd);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(c.slot("current").as_deref(), Some(LIVE));
}

/// A rule that is not the machine token's, and an invocation that names
/// no destination, are refused before anything is read.
#[test]
fn a_wrong_rule_or_invocation_is_refused() {
    let c = Case::new("refused");
    c.secret(&[("current", LIVE)]);
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .args([
            "--rule",
            &repo_root()
                .join("infra/dispatcher/rules/broker-rotates-the-forge-ops-runner-credential.toml")
                .display()
                .to_string(),
        ])
        .args(["--dest", &c.dest.display().to_string()])
        .env("BOSS_DEPOSIT_KUBECTL", &c.kubectl)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
    assert!(!c.dest.exists());
    let out = Command::new("bash")
        .arg(repo_root().join(SCRIPT))
        .args(["--rule", &repo_root().join(RULE).display().to_string()])
        .args(["--dest", "relative/path"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
}

/// THE ISOLATION, BY EFFECT: deposited as root, the tree gives another
/// account nothing — not a read, not a listing, and not through the one
/// reader every caller uses, which finds no slot and sends unstamped,
/// in silence. That other account is what a car's probe runs as on the
/// forge (BOSS_PROBE_USER). Needs root to make a root-owned tree; under
/// any other uid it says it did not run rather than pass.
#[test]
fn a_non_root_account_cannot_read_the_token_path() {
    let me = std::fs::metadata("/proc/self").unwrap().uid();
    let setpriv = Command::new("setpriv").arg("--version").output();
    if me != 0 || setpriv.is_err() {
        eprintln!(
            "NOT RUN: uid {me} cannot make a root-owned tree and read it as another account \
             (needs root and setpriv). The owner-only mode bits are held by the_tree_is_owner_only; \
             this leg was run as root on the dev pod for the car's receipt."
        );
        return;
    }
    let c = Case::new("isolation");
    // The scratch root must be traversable by the other account, so the
    // only thing between it and the token is the deposit's own tree.
    for d in [
        c.root.parent().unwrap().to_path_buf(),
        c.root.clone(),
        c.root.join("etc-boss"),
    ] {
        // mode-bits-ok: scratch directories, opened so the tree's own mode is what refuses
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    c.secret(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    c.api.accepts(LIVE, "current");
    let (rc, text) = c.run();
    assert_eq!(rc, 0, "{text}");
    // The reader, COPIED into the scratch this test opened: sourced from
    // the checkout, the reader leg answered Permission denied in any
    // checkout the other account cannot traverse, and went red on
    // correct code (review 9a1e289b, F6).
    let reader_copy = c.root.join("etc-boss").join("secret-header.sh");
    std::fs::copy(repo_root().join(READER), &reader_copy).unwrap();
    // mode-bits-ok: a sourced library, opened to the other account, never exec'd
    std::fs::set_permissions(&reader_copy, std::fs::Permissions::from_mode(0o644)).unwrap();
    let as_probe = |script: &str| {
        Command::new("setpriv")
            .args([
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "bash",
                "-c",
                script,
            ])
            .env("DIR", &c.dest)
            .env("BESIDE", c.root.join("etc-boss"))
            .env("LIB", &reader_copy)
            .env("TMPDIR", c.root.join("tmp"))
            .env_remove("RUNTIME_DIRECTORY")
            .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
            .output()
            .unwrap()
    };
    // Control first: the same account CAN read a file it is allowed to,
    // so an empty hand below is the tree's doing and not the harness's.
    let open = c.root.join("etc-boss").join("open");
    write_file(&open, "control");
    // mode-bits-ok: a data file, opened so the control read is allowed
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = as_probe("cat \"$BESIDE/open\"");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "control",
        "the control read works: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for slot in ["current", "next", "previous"] {
        let out = as_probe(&format!("cat \"$DIR/{slot}\""));
        assert!(!out.status.success(), "{slot} was read by another account");
        assert!(
            out.stdout.is_empty(),
            "{slot} gave bytes to another account"
        );
    }
    let out = as_probe("ls -A \"$DIR\"");
    assert!(
        !out.status.success() && out.stdout.is_empty(),
        "the tree was listed"
    );
    let out = as_probe(
        ". \"$LIB\"; BOSS_MACHINE_TOKEN_DIR=\"$DIR\" machine_token_header H http://127.0.0.1:7900; \
         echo \"rc=$? header=[$H]\"",
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "rc=0 header=[]",
        "the shared reader hands another account no header and stops nothing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And root still reads it: the tree is not merely unreadable.
    let out = Command::new("bash")
        .args(["-c", ". \"$LIB\"; trap : EXIT; BOSS_MACHINE_TOKEN_DIR=\"$DIR\" machine_token_header H http://127.0.0.1:7900; [ -n \"$H\" ] && echo stamped"])
        .env("DIR", &c.dest)
        .env("LIB", &reader_copy)
        .env("TMPDIR", c.root.join("tmp"))
        .env_remove("RUNTIME_DIRECTORY")
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "stamped");
}

/// The script's slot bound is boss-core's, and its slot names are the
/// rotation's (CLAUDE.md §9a: a fact that lives twice gets an equality
/// test).
#[test]
fn the_slot_bound_and_names_are_cores() {
    let script = std::fs::read_to_string(repo_root().join(SCRIPT)).unwrap();
    assert!(
        script.contains(&format!(
            "MAX_SLOT_BYTES={}",
            boss_core::machine_token::MAX_SLOT_BYTES
        )),
        "{SCRIPT} must carry boss-core's MAX_SLOT_BYTES"
    );
    assert!(script.contains("SLOTS=\"current next previous\""));
    assert!(
        script.contains(&format!(
            "\"${{DEST}}/{}\"",
            boss_core::machine_token::CURRENT
        )) || script.contains(&format!("\"$DEST/{}\"", boss_core::machine_token::CURRENT)),
        "the slot a caller reads is core's CURRENT"
    );
}

/// THE CONVERGE CALLS IT BEFORE ITS FIRST STEP THAT CAN FAIL, hands it
/// the directory the shared reader defaults to, and never carries its
/// exit into its own: the converge is the loop that installs this host's
/// repairs and owes the token nothing.
#[test]
fn the_converge_deposits_first_and_owes_it_nothing() {
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
    let deposit = at("forge/machine-token-deposit.sh");
    let fetch = at("checkout_git '$REPO' fetch");
    assert!(
        deposit < fetch,
        "the machine token's deposit must run BEFORE the fetch — the first step `set -e` can \
         end the converge on — or a broken forge token would stop the token following its Secret"
    );
    let call = live[deposit..deposit + 3].join("\n");
    assert!(
        call.contains("broker-rotates-the-machine-token.toml"),
        "the deposit reads the machine token's own rule: {call}"
    );
    assert!(
        call.contains("${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}"),
        "the destination is the directory machine_token_header defaults to: {call}"
    );
    let reader = std::fs::read_to_string(repo_root().join(READER)).unwrap();
    assert!(
        reader.contains("\"${BOSS_MACHINE_TOKEN_DIR:-/etc/boss/machine-token}\""),
        "{READER} no longer defaults to the directory the deposit fills"
    );
    assert_eq!(
        boss_core::machine_token::DEFAULT_TOKEN_DIR,
        "/etc/boss/machine-token"
    );
    for l in &live {
        assert!(
            !(l.contains("exit") && l.contains("machine_token_rc")),
            "the converge must never exit on the machine token's deposit: {l}"
        );
    }
}

/// The account a car's recorded probe runs as on the forge when nothing
/// overrides it — boss-cli prove.rs `Shell::unattended`.
const PROBE_USER: &str = "david";

/// THE FORGE UNITS THAT STILL RUN AS THE PROBE'S ACCOUNT, each with what
/// it needs of that account. They cannot read a root-only token, so
/// their calls go out UNSTAMPED: a known miss, listed, rather than a
/// token handed to the uid a probe shares. A group the probe user does
/// not hold would not change that — the unit's header file would still
/// be written as this uid (secret-header.sh), where a probe running
/// beside it reads it.
const UNSTAMPED: &[(&str, &str)] = &[
    (
        "cluster-deploy-runner",
        "fetches and checks out the owner's clone, builds with the owner's rootless docker \
         daemon and keeps its stamps under $HOME; it is the loop that deploys every fix",
    ),
    (
        "cluster-watchdog",
        "reads the stamps the deploy runner leaves under the owner's $HOME and keeps its own \
         dark and blind state there; it is the arm that acts when the record is dark",
    ),
    (
        "disk-floor-sweep",
        "prunes the owner's rootless docker daemon; as root a cache-deleting sweep would hold \
         root's reach for the sake of two stamped calls an hour",
    ),
];

fn forge_unit_users() -> Vec<(String, String)> {
    let dir = repo_root().join("infra/forge");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "service") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let user = text
            .lines()
            .find_map(|l| l.strip_prefix("User="))
            .unwrap_or("root")
            .to_string();
        out.push((p.file_stem().unwrap().to_string_lossy().into_owned(), user));
    }
    out.sort();
    out
}

/// WHO ON THE FORGE CAN READ THE TOKEN, held to the tree: the probe's
/// account is not root, the host observer moved to root with a spool
/// only root writes, and the units that still run as the probe's account
/// are exactly the roster above — a unit that joins or leaves it is a
/// decision someone makes here, with its reason.
#[test]
fn the_units_that_share_the_probes_account_are_the_unstamped_roster() {
    let prove =
        std::fs::read_to_string(repo_root().join("crates/orchestrators/boss-cli/src/prove.rs"))
            .unwrap();
    let line = prove
        .lines()
        .find(|l| l.contains("var(\"BOSS_PROBE_USER\")"))
        .expect("prove.rs reads BOSS_PROBE_USER");
    assert!(
        line.contains(&format!("\"{PROBE_USER}\"")),
        "the probe's default account moved ({line}); the roster below is about the account a \
         probe runs as, so re-derive it"
    );
    assert_ne!(PROBE_USER, "root");

    let users = forge_unit_users();
    let shared: Vec<&str> = users
        .iter()
        .filter(|(_, u)| u == PROBE_USER)
        .map(|(n, _)| n.as_str())
        .collect();
    let want: Vec<&str> = UNSTAMPED.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        shared, want,
        "the forge units running as `{PROBE_USER}` (a car probe's account) must be exactly the \
         UNSTAMPED roster: each cannot read the root-only machine token, and each needs a reason"
    );
    for (name, why) in UNSTAMPED {
        assert!(why.len() > 40, "{name}: say what it needs of the account");
    }
    for (name, user) in &users {
        assert!(
            user == "root" || user == PROBE_USER,
            "{name}.service runs as `{user}`: decide whether that account may read the machine \
             token before adding a third kind of reader"
        );
    }

    let observer =
        std::fs::read_to_string(repo_root().join("infra/forge/estate-observe-host.service"))
            .unwrap();
    assert!(
        observer.lines().any(|l| l == "User=root"),
        "the forge's host observer reads the root-only token, so it runs as root"
    );
    let spool = observer
        .lines()
        .find_map(|l| l.strip_prefix("Environment=SPOOL_DIR="))
        .expect("the root observer names its own spool");
    assert!(
        spool.starts_with("/var/lib/boss/"),
        "root must not replay a spool the probe's account can write ({spool}): its replay \
         reads every file there and POSTs it"
    );
    // Its header file lives on tmpfs systemd removes, and it dumps no
    // core (review 9a1e289b, F7) — without a mount namespace.
    for line in [
        "RuntimeDirectory=estate-observe-host",
        "RuntimeDirectoryMode=0700",
        "LimitCORE=0",
    ] {
        assert!(
            observer.lines().any(|l| l == line),
            "the root observer's unit must carry `{line}`"
        );
    }
    for forbidden in [
        "PrivateTmp=",
        "ProtectSystem=",
        "ProtectHome=",
        "ReadOnlyPaths=",
    ] {
        assert!(!observer.lines().any(|l| l.starts_with(forbidden)));
    }
}
