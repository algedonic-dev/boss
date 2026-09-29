//! `infra/gcp/cap-journal.sh` is RUN, not read — into a scratch
//! drop-in directory with a stub `systemctl` — so every verdict below is
//! one the script actually reached.
//!
//! WHY IT EXISTS (backlog d3c7eada, 2026-09-29). boss-gcp's 48 GB root
//! sat below its 17 GB estate floor for twelve days (`disk_tight:
//! boss-gcp`). The passkey-signed reclaim-gcp-root ran at 14:18Z today
//! (ops-request 46cc2304) and took root from 10421 to 16692 MiB free —
//! still under the floor, which the observer reads to the NEAREST GiB
//! (infra/estate/observe-host.sh), so it needs 16896 MiB. Its own effect
//! line says the rest: "The journal grows back without a SystemMaxUse
//! bound". Measured in that run's output: the vacuum deleted 60 archived
//! files, 3138 MiB, whose first entries span 2026-08-13 to 09-14, and
//! left 916.4M for 09-14 to 09-29 — journald's default ceiling (10% of
//! the filesystem, at most 4G) is what held the journal at 3.9G, and it
//! regrows at roughly 60 MB a day toward it. A one-off vacuum is a loan;
//! a `SystemMaxUse` drop-in is the bound, and the converge that puts
//! every other file on this host is what puts it down.
//!
//! What each case pins: the tree's drop-in declares the bound under
//! `[Journal]`; a first run installs it byte-for-byte and restarts
//! journald (the one way journald re-reads its config and applies a
//! lower ceiling); a second run changes nothing and restarts nothing (the
//! converge runs every tick); a hand-edited drop-in is put back; a
//! restart that fails is a failed run, not a quiet one; and the converge
//! itself calls the script from its converged tree.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/gcp/cap-journal.sh";
const DROP_IN: &str = "infra/gcp/journald-cap.conf";
const INSTALLED_NAME: &str = "boss-gcp-journal-cap.conf";

struct Case {
    root: PathBuf,
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(name);
        write_exec(
            &root.join("systemctl"),
            "#!/bin/sh\necho \"systemctl $*\" >>\"$STUB_LOG\"\nexit \"${STUB_RC:-0}\"\n",
        );
        Case { root }
    }

    fn conf_dir(&self) -> PathBuf {
        self.root.join("journald.conf.d")
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.root.join("calls.log")).unwrap_or_default()
    }

    fn run(&self, stub_rc: i32) -> (i32, String) {
        let out = Command::new("bash")
            .arg(repo_root().join(SCRIPT))
            .env("BOSS_JOURNALD_CONF_DIR", self.conf_dir())
            .env("BOSS_JOURNALD_SYSTEMCTL", self.root.join("systemctl"))
            .env("STUB_LOG", self.root.join("calls.log"))
            .env("STUB_RC", stub_rc.to_string())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_drop_in_bounds_the_journal_under_its_journal_section() {
    let text = read(&repo_root().join(DROP_IN));
    let live: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(
        live,
        vec!["[Journal]", "SystemMaxUse=256M"],
        "{DROP_IN} must set exactly SystemMaxUse under [Journal]:\n{text}"
    );
}

#[test]
fn a_first_run_installs_the_cap_and_restarts_journald() {
    let case = Case::new("cap-journal-first");
    let (rc, out) = case.run(0);
    assert_eq!(rc, 0, "first run failed:\n{out}");
    let installed = case.conf_dir().join(INSTALLED_NAME);
    assert_eq!(
        read(&installed),
        read(&repo_root().join(DROP_IN)),
        "the installed drop-in must be the tree's, byte for byte"
    );
    assert!(
        case.calls().contains("systemctl restart systemd-journald"),
        "journald must be restarted to apply the cap; calls:\n{}",
        case.calls()
    );
    assert!(
        out.contains("SystemMaxUse=256M"),
        "the run must name the bound:\n{out}"
    );
}

#[test]
fn a_second_run_changes_nothing_and_restarts_nothing() {
    let case = Case::new("cap-journal-idempotent");
    let (rc, out) = case.run(0);
    assert_eq!(rc, 0, "first run failed:\n{out}");
    std::fs::remove_file(case.root.join("calls.log")).unwrap();
    let (rc, out) = case.run(0);
    assert_eq!(rc, 0, "second run failed:\n{out}");
    assert_eq!(
        case.calls(),
        "",
        "an unchanged cap must not restart journald"
    );
    assert!(
        out.contains("unchanged"),
        "the second run must say it changed nothing:\n{out}"
    );
}

#[test]
fn a_hand_edited_drop_in_is_put_back() {
    let case = Case::new("cap-journal-drift");
    std::fs::create_dir_all(case.conf_dir()).unwrap();
    let installed = case.conf_dir().join(INSTALLED_NAME);
    write_file(&installed, "[Journal]\nSystemMaxUse=8G\n");
    let (rc, out) = case.run(0);
    assert_eq!(rc, 0, "run failed:\n{out}");
    assert_eq!(read(&installed), read(&repo_root().join(DROP_IN)));
    assert!(case.calls().contains("systemctl restart systemd-journald"));
}

#[test]
fn a_restart_that_fails_is_a_failed_run() {
    let case = Case::new("cap-journal-restart-fails");
    let (rc, out) = case.run(1);
    assert_ne!(rc, 0, "a failed journald restart must fail the run:\n{out}");
    assert!(
        out.contains("systemd-journald"),
        "the failure must name what failed:\n{out}"
    );
    // Left in place, the next tick would read "unchanged" and never
    // retry the restart; removed, it retries.
    assert!(
        !case.conf_dir().join(INSTALLED_NAME).exists(),
        "a drop-in whose restart failed must not stay, or no later tick retries"
    );
}

#[test]
fn the_converge_puts_the_cap_down() {
    let converge = read(&repo_root().join("infra/gcp/boss-gcp-converge.sh"));
    assert!(
        converge.contains("/gcp/cap-journal.sh"),
        "infra/gcp/boss-gcp-converge.sh must run {SCRIPT} on every tick"
    );
}
