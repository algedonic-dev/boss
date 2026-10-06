//! `docker-driver-report` (`infra/forge/docker-driver-report.sh`) — the
//! read verb that says which storage driver, and so which image store,
//! each of the forge's two docker daemons runs.
//!
//! WHY (backlog 3f28860d, from the delta review of car cf59b3bf). The
//! mirror read-back's correctness depends on whether the forge's
//! ROOTLESS dockerd uses the containerd image store, and no door could
//! read it: disk-report prints `docker system df` and a du,
//! ci-image-report reads only the system daemon, and journal-tail
//! cannot reach a user unit. This verb prints `Driver` and
//! `DriverStatus` for BOTH daemons, and names the one it could not
//! reach rather than going silent about it — an unreached daemon is
//! not a daemon with no driver.
//!
//! The script is RUN here: each daemon is a stub named by the script's
//! own env knobs, so every line below is one the script printed, and
//! nothing here touches a daemon.

use boss_testing::repo_root;
use boss_testing::scratch::{scratch_dir, write_exec};
use std::path::PathBuf;
use std::process::Command;

const SCRIPT: &str = "infra/forge/docker-driver-report.sh";
const VERB: &str = "infra/ops/verbs/docker-driver-report.json";
const ROOTLESS_SOCK: &str = "unix:///run/user/1000/docker.sock";
const CONTAINERD: &str = r#"overlayfs [["driver-type","io.containerd.snapshotter.v1"]]"#;
const OVERLAY2: &str = r#"overlay2 [["Backing Filesystem","xfs"],["Supports d_type","true"]]"#;

/// How a stub daemon answers `docker info -f …`.
#[derive(Clone, Copy)]
enum Daemon {
    Answers(&'static str),
    /// Exits non-zero with docker's own refusal on stderr.
    Dark,
    /// Exits 0 and prints nothing.
    Mute,
}

struct Report {
    dir: PathBuf,
    system: Daemon,
    rootless: Daemon,
}

impl Report {
    fn new(case: &str) -> Self {
        Report {
            dir: scratch_dir(&format!("docker-driver-report-{case}")),
            system: Daemon::Answers(OVERLAY2),
            rootless: Daemon::Answers(CONTAINERD),
        }
    }

    fn stub(&self, name: &str, daemon: Daemon) -> PathBuf {
        let path = self.dir.join(name);
        let answer = match daemon {
            Daemon::Answers(line) => format!("printf '%s\\n' '{line}'; exit 0"),
            Daemon::Dark => "echo 'Cannot connect to the Docker daemon. Is the docker daemon running?' >&2; exit 1".to_string(),
            Daemon::Mute => "exit 0".to_string(),
        };
        write_exec(
            &path,
            &format!(
                "#!/usr/bin/env bash\n\
                 printf '%s DOCKER_HOST=%s\\n' \"$*\" \"${{DOCKER_HOST:-unset}}\" >>{calls}\n\
                 {answer}\n",
                calls = self.dir.join(format!("{name}-calls")).display(),
            ),
        );
        path
    }

    fn run(&self) -> (i32, String) {
        let system = self.stub("system-docker", self.system);
        let rootless = self.stub("rootless-docker", self.rootless);
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            // A DOCKER_HOST inherited from the runner must not aim the
            // SYSTEM read at the rootless socket.
            .env("DOCKER_HOST", ROOTLESS_SOCK)
            .env("BOSS_DOCKER_REPORT_SYSTEM", &system)
            .env("BOSS_DOCKER_REPORT_ROOTLESS", &rootless)
            .output()
            .expect("docker-driver-report.sh runs");
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), all)
    }

    fn calls(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.join(format!("{name}-calls"))).unwrap_or_default()
    }
}

/// Both daemons answer: each line names the daemon and carries its
/// Driver and DriverStatus verbatim, and the rootless one was asked
/// through its own socket while the system one was asked without it.
#[test]
fn both_daemons_answer_with_their_driver_and_driver_status() {
    let r = Report::new("both");
    let (rc, all) = r.run();
    assert_eq!(rc, 0, "two answered daemons exit 0:\n{all}");
    assert!(
        all.contains(&format!("system: {OVERLAY2}")),
        "the system daemon's line:\n{all}"
    );
    assert!(
        all.contains(&format!("rootless: {CONTAINERD}")),
        "the rootless daemon's line:\n{all}"
    );
    let rootless = r.calls("rootless-docker");
    assert!(
        rootless.contains("info -f {{.Driver}} {{json .DriverStatus}}")
            && rootless.contains(&format!("DOCKER_HOST={ROOTLESS_SOCK}")),
        "the rootless daemon is read at its own socket:\n{rootless}"
    );
    let system = r.calls("system-docker");
    assert!(
        system.contains("info -f {{.Driver}} {{json .DriverStatus}}")
            && system.contains("DOCKER_HOST=unset"),
        "the system daemon is read with no DOCKER_HOST, never the rootless socket:\n{system}"
    );
    assert!(all.contains("unreached: none"), "{all}");
}

/// The system daemon is dark: its line says so with docker's own
/// reason, the rootless answer is still printed, and the exit is
/// non-zero — no evidence is not a pass.
#[test]
fn an_unreachable_system_daemon_is_named_and_the_other_still_reads() {
    let mut r = Report::new("system-dark");
    r.system = Daemon::Dark;
    let (rc, all) = r.run();
    assert_eq!(rc, 1, "an unreached daemon fails the read:\n{all}");
    assert!(
        all.contains("system: NOT REACHED (Cannot connect to the Docker daemon"),
        "{all}"
    );
    assert!(all.contains(&format!("rootless: {CONTAINERD}")), "{all}");
    assert!(all.contains("unreached: system"), "{all}");
}

/// The rootless daemon is dark — the one the packet is about.
#[test]
fn an_unreachable_rootless_daemon_is_named_with_its_socket() {
    let mut r = Report::new("rootless-dark");
    r.rootless = Daemon::Dark;
    let (rc, all) = r.run();
    assert_eq!(rc, 1, "{all}");
    assert!(
        all.contains(&format!("rootless: NOT REACHED at {ROOTLESS_SOCK}")),
        "{all}"
    );
    assert!(all.contains(&format!("system: {OVERLAY2}")), "{all}");
    assert!(all.contains("unreached: rootless"), "{all}");
}

/// A daemon that exits 0 and prints nothing gave no driver: it is
/// counted unreached, never printed as an empty driver.
#[test]
fn a_silent_answer_is_counted_as_unreached() {
    let mut r = Report::new("mute");
    r.system = Daemon::Mute;
    r.rootless = Daemon::Mute;
    let (rc, all) = r.run();
    assert_eq!(rc, 1, "{all}");
    assert!(all.contains("system: NOT REACHED"), "{all}");
    assert!(all.contains("rootless: NOT REACHED"), "{all}");
    assert!(all.contains("unreached: system rootless"), "{all}");
}

/// The verb file is read-only, serves the forge, and runs this script.
#[test]
fn the_verb_is_a_read_only_forge_verb_running_this_script() {
    let text = std::fs::read_to_string(repo_root().join(VERB)).expect("verb file");
    let v: serde_json::Value = serde_json::from_str(&text).expect("verb json");
    assert_eq!(v["argv"], serde_json::json!([SCRIPT]), "{text}");
    assert_eq!(v["hosts"], serde_json::json!(["forge"]), "{text}");
    assert_eq!(v["params"], serde_json::json!([]), "{text}");
    let about = v["about"].as_str().unwrap_or_default();
    assert!(about.starts_with("READ-ONLY"), "{about}");
    assert!(!about.contains("MUTATING"), "{about}");
}
