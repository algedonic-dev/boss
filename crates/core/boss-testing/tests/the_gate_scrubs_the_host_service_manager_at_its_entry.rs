//! `infra/gate.sh` scrubs the names a host's service manager exports
//! (`INVOCATION_ID`, `JOURNAL_STREAM`, …) ONCE, at its entry, before its
//! first child — so every check runs in the environment a forge gate
//! has, wherever the gate itself was started.
//!
//! MEASURED (backlog 92da4bdb; item 70e5bbc7, reproduced exactly by run
//! 29713605). A GitHub-hosted runner runs each job step under the
//! runner's systemd service, so the public mirror's Gate — `infra/gate.sh`
//! full, `the_mirror_runs_the_one_gate.rs` — handed every test process
//! that service's `INVOCATION_ID` and `JOURNAL_STREAM`. Every forge gate
//! is a Kubernetes pod with no systemd above it and has neither.
//! `infra/surface-usage.sh` and `infra/boss-maintenance-wrap.sh` read a
//! well-formed `INVOCATION_ID` as their OWN unit's invocation, so publish
//! PR 259's Gate went red at a key assertion every forge gate passed.
//! 70e5bbc7 fixed that one test (`surface_usage_sh.rs`,
//! `a_host_systemd_invocation_does_not_reach_the_plain_file_run`, kept);
//! a test fending for itself is the defect, and the next test that reads
//! one of these names would go red on GitHub alone the same way.
//!
//! THE CALL. The scrub lives in `infra/gate.sh`, not in the mirror's
//! workflow `env`: GitHub's `env:` can set a name but not remove one the
//! runner's service manager exported, and a developer who runs the gate
//! under a systemd user service hands it the same names with no workflow
//! in sight. It is unconditional rather than tied to the public-mirror
//! declaration (`BOSS_TRUNK=none`), because a forge pod has none of the
//! names to lose. A fixture that WANTS an identity still sets it on its
//! own child (`an_existing_packet_keeps_its_creation_identity_…`), which
//! a scrub of the gate's environment cannot reach.
//!
//! The pins: the gate's first children (`dirname`, then `git`) are
//! shimmed to record the environment they were handed, and must see none
//! of the names while still seeing a control the gate must keep; the
//! list holds the two measured names, every name systemd documents for a
//! service (`DOCUMENTED`, written out here rather than read from the
//! gate), and none a login shell carries too.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The names the runner was MEASURED to hand every step (run 29713605).
/// A floor, not a copy of the gate's list: whatever else that list
/// holds, it may not lose these.
const MEASURED: [&str; 2] = ["INVOCATION_ID", "JOURNAL_STREAM"];

/// Every name systemd documents handing a spawned process that describes
/// the UNIT rather than the login — systemd.exec(5) "Environment
/// Variables in Spawned Processes" plus systemd.service(5), read from
/// the freedesktop.org `latest` pages on 2026-11-17 — written out HERE,
/// independently of the gate's list. Review run ed9dea02 found the
/// first version planted the gate's own list back at itself, so the
/// test passed while MAINPIDFDID, MANAGERPIDFDID and LISTEN_PIDFDID
/// (systemd 258) reached the gate's first children. A name added to
/// systemd's documentation is added here first, and goes RED until the
/// gate scrubs it too.
///
/// Left out on purpose: the `KEPT` login-shell names below; TMPDIR,
/// which a login shell may carry and the checks write under; the
/// accept-socket peer's REMOTE_ADDR/REMOTE_PORT, whose names are CGI's
/// as much as systemd's; and XDG_SEAT/XDG_VTNR, which pam_systemd sets
/// on a login session, not a service.
const DOCUMENTED: [&str; 43] = [
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "SYSTEMD_EXEC_PID",
    "DEBUG_INVOCATION",
    "MAINPID",
    "MAINPIDFDID",
    "MANAGERPID",
    "MANAGERPIDFDID",
    "PIDFILE",
    "NOTIFY_SOCKET",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "LISTEN_FDS",
    "LISTEN_PID",
    "LISTEN_PIDFDID",
    "LISTEN_FDNAMES",
    "SO_COOKIE",
    "FDSTORE",
    "LOG_NAMESPACE",
    "RUNTIME_DIRECTORY",
    "STATE_DIRECTORY",
    "CACHE_DIRECTORY",
    "LOGS_DIRECTORY",
    "CONFIGURATION_DIRECTORY",
    "CREDENTIALS_DIRECTORY",
    "MEMORY_PRESSURE_WATCH",
    "MEMORY_PRESSURE_WRITE",
    "CPU_PRESSURE_WATCH",
    "CPU_PRESSURE_WRITE",
    "IO_PRESSURE_WATCH",
    "IO_PRESSURE_WRITE",
    "SERVICE_RESULT",
    "EXIT_CODE",
    "EXIT_STATUS",
    "MONITOR_SERVICE_RESULT",
    "MONITOR_EXIT_CODE",
    "MONITOR_EXIT_STATUS",
    "MONITOR_INVOCATION_ID",
    "MONITOR_UNIT",
    "TRIGGER_UNIT",
    "TRIGGER_PATH",
    "TRIGGER_TIMER_REALTIME_USEC",
    "TRIGGER_TIMER_MONOTONIC_USEC",
];

/// Names every login shell carries as well as a systemd unit. Scrubbing
/// one would change what the checks run WITH, not merely where they were
/// started from — the gate must keep them.
const KEPT: [&str; 8] = [
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TERM",
    "XDG_RUNTIME_DIR",
];

/// The array in `infra/gate.sh` that names what the gate scrubs.
const LIST_NAME: &str = "GATE_HOST_SERVICE_ENV";

/// The gate's list, read from its one definition — `None` when the
/// gate defines none.
fn gate_list() -> Option<Vec<String>> {
    let path = repo_root().join("infra/gate.sh");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let opener = format!("{LIST_NAME}=(");
    let start = text.find(&opener)? + opener.len();
    let body = &text[start..];
    let end = body.find(')')?;
    Some(
        body[..end]
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .flat_map(str::split_whitespace)
            .map(str::to_string)
            .collect(),
    )
}

/// Every name the fixture plants: the measured two, then every
/// documented one — both lists written in this file, never read from
/// the gate, so a name the gate's list drops is still planted and seen
/// to leak (review ed9dea02).
fn planted() -> Vec<String> {
    let mut names: Vec<String> = MEASURED.iter().map(|n| n.to_string()).collect();
    for n in DOCUMENTED {
        if !names.iter().any(|m| m == n) {
            names.push(n.to_string());
        }
    }
    names
}

/// A value of the shape the host hands over: 32 lowercase hex for an
/// invocation id (the shape the scripts accept as their own), something
/// plausible for the rest.
fn host_value(name: &str) -> &'static str {
    match name {
        "INVOCATION_ID" => "5f1d2c3b4a5968778695a4b3c2d1e0f9",
        "JOURNAL_STREAM" => "8:4242",
        _ => "from-the-host-service-manager",
    }
}

fn real(tool: &str) -> String {
    let out = Command::new("bash")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .unwrap_or_else(|e| panic!("spawn bash to find {tool}: {e}"));
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        out.status.success() && path.starts_with('/'),
        "no {tool} on PATH"
    );
    path
}

/// A directory of shims for the gate's first children: each appends the
/// environment it was handed to `log`, then runs the real tool.
fn shims(dir: &Path, log: &Path) -> PathBuf {
    let bin = dir.join("bin");
    boss_testing::create_dir(&bin);
    for tool in ["dirname", "git"] {
        let body = format!(
            "#!/bin/bash\n{{ printf '=== %s\\n' {tool}; env; }} >> '{log}'\nexec '{real}' \"$@\"\n",
            log = log.display(),
            real = real(tool),
        );
        boss_testing::write_exec(&bin.join(tool), &body);
    }
    bin
}

/// `infra/gate.sh --roster` — the cheapest mode, which still passes the
/// gate's entry — with `bin` first on PATH and `set` over the inherited
/// environment, `remove` taken out of it.
fn roster(bin: &Path, set: &[(String, String)], remove: &[String]) -> (Option<i32>, String) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg("infra/gate.sh")
        .arg("--roster")
        .current_dir(repo_root())
        .env("PATH", path);
    for n in remove {
        cmd.env_remove(n);
    }
    for (n, v) in set {
        cmd.env(n, v);
    }
    let out = cmd.output().expect("spawn infra/gate.sh");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The environments the shims recorded, one per call, each with the
/// tool that recorded it.
fn recorded(log: &Path) -> Vec<(String, Vec<String>)> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.split("=== ")
        .filter(|chunk| !chunk.is_empty())
        .map(|chunk| {
            let mut lines = chunk.lines();
            let tool = lines.next().unwrap_or("").to_string();
            (tool, lines.map(str::to_string).collect())
        })
        .collect()
}

#[test]
fn the_gates_first_children_see_no_host_service_manager_names() {
    let dir = boss_testing::scratch_dir("gate-host-service-env");
    let log = dir.join("children.env");
    let bin = shims(&dir, &log);

    let names = planted();
    let mut set: Vec<(String, String)> = names
        .iter()
        .map(|n| (n.clone(), host_value(n).to_string()))
        .collect();
    // Controls the gate must hand on: one of its own namespace and one a
    // systemd unit and a login shell both carry.
    set.push(("BOSS_HOST_ENV_CONTROL".into(), "kept".into()));
    set.push(("XDG_RUNTIME_DIR".into(), "control-runtime-dir".into()));

    let (rc, err) = roster(&bin, &set, &[]);
    assert_eq!(rc, Some(0), "gate.sh --roster did not exit 0:\n{err}");

    let calls = recorded(&log);
    // No evidence is not a pass: a shim that never ran proves nothing.
    for tool in ["dirname", "git"] {
        assert!(
            calls.iter().any(|(t, _)| t == tool),
            "the gate never ran {tool}, so its entry environment went unobserved; \
             recorded: {:?}",
            calls.iter().map(|(t, _)| t).collect::<Vec<_>>()
        );
    }
    // Every leak at once, by child and name, so one run names them all.
    let mut leaked: Vec<String> = calls
        .iter()
        .flat_map(|(tool, env)| {
            names
                .iter()
                .filter(move |name| {
                    let prefix = format!("{name}=");
                    env.iter().any(|l| l.starts_with(&prefix))
                })
                .map(move |name| format!("{tool}:{name}"))
        })
        .collect();
    leaked.sort();
    leaked.dedup();
    assert!(
        leaked.is_empty(),
        "the gate handed its first children the host service manager's {leaked:?} — \
         a forge gate has none, so a check reading one judges a different environment \
         on the public mirror (backlog 92da4bdb)"
    );
    for (tool, env) in &calls {
        for control in [
            "BOSS_HOST_ENV_CONTROL=kept",
            "XDG_RUNTIME_DIR=control-runtime-dir",
        ] {
            assert!(
                env.iter().any(|l| l == control),
                "the gate dropped {control} on its way to {tool}: the scrub may take \
                 only the service manager's own names"
            );
        }
    }
    // Loud, not quiet: the run's log says what it took away, by name.
    for name in MEASURED {
        assert!(
            err.contains(name),
            "the gate scrubbed {name} without saying so on stderr:\n{err}"
        );
    }
}

#[test]
fn a_gate_started_outside_any_service_says_nothing_about_a_scrub() {
    let dir = boss_testing::scratch_dir("gate-host-service-env-none");
    let log = dir.join("children.env");
    let bin = shims(&dir, &log);
    let (rc, err) = roster(&bin, &[], &planted());
    assert_eq!(rc, Some(0), "gate.sh --roster did not exit 0:\n{err}");
    assert!(
        !err.contains(LIST_NAME) && !err.contains("service manager"),
        "a gate with nothing to scrub still spoke of a scrub:\n{err}"
    );
}

#[test]
fn the_gates_list_holds_the_measured_names_and_no_login_shell_name() {
    let list = gate_list().unwrap_or_else(|| {
        panic!(
            "infra/gate.sh defines no {LIST_NAME}=( … ) — the list of the host service \
             manager's names it scrubs at its entry (backlog 92da4bdb)"
        )
    });
    for name in MEASURED {
        assert!(
            list.iter().any(|n| n == name),
            "{LIST_NAME} lost {name}, which run 29713605 measured on the public \
             mirror's runner: {list:?}"
        );
    }
    // The omission control, against this file's own copy of systemd's
    // documentation rather than the gate's: a name dropped from the
    // gate is named here (review ed9dea02).
    let missing: Vec<&str> = DOCUMENTED
        .iter()
        .copied()
        .filter(|name| !list.iter().any(|n| n == name))
        .collect();
    assert!(
        missing.is_empty(),
        "{LIST_NAME} omits {missing:?}, which systemd.exec(5)/systemd.service(5) \
         document handing a spawned process: {list:?}"
    );
    for name in KEPT {
        assert!(
            !list.iter().any(|n| n == name),
            "{LIST_NAME} scrubs {name}, which every login shell carries too — \
             the checks would run without it: {list:?}"
        );
    }
    for name in &list {
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{LIST_NAME} holds {name:?}, which is not an environment name"
        );
    }
}
