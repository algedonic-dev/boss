//! The ONE environment every child of this CLI that runs code it did
//! not write is handed: no machine token in reach (design 6805c764 car
//! 4, review ef2da426 F1 of that car; backlog 0491b7d6 and 844b936e).
//!
//! WHY A CHILD MUST NOT REACH IT. This CLI runs code from a tree or a
//! car: `boss prove` runs a car's probe text, which builders rehearse on
//! the dev pod and which may itself run `wt-cargo test`; the consist
//! check runs every lint of a train's tree; the brief asks the tree's
//! gate.sh for its roster. Handed the token, a rehearsed probe's handler
//! test stamps it onto a loopback mock, prints the request when it
//! fails, and `boss prove` records that output on the packet — the
//! transcript leak INFO-5 of backlog 1876bbdb closed, reopened one
//! process down. And a lint is a question about a TREE: its answer must
//! not depend on what credentials the asker happens to hold (920524dc,
//! 2026-10-01 00:04Z to 01:15Z, every train held by a credential event
//! that changed no code).
//!
//! WHY REMOVING THE NAME WAS NOT ENOUGH (backlog 844b936e). Until this
//! module held the struct below, it removed `BOSS_MACHINE_TOKEN_DIR`
//! from the child and stopped. boss-core's `token_dir()` reads an UNSET
//! name as its default, `/etc/boss/machine-token` — which is exactly
//! where the conductor and every pod that mounts the Secret at the
//! default keep the token. So removal kept the token out of reach only
//! on the one pod that mounts it elsewhere (the dev pod, at
//! `infra/dev/machine-token-dir`'s directory), and changed nothing on
//! the conductor. Measured by the pin below before the fix: a child of
//! the old `isolate` resolved `DIR=/etc/boss/machine-token`. The consist
//! runner learned this first and pointed the name at an empty directory
//! (#860); brief.rs and prove.rs still removed it. Three copies of one
//! rule, two of them wrong, is the pair CLAUDE.md §9a collapses — so
//! the rule lives here once and all three call it.
//!
//! THE RULE. The name is SET, to an empty directory this process made,
//! so no fallback can apply; the old single-variable token and the host
//! list are removed. Code asking a question about a tree
//! ([`NoTokenInReach::apply`]) also loses the system of record's address
//! and the rendered sor.env (unset, sor.sh and `Hosts::from_env` fall
//! back to /etc/boss/sor.env, which a managed host renders). A probe
//! ([`NoTokenInReach::token_only`]) keeps the address its door resolved
//! and hands it, because reading the record is what a probe is for.
//!
//! What this does NOT do: keep HOSTILE code off a token readable on the
//! same box — such code can `cat` the default path whatever its
//! environment says. That exposure is the holder roster's to state, in
//! crates/core/boss-testing/tests/the_machine_gate_reports_and_refuses_nothing.rs.
//! This keeps ACCIDENTS off it: every stamping client a child builds
//! reads an empty directory and sends nothing.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub(crate) const DEV_CONTROL_CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../infra/dev/control-contract.txt"
));

/// The dev isolation store and its one selector, `<store>/enabled` (b3071bf4).
/// On the /work PVC since design a68fc42a D6 (David, 2026-10-10): on the
/// container's root fs a pod roll erased it, and every child of this CLI
/// silently ran locally again. Spelled the same in the four pod doors and
/// infra/dev/activate-control.py; infra/dev/control-tools_test.py holds
/// them equal. Other hosts have no store; workers never mount /work.
const DEV_CONTROL_STORE: &str = "/work/tools/dev-control";

/// Where a child of this CLI runs, read off the selector. `boss brief`
/// reads it too, so the pre-flight a builder is told to run is the route
/// this door would take rather than one that exits 127 (5008a6c9).
#[derive(Debug, Clone)]
pub(crate) enum DevChild {
    /// No selector: isolation was never turned on here (the held state).
    Local,
    /// An activated generation: through this published launcher.
    Forward(std::path::PathBuf),
    /// Anything else — a refused selector, a regular file, a link outside
    /// the store's generations, an unreadable one (design a68fc42a D1).
    Refuse(String),
}

pub(crate) fn dev_child_route(store: &Path) -> DevChild {
    let selector = store.join("enabled");
    match std::fs::symlink_metadata(&selector) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DevChild::Local,
        Err(e) => DevChild::Refuse(format!(
            "its selector {} could not be read ({e})",
            selector.display()
        )),
        Ok(_) => match std::fs::read_link(&selector) {
            Ok(target) if is_generation(store, &target) => {
                DevChild::Forward(store.join("dev-build"))
            }
            Ok(target) => DevChild::Refuse(format!(
                "its selector {} names {}, not an activated generation",
                selector.display(),
                target.display()
            )),
            Err(_) => DevChild::Refuse(format!(
                "its selector {} is not a generation link",
                selector.display()
            )),
        },
    }
}

/// Whether dev isolation is activated on this host — the one state in
/// which the session's own `boss` reaches this CLI through the published
/// door (`infra/dev/control-tools.py`), which hands on only the names it
/// lists. `boss dispatch --from-hook` reads it to tell a dropped
/// snapshot name from a hand-run verb's absent one (backlog c02f30c5).
pub(crate) fn isolation_forwards() -> bool {
    matches!(
        dev_child_route(Path::new(DEV_CONTROL_STORE)),
        DevChild::Forward(_)
    )
}

/// `<store>/generations/<40 lowercase hex>`, exactly what activation writes.
fn is_generation(store: &Path, target: &Path) -> bool {
    target.parent() == Some(store.join("generations").as_path())
        && target
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| {
                name.len() == 40 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
}

/// The child becomes a refusal: it exits 75 naming the activation log and
/// never runs the original program here (design a68fc42a D1).
fn refuse_dev_child(cmd: &mut Command, store: &Path, why: &str) {
    let message = format!(
        "boss: REFUSED — dev isolation is on and {why}, so this child does not run beside this \
         container's credentials (design a68fc42a D1). Why: {}. boss-api and boss without \
         --built still run.",
        store.join("activation.log").display()
    );
    let mut refused = Command::new("/bin/sh");
    refused
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg("-c")
        .arg("printf '%s\\n' \"$1\" >&2; exit 75")
        .arg("boss-dev-control-refused")
        .arg(message);
    *cmd = refused;
}

/// Only a pod whose dev isolation is activated carries a selector naming
/// a generation. Other hosts retain their existing execution door;
/// workers have neither the selector nor the credential-bearing
/// controller filesystem.
fn forward_dev_child(cmd: &mut Command, launcher: &Path) {
    let program = cmd.get_program().to_os_string();
    let args: Vec<_> = cmd.get_args().map(std::ffi::OsStr::to_os_string).collect();
    let cwd = cmd.get_current_dir().map(Path::to_path_buf);
    let mut forwarded = Command::new(launcher);
    // A published interpreter is still vulnerable to inherited loader
    // and startup hooks. Only the controller's authentication context
    // crosses this boundary; candidate command environment is not the
    // environment in which the controller itself starts.
    forwarded.env_clear();
    forwarded.env("HOME", "/work/home");
    forwarded.env("PATH", "/usr/bin:/bin");
    for key in [
        "KUBERNETES_SERVICE_HOST",
        "KUBERNETES_SERVICE_PORT",
        "KUBERNETES_SERVICE_PORT_HTTPS",
    ] {
        if let Some(value) = std::env::var_os(key) {
            forwarded.env(key, value);
        }
    }
    forwarded.arg("exec-auto");
    for key in [
        "BOSS_JOBS_URL",
        "BOSS_SOR_PORTS",
        "BOSS_CAR_CONVERGED_AT",
        "BOSS_CAR_MERGE_REF",
    ] {
        let value = match cmd.get_envs().find(|(name, _)| *name == key) {
            Some((_, value)) => value.map(std::ffi::OsStr::to_os_string),
            None => std::env::var_os(key),
        };
        if let Some(value) = value {
            let mut assignment = std::ffi::OsString::from(key);
            assignment.push("=");
            assignment.push(value);
            forwarded.arg("--worker-env").arg(assignment);
        }
    }
    if let Some((_, Some(channel))) = cmd
        .get_envs()
        .find(|(name, _)| *name == "BOSS_PROBE_NOTFOUND")
    {
        forwarded.arg("--notfound-channel").arg(channel);
    }
    forwarded.arg("--").arg(program).args(args);
    if let Some(cwd) = cwd {
        forwarded.current_dir(cwd);
    }
    // Call sites set stdio after this boundary. The new program is a
    // fixed published controller; an unavailable worker is a refusal,
    // never permission to run the original program here.
    *cmd = forwarded;
}

/// An empty token directory, alive as long as this value, and the
/// environment that names it to a child.
pub(crate) struct NoTokenInReach {
    empty: tempfile::TempDir,
}

impl NoTokenInReach {
    /// Removed outright: the host list a stamping client would consult,
    /// and the single-variable token callers read until design 6805c764
    /// car 2 (a stale unit or shell may still carry it).
    const TOKEN_REMOVED: &'static [&'static str] =
        &[boss_core::machine_token::HOSTS_ENV, "BOSS_MACHINE_TOKEN"];

    /// Removed besides, for a question about a tree: the address.
    const ADDRESS_REMOVED: &'static [&'static str] = &["BOSS_JOBS_URL"];

    pub(crate) fn new() -> Result<Self> {
        let empty = tempfile::Builder::new()
            .prefix("boss-no-token-")
            .tempdir()
            .context("making the empty token directory a child runs with")?;
        // Readable by anyone, writable by its maker only: the unattended
        // probe door runs as root and drops to the probe user, who must
        // be able to look into the directory and find nothing there,
        // rather than be refused and have a client log the refusal.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // mode-bits-ok: a directory, never exec'd
            std::fs::set_permissions(empty.path(), std::fs::Permissions::from_mode(0o755))
                .context("opening the empty token directory to the probe user")?;
        }
        Ok(Self { empty })
    }

    /// The directory the child is told holds the token, and which holds
    /// nothing.
    pub(crate) fn dir(&self) -> &Path {
        self.empty.path()
    }

    /// No token in reach, the address left as the caller set it — for a
    /// probe, whose door hands it the record's address on purpose. Call
    /// it LAST on a command, so nothing set after it can name a token.
    pub(crate) fn token_only<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        for name in Self::TOKEN_REMOVED {
            cmd.env_remove(name);
        }
        cmd.env(boss_core::machine_token::TOKEN_DIR_ENV, self.dir());
        let store = Path::new(DEV_CONTROL_STORE);
        match dev_child_route(store) {
            DevChild::Local => {}
            DevChild::Forward(launcher) => forward_dev_child(cmd, &launcher),
            DevChild::Refuse(why) => refuse_dev_child(cmd, store, &why),
        }
        cmd
    }

    /// No token, no address, no rendered sor.env — for code asking a
    /// question about a tree (a lint, gate.sh's roster).
    pub(crate) fn apply<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        for name in Self::ADDRESS_REMOVED {
            cmd.env_remove(name);
        }
        self.token_only(cmd).env(
            boss_core::machine_token::SOR_ENV_FILE_ENV,
            self.dir().join("no-sor.env"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INNER: &str = "BOSS_DOOR_ENV_INNER";

    /// The names the session hands its own `boss` that are NOT identity
    /// but decide what a verb refuses or writes into a record (backlog
    /// c02f30c5): the definitions snapshot behind dispatch's
    /// unloaded-definitions refusal, and the attribution lines a brief
    /// carries. The published door dropped both, so under isolation the
    /// refusal passed every call and every brief lost its trailer.
    const SESSION_DATA_ENV: [&str; 2] = [
        crate::dispatch::SESSION_DEFINITIONS_ENV,
        crate::documents::TRAILER_ENV,
    ];

    /// A LIVE-LOOKING token: 43 base64url characters, the shape the
    /// broker mints. Not a real one — a test never reads or prints one.
    fn live_looking() -> String {
        "Zm9vYmFyLWEtdG9rZW4tdGhhdC1pcy1ub3QtcmVhbC0"
            .chars()
            .take(43)
            .collect()
    }

    /// What a child resolves the way every stamping client does —
    /// through boss-core, not a shell copy of the default — and what it
    /// would send.
    fn inner_leg() {
        let dir = boss_core::machine_token::token_dir();
        let token = boss_core::machine_token::read_slot(&dir, boss_core::machine_token::CURRENT);
        let hosts = std::env::var(boss_core::machine_token::HOSTS_ENV).is_ok();
        let old = std::env::var("BOSS_MACHINE_TOKEN").is_ok();
        let url = std::env::var("BOSS_JOBS_URL").unwrap_or_default();
        println!(
            "DIR={} TOKEN={} HOSTS={hosts} OLD={old} URL=[{url}]",
            dir.display(),
            if token.is_some() { "present" } else { "absent" },
        );
    }

    /// Re-run this test binary as a child of `apply`, the parent's
    /// environment set (or unset) the way a holder of the token has it.
    fn child(
        test: &str,
        parent_dir: Option<&Path>,
        wire: impl Fn(&NoTokenInReach, &mut Command),
    ) -> (String, std::path::PathBuf) {
        let env = NoTokenInReach::new().expect("an empty token dir");
        let mut cmd = Command::new(std::env::current_exe().expect("this test's binary"));
        cmd.args(["--exact", test, "--nocapture"]).env(INNER, "1");
        match parent_dir {
            Some(d) => cmd.env(boss_core::machine_token::TOKEN_DIR_ENV, d),
            None => cmd.env_remove(boss_core::machine_token::TOKEN_DIR_ENV),
        };
        cmd.env(boss_core::machine_token::HOSTS_ENV, "192.0.2.34")
            .env("BOSS_MACHINE_TOKEN", live_looking())
            .env("BOSS_JOBS_URL", "http://192.0.2.34:7900");
        wire(&env, &mut cmd);
        let out = cmd.output().expect("re-run this test");
        let printed = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{printed}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = printed
            .lines()
            .find(|l| l.starts_with("DIR="))
            .unwrap_or_else(|| panic!("the inner leg printed nothing: {printed}"))
            .to_string();
        (line, env.dir().to_path_buf())
    }

    /// THE PIN (backlog 844b936e). A child resolves the token directory
    /// to the EMPTY one this helper made — never boss-core's default,
    /// never the parent's — whatever the parent holds: a door's
    /// directory carrying a live-looking token, or no name at all, which
    /// is the conductor's case (the token AT the default). The second leg
    /// is the one the old removal-only `isolate` failed, measured: its
    /// child printed `DIR=/etc/boss/machine-token`. The default itself
    /// cannot be planted from a test — it is a compile-time path under
    /// /etc, and the gate runs as uid 65534 — so the leg asserts the
    /// child never RESOLVES it, which is the property; on a host that
    /// does mount a token there, the TOKEN=absent half reads it too.
    #[test]
    fn a_child_resolves_an_empty_token_dir_whatever_the_parent_holds() {
        const ME: &str =
            "door_env::tests::a_child_resolves_an_empty_token_dir_whatever_the_parent_holds";
        if std::env::var(INNER).is_ok() {
            inner_leg();
            return;
        }
        let held = boss_testing::scratch::scratch_dir("door-env-held-token");
        let doors = held.join("machine-token");
        std::fs::create_dir_all(&doors).expect("mkdir the door's token dir");
        std::fs::write(doors.join("current"), live_looking()).expect("plant a token");

        for parent in [Some(doors.as_path()), None] {
            let (line, empty) = child(ME, parent, |env, cmd| {
                env.apply(cmd);
            });
            assert!(
                line.starts_with(&format!("DIR={} ", empty.display())),
                "the child must resolve the empty dir (parent {parent:?}): {line}"
            );
            assert!(
                !line.contains(&format!(
                    "DIR={} ",
                    boss_core::machine_token::DEFAULT_TOKEN_DIR
                )),
                "{line}"
            );
            assert!(
                line.contains("TOKEN=absent HOSTS=false OLD=false URL=[]"),
                "no token, no host list, no old variable, no address: {line}"
            );
        }
    }

    /// A probe keeps the address its door set — the one difference
    /// between the two shapes — and loses everything token-shaped.
    #[test]
    fn token_only_keeps_the_address_and_nothing_token_shaped() {
        const ME: &str = "door_env::tests::token_only_keeps_the_address_and_nothing_token_shaped";
        if std::env::var(INNER).is_ok() {
            inner_leg();
            return;
        }
        let (line, empty) = child(ME, None, |env, cmd| {
            env.token_only(cmd);
        });
        assert!(
            line.starts_with(&format!("DIR={} ", empty.display())),
            "{line}"
        );
        assert!(
            line.contains("TOKEN=absent HOSTS=false OLD=false URL=[http://192.0.2.34:7900]"),
            "{line}"
        );
    }

    #[test]
    fn a_dev_control_child_is_forwarded_as_data_and_never_runs_locally() {
        let root = boss_testing::scratch_dir("dev-control-child-forwarding");
        let marker = root.join("local-candidate-ran");
        let candidate = root.join("candidate");
        boss_testing::write_exec(
            &candidate,
            &format!("#!/bin/sh\nprintf read > '{}'\n", marker.display()),
        );
        let launcher = root.join("published-launcher");
        boss_testing::write_exec(&launcher, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 75\n");
        let mut command = Command::new(&candidate);
        command.arg("literal; touch /control");
        command.env("BOSS_CAR_MERGE_REF", "refs/heads/train/example");
        command.env("BOSS_JOBS_URL", "http://example.invalid:7900");
        super::forward_dev_child(&mut command, &launcher);
        let result = command.output().expect("published launcher starts");
        assert_eq!(result.status.code(), Some(75));
        assert!(
            !marker.exists(),
            "failed transport must never fall back locally"
        );
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(output.starts_with("exec-auto\n"), "{output}");
        assert!(
            output.contains("BOSS_CAR_MERGE_REF=refs/heads/train/example"),
            "public probe input was lost: {output}"
        );
        assert!(
            output.contains("BOSS_JOBS_URL=http://example.invalid:7900"),
            "resolved probe target was lost: {output}"
        );
        assert!(
            !command.get_envs().any(|(key, _)| key == "BOSS_JOBS_URL"),
            "probe data must not select controller startup state"
        );
        assert!(
            output.contains("literal; touch /control"),
            "argv remains data: {output}"
        );
    }

    /// Design a68fc42a D1/D6 (David, 2026-10-10): the selector lives on
    /// the /work PVC, and every shape of it but a link to an activated
    /// generation REFUSES — absent alone keeps the local door, because
    /// absent is the never-activated (held) state, not a failure.
    #[test]
    fn the_dev_child_route_reads_the_one_selector_and_fails_closed() {
        use std::os::unix::fs::symlink;
        let root = boss_testing::scratch_dir("dev-control-route");
        let store = root.join("dev-control");
        std::fs::create_dir_all(&store).expect("mkdir the store");
        let selector = store.join("enabled");
        assert!(matches!(super::dev_child_route(&store), DevChild::Local));
        let generation = store.join("generations").join("a".repeat(40));
        symlink(&generation, &selector).expect("select a generation");
        match super::dev_child_route(&store) {
            DevChild::Forward(launcher) => assert_eq!(launcher, store.join("dev-build")),
            _ => panic!("an activated generation forwards"),
        }
        let foreign = Path::new("/elsewhere/generations").join("a".repeat(40));
        let short = store.join("generations/aaaa");
        for shape in [
            "refused",
            "regular file",
            "foreign generation",
            "short name",
        ] {
            std::fs::remove_file(&selector).expect("reset the selector");
            match shape {
                "refused" => symlink("refused", &selector).unwrap(),
                "regular file" => std::fs::write(&selector, "a generation").unwrap(),
                "foreign generation" => symlink(&foreign, &selector).unwrap(),
                _ => symlink(&short, &selector).unwrap(),
            }
            assert!(
                matches!(super::dev_child_route(&store), DevChild::Refuse(_)),
                "{shape} must refuse"
            );
        }
    }

    /// Backlog 1fddbe13 (reviewer run 012c8dcf, 2026-10-19): the live
    /// store is root-owned 0700, so a non-root caller (uid 65534 under
    /// infra/dev/as-gate-uid.sh) cannot stat the selector at all. That is
    /// a refusal, never the held state — the bash doors were blind to it,
    /// and infra/dev/control-tools_test.py pins them; this pins the Rust
    /// door to the same answer. The gate runs as uid 65534, where mode 0
    /// closes the store; a privileged caller reads through mode 0, so
    /// there the closed store cannot be built and the test says so.
    #[test]
    fn a_selector_the_caller_cannot_read_refuses_and_an_absent_store_does_not() {
        use std::os::unix::fs::PermissionsExt;
        let root = boss_testing::scratch_dir("dev-control-closed");
        let absent = root.join("no-such").join("dev-control");
        assert!(
            matches!(super::dev_child_route(&absent), DevChild::Local),
            "no store at all is the held state"
        );
        for closed in ["store", "ancestor"] {
            let parent = root.join(closed);
            let store = parent.join("dev-control");
            std::fs::create_dir_all(&store).expect("mkdir the store");
            let shut = if closed == "store" { &store } else { &parent };
            std::fs::set_permissions(shut, std::fs::Permissions::from_mode(0o000))
                .expect("close the store");
            let blind = matches!(
                std::fs::symlink_metadata(store.join("enabled")),
                Err(ref e) if e.kind() == std::io::ErrorKind::PermissionDenied
            );
            let route = super::dev_child_route(&store);
            // mode-bits-ok: reopens a directory for cleanup; nothing execs it.
            std::fs::set_permissions(shut, std::fs::Permissions::from_mode(0o755))
                .expect("reopen the store");
            if !blind {
                eprintln!(
                    "privileged caller: mode 0 does not close {closed} here; the gate's uid judges it"
                );
                continue;
            }
            match route {
                DevChild::Refuse(why) => assert!(why.contains("could not be read"), "{why}"),
                other => panic!("a {closed} closed to this caller must refuse, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_refused_dev_child_exits_75_naming_the_log_and_never_runs_locally() {
        let root = boss_testing::scratch_dir("dev-control-refused-child");
        let marker = root.join("local-candidate-ran");
        let candidate = root.join("candidate");
        boss_testing::write_exec(
            &candidate,
            &format!("#!/bin/sh\nprintf read > '{}'\n", marker.display()),
        );
        let store = root.join("dev-control");
        let mut command = Command::new(&candidate);
        command.arg("literal; touch /control");
        super::refuse_dev_child(&mut command, &store, "its selector names refused");
        let result = command.output().expect("the refusal runs");
        assert_eq!(result.status.code(), Some(75));
        assert!(!marker.exists(), "a refused child never runs locally");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(&store.join("activation.log").display().to_string()),
            "{stderr}"
        );
        assert!(stderr.contains("its selector names refused"), "{stderr}");
    }

    /// Backlog 19dbaab9 (2026-10-18, the first hour of isolation ON):
    /// with the selector set, `boss` and `boss-api` are the published
    /// `infra/dev/control-tools.py`, which hands the CLI only the names
    /// it lists — and it listed `BOSS_ACTOR` alone of the three this CLI
    /// reads for its own identity. `BOSS_AGENT_RUN` never arrived, so
    /// `boss dispatch <run> --started` refused and every gate-run named
    /// no run; `BOSS_ACTOR_FILE` never arrived, so a session pointing it
    /// at /dev/null signed as the shared actor file's id instead. Held
    /// here against [`crate::harness::IDENTITY_ENV`] itself, so a fourth
    /// identity name fails this test until the door carries it — and
    /// the worker-bound plans carry none, because a worker signs nothing.
    ///
    /// Held to [`SESSION_DATA_ENV`] too (backlog c02f30c5): the two names
    /// the session's own verbs read that are not identity, which the door
    /// dropped the same way.
    #[test]
    fn the_published_door_carries_every_identity_name_the_cli_reads() {
        let source = boss_testing::repo_root().join("infra/dev/control-tools.py");
        let generation = boss_testing::scratch_dir("published-door-identity");
        let data = generation.join("infra/dev");
        boss_testing::create_dir(&data);
        boss_testing::write_file(
            &generation.join("artifact.json"),
            &format!("{{\"commit\":\"{}\"}}", "a".repeat(40)),
        );
        for (name, body) in [
            ("machine-token-dir", "/fake-owned-control/machine\n"),
            ("sor-url", "http://control.invalid:7900\n"),
            ("sor-read-url", "http://read.invalid:7900\n"),
        ] {
            boss_testing::write_file(&data.join(name), body);
        }
        let out = Command::new("python3")
            .args([
                "-I",
                "-c",
                r#"
import json, sys, types
from pathlib import Path
module = types.ModuleType('published_control_tools')
exec(compile(Path(sys.argv[1]).read_bytes(), sys.argv[1], 'exec'), module.__dict__)
inherited = {name: 'fixture-' + name for name in sys.argv[3:]}
seen = {}
for tool, args in [('boss', ['dispatch']), ('boss-api', ['GET', '/api/jobs']), ('wt-cargo', []), ('wt-web', []), ('dev-build', ['start']), ('boss', ['--built', 'gate'])]:
    _, env = module.tool_plan(Path(sys.argv[2]), tool, args, dict(inherited))
    seen[' '.join([tool, *args])] = sorted(n for n in inherited if env.get(n) == inherited[n])
print(json.dumps(seen))
"#,
            ])
            .arg(&source)
            .arg(&generation)
            .args(crate::harness::IDENTITY_ENV)
            .args(SESSION_DATA_ENV)
            .output()
            .expect("run python3 over the published door's plan");
        assert!(
            out.status.success(),
            "stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let seen: std::collections::BTreeMap<String, Vec<String>> =
            serde_json::from_slice(&out.stdout).expect("the plan reads back as JSON");
        let mut carried: Vec<String> = crate::harness::IDENTITY_ENV
            .iter()
            .chain(SESSION_DATA_ENV.iter())
            .map(|n| n.to_string())
            .collect();
        carried.sort();
        for session in ["boss dispatch", "boss-api GET /api/jobs"] {
            assert_eq!(
                seen.get(session),
                Some(&carried),
                "{session}: the published door must hand the CLI every identity name and \
                 every session data name"
            );
        }
        for worker in ["wt-cargo", "wt-web", "dev-build start", "boss --built gate"] {
            assert_eq!(
                seen.get(worker),
                Some(&Vec::new()),
                "{worker}: a worker-bound plan carries no identity"
            );
        }
    }

    #[test]
    fn a_dev_control_launcher_never_loads_candidate_startup_environment() {
        let root = boss_testing::scratch_dir("dev-control-startup-env");
        let marker = root.join("candidate-startup-ran");
        let startup = root.join("startup.sh");
        boss_testing::write_file(&startup, &format!("printf read > '{}'\n", marker.display()));
        let launcher = root.join("published-launcher");
        boss_testing::write_exec(&launcher, "#!/bin/bash\nexit 75\n");
        let mut command = Command::new("candidate");
        command.env("BASH_ENV", &startup).env("PYTHONPATH", &root);
        for key in [
            "ENV",
            "CDPATH",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_COUNT",
            "BUN_CONFIG",
            "npm_config_userconfig",
            "NODE_OPTIONS",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
        ] {
            command.env(key, &startup);
        }
        command.env("PATH", &root).env("HOME", &root);
        super::forward_dev_child(&mut command, &launcher);
        let result = command.output().expect("published launcher starts");
        assert_eq!(result.status.code(), Some(75));
        assert!(
            !marker.exists(),
            "published launcher loaded candidate startup code"
        );
        assert!(
            !command
                .get_envs()
                .any(|(key, value)| key == "PYTHONPATH" && value.is_some())
        );
        assert!(command.get_envs().all(|(key, _)| matches!(
            key.to_str(),
            Some(
                "HOME"
                    | "PATH"
                    | "KUBERNETES_SERVICE_HOST"
                    | "KUBERNETES_SERVICE_PORT"
                    | "KUBERNETES_SERVICE_PORT_HTTPS"
            )
        )));
    }
}
