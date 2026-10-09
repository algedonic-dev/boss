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
        // And the one that asks which machine this is (backlog 62b09c57,
        // N7): real, so a seam this fixture forgets is a refusal here
        // rather than a write on the machine running the suite.
        copy_exec(
            &real.join("lib/host-check.sh"),
            &infra.join("lib/host-check.sh"),
        );
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
        write_exec(
            &infra.join("forge/machine-token-deposit.sh"),
            &logged("machine-token-deposit"),
        );
        write_exec(
            &infra.join("forge/probe-reader-deposit.sh"),
            &logged("probe-reader-deposit"),
        );
        write_exec(&infra.join("forge/install.sh"), &logged("install"));
        // root-tree.sh (backlog a604a35b): by default a host with NO tree
        // — the refresh answers 0 and `path` names nothing, so the run
        // executes from the directory it started in, as before. A test
        // that wants a tree writes `tree-path` (what `path` prints),
        // `tree-head` (what `head` prints) or `tree-refresh-rc`.
        write_exec(
            &infra.join("forge/root-tree.sh"),
            &format!(
                "#!/bin/sh\nR='{root}'\ncase \"$1\" in\n\
                 refresh) echo root-tree-refresh >> '{log}'; echo 'root-tree: fixture refresh'; exit \"$(cat \"$R/tree-refresh-rc\" 2>/dev/null || echo 0)\" ;;\n\
                 path) [ -f \"$R/tree-path\" ] || exit 1; cat \"$R/tree-path\" ;;\n\
                 head) [ -f \"$R/tree-head\" ] || exit 1; cat \"$R/tree-head\" ;;\n\
                 mark-good) echo \"root-tree-mark-good $2\" >> '{log}' ;;\n\
                 esac\nexit 0\n",
                root = root.display(),
                log = log.display()
            ),
        );
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
            // The three destinations the converge hands its deposits,
            // which this fixture left at the forge's own until the host
            // check judged them (backlog 62b09c57): its deposits are
            // stubs, so nothing was written — and nothing said so.
            .env("BOSS_MACHINE_TOKEN_DIR", self.root.join("machine-token"))
            .env(
                "BOSS_PROBE_READER_CREDENTIAL",
                self.root.join("probe-reader.credential"),
            )
            .env(
                "BOSS_RUNNER_CREDENTIAL_FILE",
                self.root.join("ops-runner.credential"),
            )
            .env("BOSS_FORGE_STALE_HEADER_DIR", self.root.join("stale"))
            .env("BOSS_RUN_SUMMARY_FILE", self.root.join("summary.json"))
            .env_remove("BOSS_CONVERGE_SNAPSHOT");
        c
    }

    /// An estate file that declares an address THIS machine holds as the
    /// forge's — for the tests that run the converge with a destination
    /// at the forge's own default, to read which path it hands a deposit.
    /// That shape is what infra/lib/host-check.sh refuses on any machine
    /// but the forge (backlog 62b09c57, N7); here every deposit is a stub
    /// that only records its arguments, so the this-host arm is safe to
    /// drive, and it is driven by the estate's side of the comparison —
    /// there is no seam on what the machine holds.
    fn this_machine_is_the_forge(&self) -> PathBuf {
        let o = Command::new("bash")
            .arg("-c")
            .arg(". \"$1\" && host_addresses_held")
            .arg("held")
            .arg(repo_root().join("infra/lib/host-check.sh"))
            .output()
            .expect("bash runs");
        let held = String::from_utf8_lossy(&o.stdout);
        let own = held.lines().next().expect("this machine holds an address");
        let estate = self.root.join("estate-this-machine.toml");
        write_file(
            &estate,
            &format!("[[node]]\nid = \"forge\"\naddress = \"{own}\"\n"),
        );
        estate
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

/// THE MACHINE TOKEN'S DEPOSIT RUNS BEFORE THE FETCH AND NEVER REDS THE
/// CONVERGE (backlog 88379df3). A deposit that fails — the Secret
/// unreadable, the directory unwritable, the gate matching nothing —
/// leaves its status on the packet and the run green: the converge
/// installs this host's repairs and owes the token nothing. And it runs
/// before the owner's git, the first step that can end the run, so a
/// broken forge token cannot stop the machine token following a rotation.
#[test]
fn a_failed_machine_token_deposit_never_reds_the_converge() {
    for (leg, status) in [("ok", 0), ("fault", 1), ("refused", 78), ("missing", 127)] {
        let c = Converge::new(&format!("machine-token-{leg}"));
        let stub = c.root.join("infra/forge/machine-token-deposit.sh");
        if status == 127 {
            // A checkout from before the script existed: nothing to run.
            std::fs::remove_file(&stub).expect("remove the stub");
        } else {
            write_exec(
                &stub,
                &format!(
                    "#!/bin/sh\necho \"machine-token-deposit $*\" >> '{}'\nexit {status}\n",
                    c.log.display()
                ),
            );
        }
        let out = c
            .cmd()
            .env_remove("BOSS_MACHINE_TOKEN_DIR")
            .env("BOSS_HOST_CHECK_ESTATE", c.this_machine_is_the_forge())
            .output()
            .expect("forge-converge.sh runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{leg}] the deposit's exit {status} must not become the converge's: {}",
            text(&out)
        );
        assert_eq!(
            c.summary()["machine_token_deposit_status"],
            status.to_string(),
            "[{leg}] the deposit's status rides on the packet"
        );
        let calls = c.calls();
        for ran in ["install", "protect-main", "offsite-push", "runuser-git"] {
            assert!(calls.contains(ran), "[{leg}] {ran} still ran: {calls}");
        }
        if status == 127 {
            continue;
        }
        let at = |needle: &str| {
            calls
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("[{leg}] `{needle}` never ran: {calls}"))
        };
        assert!(
            at("machine-token-deposit") < at("runuser-git"),
            "[{leg}] the deposit runs before the fetch: {calls}"
        );
        assert!(
            calls.contains("broker-rotates-the-machine-token.toml --dest /etc/boss/machine-token"),
            "[{leg}] it is handed the machine token's rule and the reader's default directory: \
             {calls}"
        );
    }
}

/// A DEPOSIT THAT NEVER RETURNS COSTS THE CONVERGE ITS BOUND, NOT ITS
/// TICK (adversarial review 9a1e289b, B1). The first draft carried the
/// deposit's exit nowhere and its TIME everywhere: a sleeping kubectl
/// held the converge at that line until the unit's TimeoutStartSec
/// killed it, before the fetch and before install.sh, on every tick. The
/// deposit is stopped at the converge's bound, 124 rides on the packet
/// with what it means, and the fetch, install.sh and main's protection
/// all still run, in seconds.
#[test]
fn a_deposit_that_never_returns_is_stopped_and_the_converge_goes_on() {
    let c = Converge::new("machine-token-hang");
    write_exec(
        &c.root.join("infra/forge/machine-token-deposit.sh"),
        &format!(
            "#!/bin/sh\necho machine-token-deposit >> '{}'\nsleep 45\n",
            c.log.display()
        ),
    );
    let started = std::time::Instant::now();
    let out = c
        .cmd()
        .env("BOSS_FORGE_DEPOSIT_BOUND_S", "1")
        .output()
        .expect("forge-converge.sh runs");
    let took = started.elapsed().as_secs();
    assert!(
        took < 30,
        "the converge waited {took}s on a deposit that never returns: {}",
        text(&out)
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(c.summary()["machine_token_deposit_status"], "124");
    assert!(
        c.summary()["machine_token_action"]
            .as_str()
            .unwrap_or_default()
            .starts_with("STOPPED: the deposit did not finish inside 1 seconds"),
        "the packet says what 124 means: {}",
        c.summary()
    );
    let calls = c.calls();
    for ran in [
        "machine-token-deposit",
        "runuser-git",
        "install",
        "protect-main",
        "offsite-push",
    ] {
        assert!(calls.contains(ran), "{ran} still ran: {calls}");
    }
}

/// THE PROBE READER'S DEPOSIT RUNS BEFORE THE FETCH, AFTER THE ESTATE
/// TOKEN'S, AND NEVER REDS THE CONVERGE (backlog d26515c5, unit 7). A
/// deposit that faults — a collision, a gate that says no, a Secret that
/// cannot be read — leaves its status on the packet and the run green,
/// and the estate token's deposit is called exactly as it was before.
#[test]
fn a_failed_probe_reader_deposit_never_reds_the_converge() {
    for (leg, status) in [("ok", 0), ("fault", 1), ("refused", 78), ("missing", 127)] {
        let c = Converge::new(&format!("probe-reader-{leg}"));
        let stub = c.root.join("infra/forge/probe-reader-deposit.sh");
        if status == 127 {
            // A checkout from before the script existed: nothing to run.
            std::fs::remove_file(&stub).expect("remove the stub");
        } else {
            write_exec(
                &stub,
                &format!(
                    "#!/bin/sh\necho \"probe-reader-deposit $*\" >> '{}'\nexit {status}\n",
                    c.log.display()
                ),
            );
        }
        write_exec(
            &c.root.join("infra/forge/machine-token-deposit.sh"),
            &format!(
                "#!/bin/sh\necho \"machine-token-deposit $*\" >> '{}'\n",
                c.log.display()
            ),
        );
        let out = c
            .cmd()
            .env_remove("BOSS_MACHINE_TOKEN_DIR")
            .env_remove("BOSS_PROBE_READER_CREDENTIAL")
            .env_remove("BOSS_PROBE_VIEW")
            .env("BOSS_HOST_CHECK_ESTATE", c.this_machine_is_the_forge())
            .output()
            .expect("forge-converge.sh runs");
        assert_eq!(
            out.status.code(),
            Some(0),
            "[{leg}] the deposit's exit {status} must not become the converge's: {}",
            text(&out)
        );
        assert_eq!(
            c.summary()["probe_reader_deposit_status"],
            status.to_string(),
            "[{leg}] the deposit's status rides on the packet"
        );
        assert_eq!(
            c.summary()["machine_token_deposit_status"],
            "0",
            "[{leg}] the estate token's status is its own"
        );
        let calls = c.calls();
        for ran in ["install", "protect-main", "offsite-push", "runuser-git"] {
            assert!(calls.contains(ran), "[{leg}] {ran} still ran: {calls}");
        }
        assert!(
            calls
                .contains("broker-rotates-the-machine-token.toml --dest /etc/boss/machine-token\n"),
            "[{leg}] the estate token's deposit is called as it always was: {calls}"
        );
        if status == 127 {
            continue;
        }
        let at = |needle: &str| {
            calls
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("[{leg}] `{needle}` never ran: {calls}"))
        };
        assert!(
            at("machine-token-deposit") < at("probe-reader-deposit")
                && at("probe-reader-deposit") < at("runuser-git"),
            "[{leg}] after the estate token's deposit and before the fetch: {calls}"
        );
        let infra = c.root.join("infra");
        assert!(
            calls.contains(&format!(
                "probe-reader-deposit --rule {infra}/dispatcher/rules/broker-rotates-the-probe-reader.toml \
                 --ports /var/lib/boss/probe-view/infra/forge/sor-ports.env \
                 --dest /etc/boss/probe-reader.credential\n",
                infra = infra.display()
            )),
            "[{leg}] it is handed the reader's rule, the ROOT-OWNED probe view's port table \
             (never the checkout's) and the door's own path, and nothing else: {calls}"
        );
    }
}

/// A PROBE READER DEPOSIT THAT NEVER RETURNS COSTS ITS BOUND, NOT THE
/// TICK: stopped, 124 on the packet with what it means, an effect line
/// that is not a pass, and the fetch, install.sh and main's protection
/// all still run.
#[test]
fn a_probe_reader_deposit_that_never_returns_is_stopped() {
    let c = Converge::new("probe-reader-hang");
    write_exec(
        &c.root.join("infra/forge/probe-reader-deposit.sh"),
        &format!(
            "#!/bin/sh\necho probe-reader-deposit >> '{}'\nsleep 45\n",
            c.log.display()
        ),
    );
    let started = std::time::Instant::now();
    let out = c
        .cmd()
        .env("BOSS_FORGE_READER_DEPOSIT_BOUND_S", "1")
        .output()
        .expect("forge-converge.sh runs");
    let took = started.elapsed().as_secs();
    assert!(took < 30, "the converge waited {took}s: {}", text(&out));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(c.summary()["probe_reader_deposit_status"], "124");
    assert!(
        c.summary()["probe_reader_action"]
            .as_str()
            .unwrap_or_default()
            .starts_with("STOPPED: the deposit did not finish inside 1 seconds"),
        "{}",
        c.summary()
    );
    assert!(
        c.summary()["probe_reader_effect"]
            .as_str()
            .unwrap_or_default()
            .starts_with("UNVERIFIED"),
        "a stopped deposit records no pass: {}",
        c.summary()
    );
    let calls = c.calls();
    for ran in [
        "probe-reader-deposit",
        "runuser-git",
        "install",
        "protect-main",
        "offsite-push",
    ] {
        assert!(calls.contains(ran), "{ran} still ran: {calls}");
    }
}

/// A generation of root's tree for the converge to run from: its own
/// install.sh, protect-main.sh and offsite-push.sh, each logging that
/// the TREE's copy ran.
fn a_tree(c: &Converge, sha: &str) -> PathBuf {
    let tree_gen = c.root.join("tree/gen").join(sha);
    create_dir(&tree_gen.join("infra/forge"));
    for name in ["install", "protect-main", "offsite-push"] {
        write_exec(
            &tree_gen.join("infra/forge").join(format!("{name}.sh")),
            &format!(
                "#!/bin/sh\necho \"tree-{name} sha=$BOSS_CONVERGE_SHA\" >> '{}'\n",
                c.log.display()
            ),
        );
    }
    write_file(
        &c.root.join("tree-path"),
        &format!("{}\n", tree_gen.display()),
    );
    // A generation of this car's kind carries root-tree.sh; a test of a
    // PRE-CAR generation (a revert) removes it.
    write_exec(&tree_gen.join("infra/forge/root-tree.sh"), "#!/bin/sh\n");
    write_file(&c.root.join("tree-head"), &format!("{sha}\n"));
    tree_gen
}

/// ROOT'S TREE IS WHERE THE REST OF THE RUN EXECUTES FROM (backlog
/// a604a35b). After the refresh, install.sh, protect-main and
/// offsite-push are the generation's own — never the checkout's
/// ($BOSS_FORGE_REPO_DIR here holds stubs that log a different word) —
/// the commit handed to install.sh is the generation's stamp, not the
/// owner's answer, and the run offers its own directory to be marked.
#[test]
fn the_run_installs_from_roots_tree_at_the_trees_commit() {
    let c = Converge::new("from-tree");
    let sha = "89abcdef0123456789abcdef0123456789abcdef";
    a_tree(&c, sha);
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let calls = c.calls();
    for name in ["install", "protect-main", "offsite-push"] {
        assert!(
            calls.contains(&format!("tree-{name} sha={sha}")),
            "{name} must run from root's tree at the tree's commit: {calls}"
        );
    }
    assert!(
        !calls.lines().any(|l| l == "install"),
        "the checkout-side install.sh must not run once a tree stands: {calls}"
    );
    assert!(calls.contains("root-tree-refresh"), "{calls}");
    assert!(calls.contains("root-tree-mark-good "), "{calls}");
    let s = c.summary();
    assert_eq!(s["converge_sha"], sha, "{s}");
    assert!(
        s["runs_from"]
            .as_str()
            .unwrap_or("")
            .ends_with(&format!("tree/gen/{sha}/infra")),
        "{s}"
    );
}

/// A REFRESH THAT FAILS REDS THE RUN AND STOPS NOTHING. The tree as it
/// stands still installs, the generation is NOT offered as good (it has
/// not shown it can bring in the next one), and the exit is the
/// refresh's own — after everything else ran.
#[test]
fn a_tree_that_could_not_be_refreshed_still_installs_and_reds_the_run() {
    let c = Converge::new("refresh-fails");
    a_tree(&c, "89abcdef0123456789abcdef0123456789abcdef");
    write_file(&c.root.join("tree-refresh-rc"), "3\n");
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    let calls = c.calls();
    assert!(calls.contains("tree-install "), "{calls}");
    assert!(
        calls.contains("credential-render"),
        "the rest of the run went on: {calls}"
    );
    assert!(!calls.contains("root-tree-mark-good"), "{calls}");
    assert_eq!(c.summary()["root_tree"], "root-tree: fixture refresh");
}

/// THE OWNER'S CHECKOUT IS NO LONGER WHAT THE RUN DEPENDS ON. A fetch or
/// checkout of it that fails used to end the run before install.sh; it
/// is carried now, named on the packet, and the units still converge.
#[test]
fn a_checkout_that_cannot_be_fetched_no_longer_stops_the_install() {
    let c = Converge::new("checkout-fails");
    write_exec(
        &c.root.join("bin/runuser"),
        "#!/bin/sh\ncase \"$1\" in\n-u) shift 3; exec \"$@\" ;;\n-l) case \"$4\" in *'rev-parse HEAD'*) echo 0123456789abcdef0123456789abcdef01234567; exit 0 ;; esac; exit 128 ;;\nesac\nexit 1\n",
    );
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(128), "{}", text(&out));
    let calls = c.calls();
    assert!(
        calls.lines().any(|l| l == "install"),
        "install.sh still ran: {calls}"
    );
    assert!(calls.contains("root-tree-refresh"), "{calls}");
    assert!(
        c.summary()["checkout"]
            .as_str()
            .unwrap_or("")
            .starts_with("FAILED (exit 128)"),
        "{}",
        c.summary()
    );
}

/// AFTER A MERGED REVERT THE RUNNER STAYS ON THE CHECKOUT (review
/// 5f3736a2, F2). A revert of the car is a generation with no
/// root-tree.sh; its installer is the pre-car one, and run from root's
/// tree it points the ops runner at the generation it stands in — an
/// export that is not a git checkout, where the publish, merge and tag
/// verbs fail. The pre-car installer cannot be changed, so the converge
/// that ran it — still the car's — runs its OWN runner installer right
/// after, naming the checkout. Not for a generation that carries
/// root-tree.sh: its installer names the checkout itself.
#[test]
fn after_a_pre_car_generation_installs_the_runner_is_pointed_back_at_the_checkout() {
    let c = Converge::new("revert");
    create_dir(&c.root.join("infra/ops"));
    write_exec(
        &c.root.join("infra/ops/install-ops-runner.sh"),
        &format!(
            "#!/bin/sh\necho \"runner-installer host=$1 repo=[$INSTALL_OPS_RUNNER_REPO]\" >> '{}'\n",
            c.log.display()
        ),
    );
    let sha = "89abcdef0123456789abcdef0123456789abcdef";
    let tree_gen = a_tree(&c, sha);
    std::fs::remove_file(tree_gen.join("infra/forge/root-tree.sh")).unwrap();
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let calls = c.calls();
    let install = calls
        .find("tree-install ")
        .expect("the generation's installer ran");
    let repoint = calls
        .find(&format!(
            "runner-installer host=forge repo=[{}]",
            c.root.display()
        ))
        .unwrap_or_else(|| panic!("the runner was not pointed back at the checkout: {calls}"));
    assert!(
        repoint > install,
        "after the pre-car installer, not before: {calls}"
    );
    assert!(
        c.summary()["ops_runner_repointed"]
            .as_str()
            .unwrap_or("")
            .contains("pre-car"),
        "{}",
        c.summary()
    );

    // A generation that carries root-tree.sh needs no second word.
    let c = Converge::new("no-revert");
    create_dir(&c.root.join("infra/ops"));
    write_exec(
        &c.root.join("infra/ops/install-ops-runner.sh"),
        &format!(
            "#!/bin/sh\necho runner-installer >> '{}'\n",
            c.log.display()
        ),
    );
    a_tree(&c, sha);
    let out = c.cmd().output().expect("forge-converge.sh runs");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(!c.calls().contains("runner-installer"), "{}", c.calls());
}
