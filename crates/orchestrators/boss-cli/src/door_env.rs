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

/// Only the published dev control container carries this marker. Other
/// hosts retain their existing execution door; workers have neither
/// the marker nor the credential-bearing controller filesystem.
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
        if std::fs::symlink_metadata("/etc/boss-dev-control/enabled").is_ok() {
            forward_dev_child(cmd, Path::new("/opt/boss-dev-control/dev-build"));
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
