//! infra/lib/host-check.sh — an installer or a converge asks WHICH
//! MACHINE it is on before it writes that machine's own system (backlog
//! 62b09c57, N7).
//!
//! THE INCIDENT THESE HOLD. On 2026-10-07 23:12Z a reviewer started the
//! forge's converge launcher in a fixture tree on the dev pod with a stub
//! for one script. It ran the fixture generation's REAL forge-converge.sh
//! as root for 1.5 s, and the pod got /etc/boss/sor.env, two files under
//! /usr/local/libexec/boss, a downloaded kubectl and a `boss-probe`
//! account (review aa901496). Nothing asked which host it was on.
//!
//! WHAT IS HELD. (1) The library's three verdicts, each by effect: a run
//! whose every seam is redirected proceeds and is asked nothing; a run
//! with one seam at its real default proceeds only on a machine holding
//! an address the estate declares for the node; anywhere else it exits 78
//! on one line, before its first write. (2) Every gated script refuses in
//! that shape with one seam forgotten, and the same run with the seam
//! supplied goes through — the control that the fixture was otherwise
//! whole. (3) The reviewer's shape itself, through the launcher.
//!
//! A REFUSAL TEST MUST NOT BE ABLE TO INSTALL A HOST ONTO THE MACHINE
//! RUNNING IT, or the day the check breaks this suite repeats the
//! incident on every dev pod. Two means, said at each test: the children
//! a converge would run are tripwires that only record that they ran; and
//! a script whose real default is this machine's own directory is run as
//! an account that cannot write it (`setpriv`, when the suite is root —
//! the gate already is such an account). A check that is gone then shows
//! as a wrong exit code, never as a write.
//!
//! WHAT RUNS NOWHERE BUT ON THE HOSTS: the this-host arm through a real
//! installer as root. Here it is driven through the library by an estate
//! file that declares an address this test's own machine holds, and
//! through install.sh only as far as its root check.

// not a tree-wide pin: its read_dir lists only this test's own scratch
// trees and the system paths it proves untouched.

use boss_testing::{create_dir, repo_root, scratch_dir, write_exec, write_file};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REFUSED_FORGE: &str = "REFUSED: this machine is not the estate's forge";
const REFUSED_GCP: &str = "REFUSED: this machine is not the estate's boss-gcp";

fn infra() -> PathBuf {
    repo_root().join("infra")
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn is_root() -> bool {
    std::fs::metadata("/proc/self").unwrap().uid() == 0
}

/// `bash <script> <args>` as an account that cannot write this machine's
/// system: the suite's own when it is not root, uid 65534 through
/// setpriv when it is. `dir` is opened to that account first.
fn harmless(dir: &Path, script: &Path, args: &[&str]) -> Command {
    // Only what this account owns: a file the harmless account made on
    // an earlier call is already its own, and this pod's root may not
    // chmod it.
    let me = std::fs::metadata("/proc/self").unwrap().uid().to_string();
    let o = Command::new("find")
        .arg(dir)
        .args(["-user", &me, "-exec", "chmod", "a+rwX", "{}", "+"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    if !is_root() {
        let mut c = Command::new("bash");
        c.arg(script).args(args);
        return c;
    }
    let mut c = Command::new("setpriv");
    c.args(["--reuid=65534", "--regid=65534", "--clear-groups", "--"])
        .arg("bash")
        .arg(script)
        .args(args);
    c
}

/// What the harmless account wrote under `dir`, removed AS that account
/// when the test ends. The dev pod's root holds no CAP_DAC_OVERRIDE, so
/// it can neither read nor remove another uid's 0600 file — and a
/// leftover it cannot clear fails the next test that is handed the same
/// scratch name (boss_testing::scratch says so by name).
struct Sweep(PathBuf);

impl Drop for Sweep {
    fn drop(&mut self) {
        if is_root() {
            let _ = Command::new("setpriv")
                .args(["--reuid=65534", "--regid=65534", "--clear-groups", "--"])
                .args(["find"])
                .arg(&self.0)
                .args(["-mindepth", "1", "-user", "65534", "-delete"])
                .output();
        }
    }
}

/// A scratch directory, and the guard that sweeps it.
fn swept(name: &str) -> (PathBuf, Sweep) {
    let dir = scratch_dir(name);
    let sweep = Sweep(dir.clone());
    (dir, sweep)
}

/// A file the harmless account may have written 0600, read as it.
fn read_harmless(p: &Path) -> String {
    if !is_root() {
        return read(p);
    }
    let o = Command::new("setpriv")
        .args([
            "--reuid=65534",
            "--regid=65534",
            "--clear-groups",
            "--",
            "cat",
            "--",
        ])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// The control on `harmless`: the account it runs as is not root. A
/// refusal test that ran as root would be the incident with a test name.
#[test]
fn the_harmless_account_is_not_root() {
    let (dir, _sweep) = swept("host-check-harmless");
    let s = dir.join("id.sh");
    write_exec(&s, "#!/bin/sh\nid -u\n");
    let o = harmless(&dir, &s, &[]).output().unwrap();
    let uid = String::from_utf8_lossy(&o.stdout).trim().to_string();
    assert!(o.status.success(), "{}", text(&o));
    assert_ne!(uid, "0", "the refusal tests must not run as root");
}

/// Every address the library says this machine holds.
fn held() -> Vec<String> {
    let o = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\" && host_addresses_held")
        .arg("held")
        .arg(infra().join("lib/host-check.sh"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn declared(estate: &Path, node: &str) -> Vec<String> {
    let o = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\" && host_addresses_declared \"$2\" \"$3\"")
        .arg("declared")
        .arg(infra().join("lib/host-check.sh"))
        .arg(estate)
        .arg(node)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// An estate file declaring `addr` as the address of `node`.
fn estate_with(dir: &Path, node: &str, addr: &str) -> PathBuf {
    let p = dir.join("estate-fixture.toml");
    write_file(
        &p,
        &format!(
            "forge_host = \"192.0.2.15\"\n\n[[node]]\nid = \"other\"\naddress = \"192.0.2.1\"\n\n\
             [[node]]\nid = \"{node}\"\nlabel = \"x\"\naddress = \"{addr}\"\nrole = \"forge\"\n"
        ),
    );
    p
}

// ── what the machine holds, and what the estate declares ─────────────

#[test]
fn the_held_addresses_are_the_kernels_and_never_loopback() {
    let held = held();
    assert!(
        !held.is_empty(),
        "this machine holds no address the library can read — the this-host arm cannot be tested \
         here, and on a host it would refuse"
    );
    assert!(
        held.iter().all(|a| !a.starts_with("127.")),
        "loopback is on every machine and must never count: {held:?}"
    );
    // An independent reader: the address the kernel would send from. No
    // packet leaves — a UDP socket is only bound and aimed.
    let s = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    if s.connect("192.0.2.1:9").is_ok() {
        let own = s.local_addr().unwrap().ip().to_string();
        assert!(
            held.contains(&own),
            "the kernel sends from {own}, which the library does not list: {held:?}"
        );
    }
}

#[test]
fn the_estate_declares_an_address_each_managed_host_holds() {
    let estate = infra().join("estate/estate.toml");
    let flat = read(&estate)
        .lines()
        .find_map(|l| l.strip_prefix("forge_host = \""))
        .map(|v| v.trim_end_matches('"').to_string())
        .expect("estate.toml declares forge_host");
    assert_eq!(
        declared(&estate, "forge"),
        [flat],
        "the forge is the machine at forge_host: its node row's address"
    );
    // boss-gcp's row carries a translated public address; what the VM
    // holds is the WireGuard hub's, and that fact lives in three files.
    let hub = read(&infra().join("cluster/wireguard/setup-hub.sh"))
        .lines()
        .find_map(|l| l.strip_prefix("HUB_IP=\""))
        .map(|v| v.trim_end_matches('"').trim_end_matches("/24").to_string())
        .expect("setup-hub.sh names the hub's address");
    let gcp = declared(&estate, "boss-gcp");
    assert!(
        gcp.contains(&hub),
        "estate.toml [host_identity] must declare the hub address setup-hub.sh gives boss-gcp's \
         wg0 ({hub}); it declares {gcp:?}"
    );
    assert!(
        read(&infra().join("forge/journal-read.sh"))
            .contains(&format!("BOSS_GCP_HOST=\"{hub}:19531\"")),
        "journal-read.sh reads boss-gcp's journal door at the same address"
    );
    assert!(
        declared(&estate, "nobody").is_empty(),
        "a node the estate does not name has no address — which the check reads as a refusal"
    );
}

// ── the library's verdicts ───────────────────────────────────────────

struct Harness {
    dir: PathBuf,
    script: PathBuf,
    out: PathBuf,
    summary: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        let (dir, _sweep) = swept(&format!("host-check-{name}"));
        let script = dir.join("harness.sh");
        write_exec(
            &script,
            "#!/usr/bin/env bash\nset -euo pipefail\n. \"$INFRA/run-summary.sh\"\n. \"$INFRA/lib/host-check.sh\"\n\
             host_seam HC_SEAM /real/default\nhost_check \"$NODE\" harness\n\
             printf 'verdict=%s\\n' \"$HOST_CHECK_VERDICT\"\n: >\"$OUT\"\n",
        );
        let out = dir.join("written");
        let summary = dir.join("summary.json");
        Self {
            dir,
            script,
            out,
            summary,
        }
    }

    fn cmd(&self, node: &str) -> Command {
        let mut c = Command::new("bash");
        c.arg(&self.script)
            .env("INFRA", infra())
            .env("NODE", node)
            .env("OUT", &self.out)
            .env("BOSS_RUN_SUMMARY_FILE", &self.summary)
            .env_remove("HC_SEAM")
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE");
        c
    }

    fn summary_field(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&read(&self.summary))
            .ok()
            .and_then(|v| {
                v.get("host_check")
                    .and_then(|f| f.as_str().map(String::from))
            })
            .unwrap_or_default()
    }
}

#[test]
fn a_real_seam_on_another_machine_is_refused_on_one_line_before_the_first_write() {
    let h = Harness::new("refused");
    for seam in [None, Some("/real/default")] {
        let mut c = h.cmd("forge");
        if let Some(v) = seam {
            c.env("HC_SEAM", v);
        }
        let o = c.output().unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{out}");
        assert_eq!(
            out.lines().count(),
            1,
            "one line, naming what it found and what it expected: {out}"
        );
        assert!(out.contains(&format!("harness: {REFUSED_FORGE}")), "{out}");
        assert!(out.contains("it holds ["), "{out}");
        assert!(out.contains("declares [10.20.0.15] for forge"), "{out}");
        assert!(out.contains("through: HC_SEAM."), "{out}");
        assert!(out.contains("nothing was written"), "{out}");
        assert!(!h.out.exists(), "refused before the first write: {out}");
        assert!(h.summary_field().starts_with("REFUSED: "), "{out}");
    }
}

#[test]
fn a_run_whose_every_seam_is_redirected_is_a_test_and_is_asked_nothing() {
    let h = Harness::new("fixture");
    let o = h
        .cmd("forge")
        .env("HC_SEAM", h.dir.join("x"))
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert_eq!(out, "verdict=\n", "silent, and no identity was claimed");
    assert!(h.out.exists());
    assert_eq!(h.summary_field(), "", "a fixture's packet claims no host");
}

#[test]
fn the_machine_that_holds_the_declared_address_proceeds_and_says_which() {
    let h = Harness::new("this-host");
    let own = held().first().cloned().expect("an address to declare");
    let estate = estate_with(&h.dir, "forge", &own);
    let o = h
        .cmd("forge")
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    let said = format!(
        "forge: this machine holds {own}, which {} declares for it",
        estate.display()
    );
    assert_eq!(out, format!("verdict={said}\n"));
    assert!(h.out.exists());
    assert_eq!(h.summary_field(), said);

    // The same file read for another node is a refusal: holding an
    // address the estate gives a DIFFERENT node is not being this one.
    std::fs::remove_file(&h.out).unwrap();
    let o = h
        .cmd("boss-gcp")
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(text(&o).contains(REFUSED_GCP), "{}", text(&o));
    assert!(text(&o).contains("declares [nothing] for boss-gcp"));
    assert!(!h.out.exists());
}

#[test]
fn the_host_identity_table_names_a_node_whose_address_is_translated() {
    let h = Harness::new("identity-table");
    let own = held().first().cloned().expect("an address to declare");
    let estate = h.dir.join("estate-fixture.toml");
    write_file(
        &estate,
        &format!(
            "[[node]]\nid = \"boss-gcp\"\naddress = \"192.0.2.40\"\n\n[host_identity]\n\
             boss-gcp = \"{own}\"\n\n[ops_credentials.boss-gcp]\nforge = \"{own}\"\n"
        ),
    );
    assert_eq!(declared(&estate, "boss-gcp"), ["192.0.2.40", own.as_str()]);
    assert!(
        declared(&estate, "forge").is_empty(),
        "a line in another table is not a declaration"
    );
    let o = h
        .cmd("boss-gcp")
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains(&format!("boss-gcp: this machine holds {own}")));
}

#[test]
fn loopback_declared_for_a_node_is_satisfied_by_no_machine() {
    let h = Harness::new("loopback");
    let estate = estate_with(&h.dir, "forge", "127.0.0.1");
    let o = h
        .cmd("forge")
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
    assert!(!h.out.exists());
}

#[test]
fn the_first_install_override_proceeds_and_is_never_silent() {
    let h = Harness::new("override");
    let o = h
        .cmd("forge")
        .env("BOSS_FIRST_INSTALL_AS", "forge")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        err.contains("harness: HOST CHECK OVERRIDDEN: BOSS_FIRST_INSTALL_AS=forge"),
        "said on stderr: {err}"
    );
    assert!(err.contains("declares [10.20.0.15] for it"), "{err}");
    assert!(h.out.exists());
    assert!(
        h.summary_field()
            .starts_with("OVERRIDDEN: BOSS_FIRST_INSTALL_AS=forge"),
        "and on the run's packet: {}",
        read(&h.summary)
    );

    // It names ONE node. Set for another, or to a word that is not a
    // node, it is not an override of this script.
    for other in ["boss-gcp", "1", "yes"] {
        std::fs::remove_file(&h.out).ok();
        let o = h
            .cmd("forge")
            .env("BOSS_FIRST_INSTALL_AS", other)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(78), "{other}: {}", text(&o));
        assert!(!h.out.exists(), "{other}");
    }
}

#[test]
fn an_account_command_is_real_only_from_a_system_directory() {
    let (dir, _sweep) = swept("host-check-command");
    create_dir(&dir.join("bin"));
    write_exec(&dir.join("bin/sh"), "#!/bin/sh\nexit 0\n");
    let ask = |cmd: &str, path: String| {
        let o = Command::new("/bin/bash")
            .arg("-c")
            .arg(". \"$1\" && host_command \"$2\" && echo \"real=${host_real[*]:-}\"")
            .arg("ask")
            .arg(infra().join("lib/host-check.sh"))
            .arg(cmd)
            .env("PATH", path)
            .output()
            .unwrap();
        text(&o)
    };
    let path = std::env::var("PATH").unwrap();
    assert_eq!(ask("sh", path.clone()), "real=sh\n", "the system's own sh");
    assert_eq!(
        ask("sh", format!("{}:{path}", dir.join("bin").display())),
        "real=\n",
        "a stub first on PATH is a redirected command"
    );
    assert_eq!(
        ask("no-such-command-62b09c57", path),
        "real=\n",
        "a command this machine does not have cannot change it"
    );
}

// ── every gated script, with one seam forgotten ──────────────────────

/// What a stray install wrote on the dev pod on 2026-10-07, and the
/// directories the installers write on a host: each one's identity now.
/// Compared before and after a refusal.
fn system_state() -> String {
    let mut s = String::new();
    for p in [
        "/etc/boss",
        "/etc/boss/sor.env",
        "/etc/passwd",
        "/etc/systemd/system",
        "/etc/sudoers.d",
        "/usr/local/libexec/boss",
        "/usr/local/bin/kubectl",
        "/usr/local/bin/talosctl",
        "/var/lib/boss",
        "/var/lib/boss/tree",
        "/var/lib/boss/probe-view",
        "/opt/boss",
        "/opt/boss-cli",
    ] {
        match std::fs::symlink_metadata(p) {
            Err(_) => s.push_str(&format!("{p}: absent\n")),
            Ok(m) => {
                s.push_str(&format!(
                    "{p}: {} {} {}\n",
                    m.mtime(),
                    m.mtime_nsec(),
                    m.size()
                ));
                if m.is_dir() {
                    let mut names: Vec<String> = std::fs::read_dir(p)
                        .map(|d| {
                            d.filter_map(|e| e.ok())
                                .map(|e| e.file_name().to_string_lossy().to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    names.sort();
                    s.push_str(&format!("  {}\n", names.join(" ")));
                }
            }
        }
    }
    s
}

fn stub_systemctl(dir: &Path) -> PathBuf {
    let p = dir.join("bin/systemctl");
    create_dir(p.parent().unwrap());
    write_exec(
        &p,
        "#!/bin/sh\necho \"$*\" >> \"$STUB_LOG\"\ncase \"$1\" in is-active) echo active ;; esac\nexit 0\n",
    );
    write_exec(
        &dir.join("bin/apt-get"),
        "#!/bin/sh\necho \"apt-get $*\" >> \"$STUB_LOG\"\nexit 0\n",
    );
    p
}

/// A unit library that already carries the journal door's socket, so the
/// installers never reach for a package manager — with or without a stub.
fn unit_lib(dir: &Path) -> PathBuf {
    let p = dir.join("unit-lib");
    create_dir(&p);
    write_file(&p.join("systemd-journal-gatewayd.socket"), "[Socket]\n");
    p
}

/// The forge installer into a scratch directory, every seam redirected.
fn forge_install(dir: &Path) -> Command {
    let sysctl = stub_systemctl(dir);
    let mut c = harmless(dir, &infra().join("forge/install.sh"), &[]);
    c.env("INSTALL_ETC", dir.join("etc"))
        .env("INSTALL_SYSTEMCTL", sysctl)
        .env("STUB_LOG", dir.join("stub.log"))
        .env("INSTALL_SOR_ENV", dir.join("sor.env"))
        .env("BOSS_SOR_ENV", dir.join("sor.env"))
        .env("INSTALL_KUBECTL", "0")
        .env("INSTALL_APT_GET", dir.join("bin/apt-get"))
        .env("INSTALL_UNIT_LIB", unit_lib(dir))
        .env("BOSS_OPS_RUNNER_RETIRED", dir.join("ops-runner.retired"))
        .env("BOSS_RUN_SUMMARY_FILE", dir.join("summary.json"))
        .env_remove("INSTALL_ROOT_TREE")
        .env_remove("BOSS_CONVERGE_HOLD")
        .env_remove("INSTALL_KIT_LIBEXEC")
        .env_remove("INSTALL_PROBE_LIBEXEC")
        .env_remove("BOSS_NODE_ROLES")
        .env_remove("BOSS_INSTALL_FROM_TREE")
        .env_remove("BOSS_FIRST_INSTALL_AS")
        .env_remove("BOSS_HOST_CHECK_ESTATE");
    c
}

#[test]
fn the_forge_installer_refuses_with_one_seam_forgotten_and_runs_with_it_named() {
    for seam in [
        "INSTALL_APT_GET",
        "INSTALL_SOR_ENV",
        "INSTALL_SYSTEMCTL",
        "INSTALL_KUBECTL",
        "BOSS_OPS_RUNNER_RETIRED",
    ] {
        let (dir, _sweep) = swept("host-check-forge-install");
        create_dir(&dir.join("etc"));
        let before = system_state();
        let o = forge_install(&dir).env_remove(seam).output().unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{seam} forgotten: {out}");
        assert!(
            out.contains(&format!("install.sh: {REFUSED_FORGE}")),
            "{out}"
        );
        assert!(out.contains(seam), "the forgotten seam is named: {out}");
        assert_eq!(
            std::fs::read_dir(dir.join("etc")).unwrap().count(),
            0,
            "{seam}: no unit file was written: {out}"
        );
        assert!(
            !dir.join("sor.env").exists(),
            "{seam}: no address file: {out}"
        );
        assert!(
            !dir.join("stub.log").exists(),
            "{seam}: nothing was reloaded: {out}"
        );
        assert_eq!(
            system_state(),
            before,
            "{seam}: this machine's own system changed"
        );
    }
    // The control: with every seam named, the same fixture installs.
    let (dir, _sweep) = swept("host-check-forge-install-control");
    create_dir(&dir.join("etc"));
    let before = system_state();
    let o = forge_install(&dir).output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(dir.join("etc/forge-converge.service").is_file(), "{out}");
    assert!(!out.contains("REFUSED"), "{out}");
    assert!(
        !out.contains("host check"),
        "a fixture claims no host: {out}"
    );
    assert_eq!(
        system_state(),
        before,
        "a whole fixture writes only its fixture"
    );
}

#[test]
fn the_forge_installer_on_the_host_or_overridden_gets_past_the_check() {
    // With its unit directory at the real default and not root, the
    // installer's next line after the check is "needs root": reaching it
    // is the check having let this machine through.
    let own = held().first().cloned().expect("an address to declare");
    let (dir, _sweep) = swept("host-check-forge-install-host");
    let estate = estate_with(&dir, "forge", &own);
    let o = forge_install(&dir)
        .env_remove("INSTALL_ETC")
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains(&format!(
            "install.sh: host check — forge: this machine holds {own}"
        )),
        "{out}"
    );
    assert!(out.contains("needs root"), "{out}");

    let o = forge_install(&dir)
        .env_remove("INSTALL_ETC")
        .env("BOSS_FIRST_INSTALL_AS", "forge")
        .output()
        .unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(1), "{out}");
    assert!(
        out.contains("install.sh: HOST CHECK OVERRIDDEN: BOSS_FIRST_INSTALL_AS=forge"),
        "{out}"
    );
    assert!(out.contains("needs root"), "{out}");

    // And neither, on a machine that is not the forge: refused first.
    let o = forge_install(&dir)
        .env_remove("INSTALL_ETC")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(78), "{}", text(&o));
}

/// THE REVIEWER'S SHAPE. A tree whose `current` generation carries the
/// REAL forge-converge.sh, a stub for the legacy converge only, and the
/// launcher started with the two seams it has. Every script the converge
/// would go on to run is a tripwire that records its name and does
/// nothing else, so this is safe to run as root — which is how it ran.
struct Reviewer {
    dir: PathBuf,
    tree: PathBuf,
    ran: PathBuf,
}

impl Reviewer {
    fn new(name: &str) -> Self {
        let (dir, _sweep) = swept(&format!("host-check-reviewer-{name}"));
        let tree = dir.join("tree");
        let generation = tree.join("gen/aaa/infra");
        let ran = dir.join("ran.log");
        for d in ["forge", "estate", "ops"] {
            create_dir(&generation.join(d));
        }
        // The real converge, the libraries it sources (all of infra/lib:
        // the summary library reads one too) and the estate's own file.
        for f in [
            "forge/forge-converge.sh",
            "run-summary.sh",
            "lib",
            "estate/estate.toml",
        ] {
            let o = Command::new("cp")
                .arg("-R")
                .arg("--preserve=mode")
                .arg(infra().join(f))
                .arg(generation.join(f))
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", text(&o));
        }
        let wire = format!("#!/bin/sh\necho \"$0 $*\" >> '{}'\nexit 0\n", ran.display());
        for f in [
            "forge/credential-deposit.sh",
            "forge/machine-token-deposit.sh",
            "forge/probe-reader-deposit.sh",
            "forge/root-tree.sh",
            "forge/install.sh",
            "forge/protect-main.sh",
            "forge/credential-render.sh",
            "forge/runner-credential-deposit.sh",
            "forge/offsite-push.sh",
            "ops/install-ops-runner.sh",
        ] {
            write_exec(&generation.join(f), &wire);
        }
        write_file(
            &generation.join("estate/node-roles.sh"),
            &format!(
                "read_node_roles() {{ echo \"node-roles $*\" >> '{}'; }}\n",
                ran.display()
            ),
        );
        create_dir(&dir.join("bin"));
        write_exec(&dir.join("bin/runuser"), &wire);
        std::os::unix::fs::symlink("gen/aaa", tree.join("current")).unwrap();
        write_exec(&dir.join("legacy.sh"), &wire);
        Self { dir, tree, ran }
    }

    fn launch(&self) -> Command {
        let mut c = Command::new("bash");
        c.arg(infra().join("forge/forge-converge-launch.sh"))
            .env("BOSS_ROOT_TREE", &self.tree)
            .env("BOSS_FORGE_LEGACY_CONVERGE", self.dir.join("legacy.sh"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.dir.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("TMPDIR", &self.dir)
            .env_remove("BOSS_CONVERGE_SNAPSHOT")
            .env_remove("BOSS_FORGE_CONVERGE_INFRA")
            .env_remove("BOSS_FORGE_REPO_DIR")
            .env_remove("BOSS_RUN_SUMMARY_FILE")
            .env_remove("RUNTIME_DIRECTORY")
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE");
        c
    }
}

#[test]
fn the_launcher_started_in_a_fixture_on_another_machine_installs_nothing() {
    let r = Reviewer::new("refused");
    let before = system_state();
    let o = r.launch().output().unwrap();
    let out = text(&o);
    assert_eq!(o.status.code(), Some(78), "{out}");
    assert!(
        out.contains("forge-converge-launch: running current (aaa), try 1"),
        "the launcher chose the generation's real converge, as it did on 2026-10-07: {out}"
    );
    assert!(
        out.contains(&format!("forge-converge: {REFUSED_FORGE}")),
        "{out}"
    );
    assert!(out.contains("BOSS_FORGE_REPO_DIR"), "{out}");
    assert_eq!(
        read(&r.ran),
        "",
        "the converge reached a script it would have run — a deposit, the owner's git, the \
         installer: {out}"
    );
    assert_eq!(system_state(), before, "this machine's own system changed");
    // Nothing of the run is left in its temp directory either: the
    // snapshot it execs is removed on the refusal.
    let left: Vec<String> = std::fs::read_dir(&r.dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("forge-converge."))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn the_same_fixture_declared_the_host_runs_the_converge() {
    // The control on the tripwires, and the this-host arm through the
    // real converge: an estate file in the generation that declares an
    // address this machine holds, and the converge goes on to its
    // children — tripwires here, so nothing is installed.
    let r = Reviewer::new("host");
    let own = held().first().cloned().expect("an address to declare");
    let estate = estate_with(&r.dir, "forge", &own);
    let o = r
        .launch()
        .env("BOSS_HOST_CHECK_ESTATE", &estate)
        .output()
        .unwrap();
    let out = text(&o);
    assert!(
        out.contains(&format!(
            "forge-converge: host check — forge: this machine holds {own}"
        )),
        "{out}"
    );
    let ran = read(&r.ran);
    for f in [
        "credential-deposit.sh",
        "machine-token-deposit.sh",
        "root-tree.sh",
        "install.sh",
    ] {
        assert!(ran.contains(f), "{f} did not run: {ran}\n{out}");
    }
    assert!(!out.contains("REFUSED: this machine"), "{out}");

    // And overridden, said out loud.
    let r = Reviewer::new("override");
    let o = r
        .launch()
        .env("BOSS_FIRST_INSTALL_AS", "forge")
        .output()
        .unwrap();
    let out = text(&o);
    assert!(
        out.contains("forge-converge: HOST CHECK OVERRIDDEN: BOSS_FIRST_INSTALL_AS=forge"),
        "{out}"
    );
    assert!(read(&r.ran).contains("install.sh"), "{out}");
}

#[test]
fn the_root_trees_writing_verbs_refuse_and_its_reads_do_not() {
    let (dir, _sweep) = swept("host-check-root-tree");
    let script = infra().join("forge/root-tree.sh");
    let before = system_state();
    for verb in [vec!["refresh"], vec!["mark-good", "/nonexistent"]] {
        let o = harmless(&dir, &script, &verb)
            .env_remove("BOSS_ROOT_TREE")
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE")
            .output()
            .unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{verb:?}: {out}");
        assert!(
            out.contains(&format!("root-tree: {REFUSED_FORGE}")),
            "{out}"
        );
        assert!(out.contains("BOSS_ROOT_TREE"), "{out}");
    }
    assert_eq!(system_state(), before);
    // The reads answer on any machine: they write nothing, and the status
    // verb is how an operator asks a host what its tree says.
    for verb in ["status", "path", "head"] {
        let o = harmless(&dir, &script, &[verb])
            .env_remove("BOSS_ROOT_TREE")
            .output()
            .unwrap();
        assert_ne!(o.status.code(), Some(78), "{verb}: {}", text(&o));
        assert!(
            !text(&o).contains("REFUSED: this machine"),
            "{verb}: {}",
            text(&o)
        );
    }
    // The control: pointed at a scratch tree, a refresh is asked nothing
    // about the machine (it fails here on the source, which is absent).
    let o = harmless(&dir, &script, &["refresh"])
        .env("BOSS_ROOT_TREE", dir.join("tree"))
        .env("BOSS_ROOT_TREE_SOURCE", dir.join("nowhere"))
        .env("ROOT_TREE_SOURCE_RUNAS", "")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(
        text(&o).contains("the forge's repository is not at"),
        "{}",
        text(&o)
    );
}

#[test]
fn the_probe_accounts_ensure_refuses_with_one_seam_forgotten() {
    let script = infra().join("forge/probe-account.sh");
    for seam in [
        "INSTALL_PROBE_ETC",
        "BOSS_PROBE_VIEW",
        "INSTALL_PROBE_GITCONFIG",
        "INSTALL_PROBE_LIBEXEC",
    ] {
        let (dir, _sweep) = swept("host-check-probe-account");
        create_dir(&dir.join("etc"));
        // The account commands have no seam but PATH: stubs first on it,
        // as probe_account_sh.rs has them, so the one thing real in each
        // pass is the seam it forgets.
        create_dir(&dir.join("bin"));
        for cmd in ["useradd", "usermod"] {
            write_exec(&dir.join("bin").join(cmd), "#!/bin/sh\nexit 1\n");
        }
        let before = system_state();
        let mut c = harmless(&dir, &script, &["ensure"]);
        c.env(
            "PATH",
            format!(
                "{}:{}",
                dir.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("INSTALL_ETC", dir.join("etc"))
        .env("INSTALL_PROBE_ETC", dir.join("host-etc"))
        .env("INSTALL_PROBE_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_PROBE_GITCONFIG", dir.join("gitconfig"))
        .env("BOSS_PROBE_VIEW", dir.join("view"))
        .env("BOSS_PROBE_ACCOUNT", "boss-host-check-fixture")
        .env_remove("BOSS_FIRST_INSTALL_AS")
        .env_remove("BOSS_HOST_CHECK_ESTATE")
        .env_remove(seam);
        let o = c.output().unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{seam} forgotten: {out}");
        assert!(
            out.contains(&format!("probe-account: {REFUSED_FORGE}")),
            "{out}"
        );
        assert!(out.contains(seam), "{out}");
        assert!(
            !dir.join("libexec").exists(),
            "{seam}: refused before the first write: {out}"
        );
        assert!(!dir.join("host-etc").exists(), "{seam}: {out}");
        assert_eq!(system_state(), before, "{seam}");
    }
}

/// The account commands have no seam but PATH. With every path seam
/// named and NO stub ahead of the system's own useradd, `ensure` would
/// make a real account on this machine — as it did on the dev pod.
#[test]
fn the_probe_accounts_ensure_refuses_the_systems_own_useradd() {
    let (dir, _sweep) = swept("host-check-probe-account-useradd");
    create_dir(&dir.join("etc"));
    let system_useradd = Command::new("bash")
        .arg("-c")
        .arg("command -v useradd")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let before = system_state();
    let o = harmless(&dir, &infra().join("forge/probe-account.sh"), &["ensure"])
        .env("INSTALL_ETC", dir.join("etc"))
        .env("INSTALL_PROBE_ETC", dir.join("host-etc"))
        .env("INSTALL_PROBE_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_PROBE_GITCONFIG", dir.join("gitconfig"))
        .env("BOSS_PROBE_VIEW", dir.join("view"))
        .env("BOSS_PROBE_ACCOUNT", "boss-host-check-fixture")
        .env_remove("BOSS_FIRST_INSTALL_AS")
        .env_remove("BOSS_HOST_CHECK_ESTATE")
        .output()
        .unwrap();
    let out = text(&o);
    if system_useradd.starts_with('/') {
        assert_eq!(o.status.code(), Some(78), "{system_useradd}: {out}");
        assert!(
            out.contains(&format!("probe-account: {REFUSED_FORGE}")),
            "{out}"
        );
        assert!(out.contains("useradd"), "{out}");
        assert!(!dir.join("libexec").exists(), "{out}");
    } else {
        // A machine with no useradd cannot be given an account by it:
        // nothing here is real, and the run is asked nothing.
        assert_ne!(o.status.code(), Some(78), "{out}");
    }
    assert_eq!(system_state(), before);
}

#[test]
fn the_two_deposits_refuse_the_hosts_own_destination() {
    let before = system_state();
    for (script, rule, dest, extra) in [
        (
            "forge/machine-token-deposit.sh",
            "dispatcher/rules/broker-rotates-the-machine-token.toml",
            "/etc/boss/machine-token",
            vec![],
        ),
        (
            "forge/probe-reader-deposit.sh",
            "dispatcher/rules/broker-rotates-the-probe-reader.toml",
            "/etc/boss/probe-reader.credential",
            vec!["--ports", "/nonexistent/sor-ports.env"],
        ),
    ] {
        let (dir, _sweep) = swept("host-check-deposit");
        let kubectl = dir.join("kubectl");
        write_exec(
            &kubectl,
            &format!(
                "#!/bin/sh\necho ran >> '{}'\nexit 1\n",
                dir.join("ran.log").display()
            ),
        );
        let rule = infra().join(rule);
        let mut args = vec!["--rule", rule.to_str().unwrap(), "--dest", dest];
        args.extend(extra.iter());
        let o = harmless(&dir, &infra().join(script), &args)
            .env("BOSS_DEPOSIT_KUBECTL", &kubectl)
            .env("TMPDIR", &dir)
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE")
            .output()
            .unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{script}: {out}");
        assert!(out.contains(REFUSED_FORGE), "{script}: {out}");
        assert!(out.contains("--dest"), "{script}: {out}");
        assert!(
            !dir.join("ran.log").exists(),
            "{script}: the cluster was read before the refusal"
        );
    }
    assert_eq!(system_state(), before);
}

/// A scratch checkout for boss-gcp's converge: a repository on main with
/// nothing to fetch from, which is as far as an unrefused run gets.
///
/// Made BY the harmless account: the converge runs every git call as the
/// checkout's owner, and git refuses a repository another uid owns.
fn gcp_checkout(dir: &Path) -> PathBuf {
    let repo = dir.join("checkout");
    let make = dir.join("make-checkout.sh");
    write_exec(
        &make,
        "#!/bin/sh\nset -e\ngit init -q -b main \"$1\"\n\
         git -C \"$1\" -c user.name=fixture -c user.email=fixture@example.invalid \
         commit -q --allow-empty -m one\n",
    );
    let o = harmless(dir, &make, &[repo.to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    repo
}

#[test]
fn boss_gcps_converge_refuses_before_it_reads_or_moves_the_checkout() {
    let (dir, _sweep) = swept("host-check-gcp-converge");
    let repo = gcp_checkout(&dir);
    let sysctl = stub_systemctl(&dir);
    let before = system_state();
    let run = |forget: Option<&str>| {
        let mut c = harmless(&dir, &infra().join("gcp/boss-gcp-converge.sh"), &[]);
        c.env("BOSS_GCP_REPO_DIR", &repo)
            .env_remove("BOSS_GCP_CONVERGE_OWNER")
            .env("BOSS_GCP_CONVERGE_SOR_ENV", dir.join("sor.env"))
            .env("BOSS_JOURNALD_CONF_DIR", dir.join("journald.conf.d"))
            .env("BOSS_JOURNALD_SYSTEMCTL", &sysctl)
            .env("INSTALL_TALOSCTL", "0")
            .env("BOSS_GCP_CONVERGE_CLI_INSTALLER", dir.join("bin/apt-get"))
            .env("STUB_LOG", dir.join("stub.log"))
            .env("BOSS_RUN_SUMMARY_FILE", dir.join("summary.json"))
            .env("TMPDIR", &dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("BOSS_GCP_CONVERGE_SNAPSHOT")
            .env_remove("BOSS_GCP_CONVERGE_INFRA")
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE");
        if let Some(s) = forget {
            c.env_remove(s);
        }
        let o = c.output().unwrap();
        (o.status.code(), text(&o))
    };
    for seam in [
        "BOSS_GCP_CONVERGE_SOR_ENV",
        "BOSS_JOURNALD_CONF_DIR",
        "INSTALL_TALOSCTL",
    ] {
        let (rc, out) = run(Some(seam));
        assert_eq!(rc, Some(78), "{seam} forgotten: {out}");
        assert!(
            out.contains(&format!("boss-gcp-converge: {REFUSED_GCP}")),
            "{out}"
        );
        assert!(out.contains(seam), "{out}");
        assert!(!dir.join("sor.env").exists(), "{out}");
        let summary = read_harmless(&dir.join("summary.json"));
        assert!(
            summary.contains("REFUSED: this machine is not"),
            "the packet says why: {summary}"
        );
        assert!(
            !summary.contains("converge_remote"),
            "refused before the remote was read: {summary}"
        );
    }
    assert_eq!(system_state(), before);
    // The control: every seam named, the run is asked nothing about the
    // machine and stops where this fixture ends — it has no forge remote.
    let (rc, out) = run(None);
    assert_eq!(rc, Some(1), "{out}");
    assert!(out.contains("cannot tell which remote"), "{out}");
    assert!(!out.contains("REFUSED: this machine"), "{out}");
}

#[test]
fn boss_gcps_installers_refuse_with_one_seam_forgotten() {
    // install-units.sh `units`.
    for seam in [
        "INSTALL_APT_GET",
        "INSTALL_SYSTEMCTL",
        "BOSS_OPS_RUNNER_RETIRED",
    ] {
        let (dir, _sweep) = swept("host-check-gcp-units");
        create_dir(&dir.join("etc"));
        let sysctl = stub_systemctl(&dir);
        let before = system_state();
        let o = harmless(&dir, &infra().join("gcp/install-units.sh"), &["units"])
            .env("BOSS_REPO_ROOT", repo_root())
            .env("INSTALL_ETC", dir.join("etc"))
            .env("INSTALL_SYSTEMCTL", &sysctl)
            .env("INSTALL_APT_GET", dir.join("bin/apt-get"))
            .env("INSTALL_UNIT_LIB", unit_lib(&dir))
            .env("BOSS_OPS_RUNNER_RETIRED", dir.join("ops-runner.retired"))
            .env("STUB_LOG", dir.join("stub.log"))
            .env_remove("BOSS_FIRST_INSTALL_AS")
            .env_remove("BOSS_HOST_CHECK_ESTATE")
            .env_remove(seam)
            .output()
            .unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{seam} forgotten: {out}");
        assert!(
            out.contains(&format!("install-units: {REFUSED_GCP}")),
            "{out}"
        );
        assert!(out.contains(seam), "{out}");
        assert_eq!(
            std::fs::read_dir(dir.join("etc")).unwrap().count(),
            0,
            "{out}"
        );
        assert!(!dir.join("stub.log").exists(), "{out}");
        assert_eq!(system_state(), before, "{seam}");
    }
    // The reads of the same file are asked nothing.
    for mode in ["rows", "roster"] {
        let o = Command::new("bash")
            .arg(infra().join("gcp/install-units.sh"))
            .arg(mode)
            .env("BOSS_REPO_ROOT", repo_root())
            .env_remove("INSTALL_ETC")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{mode}: {}", text(&o));
    }

    // The credential receiver's installer.
    for seam in ["INSTALL_RECV_SUDOERS_DIR", "INSTALL_RECV_LIBEXEC"] {
        let (dir, _sweep) = swept("host-check-gcp-receiver");
        create_dir(&dir.join("sudoers.d"));
        let before = system_state();
        let o = harmless(
            &dir,
            &infra().join("gcp/install-ops-credential-receiver.sh"),
            &[],
        )
        .env("INSTALL_RECV_LIBEXEC", dir.join("libexec"))
        .env("INSTALL_RECV_SUDOERS_DIR", dir.join("sudoers.d"))
        .env("INSTALL_RECV_USER", "deposit")
        .env("INSTALL_RECV_OWNER", "")
        .env("INSTALL_VISUDO", "true")
        .env_remove("BOSS_FIRST_INSTALL_AS")
        .env_remove("BOSS_HOST_CHECK_ESTATE")
        .env_remove(seam)
        .output()
        .unwrap();
        let out = text(&o);
        assert_eq!(o.status.code(), Some(78), "{seam} forgotten: {out}");
        assert!(out.contains(REFUSED_GCP), "{out}");
        assert!(!dir.join("libexec").exists(), "{seam}: {out}");
        assert_eq!(
            std::fs::read_dir(dir.join("sudoers.d")).unwrap().count(),
            0,
            "{out}"
        );
        assert_eq!(system_state(), before, "{seam}");
    }
}

#[test]
fn the_ops_runners_installer_asks_about_the_node_it_is_told() {
    // Started directly, as the forge's converge starts it after a
    // pre-tree generation's installer, and as a hand does.
    for (node, refused) in [("forge", REFUSED_FORGE), ("boss-gcp", REFUSED_GCP)] {
        let (dir, _sweep) = swept("host-check-ops-runner");
        create_dir(&dir.join("etc"));
        let sysctl = stub_systemctl(&dir);
        let before = system_state();
        let run = |forget: Option<&str>| {
            let mut c = harmless(&dir, &infra().join("ops/install-ops-runner.sh"), &[node]);
            c.env("INSTALL_ETC", dir.join("etc"))
                .env("INSTALL_SYSTEMCTL", &sysctl)
                .env("BOSS_OPS_RUNNER_RETIRED", dir.join("ops-runner.retired"))
                .env("STUB_LOG", dir.join("stub.log"))
                .env("BOSS_JOBS_URL", "http://127.0.0.1:9")
                .env_remove("BOSS_NODE_ROLES")
                .env_remove("BOSS_RUN_SUMMARY_FILE")
                .env_remove("BOSS_FIRST_INSTALL_AS")
                .env_remove("BOSS_HOST_CHECK_ESTATE");
            if let Some(s) = forget {
                c.env_remove(s);
            }
            let o = c.output().unwrap();
            (o.status.code(), text(&o))
        };
        for seam in [
            "BOSS_OPS_RUNNER_RETIRED",
            "INSTALL_SYSTEMCTL",
            "INSTALL_ETC",
        ] {
            let (rc, out) = run(Some(seam));
            assert_eq!(rc, Some(78), "{node}, {seam} forgotten: {out}");
            assert!(
                out.contains(&format!("install-ops-runner: {refused}")),
                "{out}"
            );
            assert!(out.contains(seam), "{out}");
            assert_eq!(
                std::fs::read_dir(dir.join("etc")).unwrap().count(),
                0,
                "{out}"
            );
            assert!(!dir.join("stub.log").exists(), "{out}");
        }
        assert_eq!(system_state(), before, "{node}");
        // The control: every seam named, it installs into the fixture.
        let (rc, out) = run(None);
        assert_eq!(rc, Some(0), "{node}: {out}");
        assert!(dir.join("etc/boss-ops-runner.service").is_file(), "{out}");
    }
    // `--in-role` writes nothing and is asked nothing.
    let o = Command::new("bash")
        .arg(infra().join("ops/install-ops-runner.sh"))
        .arg("--in-role")
        .env_remove("INSTALL_ETC")
        .env_remove("BOSS_NODE_ROLES")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
}

// ── the roster, and the seams ────────────────────────────────────────

/// Every script that asks, and the node it asks about. A script that
/// installs or converges a managed host as root and is not here is in
/// LEFT below with its reason, or it is the defect this file exists for.
const GATED: &[(&str, &str)] = &[
    ("forge/install.sh", "forge"),
    ("forge/forge-converge.sh", "forge"),
    ("forge/root-tree.sh", "forge"),
    ("forge/probe-account.sh", "forge"),
    ("forge/machine-token-deposit.sh", "forge"),
    ("forge/probe-reader-deposit.sh", "forge"),
    ("gcp/boss-gcp-converge.sh", "boss-gcp"),
    ("gcp/install-units.sh", "boss-gcp"),
    ("gcp/install-ops-credential-receiver.sh", "boss-gcp"),
    // Shared by both hosts, and told which one: it asks about its argument.
    ("ops/install-ops-runner.sh", "\"$HOST\""),
];

#[test]
fn every_gated_script_sources_the_one_definition_and_names_its_node() {
    for (script, node) in GATED {
        let body = read(&infra().join(script));
        assert!(
            body.lines().any(|l| {
                let l = l.trim();
                l.starts_with(". ") && l.contains("lib/host-check.sh\"")
            }),
            "{script} does not source infra/lib/host-check.sh"
        );
        assert!(
            body.lines()
                .any(|l| l.trim().starts_with(&format!("host_check {node} "))),
            "{script} does not call host_check for {node}"
        );
        assert!(
            !body.contains("host_addresses_held") && !body.contains("fib_trie"),
            "{script} reads the machine's addresses itself — the lib is the one definition"
        );
    }
    // The launcher is NOT gated, on purpose, and says so: it is a
    // root-owned copy outside every tree that sources nothing, its one
    // write is inside the tree it was pointed at, and everything it can
    // exec asks for itself.
    let launcher = read(&infra().join("forge/forge-converge-launch.sh"));
    assert!(launcher.contains("It sources nothing."));
    assert!(
        launcher.contains("infra/lib/host-check.sh"),
        "the launcher says where the check is"
    );
    assert!(!launcher.lines().any(|l| l.trim().starts_with(". ")));
}

/// Every INSTALL_* seam a gated installer reads is one the check judges,
/// or is listed here with why it redirects no write. A seam added to an
/// installer and to neither is a way to write a host's own system from a
/// fixture that names it — the tripwire for the next forgotten one.
#[test]
fn every_install_seam_of_a_gated_installer_is_judged_or_named_a_read() {
    let not_writes: &[(&str, &str)] = &[
        (
            "INSTALL_UNIT_LIB",
            "where the journal door's socket unit is LOOKED for",
        ),
        (
            "INSTALL_ROOT_TREE",
            "a switch that turns tree management ON in a scratch run",
        ),
        (
            "INSTALL_WATCHDOG_OLD_HOME",
            "the old home the watchdog's state is READ from once",
        ),
        (
            "INSTALL_CLI",
            "set to 0 it skips the CLI step; the step's writes are BOSS_CLI_STORE and BOSS_CLI_LINK, judged",
        ),
        (
            "INSTALL_PROBE_CLI",
            "the boss binary the probe account's ensure greps",
        ),
        (
            "INSTALL_PROBE_MARKER",
            "written by verify-tick from root's installed copy, not by ensure",
        ),
        (
            "INSTALL_OPS_RUNNER_REPO",
            "the checkout the runner's ExecStart names, written into INSTALL_ETC",
        ),
        ("INSTALL_RECV_USER", "the account the sudoers rule names"),
        (
            "INSTALL_RECV_OWNER",
            "who owns the two files written under the judged directories",
        ),
        ("INSTALL_VISUDO", "the checker the rendered rule is read by"),
        (
            "INSTALL_KIT_USER",
            "the account the recovery kit's rule names",
        ),
        (
            "INSTALL_KIT_LIBEXEC",
            "set, it is the directory its caller chose and switches the kit reader's step on \
             in a scratch run; unset, the step runs only when INSTALL_ETC is the host's own, \
             which is judged. Its sudoers directory is judged under it",
        ),
    ];
    let code_of = |script: &str| -> String {
        read(&infra().join(script))
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // Judged by ANY gated script: install.sh passes the probe account's
    // and the ops runner's seams through, and each of those asks itself.
    let every: String = GATED.iter().map(|(s, _)| code_of(s)).collect();
    for (script, _) in GATED {
        let code = code_of(script);
        if !code.contains("INSTALL_") {
            continue;
        }
        let mut names: Vec<&str> = code
            .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .filter(|w| w.starts_with("INSTALL_") && w.len() > "INSTALL_".len())
            .collect();
        names.sort();
        names.dedup();
        assert!(!names.is_empty(), "{script}");
        for n in names {
            let judged = every.contains(&format!("host_seam {n}"));
            let read_only = not_writes.iter().any(|(k, _)| *k == n);
            assert!(
                judged || read_only,
                "{script} reads the seam {n}, which host_check does not judge and this test does \
                 not name as a read"
            );
            assert!(
                !(judged && read_only),
                "{script}: {n} is judged, so it is not also a read"
            );
        }
    }
}
