//! The converge hold is an operator's hand on the cluster converge: a
//! file the ops runner writes as root (`hold-converge`), the deploy
//! runner reads as david before it builds or rolls anything, and the
//! root verbs (move-forgejo-data, switch-instance-database) read and
//! place around their own work. infra/forge/converge-hold.sh owns every
//! write to it; infra/forge/forge-defaults.sh owns its path.
//!
//! Three facts about it were one defect (backlog d94d287e, 2026-09-28):
//!
//!   1. It lived in /var/tmp — sticky and world-writable — so any local
//!      account on the forge could create it first (holding every
//!      converge) or plant a symlink the root verbs then `cat` through.
//!      It now lives under /var/lib/boss, a directory install.sh makes
//!      root:root 0755 on every converge (`converge-hold.sh prepare`):
//!      root writes it, david reads it, nobody else can place it.
//!   2. Its path was spelled twice: cluster-deploy-runner.sh respelled
//!      the default although it sources forge-defaults.sh (CLAUDE.md
//!      §9a). It is spelled once now, and this file holds it to that.
//!   3. `converge-hold.sh hold` printed HELD whether or not its write
//!      landed. It now reads the effect back and says NOT HELD, exit 1,
//!      when it did not — and `release` says NOT released the same way.
//!
//! A hold standing at the old path when this lands is carried across
//! ONCE, by `prepare`, as root: a symlink there is never followed, and
//! after the one carry the old path is read by nothing, so a file an
//! account plants there later holds nothing.
//!
//! tree-wide pin — the spelled-once check walks every file under infra/,
//! which no changed-file map attributes to this crate, so every scoped
//! gate runs it whatever its scope (`tree_wide_pins` in infra/gate.sh).

use boss_testing::{create_dir, repo_root, scratch_dir, write_file};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

const VERB: &str = "infra/forge/converge-hold.sh";
const DEFAULTS: &str = "infra/forge/forge-defaults.sh";
const RUNNER: &str = "infra/forge/cluster-deploy-runner.sh";
const INSTALL: &str = "infra/forge/install.sh";
/// The one spelling of the hold's path, in forge-defaults.sh.
const HOLD_DEFAULT: &str = "/var/lib/boss/converge-hold";
/// The path the hold lived at until backlog d94d287e; only the carry in
/// converge-hold.sh may still name it.
// shared-tmp-ok: the retired production path the one-shot carry reads, named so the pin can find it
const LEGACY: &str = "/var/tmp/boss-converge-hold";

struct Run {
    code: i32,
    out: String,
    err: String,
}

/// Run `bash -c <script>` with a clean hold environment plus `env`.
fn bash(script: &str, env: &[(&str, &Path)]) -> Run {
    let mut c = Command::new("bash");
    c.arg("-c")
        .arg(script)
        .env_remove("BOSS_CONVERGE_HOLD")
        .env_remove("BOSS_CONVERGE_HOLD_LEGACY")
        .env_remove("BOSS_SOR_ENV");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().expect("run bash");
    Run {
        code: o.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&o.stdout).into_owned(),
        err: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// `converge-hold.sh <args>` against a hold at `hold` and an old path at
/// `legacy`.
fn verb(args: &str, hold: &Path, legacy: &Path) -> Run {
    let script = format!("bash '{}' {args}", repo_root().join(VERB).display());
    bash(
        &script,
        &[
            ("BOSS_CONVERGE_HOLD", hold),
            ("BOSS_CONVERGE_HOLD_LEGACY", legacy),
        ],
    )
}

/// A directory of our own at mode 0755 — the shape install.sh gives
/// /var/lib/boss on the forge.
fn private_dir(p: &Path) -> PathBuf {
    create_dir(p);
    // mode-bits-ok: a directory's mode, the shape the hold's directory has on the forge; nothing execs it
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_path_buf()
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn the_hold_lives_where_no_other_account_can_place_it() {
    let dir = scratch_dir("converge-hold-default");
    let absent = dir.join("no-sor.env");
    // Sourced with no override and no address file: a verb that never
    // touches the registry still gets its hold path.
    let r = bash(
        &format!(
            ". '{}'; echo \"$HOLD_FILE\"",
            repo_root().join(DEFAULTS).display()
        ),
        &[("BOSS_SOR_ENV", &absent)],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let hold = r.out.trim();
    assert_eq!(hold, HOLD_DEFAULT);
    for shared in ["/tmp/", "/var/tmp/", "/dev/shm/"] {
        assert!(
            !hold.starts_with(shared),
            "the hold is back under {shared}, where any account can create it first or plant a symlink"
        );
    }
}

/// Code lines only: a comment may name a path to say why it is not used.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .map(|(i, l)| (i + 1, l))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            walk(&p, out);
        } else if ft.is_file() {
            out.push(p);
        }
    }
}

#[test]
fn the_hold_path_is_spelled_once_and_the_runner_reads_it() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("infra"), &mut files);
    let mut new_sites = Vec::new();
    let mut legacy_sites = Vec::new();
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f.strip_prefix(&root).unwrap().display().to_string();
        for (n, line) in code_lines(&text) {
            if line.contains(HOLD_DEFAULT) {
                new_sites.push(format!("{rel}:{n}"));
            }
            if line.contains("boss-converge-hold") {
                legacy_sites.push(format!("{rel}:{n}"));
            }
        }
    }
    assert_eq!(
        new_sites.len(),
        1,
        "the hold's path is spelled in {new_sites:?}; it lives once, in {DEFAULTS} (CLAUDE.md §9a)"
    );
    assert!(new_sites[0].starts_with(DEFAULTS), "{new_sites:?}");
    // The old path survives in ONE place: the carry that retires it.
    assert_eq!(
        legacy_sites.len(),
        1,
        "{LEGACY} is named in code at {legacy_sites:?}; only converge-hold.sh's one-shot carry may name it"
    );
    assert!(legacy_sites[0].starts_with(VERB), "{legacy_sites:?}");

    // The runner asks the one definition, not a respelling of it.
    let runner = read(&root.join(RUNNER));
    assert!(
        code_lines(&runner).any(|(_, l)| l.contains("converge_held \"$HOLD_FILE\"")),
        "{RUNNER} does not read the hold through forge-defaults.sh's HOLD_FILE"
    );
    assert!(
        !code_lines(&runner).any(|(_, l)| l.contains("BOSS_CONVERGE_HOLD")),
        "{RUNNER} respells the hold's override instead of reading HOLD_FILE"
    );
    // And the forge's converge, which runs as root, prepares it.
    let install = read(&root.join(INSTALL));
    assert!(
        code_lines(&install).any(|(_, l)| l.contains("converge-hold.sh\" prepare")),
        "{INSTALL} never runs converge-hold.sh prepare, so nothing makes the hold's directory root's or carries a standing hold"
    );
}

#[test]
fn a_hold_is_reported_held_only_once_it_is_written_and_read_back() {
    let dir = scratch_dir("converge-hold-write");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    let legacy = dir.join("legacy");

    let r = verb("hold forgejo-data-move-abc", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(
        r.out
            .contains("converge-hold: HELD — forgejo-data-move-abc"),
        "{}",
        r.out
    );
    assert_eq!(read(&hold), "forgejo-data-move-abc\n");
    // Readable by the converge, which reads it as another account.
    assert_eq!(mode(&hold), 0o644, "the hold must be readable by david");

    // A write that cannot land: its directory does not exist.
    let nowhere = dir.join("absent").join("converge-hold");
    let r = verb("hold never-written", &nowhere, &legacy);
    assert_ne!(r.code, 0, "a failed write exited 0:\n{}", r.out);
    assert!(
        !r.out.contains("HELD —"),
        "a failed write said HELD:\n{}",
        r.out
    );
    assert!(r.err.contains("NOT HELD"), "{}", r.err);
    assert!(r.err.contains(&nowhere.display().to_string()), "{}", r.err);
    assert!(!nowhere.exists());

    // A write that cannot land even as root: the path is a directory.
    let occupied = private_dir(&dir.join("occupied"));
    let r = verb("hold never-written", &occupied, &legacy);
    assert_ne!(r.code, 0, "{}", r.out);
    assert!(!r.out.contains("HELD —"), "{}", r.out);
    assert!(r.err.contains("NOT HELD"), "{}", r.err);

    // A directory any account can write is refused before the write:
    // that is the shape /var/tmp had.
    let shared = dir.join("shared");
    create_dir(&shared);
    // mode-bits-ok: a directory made world-writable like /var/tmp; nothing execs it
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let r = verb("hold in-the-open", &shared.join("converge-hold"), &legacy);
    assert_ne!(r.code, 0, "{}", r.out);
    assert!(r.err.contains("NOT HELD"), "{}", r.err);
    assert!(r.err.contains("writable by any account"), "{}", r.err);
    assert!(!shared.join("converge-hold").exists());
}

#[test]
fn a_release_that_cannot_remove_the_hold_says_so() {
    let dir = scratch_dir("converge-hold-release");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    let legacy = dir.join("legacy");
    write_file(&hold, "learning\n");
    let r = verb("release", &hold, &legacy);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("released (was: learning)"), "{}", r.out);
    assert!(!hold.exists());

    let r = verb("release", &hold, &legacy);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("no hold was standing"), "{}", r.out);

    // Something stands at the path that rm -f cannot remove, even as root.
    create_dir(&hold.join("inside"));
    let r = verb("release", &hold, &legacy);
    assert_ne!(r.code, 0, "{}", r.out);
    assert!(!r.out.contains("released (was"), "{}", r.out);
    assert!(r.err.contains("NOT released"), "{}", r.err);
}

#[test]
fn the_runner_treats_a_hold_it_cannot_read_as_held() {
    let dir = scratch_dir("converge-hold-unreadable");
    // A path that exists but cannot be read as a file. The runner reads
    // the hold as a different account than wrote it; a hold that stands
    // and cannot be read must stop the roll, never pass as "no hold".
    let hold = private_dir(&dir.join("hold-is-a-dir"));
    let lib = repo_root().join("infra/forge/cluster-deploy-lib.sh");
    let r = bash(
        &format!(
            ". '{}'; if converge_held \"$H\"; then echo HELD-BY-RUNNER; else echo NOT-HELD; fi",
            lib.display()
        ),
        &[("H", &hold)],
    );
    assert!(r.out.contains("HELD-BY-RUNNER"), "{}{}", r.out, r.err);
    // The reason names what stands there, not "cannot be read": a
    // directory is not an unreadable hold (review F3 of d94d287e).
    assert!(r.out.contains("directory"), "{}", r.out);
    assert!(!r.out.contains("cannot be read"), "{}", r.out);
    let r = bash(
        &format!(
            ". '{}'; converge_held \"$H\" || echo NOT-HELD",
            lib.display()
        ),
        &[("H", &dir.join("absent"))],
    );
    assert_eq!(r.out.trim(), "NOT-HELD", "{}", r.err);
}

/// The runner reads the hold as david. When david cannot SEARCH the
/// hold's directory, `[ -e ]` answers false exactly as it does for no
/// hold at all — and that used to let the roll go ahead (review F3 of
/// d94d287e). Unsearchable is held. Root searches anything, so under
/// root (the dev pod) the check runs as uid 65534 through setpriv, the
/// account the gate already runs as.
#[test]
fn the_runner_treats_a_directory_it_cannot_search_as_held() {
    let dir = scratch_dir("converge-hold-unsearchable");
    let state = private_dir(&dir.join("state"));
    write_file(&state.join("converge-hold"), "a-real-hold\n");
    // Mode 000, not root's 0700: under the gate (uid 65534) this process
    // OWNS the directory, and 0700 would still let it search; 000 denies
    // search to a non-root owner and to every other account alike.
    // mode-bits-ok: a directory made unsearchable; nothing execs it
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o000)).unwrap();
    let lib = repo_root().join("infra/forge/cluster-deploy-lib.sh");
    let script = format!(
        ". '{}'; if converge_held \"$H\"; then echo HELD-BY-RUNNER; else echo NOT-HELD; fi",
        lib.display()
    );
    let root = std::fs::metadata(&state).unwrap().uid() == 0;
    let mut c = if root {
        let mut c = Command::new("setpriv");
        c.args(["--reuid=65534", "--regid=65534", "--clear-groups", "bash"]);
        c
    } else {
        Command::new("bash")
    };
    let o = c
        .arg("-c")
        .arg(&script)
        .env("H", state.join("converge-hold"))
        .output()
        .expect("run the runner's hold check");
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr);
    // mode-bits-ok: restore the directory so the scratch root can be emptied; nothing execs it
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(out.contains("HELD-BY-RUNNER"), "{out}{err}");
    assert!(out.contains("cannot be searched"), "{out}");
}

#[test]
fn prepare_makes_the_directory_private_and_carries_a_standing_hold_once() {
    let dir = scratch_dir("converge-hold-carry");
    let state = dir.join("state");
    let hold = state.join("converge-hold");
    let legacy = dir.join("legacy-hold");
    // The happy path: a file owned by the uid running prepare (root on
    // the forge, which is who the ops runner wrote it as) with ONE link
    // is read in place and becomes the hold as it stands.
    write_file(&legacy, "forgejo-data-move-0123456789ab\n");

    let r = verb("prepare", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(mode(&state), 0o755, "the hold's directory is not 0755");
    // Owned by whoever prepares it — root on the forge; here, the uid
    // that made the scratch directory around it.
    assert_eq!(
        std::fs::metadata(&state).unwrap().uid(),
        std::fs::metadata(&dir).unwrap().uid()
    );
    assert_eq!(read(&hold), "forgejo-data-move-0123456789ab\n");
    assert_eq!(mode(&hold), 0o644);
    assert!(!legacy.exists(), "the old hold was left where it was");
    assert!(r.out.contains("carried"), "{}", r.out);
    assert!(
        r.out.contains("forgejo-data-move-0123456789ab"),
        "{}",
        r.out
    );

    // ONCE: a file created at the old path afterwards holds nothing.
    std::fs::remove_file(&hold).unwrap();
    write_file(&legacy, "planted-by-anyone\n");
    let r = verb("prepare", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(
        !hold.exists(),
        "a file planted at the old path was carried into a hold"
    );
    assert!(r.out.contains("not carried"), "{}", r.out);

    // And it is idempotent with nothing to carry.
    std::fs::remove_file(&legacy).unwrap();
    let r = verb("prepare", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert!(!hold.exists());
}

#[test]
fn prepare_keeps_a_hold_already_standing_at_the_new_path() {
    let dir = scratch_dir("converge-hold-both");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    let legacy = dir.join("legacy-hold");
    write_file(&hold, "the-new-one\n");
    write_file(&legacy, "the-old-one\n");
    let r = verb("prepare", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(read(&hold), "the-new-one\n");
    assert!(!legacy.exists());
    assert!(r.out.contains("the-old-one"), "{}", r.out);
    assert!(r.out.contains("the-new-one"), "{}", r.out);
}

#[test]
fn prepare_never_follows_a_symlink_at_the_old_path() {
    let dir = scratch_dir("converge-hold-symlink");
    let hold = dir.join("state").join("converge-hold");
    let secret = dir.join("secret");
    write_file(&secret, "NOT-A-HOLD-A-SECRET\n");
    let legacy = dir.join("legacy-hold");
    symlink(&secret, &legacy).unwrap();

    let r = verb("prepare", &hold, &legacy);
    assert_ne!(
        r.code, 0,
        "a planted symlink was accepted quietly:\n{}",
        r.out
    );
    assert!(r.err.contains("symlink"), "{}", r.err);
    assert!(!hold.exists(), "a symlink's target was carried into a hold");
    assert!(
        !r.out.contains("NOT-A-HOLD-A-SECRET") && !r.err.contains("NOT-A-HOLD-A-SECRET"),
        "the symlink's target was read"
    );
    assert_eq!(
        read(&secret),
        "NOT-A-HOLD-A-SECRET\n",
        "the target was touched"
    );
    assert!(
        std::fs::symlink_metadata(&legacy).is_err(),
        "the planted link was left"
    );
}

/// Review F1 of d94d287e, reproduced as uid 0: a rename moves a HARDLINK
/// as-is, so a legacy "hold" planted as a hardlink to a secret was moved,
/// read (300 bytes into the journal and the run summary), chowned and
/// made 0644 — the secret world-readable. Only a file owned by the uid
/// running prepare (root on the forge) with ONE link is read in place;
/// anything else becomes a FRESH hold that says its contents were not
/// read, and the planted link is removed without touching its inode.
#[test]
fn prepare_never_reads_or_rechmods_a_hardlink_planted_at_the_old_path() {
    let dir = scratch_dir("converge-hold-hardlink");
    let hold = dir.join("state").join("converge-hold");
    let secret = dir.join("secret");
    write_file(&secret, "NOT-A-HOLD-A-SECRET\n");
    // mode-bits-ok: a data file's mode, the 0600 a secret has; nothing execs it
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let legacy = dir.join("legacy-hold");
    std::fs::hard_link(&secret, &legacy).unwrap();

    let r = verb("prepare", &hold, &legacy);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    let both = format!("{}{}", r.out, r.err);
    assert!(
        !both.contains("NOT-A-HOLD-A-SECRET"),
        "the linked inode was read:\n{both}"
    );
    assert_eq!(mode(&secret), 0o600, "the linked inode's mode was changed");
    assert_eq!(read(&secret), "NOT-A-HOLD-A-SECRET\n");
    assert_eq!(
        std::fs::metadata(&secret).unwrap().nlink(),
        1,
        "the planted link was left"
    );
    assert!(std::fs::symlink_metadata(&legacy).is_err());
    // Still a hold — a stop someone placed is never dropped — but a
    // fresh one, root's own words, saying what it did not do.
    let h = read(&hold);
    assert!(!h.contains("NOT-A-HOLD-A-SECRET"), "{h}");
    assert!(h.contains("contents not read"), "{h}");
    assert!(h.contains("release-converge lifts it"), "{h}");
    assert_eq!(mode(&hold), 0o644);
    assert_ne!(
        std::fs::metadata(&hold).unwrap().ino(),
        std::fs::metadata(&secret).unwrap().ino(),
        "the hold is the secret's inode"
    );
}

/// Review F2 of d94d287e: a carry that failed after taking the old file
/// left it staged in the hold's directory and exited before the marker;
/// the next tick found the old path empty, wrote the marker, and the
/// staged hold never became the hold. A staged leftover with no marker
/// is finished first.
#[test]
fn prepare_finishes_a_carry_a_failed_tick_left_staged() {
    let dir = scratch_dir("converge-hold-staged");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    let staged = state.join(".converge-hold.carry");
    write_file(&staged, "forgejo-data-move-left-staged\n");
    let r = verb("prepare", &hold, &dir.join("legacy-hold"));
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    assert_eq!(read(&hold), "forgejo-data-move-left-staged\n");
    assert!(!staged.exists(), "the staged file was left");
    assert!(state.join(".converge-hold.carried").exists());
}

/// What one held tick leaves: stderr, the run summary, the alerts raised
/// (one `title|detail` line each, through a stand-in for the converge's
/// alert door — the real one posts to the system of record).
struct HeldTick {
    code: i32,
    err: String,
    summary: serde_json::Value,
    alerts: Vec<String>,
}

/// `converge_hold_report` as the runner calls it — under `set -euo
/// pipefail`, after run-summary.sh and the lib — for a hold at `hold`,
/// the cluster on build `at` (the converge's stamp), forge main at
/// `head`, and an alarm bound of `bound` seconds. `dir` is the converge's
/// state directory here: it keeps the summary, the marker and the alerts.
fn held_tick(dir: &Path, hold: &Path, at: &str, head: &str, bound: &str) -> HeldTick {
    let root = repo_root();
    let summary = dir.join("summary.json");
    let alerts = dir.join("alerts.log");
    let script = format!(
        "set -euo pipefail\n. '{rs}'\n. '{lib}'\n\
         tenant_check_alert() {{ printf '%s|%s\\n' \"$1\" \"$2\" >> \"$ALERTS\"; }}\n\
         run_summary_reset\n\
         converge_hold_report \"$H\" 'forgejo-data-move-abc' '{head}' '{at}'\n\
         echo REPORTED\n",
        rs = root.join("infra/run-summary.sh").display(),
        lib = root.join("infra/forge/cluster-deploy-lib.sh").display(),
    );
    let o = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("H", hold)
        .env("ALERTS", &alerts)
        .env("CONVERGE_STATE", dir)
        .env("BOSS_RUN_SUMMARY_FILE", &summary)
        .env("BOSS_FORGE_HOLD_ALARM_S", bound)
        .env_remove("BOSS_FORGE_HOLD_ALERTED")
        .output()
        .expect("run the held tick");
    let out = String::from_utf8_lossy(&o.stdout);
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(
        out.contains("REPORTED"),
        "the report ended the run it is only visibility for:\n{out}{err}"
    );
    HeldTick {
        code: o.status.code().unwrap_or(-1),
        err,
        summary: serde_json::from_str(&std::fs::read_to_string(&summary).unwrap_or_default())
            .unwrap_or(serde_json::Value::Null),
        alerts: std::fs::read_to_string(&alerts)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect(),
    }
}

fn aged(p: &Path, secs: u64) {
    let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    std::fs::File::options()
        .write(true)
        .open(p)
        .and_then(|f| f.set_modified(at))
        .unwrap_or_else(|e| panic!("set mtime {}: {e}", p.display()));
}

/// A HELD TICK SAYS WHAT THE HOLD KEEPS BACK, AT THE POINT OF REFUSAL
/// (delta review aa901496, N1; backlog 62b09c57): the build and the
/// roll — the cluster stays on the build it is on, named, and main is
/// named beside it, on stderr and on its packet, with how long the hold
/// has stood. Since backlog 683d7fe9 the converge runs from root's tree,
/// which follows main hold or no hold, so there is no checkout a hold
/// keeps back and the tick no longer names one.
#[test]
fn a_held_tick_says_what_it_keeps_back_and_how_long_the_hold_has_stood() {
    let dir = scratch_dir("converge-hold-report");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    write_file(&hold, "forgejo-data-move-abc\n");
    let head = "89abcdef0123456789abcdef0123456789abcdef";

    let t = held_tick(&dir, &hold, "abc1234", head, "21600");
    assert_eq!(t.code, 0, "{}", t.err);
    assert!(
        t.err
            .contains("converge HELD — forgejo-data-move-abc — main at 89abcde not built or rolled (release-converge lifts it)"),
        "{}",
        t.err
    );
    assert!(
        t.err
            .contains("the cluster stays on build abc1234 while the hold stands"),
        "{}",
        t.err
    );
    assert!(
        t.err
            .contains(&format!("root's tree is at main ({})", &head[..12])),
        "{}",
        t.err
    );
    assert_eq!(t.summary["converge_held"], "forgejo-data-move-abc");
    assert_eq!(t.summary["cluster_at"], "abc1234");
    assert_eq!(t.summary["main_at"], head);
    assert!(
        t.summary.get("checkout_at").is_none(),
        "there is no checkout a hold keeps back: {}",
        t.summary
    );
    let age: u64 = t.summary["held_for_s"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("held_for_s is not seconds: {}", t.summary));
    assert!(age < 600, "a hold just written read as {age} s old");
    assert!(
        t.summary["held_since"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "{}",
        t.summary
    );
    assert!(t.alerts.is_empty(), "a fresh hold alarmed: {:?}", t.alerts);
    assert!(
        t.summary["hold_alarm"]
            .as_str()
            .unwrap_or("")
            .starts_with("not due"),
        "{}",
        t.summary
    );

    // A stamp that is not a build name (none before a first converge)
    // and a hold that cannot be aged are each said by name; neither is
    // a number, and neither alarms.
    let t = held_tick(&dir, &state.join("absent"), "garbage", head, "0");
    assert_eq!(t.code, 0, "{}", t.err);
    assert_eq!(t.summary["held_for_s"], "unknown");
    assert_eq!(t.summary["cluster_at"], "none");
    assert!(t.alerts.is_empty(), "{:?}", t.alerts);
}

/// A HOLD THAT OUTLIVES ITS PURPOSE REACHES SOMEONE, ONCE (delta review
/// aa901496, the ALARM note; backlog 62b09c57). A held tick exits 0 —
/// a hold is a decision, not a failure — so a hold nobody released was a
/// line in a journal every ten minutes and nothing else. Past the bound
/// the tick raises ONE alert through the converge's own door, naming the
/// reason, the age and both commits; the same hold never raises a
/// second, and a hold placed again is a new hold.
#[test]
fn a_hold_past_its_bound_is_alarmed_once_per_hold() {
    let dir = scratch_dir("converge-hold-alarm");
    let state = private_dir(&dir.join("state"));
    let hold = state.join("converge-hold");
    write_file(&hold, "forgejo-data-move-abc\n");
    aged(&hold, 7 * 3600 + 120);
    let at = "abc1234".to_string();
    let head = "89abcdef0123456789abcdef0123456789abcdef";

    let t = held_tick(&dir, &hold, &at, head, "21600");
    assert_eq!(t.code, 0, "{}", t.err);
    assert_eq!(t.alerts.len(), 1, "{:?}", t.alerts);
    let alert = &t.alerts[0];
    assert!(alert.contains("HELD for 7 h"), "{alert}");
    for fact in [
        "forgejo-data-move-abc",
        head,
        at.as_str(),
        "release-converge",
    ] {
        assert!(
            alert.contains(fact),
            "the alert does not carry {fact}: {alert}"
        );
    }
    assert!(
        t.summary["hold_alarm"]
            .as_str()
            .unwrap_or("")
            .starts_with("raised"),
        "{}",
        t.summary
    );

    // The next tick of the same hold: on the packet, not a second alert.
    let t = held_tick(&dir, &hold, &at, head, "21600");
    assert_eq!(
        t.alerts.len(),
        1,
        "the same hold alarmed twice: {:?}",
        t.alerts
    );
    assert!(
        t.summary["hold_alarm"]
            .as_str()
            .unwrap_or("")
            .starts_with("already raised"),
        "{}",
        t.summary
    );

    // Released and placed again, and again left standing: a new hold.
    std::fs::remove_file(&hold).unwrap();
    write_file(&hold, "forgejo-data-move-abc\n");
    aged(&hold, 9 * 3600);
    let t = held_tick(&dir, &hold, &at, head, "21600");
    assert_eq!(t.alerts.len(), 2, "{:?}", t.alerts);

    // A bound that is not a number alarms nothing and says so.
    let t = held_tick(&dir, &hold, &at, head, "soon");
    assert_eq!(t.alerts.len(), 2, "{:?}", t.alerts);
    assert!(
        t.summary["hold_alarm"]
            .as_str()
            .unwrap_or("")
            .contains("not a number of seconds"),
        "{}",
        t.summary
    );
}

/// The runner's held branch is that report, and the runbook's rollback
/// holds LAST (delta review aa901496, N1): its step that needs the
/// checkout at the revert comes before the step that holds the converge
/// which would have put it there, and the read it gives answers as the
/// checkout's owner — a root login shell gets git's `dubious ownership`.
#[test]
fn the_runner_reports_a_held_tick_and_the_rollback_runbook_holds_last() {
    let root = repo_root();
    let runner = read(&root.join(RUNNER));
    assert!(
        code_lines(&runner).any(|(_, l)| l
            .contains("converge_hold_report \"$HOLD_FILE\" \"$reason\" \"$HEAD_FULL\" \"$LAST\"")),
        "{RUNNER} does not report a held tick through converge_hold_report"
    );
    let runbook = read(&root.join("infra/forge/OPERATIONS.md"));
    let section = runbook
        .split("### 9. Root's tree does not move")
        .nth(1)
        .and_then(|s| s.split("\n### ").next())
        .expect("OPERATIONS.md has failure mode 9");
    let read_at = section
        .find("sudo -u david git -C /home/david/boss rev-parse HEAD")
        .expect("failure mode 9 gives the checkout read as the checkout's owner");
    let hold_at = section
        .find("boss ops forge hold-converge <reason>")
        .expect("failure mode 9 names the hold verb");
    assert!(
        read_at < hold_at,
        "failure mode 9 holds the cluster converge before the checkout is read at the revert — the held converge never checks it out"
    );
    assert!(
        !section.contains(
            "with the\n   checkout AT THE REVERT — `git -C /home/david/boss rev-parse HEAD`"
        ),
        "failure mode 9 still gives the read that fails in a root login shell"
    );
}

#[test]
fn prepare_refuses_a_directory_it_cannot_own() {
    let dir = scratch_dir("converge-hold-dir-link");
    let elsewhere = private_dir(&dir.join("elsewhere"));
    let linked = dir.join("state");
    symlink(&elsewhere, &linked).unwrap();
    let r = verb(
        "prepare",
        &linked.join("converge-hold"),
        &dir.join("legacy"),
    );
    assert_ne!(r.code, 0, "{}", r.out);
    assert!(r.err.contains("symlink"), "{}", r.err);
}
