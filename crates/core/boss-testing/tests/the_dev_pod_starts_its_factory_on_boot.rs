//! A dev pod boot starts the factory loop beside the conductor (backlog
//! 4e2ec7d2; conductor decision 2026-10-29T15:57Z, David delegating the
//! dev pod roll to the agents as a protocol trial).
//!
//! The postStart already starts Claude Code in tmux `dev`, and with the
//! drain switch on its first prompt is infra/dev/drain.md, so the
//! conductor comes back from a roll by itself. The factory did not: its
//! loop (tmux `codex-factory`, infra/dev/factory-loop.sh) started only
//! from the login menu's option 6, so after every roll the builders
//! stayed down until David logged in. The boot now writes
//! /work/dev-factory.sh and runs it, detached and after sshd and the
//! `dev` start, and it starts the loop through the one door that defines
//! it — `dev-session.sh --detached 6` (car 0dd77a5c) — which itself
//! refuses when the loop or the interactive codex is already running.
//!
//! Pinned the way the_dev_pod_resumes_the_drain_on_boot.rs pins
//! dev-claude.sh: the heredoc is lifted out of boss-dev.yaml, its
//! `/work/` paths moved under a scratch root, and a stub dev-session.sh
//! records the argv it was run with. Every verdict lands in
//! postStart.log, and no outcome fails the boot.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "infra/cluster/manifests/boss-dev.yaml";
const OPENER: &str = "cat > /work/dev-factory.sh <<'DF'";
/// The line the boot's guard looks for in the checkout's dev-session.sh
/// before it calls `--detached 6`, held equal to the real door below.
const DETACHED_MARKER: &str = "= --detached ]";

fn manifest() -> String {
    std::fs::read_to_string(repo_root().join(MANIFEST)).expect(MANIFEST)
}

/// The body of the `cat > /work/dev-factory.sh <<'DF'` heredoc, with the
/// YAML block's indentation taken off: what the pod writes.
fn dev_factory_sh() -> String {
    let m = manifest();
    let lines: Vec<&str> = m.lines().collect();
    let opener = lines
        .iter()
        .position(|l| l.trim() == OPENER)
        .expect("the postStart writes /work/dev-factory.sh through a quoted DF heredoc");
    let indent = lines[opener].len() - lines[opener].trim_start().len();
    let body: Vec<&str> = lines[opener + 1..]
        .iter()
        .take_while(|l| l.trim() != "DF")
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect();
    assert!(
        lines[opener + 1 + body.len()..]
            .first()
            .is_some_and(|l| l.trim() == "DF"),
        "the dev-factory.sh heredoc has no DF terminator"
    );
    body.join("\n") + "\n"
}

/// What one boot left behind.
struct Booted {
    /// The argv dev-session.sh was run with, or None when it never ran.
    argv: Option<Vec<String>>,
    log: String,
}

#[derive(Clone, Copy)]
struct Pod {
    switch_on: bool,
    codex: bool,
    login: bool,
    /// The checkout's dev-session.sh carries `--detached 6` (car 0dd77a5c).
    detached_door: bool,
    /// The checkout's dev-session.sh carries `--boot` (backlog b513514c).
    boot_door: bool,
    /// The postStart gave the drain to the conductor's seat:
    /// `/work/drain-seat` reads `conductor` (review ce8970ce N2).
    drain_in_seat: bool,
    /// What the stub door exits with: 0 started, 1 refused.
    door_exit: i32,
}

const READY: Pod = Pod {
    switch_on: true,
    codex: true,
    login: true,
    detached_door: true,
    boot_door: false,
    drain_in_seat: false,
    door_exit: 0,
};

/// A checkout that carries `--boot`, on a boot that gave the drain to
/// the conductor's seat: every seat of infra/dev/seats.tsv.
const SEATED: Pod = Pod {
    boot_door: true,
    drain_in_seat: true,
    ..READY
};

fn boot(pod: Pod) -> Booted {
    let root = scratch_dir("factory-on-boot");
    let work = root.join("work");
    let at = |p: &str| -> PathBuf { work.join(p) };
    create_dir(&at("ssh"));
    create_dir(&at("boss/infra/dev"));
    if pod.switch_on {
        create_dir(&at("home/.config/boss"));
        write_file(&at("home/.config/boss/drain-on-boot"), "");
    }
    if pod.codex {
        // The login is codex's own answer, `codex login status` (0 logged
        // in, 1 not, measured on codex-cli 0.159.3), read from the PVC
        // home: the manifest never names the CLI's credential file
        // (dev_session_sh.rs holds it to that).
        create_dir(&at("home/.local/bin"));
        write_exec(
            &at("home/.local/bin/codex"),
            &format!(
                "#!/usr/bin/env bash\n\
                 [ \"$*\" = 'login status' ] || exit 2\n\
                 [ \"$HOME\" = '{home}' ] || exit 3\n\
                 {answer}\n",
                home = at("home").display(),
                answer = if pod.login {
                    "echo 'Logged in'; exit 0"
                } else {
                    "echo 'Not logged in'; exit 1"
                },
            ),
        );
    }
    if pod.drain_in_seat {
        write_file(&at("drain-seat"), "conductor\n");
    }
    let mut marker = String::new();
    if pod.boot_door {
        marker.push_str("if [ \"${1:-}\" = --boot ]; then :; fi\n");
    }
    if pod.detached_door {
        marker.push_str("if [ \"${1:-}\" = --detached ]; then :; fi\n");
    }
    let reply = if pod.door_exit == 0 {
        "echo 'codex-factory 4242'"
    } else {
        "echo 'dev-session: tmux `codex-factory` is already running - --detached never joins, nothing started'"
    };
    write_exec(
        &at("boss/infra/dev/dev-session.sh"),
        &format!(
            "#!/usr/bin/env bash\n{marker}printf '%s\\0' \"$@\" > \"{argv}\"\n{reply}\nexit {code}\n",
            argv = root.join("door-argv").display(),
            code = pod.door_exit,
        ),
    );
    let script = root.join("dev-factory.sh");
    let body = dev_factory_sh().replace("/work/", &format!("{}/", work.display()));
    write_exec(&script, &body);
    let out = Command::new("bash")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "dev-factory.sh must never fail the boot; it exited {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    Booted {
        argv: read_argv(&root.join("door-argv")),
        log: std::fs::read_to_string(at("ssh/postStart.log")).unwrap_or_default(),
    }
}

fn read_argv(path: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read(path).ok()?;
    Some(
        raw.split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

#[test]
fn a_ready_pod_starts_the_factory_through_the_detached_door() {
    let b = boot(READY);
    assert_eq!(
        b.argv.as_deref(),
        Some(&["--detached".to_string(), "6".to_string()][..]),
        "the boot starts option 6 through dev-session.sh --detached 6 and nothing else"
    );
    assert!(
        b.log.contains("factory: started") && b.log.contains("codex-factory 4242"),
        "postStart.log names the start and the door's own `<session> <pane pid>` line:\n{}",
        b.log
    );
}

#[test]
fn the_doors_refusal_is_logged_in_its_own_words_and_never_fails_the_boot() {
    let b = boot(Pod {
        door_exit: 1,
        ..READY
    });
    assert!(b.argv.is_some(), "the door was asked");
    assert!(
        b.log.contains("factory: not started")
            && b.log.contains("(exit 1)")
            && b.log
                .contains("is already running - --detached never joins"),
        "a refusal carries the door's exit and its own words:\n{}",
        b.log
    );
}

#[test]
fn without_the_drain_switch_the_factory_is_not_started_and_the_log_says_how() {
    let b = boot(Pod {
        switch_on: false,
        ..READY
    });
    assert_eq!(
        b.argv, None,
        "no switch, no factory: the boot stays idle as before"
    );
    assert!(
        b.log.contains("factory: drain-on-boot is off")
            && b.log.contains("touch /")
            && b.log.contains("drain-on-boot"),
        "the log names the switch that would start it:\n{}",
        b.log
    );
}

#[test]
fn a_missing_codex_binary_or_login_starts_nothing_and_names_which() {
    for (pod, which) in [
        (
            Pod {
                codex: false,
                ..READY
            },
            "no codex binary",
        ),
        (
            Pod {
                login: false,
                ..READY
            },
            "codex is not logged in (codex login status exit 1)",
        ),
    ] {
        let b = boot(pod);
        assert_eq!(b.argv, None, "no binary or no login: the door is not asked");
        assert!(
            b.log.contains("factory: not started") && b.log.contains(which),
            "the log names which prerequisite is missing ({which}):\n{}",
            b.log
        );
    }
}

#[test]
fn a_checkout_without_the_detached_door_starts_nothing_and_names_the_car() {
    let b = boot(Pod {
        detached_door: false,
        ..READY
    });
    assert_eq!(
        b.argv, None,
        "an older dev-session.sh would show a menu or join `dev`, so it is never called with --detached"
    );
    assert!(
        b.log.contains("factory: not started") && b.log.contains("0dd77a5c"),
        "the log names the car that brings the door:\n{}",
        b.log
    );
}

#[test]
fn the_guard_reads_the_marker_the_real_door_carries() {
    // A fact that lives twice gets an equality test (CLAUDE.md §9a): the
    // boot's guard and the door it guards must agree on the spelling.
    let script = dev_factory_sh();
    assert!(
        script.contains(&format!("grep -q -F -- '{DETACHED_MARKER}'")),
        "dev-factory.sh guards on the marker {DETACHED_MARKER:?}"
    );
    let door = std::fs::read_to_string(repo_root().join("infra/dev/dev-session.sh"))
        .expect("infra/dev/dev-session.sh");
    assert!(
        door.lines()
            .any(|l| l.contains(DETACHED_MARKER) && l.trim_start().starts_with("if ")),
        "infra/dev/dev-session.sh must parse --detached on a line carrying {DETACHED_MARKER:?}"
    );
    assert!(
        door.contains("start_factory"),
        "--detached 6 starts the menu's own option 6"
    );
}

#[test]
fn the_post_start_runs_it_detached_after_sshd_and_the_dev_session() {
    let m = manifest();
    let sshd = m
        .find("/usr/sbin/sshd -f /work/ssh/sshd_config")
        .expect("the sshd start");
    let dev = m
        .find("tmux new-session -d -s dev -c /work/boss /work/dev-claude.sh")
        .expect("the dev session start");
    let written = m.find(OPENER).expect("the DF heredoc");
    let call = m
        .find("/work/dev-factory.sh || echo")
        .expect("the postStart runs /work/dev-factory.sh, and a failure is a log line");
    assert!(written < call, "the script is written before it is run");
    assert!(
        sshd < call && dev < call,
        "the factory starts after sshd and after the dev session"
    );
    // Inside the detached setsid block: the kubelet marks the pod Ready
    // (and opens the ssh door) only once postStart returns, so nothing
    // that waits on the jobs API or a CLI may run inline.
    let setsid = m[..call]
        .rfind("setsid bash -c '")
        .expect("a setsid block before the call");
    let closed = m[setsid..]
        .find("' < /dev/null >> /work/ssh/postStart.log 2>&1 &")
        .map(|i| setsid + i)
        .expect("the setsid block's close");
    assert!(
        setsid < call && call < closed,
        "/work/dev-factory.sh runs inside the detached block that appends to postStart.log"
    );
}

/// Every seat, from data (backlog b513514c): a checkout whose door has
/// `--boot` is asked for that and nothing else, whatever codex's state —
/// the door checks each seat's CLI and login itself — so a pod roll
/// brings back the conductor and Claude B as well as Codex, and a new
/// seat is a row of infra/dev/seats.tsv, never an edit here.
#[test]
fn a_seated_checkout_starts_every_seat_through_the_boot_door() {
    for pod in [
        SEATED,
        Pod {
            codex: false,
            ..SEATED
        },
        Pod {
            login: false,
            ..SEATED
        },
    ] {
        let b = boot(pod);
        assert_eq!(
            b.argv.as_deref(),
            Some(&["--boot".to_string()][..]),
            "the boot asks the door for every seat, and only that:\n{}",
            b.log
        );
        assert!(
            b.log.contains("codex-factory 4242")
                && b.log
                    .contains("factory: dev-session.sh --boot started the seats"),
            "the door's own lines land in postStart.log:\n{}",
            b.log
        );
    }
    let off = boot(Pod {
        switch_on: false,
        ..SEATED
    });
    assert_eq!(off.argv, None, "the drain switch still governs every seat");
    let refused = boot(Pod {
        door_exit: 1,
        ..SEATED
    });
    assert!(
        refused
            .log
            .contains("factory: dev-session.sh --boot exited 1"),
        "{}",
        refused.log
    );
}

/// One drain, decided once (review ce8970ce N2): when the postStart
/// left the drain in tmux `dev` — its checkout predated `--boot`, and
/// the reclaim sidecar fast-forwarded it before this script ran — the
/// door that now has `--boot` is asked to start every seat EXCEPT the
/// conductor's, so the conductor never drains beside `dev`.
#[test]
fn a_boot_that_left_the_drain_in_dev_never_starts_the_conductors_seat() {
    let b = boot(Pod {
        drain_in_seat: false,
        ..SEATED
    });
    assert_eq!(
        b.argv.as_deref(),
        Some(
            &[
                "--boot".to_string(),
                "--except".to_string(),
                "conductor".to_string()
            ][..]
        ),
        "{}",
        b.log
    );
    assert!(
        b.log
            .contains("factory: dev-session.sh --boot --except conductor started the seats"),
        "{}",
        b.log
    );
}

#[test]
fn the_boot_guard_reads_the_marker_the_real_door_carries() {
    const BOOT_MARKER: &str = "= --boot ]";
    assert!(
        dev_factory_sh().contains(&format!("grep -q -F -- '{BOOT_MARKER}'")),
        "dev-factory.sh guards on the marker {BOOT_MARKER:?}"
    );
    let door = std::fs::read_to_string(repo_root().join("infra/dev/dev-session.sh"))
        .expect("infra/dev/dev-session.sh");
    assert!(
        door.lines()
            .any(|l| l.contains(BOOT_MARKER) && l.trim_start().starts_with("if ")),
        "infra/dev/dev-session.sh must parse --boot on a line carrying {BOOT_MARKER:?}"
    );
}
