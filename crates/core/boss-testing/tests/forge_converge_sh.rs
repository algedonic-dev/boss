//! `infra/forge/forge-converge.sh` keeps the forge token's Authorization
//! header in the unit's RUNTIME DIRECTORY, or writes none and skips its
//! readers — while still installing the unit that carries the directory
//! (backlog 359a811c, 2026-09-28).
//!
//! THE DEFECT. The header protect-main.sh and offsite-push.sh read was a
//! `mktemp -t forge-auth.XXXXXX` file under the host's shared /tmp. It was
//! 0600 and an EXIT trap removed it, so it outlived its run only on a
//! SIGKILL (TimeoutStartSec's second signal) or a power loss — and then
//! the token stayed on disk until tmp cleanup. The same class the ops
//! runner's github-act.sh left in 5f77b205.
//!
//! THE FIX. forge-converge.service declares `RuntimeDirectory=` (tmpfs
//! under /run, 0700, removed by systemd when the oneshot run stops,
//! whatever the exit) and the script makes the header there, first
//! sweeping any `forge-auth.*` an earlier killed run left in /tmp. Without
//! the variable the script writes no header, skips protect-main and
//! offsite-push, records `refused`, and ends 78 — but the deposit, the
//! fetch, install.sh and the DR render still run. That last half is the
//! point: cluster-deploy-runner checks the new script out a minute after a
//! merge, so its first tick runs under the OLD unit, and only install.sh
//! can bring the new one. The first draft refused the whole run there and
//! would have wedged the converge for good (adversarial review,
//! 2026-09-28). The unit's shape (and why it takes no mount namespace) is
//! pinned in ops_runner_unit_keeps_the_host_mount_namespace.rs.
//!
//! The script is RUN here, not read: every program it calls is a stub in
//! a scratch tree (`BOSS_FORGE_REPO_DIR` / `BOSS_FORGE_CONVERGE_INFRA`),
//! `runuser` included, and protect-main's stub records the header path it
//! was handed and what the file held while it ran.

// not a tree-wide pin: its read_dir lists only this test's own scratch
// tree, and the `infra` it joins is that scratch tree's stub directory.

use boss_testing::{copy_exec, create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

const TOKEN: &str = "fake-forge-token-359a811c";

struct Converge {
    root: PathBuf,
    run_dir: PathBuf,
    log: PathBuf,
}

impl Converge {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("forge-converge-{name}"));
        let infra = root.join("infra");
        for d in [
            "bin",
            "tmp",
            "infra/lib",
            "infra/estate",
            "infra/forge",
            "run",
            "stale",
        ] {
            create_dir(&root.join(d));
        }
        let run_dir = root.join("run/forge-converge");
        create_dir(&run_dir);
        // mode-bits-ok: a directory, as systemd makes RuntimeDirectoryMode=0700.
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod the runtime directory");
        let log = root.join("calls.log");
        write_file(&log, "");

        // The two libraries the script sources from its infra dir, real.
        let real = repo_root().join("infra");
        copy_exec(&real.join("run-summary.sh"), &infra.join("run-summary.sh"));
        copy_exec(&real.join("lib/jq.sh"), &infra.join("lib/jq.sh"));
        // The roles read goes to the system of record; not this test's.
        write_file(
            &infra.join("estate/node-roles.sh"),
            "read_node_roles() { :; }\n",
        );

        let logged = |name: &str| format!("#!/bin/sh\necho {name} >> '{}'\n", log.display());
        write_exec(
            &infra.join("forge/credential-deposit.sh"),
            &logged("credential-deposit"),
        );
        write_exec(
            &infra.join("forge/credential-render.sh"),
            &logged("credential-render"),
        );
        write_exec(
            &infra.join("forge/runner-credential-deposit.sh"),
            &logged("runner-credential-deposit"),
        );
        write_exec(&infra.join("forge/install.sh"), &logged("install"));
        write_exec(
            &infra.join("forge/protect-main.sh"),
            &format!(
                "#!/bin/sh\nf=\"$BOSS_FORGE_AUTH_HEADER_FILE\"\n\
                 echo \"protect-main header=$f\" >> '{log}'\n\
                 [ -f \"$f\" ] && echo \"protect-main mode=$(stat -c %a \"$f\") body=$(cat \"$f\")\" >> '{log}'\n\
                 exit 0\n",
                log = log.display()
            ),
        );
        write_exec(
            &infra.join("forge/offsite-push.sh"),
            &format!(
                "#!/bin/sh\necho \"offsite-push header=$BOSS_FORGE_AUTH_HEADER_FILE\" >> '{}'\n",
                log.display()
            ),
        );
        // runuser: `-u OWNER -- cmd…` runs cmd (the token read), `-l OWNER
        // -c '…'` is the fetch/checkout (nothing) or the sha read.
        write_exec(
            &root.join("bin/runuser"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n\
                 -u) echo runuser-read >> '{log}'; shift 3; exec \"$@\" ;;\n\
                 -l) case \"$4\" in *'rev-parse HEAD'*) echo 0123456789abcdef0123456789abcdef01234567 ;; *) echo runuser-git >> '{log}' ;; esac; exit 0 ;;\n\
                 esac\nexit 1\n",
                log = log.display()
            ),
        );
        write_file(&root.join("forge-checkout.token"), TOKEN);
        Self { root, run_dir, log }
    }

    fn cmd(&self) -> Command {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut c = Command::new("bash");
        c.arg(repo_root().join("infra/forge/forge-converge.sh"))
            .env("PATH", path)
            .env("TMPDIR", self.root.join("tmp"))
            .env("RUNTIME_DIRECTORY", &self.run_dir)
            .env("BOSS_FORGE_REPO_DIR", &self.root)
            .env("BOSS_FORGE_REPO_OWNER", "owner")
            .env("BOSS_FORGE_CONVERGE_INFRA", self.root.join("infra"))
            .env(
                "BOSS_FORGE_TOKEN_FILE",
                self.root.join("forge-checkout.token"),
            )
            .env(
                "BOSS_GITHUB_DR_TOKEN_FILE",
                self.root.join("github-dr.token"),
            )
            .env("BOSS_FORGE_STALE_HEADER_DIR", self.root.join("stale"))
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env_remove("BOSS_CONVERGE_SNAPSHOT");
        c
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).expect("calls.log")
    }

    fn summary(&self) -> serde_json::Value {
        let p = self.root.join("summary.json");
        let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}: {text}", p.display()))
    }

    /// A header file in the stale dir, its mtime shifted by `secs`.
    fn plant(&self, name: &str, secs: i64) -> PathBuf {
        let p = self.root.join("stale").join(name);
        write_file(&p, "Authorization: token old\n");
        let now = SystemTime::now();
        let at = if secs < 0 {
            now - Duration::from_secs(secs.unsigned_abs())
        } else {
            now + Duration::from_secs(secs.unsigned_abs())
        };
        std::fs::File::options()
            .write(true)
            .open(&p)
            .and_then(|f| f.set_modified(at))
            .unwrap_or_else(|e| panic!("set mtime {}: {e}", p.display()));
        p
    }
}

fn text(o: &Output) -> String {
    format!(
        "exit {:?}\n--- stdout\n{}\n--- stderr\n{}",
        o.status.code(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .count()
}

/// THE HEADER IS MADE IN THE RUNTIME DIRECTORY, 0600, carrying the token
/// the owner's file holds, and handed to both readers; the trap empties
/// the directory on a normal end, and nothing is left under TMPDIR. A
/// `forge-auth.*` older than the run's directory — a leftover of a killed
/// run from before this car — is swept and counted; a newer one is not.
#[test]
fn the_forge_header_is_made_in_the_runtime_directory() {
    let c = Converge::new("header");
    let stale = c.plant("forge-auth.OLD123", -3600);
    let fresh = c.plant("forge-auth.NEW456", 3600);
    let other = c.plant("not-a-header.OLD", -3600);
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let calls = c.calls();

    let header = calls
        .lines()
        .find_map(|l| l.strip_prefix("protect-main header="))
        .unwrap_or_else(|| panic!("protect-main was not run: {calls}\n{}", text(&out)));
    let want = format!("{}/forge-auth.", c.run_dir.display());
    assert!(
        header.starts_with(&want),
        "the forge token's header file {header} is not under the unit's RUNTIME_DIRECTORY \
         ({want}…) — under the shared /tmp a SIGKILL or a power loss leaves the token on disk"
    );
    assert!(
        calls.contains(&format!(
            "protect-main mode=600 body=Authorization: token {TOKEN}"
        )),
        "protect-main must read a 0600 header carrying the owner's token: {calls}"
    );
    assert!(
        calls.contains(&format!("offsite-push header={header}")),
        "offsite-push reads the same header file: {calls}"
    );
    assert_eq!(
        entries(&c.run_dir),
        0,
        "the header must be gone from the runtime directory after a normal end"
    );
    assert_eq!(
        entries(&c.root.join("tmp")),
        0,
        "nothing — the snapshot included — is left under TMPDIR"
    );
    assert!(!stale.exists(), "a leftover older than the run is swept");
    assert!(
        fresh.exists(),
        "a header newer than the run's directory is not a leftover"
    );
    assert!(other.exists(), "the sweep touches only forge-auth.*");
    assert_eq!(
        c.summary()["stale_headers_removed"],
        "1",
        "the sweep's count reaches the packet"
    );
    assert!(
        c.summary().get("refused").is_none(),
        "a run with its directory refuses nothing"
    );
}

/// THE NEW SCRIPT UNDER THE OLD UNIT STILL INSTALLS. Without the
/// directory — unset, or a path that is not one — no header is written
/// and neither reader runs, the run ends 78 naming the variable and the
/// unit line, `refused` reaches the packet, and the token is never read.
/// But the deposit, the fetch, install.sh and the DR render DO run: that
/// tick is the one that installs the unit carrying RuntimeDirectory=, so
/// the next tick runs clean. Refusing the whole run here wedged the
/// converge for good (adversarial review of the first draft, 2026-09-28).
#[test]
fn the_new_script_under_the_old_unit_still_installs() {
    for (leg, missing) in [("unset", None), ("absent", Some("run/not-made"))] {
        let c = Converge::new(&format!("refuse-{leg}"));
        let mut cmd = c.cmd();
        match missing {
            None => {
                cmd.env_remove("RUNTIME_DIRECTORY");
            }
            Some(rel) => {
                cmd.env("RUNTIME_DIRECTORY", c.root.join(rel));
            }
        }
        let out = cmd.output().expect("forge-converge.sh runs");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(78),
            "[{leg}] a missing runtime directory must be REFUSED (78): {}",
            text(&out)
        );
        assert!(
            err.contains("RUNTIME_DIRECTORY") && err.contains("RuntimeDirectory="),
            "[{leg}] the refusal names the variable and the unit line that sets it: {err}"
        );
        assert!(!err.contains(TOKEN), "[{leg}] the refusal names no token");
        let calls = c.calls();
        for ran in [
            "credential-deposit",
            "runuser-git",
            "install",
            "credential-render",
        ] {
            assert!(
                calls.lines().any(|l| l == ran),
                "[{leg}] `{ran}` must still run without the directory — it never touches the \
                 header, and install.sh is what brings the unit that carries it: {calls}"
            );
        }
        for skipped in ["protect-main", "offsite-push", "runuser-read"] {
            assert!(
                !calls.lines().any(|l| l.starts_with(skipped)),
                "[{leg}] `{skipped}` must not run without the directory (no header, no token \
                 read): {calls}"
            );
        }
        for dir in ["tmp", "stale", "run"] {
            let hdrs: Vec<_> = std::fs::read_dir(c.root.join(dir))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("forge-auth."))
                .collect();
            assert!(hdrs.is_empty(), "[{leg}] a header was written under {dir}/");
        }
        let refused = c.summary()["refused"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            refused.contains("RUNTIME_DIRECTORY"),
            "[{leg}] the packet's summary carries `refused` naming RUNTIME_DIRECTORY: {}",
            c.summary()
        );
    }
}
