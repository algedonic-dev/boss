//! The estate machine token reaches boss-gcp by a PUSH from the cluster
//! (design-doc bdc60b65, question gcp-push, David 2026-10-06; backlog
//! 88379df3): `infra/cluster/machine-token-push.sh` pipes the three slots
//! over ssh, on a broker-prepared key forced to one command, to the
//! `machine-token` purpose of `infra/gcp/ops-credential-recv.sh`; and
//! boss-gcp's converge states the effect with
//! `infra/estate/machine-token-effect.sh`.
//!
//! HOW THIS IS MEASURED. Every script runs for real. The receiver is
//! handed its deposit on stdin with its three test knobs pointed into
//! scratch; the sender runs against a stub `ssh` that either refuses the
//! key the way sshd does or hands its stdin to the REAL receiver, so a
//! push that passes here passed the receiver's own judgement; the effect
//! check asks a tiny HTTP gate on 127.0.0.1. Fixture values are fake, and
//! every case checks that none reaches an output, an argv or a request
//! line — not even its last eight.
//!
//! WHAT IT CANNOT HOLD: that sshd on the real host applies the
//! forced-command line as written, and that the gate never builds the
//! image — the name the manifest runs is held equal to the name the
//! Dockerfile copies instead.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const RECV: &str = "infra/gcp/ops-credential-recv.sh";
const INSTALLER: &str = "infra/gcp/install-ops-credential-receiver.sh";
const PUSH: &str = "infra/cluster/machine-token-push.sh";
const EFFECT: &str = "infra/estate/machine-token-effect.sh";
const RULE: &str = "infra/dispatcher/rules/broker-prepares-the-machine-token-deposit-key.toml";
const DECLARATION: &str = "infra/gcp/machine-token-deposit-declaration.toml";
const MANIFEST: &str = "infra/cluster/manifests/boss-machine-token-push.yaml";
const BROKER: &str = "infra/cluster/manifests/boss-credential-broker.yaml";
const DOCKERFILE: &str = "infra/oss-quickstart/Dockerfile";
const GCP_UNIT: &str = "infra/gcp/boss-gcp-converge.service";
const GCP_CONVERGE: &str = "infra/gcp/boss-gcp-converge.sh";

/// The one command the key may run, as three files must spell it.
const FORCED: &str = "exec sudo -n /usr/local/libexec/boss/ops-credential-recv machine-token";

// 43-character base64url fixtures, fake — the shape the broker mints.
const OLDER: &str = "prevPREVprevPREVprevPREVprevPREVprevPREV001";
const LIVE: &str = "currCURRcurrCURRcurrCURRcurrCURRcurrCURR002";
const STAGED: &str = "nextNEXTnextNEXTnextNEXTnextNEXTnextNEXT003";
const FIXTURES: [&str; 3] = [OLDER, LIVE, STAGED];

fn clean(text: &str) {
    for t in FIXTURES {
        assert!(!text.contains(t), "a slot's value reached:\n{text}");
        assert!(
            !text.contains(&t[t.len() - 8..]),
            "the last eight of a slot reached:\n{text}"
        );
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

// --- the receiver --------------------------------------------------------

struct Recv {
    root: PathBuf,
    dir: PathBuf,
    stage: PathBuf,
}

impl Recv {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("machine-token-recv-{name}"));
        let stage = root.join("run");
        create_dir(&stage);
        Self {
            dir: root.join("etc-boss").join("machine-token"),
            root,
            stage,
        }
    }

    fn held(&self, slots: &[(&str, &str)]) {
        create_dir(&self.dir);
        for (k, v) in slots {
            std::fs::write(self.dir.join(k), v).unwrap();
        }
    }

    fn slot(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(name)).ok()
    }

    /// Run the receiver with `args`, `stdin` on its stdin.
    fn run(&self, args: &[&str], stdin: &[u8]) -> (i32, String) {
        create_dir(self.dir.parent().unwrap());
        let mut child = Command::new("bash")
            .arg(repo_root().join(RECV))
            .args(args)
            .env("BOSS_RECV_TOKEN_DIR", &self.dir)
            .env("BOSS_RECV_STAGE_DIR", &self.stage)
            .env("BOSS_RECV_OWNER", "")
            .env("BOSS_OPS_DIR", self.root.join("ops"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run ops-credential-recv.sh");
        // The receiver may stop reading early (an oversize deposit).
        let _ = child.stdin.take().unwrap().write_all(stdin);
        let out = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        clean(&text);
        assert_eq!(
            std::fs::read_dir(&self.stage).unwrap().count(),
            0,
            "the staged deposit must not outlive the run: {text}"
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

fn deposit(current: &str, next: &str, previous: &str) -> Vec<u8> {
    format!("current={current}\nnext={next}\nprevious={previous}\n").into_bytes()
}

/// Three slots land where `machine_token_header` reads them, owner-only,
/// and the answer names each slot's LENGTH — what the sender reads back.
#[test]
fn the_receiver_installs_three_slots_owner_only_and_reports_lengths() {
    let r = Recv::new("three");
    let (rc, text) = r.run(&["machine-token"], &deposit(LIVE, STAGED, OLDER));
    assert_eq!(rc, 0, "{text}");
    assert_eq!(r.slot("current").as_deref(), Some(LIVE));
    assert_eq!(r.slot("next").as_deref(), Some(STAGED));
    assert_eq!(r.slot("previous").as_deref(), Some(OLDER));
    assert_eq!(mode(&r.dir), 0o700, "{text}");
    for slot in ["current", "next", "previous"] {
        assert_eq!(mode(&r.dir.join(slot)), 0o600, "{slot}: {text}");
    }
    assert!(
        text.contains(
            "machine-token: received current 43 bytes, next 43 bytes, previous 43 bytes;"
        ),
        "{text}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&r.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["current", "next", "previous"],
        "no temp file is left"
    );

    // A blank slot is REMOVED, an equal one only held at its mode.
    // mode-bits-ok: a data file, loosened to be re-tightened
    std::fs::set_permissions(
        r.dir.join("current"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let (rc, text) = r.run(&["machine-token"], &deposit(LIVE, "", ""));
    assert_eq!(rc, 0, "{text}");
    assert_eq!(r.slot("current").as_deref(), Some(LIVE));
    assert!(
        r.slot("next").is_none() && r.slot("previous").is_none(),
        "{text}"
    );
    assert_eq!(mode(&r.dir.join("current")), 0o600);
    assert!(text.contains("removed: next previous"), "{text}");
    assert!(text.contains("unchanged: current"), "{text}");
}

/// THE HOSTILE-SENDER MATRIX. Whoever holds the deposit key chooses the
/// bytes on stdin and nothing else; every shape below is refused BEFORE
/// any slot is touched, the slots already held are byte-for-byte what
/// they were, nothing is created outside the token's directory, and no
/// value is printed.
#[test]
fn a_hostile_deposit_is_refused_and_changes_nothing() {
    let long = "A".repeat(4097);
    let huge = "A".repeat(40_000);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("oversize", deposit(&huge, "", "")),
        ("slot past the bound", deposit(&long, "", "")),
        (
            "path traversal in a value",
            deposit("../../etc/passwd", "", ""),
        ),
        ("absolute path as a value", deposit("/etc/shadow", "", "")),
        (
            "a slot named as a path",
            b"../current=AAAA\nnext=\nprevious=\n".to_vec(),
        ),
        (
            "a slot the sender invents",
            b"current=AAAA\nnext=\nauthorized_keys=BBBB\n".to_vec(),
        ),
        (
            "a fourth slot",
            [deposit(LIVE, "", ""), b"extra=AAAA\n".to_vec()].concat(),
        ),
        (
            "a fourth, blank, line",
            [deposit(LIVE, "", ""), b"\n".to_vec()].concat(),
        ),
        (
            "slots out of order",
            format!("next=\ncurrent={LIVE}\nprevious=\n").into_bytes(),
        ),
        (
            "current twice",
            format!("current={LIVE}\ncurrent={STAGED}\nprevious=\n").into_bytes(),
        ),
        (
            "two lines only",
            format!("current={LIVE}\nnext=\n").into_bytes(),
        ),
        (
            "no separator",
            format!("current {LIVE}\nnext=\nprevious=\n").into_bytes(),
        ),
        ("a blank current", deposit("", STAGED, OLDER)),
        ("two words", deposit("two words", "", "")),
        ("a value carrying =", deposit("abc=def", "", "")),
        ("shell text", deposit("$(id)", "", "")),
        (
            "leading space",
            format!(" current={LIVE}\nnext=\nprevious=\n").into_bytes(),
        ),
        (
            "cut short, no final newline",
            format!("current={LIVE}\nnext=\nprevious=").into_bytes(),
        ),
        (
            "CR line ends",
            format!("current={LIVE}\r\nnext=\r\nprevious=\r\n").into_bytes(),
        ),
        (
            "a NUL byte",
            format!("current={LIVE}\0\nnext=\nprevious=\n").into_bytes(),
        ),
        (
            "binary",
            vec![
                0xff, 0xfe, 0x00, 0x01, 0x80, b'\n', 0x7f, b'\n', 0x00, b'\n',
            ],
        ),
        (
            "a kubeconfig",
            b"apiVersion: v1\nkind: Config\nusers:\n".to_vec(),
        ),
    ];
    for (leg, bytes) in cases {
        let r = Recv::new("hostile");
        r.held(&[("current", OLDER), ("previous", STAGED)]);
        let (rc, text) = r.run(&["machine-token"], &bytes);
        assert_eq!(rc, 65, "[{leg}] must be REFUSED (65): {text}");
        assert!(text.contains("REFUSED"), "[{leg}] {text}");
        assert_eq!(r.slot("current").as_deref(), Some(OLDER), "[{leg}]");
        assert_eq!(r.slot("previous").as_deref(), Some(STAGED), "[{leg}]");
        assert!(r.slot("next").is_none(), "[{leg}]");
        let mut names: Vec<String> = std::fs::read_dir(&r.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["current", "previous"],
            "[{leg}] nothing else was made"
        );
        let beside: Vec<String> = std::fs::read_dir(r.dir.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            beside,
            ["machine-token"],
            "[{leg}] nothing beside the directory"
        );
    }
}

/// THE PURPOSE IS ONE FIXED WORD, and it is the only thing the caller's
/// side says. A wrong purpose, a purpose dressed as a path, a second
/// argument and no argument are refused as invocations (78), with stdin
/// never read into a slot.
#[test]
fn a_wrong_purpose_or_an_extra_argument_is_refused() {
    let argvs: Vec<Vec<&str>> = vec![
        vec![],
        vec!["talosconfig"],
        vec!["machine-token", "kubeconfig"],
        vec!["machine-token", "/etc/boss/machine-token"],
        vec!["machine-token/../kubeconfig"],
        vec!["machine-token "],
        vec!["MACHINE-TOKEN"],
        vec!["--dir=/tmp", "machine-token"],
        vec!["machine-token;id"],
    ];
    for argv in argvs {
        let r = Recv::new("argv");
        r.held(&[("current", OLDER)]);
        let (rc, text) = r.run(&argv, &deposit(LIVE, STAGED, ""));
        assert_eq!(rc, 78, "{argv:?} must be refused as an invocation: {text}");
        assert_eq!(r.slot("current").as_deref(), Some(OLDER), "{argv:?}");
        assert!(r.slot("next").is_none(), "{argv:?}");
    }
}

/// A destination that is not the token's own directory is refused, a
/// slot path something was planted at fails naming it with the slots
/// before it whole, and a deposit a killed run left staged is swept.
#[test]
fn a_planted_destination_and_a_stale_stage_are_handled() {
    let r = Recv::new("link");
    let elsewhere = r.root.join("elsewhere");
    create_dir(&elsewhere);
    create_dir(r.dir.parent().unwrap());
    std::os::unix::fs::symlink(&elsewhere, &r.dir).unwrap();
    let (rc, text) = r.run(&["machine-token"], &deposit(LIVE, "", ""));
    assert_eq!(rc, 65, "{text}");
    assert_eq!(
        std::fs::read_dir(&elsewhere).unwrap().count(),
        0,
        "written through a link"
    );

    let r = Recv::new("slot-dir");
    create_dir(&r.dir.join("next").join("planted"));
    let (rc, text) = r.run(&["machine-token"], &deposit(LIVE, STAGED, ""));
    assert_eq!(rc, 1, "{text}");
    assert_eq!(r.slot("current").as_deref(), Some(LIVE));
    assert!(text.contains("next"), "{text}");

    let r = Recv::new("stale");
    write_file(
        &r.stage.join(".machine-token.recv.AbCdEf"),
        "current=leftover\n",
    );
    let (rc, text) = r.run(&["machine-token"], &deposit(LIVE, "", ""));
    assert_eq!(rc, 0, "{text}");
}

/// The sudoers fragment grants the receiver for exactly its two
/// purposes, each on its own line, and the installer prints the machine
/// token's forced command as the rule and the sender spell it — while
/// the break-glass key's line is the one it always was.
#[test]
fn the_installer_grants_two_exact_purposes_and_widens_neither() {
    let root = scratch_dir("machine-token-recv-install");
    for d in ["libexec", "sudoers"] {
        create_dir(&root.join(d));
    }
    write_exec(&root.join("visudo"), "#!/bin/sh\nexit 0\n");
    let out = Command::new("bash")
        .arg(repo_root().join(INSTALLER))
        .env("INSTALL_RECV_LIBEXEC", root.join("libexec"))
        .env("INSTALL_RECV_SUDOERS_DIR", root.join("sudoers"))
        .env("INSTALL_VISUDO", root.join("visudo"))
        .env("INSTALL_RECV_USER", "depositor")
        .env("INSTALL_RECV_OWNER", "")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{text}");
    let sudoers = std::fs::read_to_string(root.join("sudoers/boss-ops-credential-recv")).unwrap();
    let grants: Vec<&str> = sudoers.lines().filter(|l| l.contains("NOPASSWD")).collect();
    assert_eq!(
        grants,
        [
            "depositor ALL=(root) NOPASSWD: /usr/local/libexec/boss/ops-credential-recv kubeconfig",
            "depositor ALL=(root) NOPASSWD: /usr/local/libexec/boss/ops-credential-recv machine-token",
        ],
        "two grants, each naming its whole command line — never a wildcard"
    );
    assert!(!sudoers.contains('*'), "{sudoers}");
    assert!(
        text.contains(&format!(
            "command=\"{FORCED}\",restrict <the machine token deposit key's public half>"
        )),
        "the installer prints the machine token's forced command: {text}"
    );
    assert!(
        text.contains(
            "command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig\",restrict <the deposit key's public half>"
        ),
        "the break-glass key's line is unchanged: {text}"
    );
    // The break-glass sender still forces `kubeconfig` and nothing else.
    let glass =
        std::fs::read_to_string(repo_root().join("infra/cluster/break-glass-deposit.sh")).unwrap();
    assert!(
        glass.contains(
            "FORCED='command=\"exec sudo -n /usr/local/libexec/boss/ops-credential-recv kubeconfig\",restrict'"
        ) && !glass.contains("machine-token"),
        "the break-glass deposit must not learn the machine token's purpose"
    );
}

// --- the sender ----------------------------------------------------------

/// A stub ssh. `SSH_MODE` picks what the far side does, and every mode
/// that can be handed bytes without being the receiver records them in
/// `SSH_STDIN`, so "no token byte was sent" is a file's length and not a
/// reading of the sender. `recv` hands stdin to the REAL receiver;
/// `denied`, `changed`, `odd-255` and `unanchored-255` answer as ssh
/// itself does before any channel exists, reading nothing; `lie` greets
/// as the receiver does and then answers wrong counts; `hang` never
/// answers. THE ONES THAT ARE NOT THE RECEIVER (backlog dea2236f):
/// `shell` is a real shell behind a login banner — what a key placed
/// without its `command=` reaches, the first live enrolment's own shape
/// (packet 96a0f7bb, 2026-10-07); `silent-shell` is that shell with no
/// banner, `slow-shell` one whose banner arrives after the greeting
/// bound; `old-recv` is the receiver as it stood before it greeted,
/// which reads its whole stdin first and refuses an empty one. Its argv
/// is logged so a value in it would be seen.
const STUB_SSH: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$SSH_LOG"
case "$SSH_MODE" in
  recv) exec bash "$SSH_RECV" machine-token ;;
  denied) echo "depositor@boss-gcp: Permission denied (publickey)." >&2; exit 255 ;;
  lie) echo "ops-credential-recv: machine-token: ready for three slots"; cat > /dev/null; echo "ops-credential-recv: machine-token: received current 1 bytes, next 0 bytes, previous 0 bytes; x"; exit 0 ;;
  changed) echo "Host key verification failed." >&2; exit 255 ;;
  far-denied) echo "mktemp: failed to create file via template '/run/.machine-token.recv.XXXXXX': Permission denied" >&2; echo "ops-credential-recv: FAILED — cannot stage the deposit in /run" >&2; exit 1 ;;
  sudo-denied) echo "sudo: unable to execute /usr/local/libexec/boss/ops-credential-recv: Permission denied" >&2; exit 126 ;;
  odd-255) echo "kex_exchange_identification: Permission denied by peer" >&2; exit 255 ;;
  far-publickey) echo "git@forge: Permission denied (publickey)." >&2; exit 1 ;;
  unanchored-255) echo "sshd: Permission denied (publickey) for the forwarded agent" >&2; exit 255 ;;
  hang) exec sleep 30 ;;
  shell) echo "Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-1015-gcp x86_64)"; echo; echo " * Documentation:  https://help.ubuntu.com"; tee "$SSH_STDIN" | bash --noprofile --norc; exit 0 ;;
  silent-shell) tee "$SSH_STDIN" | bash --noprofile --norc; exit 0 ;;
  slow-shell) sleep 2; echo "Last login: Wed Oct  7 14:41:10 2026 from 10.20.0.34"; tee "$SSH_STDIN" | bash --noprofile --norc; exit 0 ;;
  old-recv) n="$(tee "$SSH_STDIN" | wc -c)"; if [ "$n" -eq 0 ]; then echo "ops-credential-recv: REFUSED — an empty deposit — the transfer carried nothing; /etc/boss/machine-token left as it was" >&2; exit 65; fi; echo "ops-credential-recv: machine-token: received current 43 bytes, next 0 bytes, previous 0 bytes; x"; exit 0 ;;
  late-recv) sleep 2; exec bash "$SSH_RECV" machine-token ;;
  near-greeting) echo "ops-credential-recv: machine-token: ready for three slots, honest"; tee "$SSH_STDIN" > /dev/null; exit 0 ;;
esac
"#;

struct Push {
    root: PathBuf,
    recv: Recv,
}

impl Push {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("machine-token-push-{name}"));
        for d in ["bin", "keys", "known", "token", "work"] {
            create_dir(&root.join(d));
        }
        write_exec(&root.join("bin/ssh"), STUB_SSH);
        write_file(&root.join("ssh.log"), "");
        write_file(
            &root.join("known/known_hosts"),
            "boss-gcp ssh-ed25519 AAAAfixture\n",
        );
        Self {
            recv: Recv::new(&format!("push-{name}")),
            root,
        }
    }

    fn key(&self) {
        write_file(
            &self.root.join("keys/private_key"),
            // Not key material and not shaped like it: the stub ssh never
            // parses what `-i` names.
            "fixture-not-a-key\nsecond-line-of-the-fixture",
        );
        write_file(&self.root.join("keys/public_key"), "ssh-rsa AAAAfixture");
        write_file(
            &self.root.join("keys/minted_for"),
            "1d31a6a5-1c9e-47bb-bff2-b0421e6960b7",
        );
    }

    fn token(&self, slots: &[(&str, &str)]) {
        for (k, v) in slots {
            write_file(&self.root.join("token").join(k), v);
        }
    }

    fn run(&self, mode: &str, extra: &[(&str, &str)]) -> (i32, String) {
        create_dir(self.recv.dir.parent().unwrap());
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join(PUSH))
            .env("PATH", path)
            .env("TMPDIR", self.root.join("work"))
            .env("BOSS_PUSH_KEY_DIR", self.root.join("keys"))
            .env("BOSS_PUSH_KNOWN_HOSTS", self.root.join("known/known_hosts"))
            .env("BOSS_PUSH_TARGET", "depositor@boss-gcp")
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("token"))
            .env("SSH_MODE", mode)
            .env("SSH_LOG", self.root.join("ssh.log"))
            .env("SSH_STDIN", self.root.join("ssh.stdin"))
            .env("SSH_RECV", repo_root().join(RECV))
            .env("BOSS_RECV_TOKEN_DIR", &self.recv.dir)
            .env("BOSS_RECV_STAGE_DIR", &self.recv.stage)
            .env("BOSS_RECV_OWNER", "");
        for (k, v) in extra {
            c.env(k, v);
        }
        let out = c.output().expect("run machine-token-push.sh");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        clean(&text);
        clean(&std::fs::read_to_string(self.root.join("ssh.log")).unwrap());
        assert_eq!(
            std::fs::read_dir(self.root.join("work")).unwrap().count(),
            0,
            "the key's private copy and ssh's output must not outlive the run: {text}"
        );
        (out.status.code().unwrap_or(-1), text)
    }

    /// What a far side that is not the receiver was handed on its stdin:
    /// `None` when it was never reached, else the byte count.
    fn far_stdin(&self) -> Option<u64> {
        std::fs::metadata(self.root.join("ssh.stdin"))
            .ok()
            .map(|m| m.len())
    }

    fn sent(&self) -> usize {
        std::fs::read_to_string(self.root.join("ssh.log"))
            .unwrap()
            .lines()
            .count()
    }
}

/// An enrolled key and a minted token: the three slots land on the far
/// side through the real receiver, and the receiver's lengths are read
/// back against what was sent.
#[test]
fn the_push_delivers_three_slots_and_reads_the_lengths_back() {
    let p = Push::new("ok");
    p.key();
    p.token(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    let (rc, text) = p.run("recv", &[]);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(p.recv.slot("current").as_deref(), Some(LIVE));
    assert_eq!(p.recv.slot("next").as_deref(), Some(STAGED));
    assert_eq!(p.recv.slot("previous").as_deref(), Some(OLDER));
    assert!(
        text.contains("depositor@boss-gcp answered first as the receiver; sending the three slots"),
        "the record says the receiver spoke before anything was sent: {text}"
    );
    assert!(
        text.contains(
            "pushed: depositor@boss-gcp received current 43 bytes, next 43 bytes, previous 43 bytes"
        ),
        "{text}"
    );
    let argv = std::fs::read_to_string(p.root.join("ssh.log")).unwrap();
    assert!(
        argv.contains("StrictHostKeyChecking=yes"),
        "the host key is pinned: {argv}"
    );
    // Pinned to THIS file and to nothing else: without it ssh falls
    // back to the account's own known_hosts, and without the global one
    // set to /dev/null to the machine's (review ecd9263a, F5).
    let pin = p.root.join("known/known_hosts");
    assert!(
        argv.contains(&format!("UserKnownHostsFile={}", pin.display()))
            && argv.contains("GlobalKnownHostsFile=/dev/null")
            && argv.contains("-F /dev/null"),
        "the pinned known_hosts is the only one ssh may read: {argv}"
    );
    assert!(
        argv.trim_end().ends_with("depositor@boss-gcp"),
        "no remote command is sent: {argv}"
    );

    // A ROTATION: the mount now shows the promoted set, and the next
    // push makes boss-gcp hold exactly that — the blanked slot removed.
    std::fs::remove_file(p.root.join("token/next")).unwrap();
    p.token(&[("current", STAGED), ("previous", LIVE)]);
    let (rc, text) = p.run("recv", &[]);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(p.recv.slot("current").as_deref(), Some(STAGED));
    assert_eq!(p.recv.slot("previous").as_deref(), Some(LIVE));
    assert!(p.recv.slot("next").is_none(), "{text}");
}

/// UNTIL DAVID HAS ENROLLED, THE PUSH FAILS SOFT AND SAYS WHICH
/// ENROLLMENT IS MISSING: no key prepared (the packet is not filed or
/// scoped), a key boss-gcp does not accept (the enroll step's line is
/// not placed), no token minted. Each is exit 75 — `not-yet` on the
/// chore's packet — and none is a RED line.
#[test]
fn until_enrollment_the_push_is_not_yet_and_names_what_is_missing() {
    let p = Push::new("no-key");
    p.token(&[("current", LIVE)]);
    let (rc, text) = p.run("recv", &[]);
    assert_eq!(rc, 75, "{text}");
    assert!(
        text.contains("MISSING")
            && text.contains("prepare-the-machine-token-deposit-key")
            && text.contains("machine-token-deposit-key")
            && text.contains("scope"),
        "no key names the packet to file and the step to complete: {text}"
    );
    assert_eq!(p.sent(), 0, "nothing is sent without a key");

    let p = Push::new("denied");
    p.key();
    p.token(&[("current", LIVE)]);
    let (rc, text) = p.run("denied", &[]);
    assert_eq!(rc, 75, "{text}");
    assert!(
        text.contains("MISSING: the enrollment on boss-gcp")
            && text.contains("authorized_keys_line")
            && text.contains("enroll step")
            && text.contains("1d31a6a5-1c9e-47bb-bff2-b0421e6960b7")
            && text.contains(FORCED),
        "a refused key names the enroll step, its packet and the forced command: {text}"
    );

    let p = Push::new("unminted");
    p.key();
    let (rc, text) = p.run("recv", &[]);
    assert_eq!(rc, 75, "{text}");
    assert!(text.contains("no current slot"), "{text}");
    assert_eq!(p.sent(), 0);

    for p in [Push::new("a"), Push::new("b")] {
        p.key();
        p.token(&[("current", LIVE)]);
        let (_, text) = p.run("denied", &[]);
        assert!(!text.contains("RED "), "not-yet is not a RED line: {text}");
    }
}

/// EVERYTHING ELSE IS A NAMED REFUSAL: a host key that changed, lengths
/// that disagree, a transfer that hangs past its bound, no pinned host
/// key, a malformed slot in the mount — one RED line each, exit 1, and
/// a malformed slot is never sent at all.
#[test]
fn a_refused_push_is_one_red_line() {
    // The three after `changed` each carry the words `Permission denied`
    // and are NOT sshd refusing the key: the receiver failing after the
    // key was accepted (exit 1), sudo unable to run it (126), and ssh's
    // own 255 without sshd's `(publickey` shape. The first draft took
    // the words from anywhere in stderr and closed all three not-yet,
    // for ever (review ecd9263a, F3).
    //
    // The last two hold the two HALVES of the classifier apart (delta
    // review 76249509, mutants N1 and N3: each half could be dropped and
    // every test stayed green). `far-publickey` is sshd's exact refusal
    // line arriving with an exit that is NOT ssh's own 255 — what a far
    // side that itself runs ssh would print through ours; `unanchored-255`
    // is ssh's own 255 with the words `Permission denied (publickey` not
    // on sshd's `<account>@<host>:` line. BOTH ARE SYNTHETIC: the
    // receiver runs no ssh, and neither the reviewer nor this fold could
    // construct a real failure of either shape. They pin the stated rule
    // (quiet needs BOTH the exit and the line), not an observed incident.
    //
    // `late-recv` is the REAL receiver, ready after the bound this pass
    // waits for its first line (backlog dea2236f): it is handed nothing,
    // and it is a slow host on the push's own route — never the finding
    // that the key reaches a shell.
    let cases: [(&str, &str, &[(&str, &str)]); 9] = [
        (
            "late-recv",
            "said it was ready only after the 1 seconds",
            &[("BOSS_PUSH_GREETING_TIMEOUT_S", "1")],
        ),
        ("changed", "failed (ssh exit 255)", &[]),
        ("far-denied", "failed (ssh exit 1)", &[]),
        ("sudo-denied", "failed (ssh exit 126)", &[]),
        ("odd-255", "failed (ssh exit 255)", &[]),
        ("far-publickey", "failed (ssh exit 1)", &[]),
        ("unanchored-255", "failed (ssh exit 255)", &[]),
        ("lie", "is not proven", &[]),
        (
            "hang",
            "did not finish inside 1 seconds",
            &[("BOSS_PUSH_TIMEOUT_S", "1")],
        ),
    ];
    for (mode, want, extra) in cases {
        let p = Push::new(mode);
        p.key();
        p.token(&[("current", LIVE)]);
        let (rc, text) = p.run(mode, extra);
        assert_eq!(rc, 1, "[{mode}] {text}");
        let reds: Vec<&str> = text.lines().filter(|l| l.starts_with("RED ")).collect();
        assert_eq!(reds.len(), 1, "[{mode}] one RED line: {text}");
        assert!(
            reds[0].starts_with("RED machine-token-push refused: ") && reds[0].contains(want),
            "[{mode}] {text}"
        );
    }

    let p = Push::new("no-pin");
    p.key();
    p.token(&[("current", LIVE)]);
    std::fs::remove_file(p.root.join("known/known_hosts")).unwrap();
    let (rc, text) = p.run("recv", &[]);
    assert_eq!(rc, 1, "{text}");
    assert!(text.contains("no pinned host key"), "{text}");
    assert_eq!(p.sent(), 0);

    for bad in ["two words", "has/slash", "line\nbreak"] {
        let p = Push::new("malformed");
        p.key();
        p.token(&[("current", LIVE), ("previous", bad)]);
        let (rc, text) = p.run("recv", &[]);
        assert_eq!(rc, 1, "[{bad:?}] {text}");
        assert!(text.contains("nothing was sent"), "[{bad:?}] {text}");
        assert_eq!(p.sent(), 0, "[{bad:?}] a malformed slot set is never sent");
    }
}

/// The receiver's first line, which the sender requires before it writes
/// a slot. One literal in two scripts: held equal below.
const GREETING: &str = "ops-credential-recv: machine-token: ready for three slots";
/// The route a key that is not confined to the receiver is filed under —
/// its own, so an open item for some other push refusal cannot swallow
/// it (file-backlog-items-on-machine-token-push-red dedups by route).
const UNCONFINED: &str = "RED machine-token-deposit-key-unconfined refused: ";

/// A KEY PLACED WITHOUT ITS `command=` REACHES A SHELL, AND THE PUSH
/// HANDS THAT SHELL NOTHING (backlog dea2236f; packet 96a0f7bb,
/// 2026-10-07). On the first live enrolment the bare public key was
/// placed in authorized_keys, ssh accepted it, the far side answered
/// with a login banner, and the push — which wrote its three lines
/// before it read anything — put them on that shell's stdin and then
/// reported `no slot counts`. Here a real shell stands behind the stub:
/// the push must send it NO byte (the stub records its stdin, and the
/// file is empty), name the cause on a route of its own, and say what to
/// remove. A shell that prints no banner, one whose banner is late, and
/// a first line that merely begins like the greeting are the same
/// finding.
#[test]
fn a_key_that_reaches_a_shell_is_named_and_handed_no_token_byte() {
    let cases: [(&str, &str, &[(&str, &str)]); 4] = [
        ("shell", "answered with its own text", &[]),
        (
            "silent-shell",
            "said nothing",
            &[("BOSS_PUSH_GREETING_TIMEOUT_S", "1")],
        ),
        (
            "slow-shell",
            "said nothing",
            &[("BOSS_PUSH_GREETING_TIMEOUT_S", "1")],
        ),
        ("near-greeting", "answered with its own text", &[]),
    ];
    for (mode, want, extra) in cases {
        let p = Push::new(mode);
        p.key();
        p.token(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
        let (rc, text) = p.run(mode, extra);
        assert_eq!(rc, 1, "[{mode}] {text}");
        assert_eq!(
            p.far_stdin(),
            Some(0),
            "[{mode}] the far side was reached and handed NO byte: {text}"
        );
        let reds: Vec<&str> = text.lines().filter(|l| l.starts_with("RED ")).collect();
        assert_eq!(reds.len(), 1, "[{mode}] one RED line: {text}");
        let red = reds[0];
        assert!(red.starts_with(UNCONFINED), "[{mode}] {text}");
        for needle in [
            "this key reaches a shell on boss-gcp",
            want,
            "command=",
            "REMOVE THAT LINE NOW",
            "~depositor/.ssh/authorized_keys",
            "1d31a6a5-1c9e-47bb-bff2-b0421e6960b7",
            "No token byte was sent",
        ] {
            assert!(red.contains(needle), "[{mode}] wants `{needle}`: {text}");
        }
        assert!(
            !text.contains("no slot counts") && !text.contains("not yet"),
            "[{mode}] never the generic refusal, never quiet: {text}"
        );
        assert!(p.recv.slot("current").is_none(), "[{mode}]");
    }
}

/// THE RECEIVER SPEAKS FIRST, AND AN OLDER ONE THAT DOES NOT IS `not
/// yet`, NOT A SHELL. The sender and boss-gcp's receiver land by two
/// different converges, so for a while the new sender may face the
/// receiver that reads its whole stdin before it says anything. Silence
/// is answered by closing the stream with nothing written: that
/// receiver then refuses an empty deposit in its own words, which is
/// proof of WHAT is behind the key, so the pass closes not-yet naming
/// the converge that is owed — and still sends no byte.
#[test]
fn a_receiver_that_does_not_greet_yet_is_not_yet_and_is_sent_nothing() {
    let p = Push::new("old-recv");
    p.key();
    p.token(&[("current", LIVE)]);
    let (rc, text) = p.run("old-recv", &[("BOSS_PUSH_GREETING_TIMEOUT_S", "1")]);
    assert_eq!(rc, 75, "{text}");
    assert_eq!(p.far_stdin(), Some(0), "{text}");
    assert!(
        text.contains("MISSING: boss-gcp's converge") && !text.contains("RED "),
        "{text}"
    );

    // The greeting is ONE literal in the two scripts.
    for file in [PUSH, RECV] {
        assert!(read(file).contains(GREETING), "{file} carries the greeting");
    }
    for (name, value) in [
        ("BOSS_PUSH_GREETING_TIMEOUT_S", "0"),
        ("BOSS_PUSH_GREETING_TIMEOUT_S", "soon"),
    ] {
        let p = Push::new("bad-bound");
        p.key();
        p.token(&[("current", LIVE)]);
        let (rc, text) = p.run("recv", &[(name, value)]);
        assert_eq!(rc, 78, "{text}");
        assert_eq!(p.sent(), 0);
    }

    // And the real receiver says it BEFORE it has been handed a byte:
    // its stdin is held open and unwritten while the line is read.
    let r = Recv::new("greets-first");
    create_dir(r.dir.parent().unwrap());
    let mut child = Command::new("bash")
        .arg(repo_root().join(RECV))
        .arg("machine-token")
        .env("BOSS_RECV_TOKEN_DIR", &r.dir)
        .env("BOSS_RECV_STAGE_DIR", &r.stage)
        .env("BOSS_RECV_OWNER", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ops-credential-recv.sh");
    // `child.stdin` stays where it is — open, unwritten — until the line
    // has been read; only then is the deposit fed.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    assert_eq!(first.trim_end(), GREETING);
    boss_testing::feed_stdin(&mut child, &deposit(LIVE, "", ""));
    let mut rest = String::new();
    out.read_to_string(&mut rest).unwrap();
    assert!(child.wait().unwrap().success(), "{rest}");
    assert!(rest.contains("received current 43 bytes"), "{rest}");
    assert_eq!(r.slot("current").as_deref(), Some(LIVE));
}

// --- the effect, on boss-gcp ---------------------------------------------

/// A gate: `GET /api/machine-gate/accepts` answers the slot NAME the
/// presented header matches, or `none`.
fn gate(slots: Vec<(String, String)>) -> (u16, Arc<Mutex<Vec<(String, bool)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<(String, bool)>>> = Default::default();
    let s = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
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
            s.lock()
                .unwrap()
                .push((line.trim().to_string(), presented.is_some()));
            let matched = presented
                .as_deref()
                .and_then(|p| slots.iter().find(|(v, _)| v == p))
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| "none".into());
            let body = format!(
                r#"{{"service":"jobs","mode":"report","matched":"{matched}","degraded":false}}"#
            );
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.flush();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink);
        }
    });
    (port, seen)
}

fn effect(root: &Path, url: &str, extra: &[(&str, &str)]) -> (i32, String, String) {
    create_dir(&root.join("run"));
    create_dir(&root.join("tmp"));
    let out = Command::new("bash")
        .arg(repo_root().join(EFFECT))
        .env("BOSS_MACHINE_TOKEN_DIR", root.join("token"))
        .env("BOSS_JOBS_URL", url)
        .env("BOSS_EFFECT_WAIT", "1")
        .env("RUNTIME_DIRECTORY", root.join("run"))
        .env("TMPDIR", root.join("tmp"))
        .env("BOSS_RUN_SUMMARY_FILE", root.join("summary.json"))
        .env("BOSS_SOR_ENV", root.join("no-sor.env"))
        .env_remove("BOSS_MACHINE_TOKEN_HOSTS")
        .envs(extra.iter().copied())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let summary = std::fs::read_to_string(root.join("summary.json")).unwrap_or_default();
    clean(&text);
    clean(&summary);
    let v: serde_json::Value = serde_json::from_str(&summary).unwrap_or_default();
    (
        out.status.code().unwrap_or(-1),
        v["machine_token_effect"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        text,
    )
}

/// THE EFFECT IS THE GATE'S WORD, READ THROUGH THE HOST'S OWN READER, and
/// it never stops the converge that asks: every answer is exit 0. The
/// header file it sends lands in the unit's RUNTIME_DIRECTORY and is
/// gone when it ends — nothing under the shared temp root.
#[test]
fn the_effect_check_states_the_gates_answer_and_stops_nothing() {
    let root = scratch_dir("machine-token-effect-current");
    create_dir(&root.join("token"));
    write_file(&root.join("token/current"), LIVE);
    let (port, seen) = gate(vec![(LIVE.into(), "current".into())]);
    let (rc, effect_said, text) = effect(&root, &format!("http://127.0.0.1:{port}"), &[]);
    assert_eq!(rc, 0, "{text}");
    assert!(effect_said.starts_with("matched current"), "{text}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|(l, stamped)| l.starts_with("GET /api/machine-gate/accepts ") && *stamped),
        "a STAMPED read of the accepts route"
    );
    for dir in ["run", "tmp"] {
        assert_eq!(
            std::fs::read_dir(root.join(dir)).unwrap().count(),
            0,
            "nothing — the header file included — is left under {dir}/: {text}"
        );
    }

    let root = scratch_dir("machine-token-effect-none");
    create_dir(&root.join("token"));
    write_file(&root.join("token/current"), LIVE);
    let (port, _) = gate(vec![(OLDER.into(), "current".into())]);
    let (rc, effect_said, text) = effect(&root, &format!("http://127.0.0.1:{port}"), &[]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        effect_said.starts_with("NOT ACCEPTED"),
        "a file in place is not the proof: {text}"
    );

    let root = scratch_dir("machine-token-effect-absent");
    let (rc, effect_said, text) = effect(&root, "http://127.0.0.1:1", &[]);
    assert_eq!(rc, 0, "{text}");
    assert!(effect_said.starts_with("nothing to present"), "{text}");

    let root = scratch_dir("machine-token-effect-dark");
    create_dir(&root.join("token"));
    write_file(&root.join("token/current"), LIVE);
    let (rc, effect_said, text) = effect(&root, "http://127.0.0.1:1", &[]);
    assert_eq!(rc, 0, "{text}");
    assert!(
        effect_said.starts_with("UNVERIFIED"),
        "no answer is said, never read as a pass: {text}"
    );
}

/// THE WAITS ARE BOUNDED AND A TRACED SHELL PRINTS NOTHING (adversarial
/// review 9a1e289b of the forge's deposit, B1 and F8, folded here before
/// this car's own review). A jobs API that takes the connection and
/// never answers costs the effect check its bound and reads UNVERIFIED;
/// and with an exported SHELLOPTS=xtrace none of the three scripts
/// prints a slot — each helper refuses any fixture in what it captured.
#[test]
fn a_silent_gate_costs_one_bound_and_an_inherited_xtrace_prints_no_value() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = silent.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in silent.incoming().flatten() {
            // Held well past the bound, then dropped, so a script that
            // lost its bound fails this test instead of hanging it.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(45));
                drop(conn);
            });
        }
    });
    let root = scratch_dir("machine-token-effect-silent");
    create_dir(&root.join("token"));
    write_file(&root.join("token/current"), LIVE);
    let started = std::time::Instant::now();
    let (rc, said, text) = effect(
        &root,
        &format!("http://127.0.0.1:{port}"),
        &[("BOSS_EFFECT_BOUND_S", "2")],
    );
    let took = started.elapsed().as_secs();
    assert!(
        took < 20,
        "the effect check waited {took}s on a gate that never answers: {text}"
    );
    assert_eq!(rc, 0, "{text}");
    assert!(said.starts_with("UNVERIFIED"), "{text}");

    let root = scratch_dir("machine-token-effect-xtrace");
    create_dir(&root.join("token"));
    write_file(&root.join("token/current"), LIVE);
    let (port, _) = gate(vec![(LIVE.into(), "current".into())]);
    let (rc, said, text) = effect(
        &root,
        &format!("http://127.0.0.1:{port}"),
        &[("SHELLOPTS", "xtrace")],
    );
    assert_eq!(rc, 0, "{text}");
    assert!(said.starts_with("matched current"), "{text}");

    let p = Push::new("xtrace");
    p.key();
    p.token(&[("current", LIVE), ("next", STAGED), ("previous", OLDER)]);
    let (rc, text) = p.run("recv", &[("SHELLOPTS", "xtrace")]);
    assert_eq!(rc, 0, "{text}");
    assert_eq!(p.recv.slot("current").as_deref(), Some(LIVE));
}

// --- what the tree declares ----------------------------------------------

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn live(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

/// ONE FORCED COMMAND, SPELLED IN THREE PLACES AND HELD EQUAL: the rule
/// row the broker writes onto the enroll step from, the sender's own
/// not-yet line, and the installer's sudoers grant (CLAUDE.md §9a). And
/// the registry row the handler will demand is the row this tree carries
/// for the operator to publish.
#[test]
fn the_rule_the_sender_the_installer_and_the_declaration_agree() {
    let rule: toml::Value = toml::from_str(&read(RULE)).unwrap();
    let args = &rule["rule"][0]["do"][0]["args"];
    let arg = |k: &str| {
        args[k]
            .as_str()
            .unwrap_or_else(|| panic!("{RULE} has no arg {k}"))
            .trim_matches('"')
            .to_string()
    };
    assert_eq!(
        rule["rule"][0]["do"][0]["handler"].as_str(),
        Some("credential.prepare.ssh-deposit")
    );
    assert_eq!(arg("forced_command"), FORCED);
    assert_eq!(arg("receiver_host"), "boss-gcp");
    assert_eq!(
        arg("protocol_kind"),
        "prepare-the-machine-token-deposit-key"
    );
    assert_eq!(arg("secret_namespace"), "boss");
    let secret = arg("secret_name");
    assert_eq!(arg("credential_id"), secret);
    assert_ne!(arg("purpose"), "ops-runner credential deposit");
    assert!(
        rule["rule"][0]["when"]
            .as_str()
            .unwrap()
            .contains(&format!("subject_id = \"{secret}\"")),
        "the rule fires for its own credential's packet"
    );

    let push = read(PUSH);
    assert!(
        push.contains(&format!("FORCED_COMMAND='{FORCED}'")),
        "{PUSH}"
    );
    assert!(
        push.contains(&format!(
            "KEY_SECRET=\"${{BOSS_PUSH_KEY_SECRET:-{secret}}}\""
        )),
        "the sender names the Secret the rule declares"
    );
    let receiver = FORCED.trim_start_matches("exec sudo -n ");
    assert!(
        read(INSTALLER).contains("NOPASSWD: $RECEIVER machine-token")
            && receiver == "/usr/local/libexec/boss/ops-credential-recv machine-token",
        "the sudoers grant is the forced command's own argv"
    );

    let decl: toml::Value = toml::from_str(&read(DECLARATION)).unwrap();
    let rows = decl["credential"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "one row, and nothing but rows — it is appended verbatim"
    );
    assert_eq!(decl.as_table().unwrap().len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"].as_str(), Some(secret.as_str()));
    assert_eq!(row["kind"].as_str(), Some("ssh-transport-key"));
    assert_eq!(
        row["storage_location"].as_str(),
        Some(format!("Secret boss/{secret}").as_str()),
        "the handler refuses to mint unless the row says exactly this"
    );
    for forbidden in ["value", "private_key", "token"] {
        assert!(
            row.get(forbidden).is_none(),
            "a declaration never carries `{forbidden}`"
        );
    }

    // boss-core's bound, in both scripts that judge a slot.
    let bound = format!(
        "MAX_SLOT_BYTES={}",
        boss_core::machine_token::MAX_SLOT_BYTES
    );
    assert!(read(RECV).contains(&bound) && push.contains(&bound));
}

/// THE ONE ROLE ENTRY AND THE ONE NEW MOUNT, exactly as David accepted
/// them (design-doc bdc60b65, gcp-push) and nothing wider: the broker's
/// transport-key Role gains one resourceName and no verb, and the push
/// CronJob mounts the token and its key read-only, holds no Role, and
/// mounts no API token.
#[test]
fn the_grant_is_one_role_entry_and_the_job_holds_no_role() {
    let broker = read(BROKER);
    let lines = live(&broker);
    let at = lines
        .iter()
        .position(|l| l.contains("resourceNames:") && l.contains("machine-token-deposit-key"))
        .expect("the broker may write the deposit key's Secret by name");
    assert_eq!(
        lines[at].trim(),
        "resourceNames: [runner-deposit-key, machine-token-deposit-key]"
    );
    assert_eq!(lines[at + 1].trim(), "verbs: [get, patch]");
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("machine-token-deposit-key"))
            .count(),
        1,
        "named once in the broker's grants"
    );
    // No rule over secrets without resourceNames, and no create.
    for (i, l) in lines.iter().enumerate() {
        if l.trim() == "resources: [secrets]" {
            assert!(
                lines[i + 1].trim().starts_with("resourceNames:"),
                "{BROKER}: a secrets rule with no resourceNames"
            );
            assert!(
                !lines[i + 2].contains("create")
                    && !lines[i + 2].contains("list")
                    && !lines[i + 2].contains('*'),
                "{BROKER}: {}",
                lines[i + 2]
            );
        }
    }

    let manifest = read(MANIFEST);
    let m = live(&manifest);
    let kinds: Vec<&str> = m
        .iter()
        .filter(|l| l.starts_with("kind:"))
        .copied()
        .collect();
    assert_eq!(
        kinds,
        ["kind: CronJob"],
        "one object: no Role, no ServiceAccount, no Secret"
    );
    assert!(
        m.iter()
            .any(|l| l.trim() == "automountServiceAccountToken: false")
    );
    assert!(!manifest.contains("serviceAccountName"));
    assert!(
        m.iter().any(|l| l.trim() == "schedule: \"*/10 * * * *\""),
        "every ten minutes"
    );
    let secrets: Vec<&str> = m
        .iter()
        .filter(|l| l.contains("secretName:"))
        .copied()
        .collect();
    assert_eq!(secrets.len(), 2, "{secrets:?}");
    assert!(secrets[0].contains("secretName: boss-machine-token,"));
    assert!(secrets[1].contains("secretName: machine-token-deposit-key,"));
    // Mutants a review found alive (ecd9263a, F5), each now held: the
    // mounts are group-readable and no wider; a wedged run is ended; and
    // two runs never push at once — the receiver sweeps a staged deposit
    // it finds, which is only safe while pushes do not overlap.
    for secret in &secrets {
        assert!(
            secret.contains("defaultMode: 0440") && secret.contains("optional: true"),
            "a Secret mount here is 0440 (root and the pod's group) and optional: {secret}"
        );
    }
    assert!(m.iter().any(|l| l.trim() == "concurrencyPolicy: Forbid"));
    assert!(m.iter().any(|l| l.trim() == "activeDeadlineSeconds: 240"));
    assert!(m.iter().any(|l| l.trim() == "backoffLimit: 0"));
    assert!(
        read(RECV).contains("stage=\"${BOSS_RECV_STAGE_DIR:-/run}\""),
        "the receiver stages a deposit on tmpfs by default, never the host's shared /tmp"
    );
    for mount in m
        .iter()
        .filter(|l| l.contains("mountPath:") && !l.contains("/work"))
    {
        assert!(mount.contains("readOnly: true"), "{mount}");
    }
    assert!(
        m.iter().any(|l| l.contains("emptyDir: {medium: Memory")),
        "scratch is memory"
    );
    let at = m
        .iter()
        .position(|l| l.contains("BOSS_CHORE_NOT_YET_ON_75"))
        .unwrap();
    assert!(m[at + 1].contains("\"1\""));

    // The name the manifest runs is the name the image carries.
    assert!(manifest.contains("-- /usr/local/bin/boss-machine-token-push\n"));
    assert!(read(DOCKERFILE).contains(
        "COPY infra/cluster/machine-token-push.sh /usr/local/bin/boss-machine-token-push\n"
    ));
    assert!(manifest.contains("boss-chore.sh maintenance-machine-token-push "));
    let workflow = read("infra/platform/workflows/maintenance-machine-token-push.toml");
    assert!(workflow.contains("kind = \"maintenance-machine-token-push\""));
    assert!(workflow.contains("terminal = { outcome = \"not-yet\" }"));
    assert!(
        read("infra/dispatcher/rules/file-backlog-items-on-machine-token-push-red.toml").contains(
            "when = 'kind = \"maintenance-machine-token-push\" AND outcome = \"failed\"'"
        )
    );
    // … and a CronJob that STOPS running files nothing at all, so its
    // cadence is declared on the silence sweep, at the schedule's own
    // ten minutes (review ecd9263a, F2). The entry was there and nothing
    // held it: removing it survived every test that names the kind
    // (delta review 76249509, mutant N12). The break-glass deposit's is
    // pinned the same way, break_glass_deposit_sh.rs.
    assert!(
        manifest.contains("schedule: \"*/10 * * * *\""),
        "the push's schedule moved; move its silence interval with it"
    );
    assert!(
        read("infra/dispatcher/rules/cadence-silence-sweep-daily.toml")
            .contains("\"interval_minutes.maintenance-machine-token-push\" = \"10\""),
        "declare the ten-minute push on the silence sweep"
    );
}

/// boss-gcp's units that run as another account than root, by what the
/// host installs (infra/estate/roles.toml). The token's directory there
/// is root-only, so these send UNSTAMPED: a known miss, listed with what
/// each needs of its account, rather than a group nobody decided who
/// holds. No car probe runs on boss-gcp (run-car-probe names the forge
/// alone), so this is not an isolation question there — it is simply
/// who was given the token.
const GCP_UNSTAMPED: &[(&str, &str)] = &[
    (
        "boss-codebase-metrics",
        "reads git history in the owner's checkout, which root is refused (dubious ownership)",
    ),
    (
        "boss-protocol-drift",
        "reads the owner's checkout through git for the same reason",
    ),
    (
        "boss-surface-usage",
        "runs the tree's CLI against the owner's checkout as its owner",
    ),
];

#[test]
fn boss_gcps_converge_states_the_effect_and_its_other_units_are_a_named_roster() {
    let unit = read(GCP_UNIT);
    let u = live(&unit);
    assert!(u.contains(&"User=root"));
    assert!(
        u.contains(&"RuntimeDirectory=boss-gcp-converge")
            && u.contains(&"RuntimeDirectoryMode=0700"),
        "the converge's stamped sends keep their header file on tmpfs systemd removes"
    );
    for forbidden in [
        "PrivateTmp=",
        "ProtectSystem=",
        "ReadOnlyPaths=",
        "ProtectHome=",
    ] {
        assert!(
            !u.iter().any(|l| l.starts_with(forbidden)),
            "{GCP_UNIT} must not take a mount namespace: it installs the host it runs on"
        );
    }
    let converge = read(GCP_CONVERGE);
    let c = live(&converge);
    let at = c
        .iter()
        .position(|l| l.contains("estate/machine-token-effect.sh"))
        .expect("the converge asks the gate");
    assert!(
        c[at].contains("|| effect_rc=$?"),
        "its exit is carried, never fatal: {}",
        c[at]
    );
    assert!(
        c[at].starts_with("timeout -k 5 "),
        "and its TIME is bounded: a reading taken inside the converge spends the converge's \
         time (review 9a1e289b, B1): {}",
        c[at]
    );
    assert!(
        !c.iter()
            .any(|l| l.contains("exit") && l.contains("effect_rc")),
        "the converge never exits on the effect check"
    );

    // Which units boss-gcp installs, and which of them are not root's.
    let roles = read("infra/estate/roles.toml");
    let mut other: Vec<(String, String)> = Vec::new();
    for l in roles.lines().filter(|l| l.starts_with("units = [")) {
        for name in l
            .trim_start_matches("units = [")
            .trim_end_matches(']')
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .filter(|s| !s.is_empty())
        {
            let file = ["infra", "infra/estate", "infra/gcp", "infra/ml"]
                .iter()
                .map(|d| repo_root().join(d).join(format!("{name}.service")))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("{name}.service is in roles.toml and nowhere in infra/"));
            let text = std::fs::read_to_string(&file).unwrap();
            if let Some(user) = text
                .lines()
                .find_map(|l| l.strip_prefix("User="))
                .filter(|user| *user != "root")
            {
                other.push((name.to_string(), user.to_string()));
            }
        }
    }
    other.sort();
    let got: Vec<&str> = other
        .iter()
        .filter(|(_, u)| u == "david")
        .map(|(n, _)| n.as_str())
        .collect();
    let want: Vec<&str> = GCP_UNSTAMPED.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        got, want,
        "boss-gcp's units that run as the checkout's owner must be exactly the GCP_UNSTAMPED \
         roster: each cannot read the root-only token, and each needs a reason"
    );
    for (name, why) in GCP_UNSTAMPED {
        assert!(why.len() > 30, "{name}: say what it needs of the account");
    }
    // Any other account is a third kind of reader nobody has decided on.
    for (name, user) in &other {
        assert!(
            user == "david" || name == "boss-ml-inference-batch",
            "{name}.service runs as `{user}`: decide whether that account may read the token"
        );
    }
}
