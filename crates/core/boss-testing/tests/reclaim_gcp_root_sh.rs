//! `infra/gcp/reclaim-gcp-root.sh` is RUN, not read — against a scratch
//! `/opt`, a scratch process table, mount table and unit directory, a
//! stubbed `systemctl` and a stubbed `journalctl`, so every verdict
//! below is one the script actually reached and every removal is one it
//! actually made.
//!
//! WHY THE VERB EXISTS (backlog d3c7eada car 2, 2026-09-26). boss-gcp's
//! 48 GB root sat at 11-13 GB free for nine days against its 17 GB
//! floor. Car 1's disk-report reading found ~2.3 GB of hand-made July
//! 2026 binary backups under /opt (`boss-binbak-*`, `boss-dev-bak`) and
//! a 4.1 GB journal, beside DATA that is David's call — the cluster-pg
//! dumps and the second-stack capture under /var/backups, the home
//! directories, /usr/local, and the live /opt/boss and /opt/boss-cli. So
//! the reclaim is a MUTATING approval verb whose paths are the script's
//! own: `--dry-run` renders a plan, David's passkey signs its hash, and
//! the write removes only what that signed plan names.
//!
//! What each case pins: the dry run renders a deterministic plan that
//! names its own hash and removes nothing; the signed hash removes
//! exactly the backups and leaves every neighbour intact; a plan that
//! moved, and a second run of an applied one, remove nothing; a checkout
//! is KEPT with its unpushed state in the plan while the backups beside
//! it still plan (2026-09-27 — it used to refuse the whole run); a path
//! handed to the script, a symlinked candidate, a backup
//! COPIED today with its old mtimes (the adversarial review's H1), and a
//! backup that a live tree, a mount, a symlink, a running process, a
//! loaded unit or a unit file on disk resolves into are each refused
//! with nothing removed; a unit bound that cannot be evaluated is a
//! refusal; and, through `ops-runner.sh` with the shipped verb files,
//! the plan verb is answered on boss-gcp while a path, or a hash with no
//! approval, never reaches the write.
//!
//! THE ONE EXCEPTION UNDER /var/backups (backlog f44ca628, 2026-09-28).
//! David: "Delete the second-stack capture, build it as a verb." The
//! capture retire-second-stack took on 2026-09-15 (ops-request 7912c9ae)
//! is 2.3 GB, and without it boss-gcp stays ~0.6 GB under its floor after
//! every other reclaim. So the plan also names each capture in the
//! scratch `var/backups/boss/second-stack` with its size and sha256 — the
//! signature binds those bytes — and the write removes it LAST, re-hashed
//! at the moment before `rm -f`. Pinned here: a symlinked capture or
//! capture directory, a name retire-second-stack does not write (a
//! 14-digit stamp, `second-stack-x.sql`), a directory by a capture's
//! name, a capture a process holds open, and a capture whose bytes
//! changed are each refused or left; a sibling cluster-pg dump, a
//! compressed copy and a capture-named file in ANOTHER directory survive
//! every run.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn has(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .is_ok_and(|s| s.success())
}

const SCRIPT: &str = "infra/gcp/reclaim-gcp-root.sh";

/// The four backups car 1 read on boss-gcp, by their real names.
const BACKUPS: [&str; 4] = [
    "boss-binbak-pre-pr73-0702-1801",
    "boss-binbak-pre-latest-194355",
    "boss-binbak-pre-pr5-20260701-032322",
    "boss-dev-bak",
];

/// Neighbours that must survive every run: the live trees, and names
/// that only resemble a backup.
const KEPT: [&str; 4] = ["boss", "boss-cli", "boss-dev-bak2", "boss-binbak"];

/// The capture's real name, as retire-second-stack wrote it:
/// `second-stack-$(date -u +%Y%m%dT%H%M%SZ).sql`.
const CAPTURE: &str = "second-stack-20260915T211123Z.sql";
const CAPTURE_BYTES: &str = "-- PostgreSQL database dump of the retired second stack\n";

/// Beside the capture, under /var/backups, and never this verb's: the
/// cluster-pg dumps (not decided), and a compressed copy of a capture —
/// a name retire-second-stack never writes.
const NOT_OURS: [&str; 2] = [
    "boss-cluster-pg/boss-cluster-pg-20260920T030000Z.sql.gz",
    "second-stack/second-stack-20260915T211123Z.sql.gz",
];

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The clock every case runs at unless it says otherwise: sixty days on,
/// so a fixture written a moment ago is old by ctime — which a test
/// cannot set any other way.
fn sixty_days_on() -> String {
    (epoch_now() + 60 * 86400).to_string()
}

/// `write_file`, creating the parents first.
fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).unwrap();
    write_file(path, body);
}

/// Age every file and directory under `p` by 60 days BY MTIME, the way
/// the July backups are on the host — and the way `cp -a` carries them.
fn age(p: &Path) {
    let ok = Command::new("find")
        .arg(p)
        .args(["-exec", "touch", "-h", "-d", "60 days ago", "{}", "+"])
        .status()
        .expect("find runs")
        .success();
    assert!(ok, "could not age {}", p.display());
}

fn sha256(bytes: &str) -> String {
    let dir = scratch_dir("reclaim-gcp-root-sha");
    let f = dir.join("bytes");
    write_file(&f, bytes);
    let out = Command::new("sha256sum")
        .arg(&f)
        .output()
        .expect("sha256sum");
    String::from_utf8_lossy(&out.stdout)[..64].to_string()
}

struct Case {
    root: PathBuf,
    bin: PathBuf,
    opt: PathBuf,
    proc_dir: PathBuf,
    links: PathBuf,
    units: PathBuf,
    mountinfo: PathBuf,
    calls: PathBuf,
    /// The scratch /var/backups/boss, canonical: the script refuses a
    /// capture directory whose realpath is not its own spelling.
    backups: PathBuf,
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

impl Run {
    fn text(&self) -> String {
        format!("{}{}", self.out, self.err)
    }

    /// The hash the dry run named on stderr.
    fn plan_sha(&self) -> String {
        self.err
            .lines()
            .find_map(|l| l.strip_prefix("plan-sha256: "))
            .unwrap_or_else(|| panic!("no plan-sha256 line:\n{}", self.text()))
            .to_string()
    }
}

impl Case {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("reclaim-gcp-root-{name}"));
        let bin = root.join("bin");
        let opt = root.join("opt");
        let proc_dir = root.join("proc");
        let links = root.join("links");
        let units = root.join("units");
        for d in [&bin, &opt, &proc_dir, &links, &units] {
            std::fs::create_dir_all(d).unwrap();
        }
        for b in BACKUPS.iter().chain(KEPT.iter()) {
            put(&opt.join(b).join("bin/boss-jobs-api"), "binary\n");
            put(&opt.join(b).join("VERSION"), "july\n");
        }
        age(&opt);
        // One running process, whose binary is the live tree's.
        std::fs::create_dir_all(proc_dir.join("101")).unwrap();
        std::os::unix::fs::symlink(opt.join("boss/bin/boss-jobs-api"), proc_dir.join("101/exe"))
            .unwrap();
        write_file(&proc_dir.join("101/comm"), "boss-jobs-api\n");
        // Its open files: one, nothing under /var/backups.
        std::fs::create_dir_all(proc_dir.join("101/fd")).unwrap();
        std::os::unix::fs::symlink("/dev/null", proc_dir.join("101/fd/0")).unwrap();
        // Init, whose fd table the capture bound requires it read: a
        // process table without pid 1 is not a whole host's.
        std::fs::create_dir_all(proc_dir.join("1/fd")).unwrap();
        std::os::unix::fs::symlink("/dev/null", proc_dir.join("1/fd/0")).unwrap();
        write_file(&proc_dir.join("1/comm"), "systemd\n");
        // /var/backups/boss: the capture, and what is never this verb's.
        // Modes set outright, not left to the umask: the bound refuses a
        // group- or other-writable capture directory or parent.
        let backups = root.join("var/backups/boss");
        std::fs::create_dir_all(backups.join("second-stack")).unwrap();
        let backups = std::fs::canonicalize(&backups).unwrap();
        for d in [backups.clone(), backups.join("second-stack")] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap(); // mode-bits-ok: a fixture directory the capture bound reads the mode of, not a script
        }
        put(&backups.join("second-stack").join(CAPTURE), CAPTURE_BYTES);
        for n in NOT_OURS {
            put(&backups.join(n), "not this verb's\n");
        }
        // A unit file on disk that runs from the live tree.
        write_file(
            &units.join("boss-jobs-api.service"),
            &format!(
                "[Service]\nExecStart={}\n",
                opt.join("boss/bin/boss-jobs-api").display()
            ),
        );
        // The mount table: root, and one mount elsewhere.
        let mountinfo = root.join("mountinfo");
        write_file(
            &mountinfo,
            "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
             40 22 8:17 / /var/lib/boss-build rw,relatime shared:2 - ext4 /dev/sdc rw\n",
        );
        let calls = root.join("calls.log");
        write_exec(
            &bin.join("systemctl"),
            r#"#!/bin/sh
# stub systemctl: two loaded services; show prints STUB_EXEC for the
# first and a /usr path for the second.
echo "systemctl $*" >> "$STUB_CALLS"
case "$1" in
  list-units)
    [ -n "${STUB_LIST_FAIL:-}" ] && { echo "Failed to connect to bus (stub)" >&2; exit 1; }
    echo "boss-ops-runner.service loaded active running ops"
    echo "boss-ml-api.service loaded active running ml"
    ;;
  show)
    [ -n "${STUB_SHOW_FAIL:-}" ] && { echo "Failed to get properties (stub)" >&2; exit 1; }
    for a in "$@"; do u="$a"; done
    case "$u" in
      boss-ops-runner.service) echo "{ path=${STUB_EXEC:-/usr/bin/true} ; argv[]=${STUB_EXEC:-/usr/bin/true} * ; }" ;;
      *) echo "{ path=/usr/bin/env ; argv[]=/usr/bin/env ; }"; echo "/etc/systemd/system/$u" ;;
    esac
    ;;
  *) echo "stub systemctl: unexpected $*" >&2; exit 99 ;;
esac
"#,
        );
        write_exec(
            &bin.join("journalctl"),
            "#!/bin/sh\n# stub journalctl: records its argv. STUB_PLANT_GIT plants a .git\n\
             # there on --disk-usage, which the write calls AFTER it re-renders the plan;\n\
             # STUB_REWRITE rewrites that file's bytes at the same moment, STUB_HARDLINK\n\
             # (\"<from> <to>\") hardlinks, STUB_FD (\"<fd path> <target>\") plants an open\n\
             # fd, and STUB_SWAP_DIR moves a directory aside and links it back.\n\
             echo \"journalctl $*\" >> \"$STUB_CALLS\"\n\
             case \"$1\" in --disk-usage)\n\
               if [ -n \"${STUB_PLANT_GIT:-}\" ]; then mkdir -p \"$STUB_PLANT_GIT/.git\"; fi\n\
               if [ -n \"${STUB_REWRITE:-}\" ]; then printf 'late bytes\\n' > \"$STUB_REWRITE\"; fi\n\
               if [ -n \"${STUB_HARDLINK:-}\" ]; then set -- $STUB_HARDLINK; ln \"$1\" \"$2\"; fi\n\
               if [ -n \"${STUB_FD:-}\" ]; then set -- $STUB_FD; mkdir -p \"${1%/*}\"; ln -s \"$2\" \"$1\"; fi\n\
               if [ -n \"${STUB_SWAP_DIR:-}\" ]; then mv \"$STUB_SWAP_DIR\" \"$STUB_SWAP_DIR.moved\"; ln -s \"$STUB_SWAP_DIR.moved\" \"$STUB_SWAP_DIR\"; fi\n\
               echo 'Archived and active journals take up 4.1G in the file system.';; esac\n",
        );
        Self {
            root,
            bin,
            opt,
            proc_dir,
            links,
            units,
            mountinfo,
            calls,
            backups,
        }
    }

    fn capture_dir(&self) -> PathBuf {
        self.backups.join("second-stack")
    }

    /// The uid that owns the fixture's /var/backups/boss.
    fn owner(&self) -> String {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&self.backups).unwrap().uid().to_string()
    }

    fn capture(&self) -> PathBuf {
        self.capture_dir().join(CAPTURE)
    }

    /// The plan's line for a capture holding `bytes`.
    fn capture_line(&self, path: &Path, bytes: &str) -> String {
        format!(
            "would remove {} ({} bytes, sha256 {})",
            path.display(),
            bytes.len(),
            sha256(bytes)
        )
    }

    /// Nothing under the scratch /var/backups that is not this verb's was
    /// touched.
    fn assert_not_ours_intact(&self, text: &str) {
        for n in NOT_OURS {
            assert!(
                self.backups.join(n).is_file(),
                "{n} under /var/backups was touched:\n{text}"
            );
        }
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_CALLS", &self.calls)
            .env("BOSS_RECLAIM_OPT_DIR", &self.opt)
            .env("BOSS_RECLAIM_PROC_DIR", &self.proc_dir)
            .env("BOSS_RECLAIM_MOUNTINFO", &self.mountinfo)
            .env("BOSS_RECLAIM_UNIT_DIRS", &self.units)
            .env("BOSS_RECLAIM_NOW", sixty_days_on())
            .env("BOSS_RECLAIM_CAPTURE_DIR", self.capture_dir())
            // The fixture's owner stands in for root: no test account can
            // chown to uid 0 (the gate runs as 65534, the pod has no CAP_CHOWN).
            .env("BOSS_RECLAIM_CAPTURE_OWNER", self.owner())
            .env(
                "BOSS_RECLAIM_LINK_DIRS",
                format!("{} {}", self.opt.display(), self.links.display()),
            );
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, String)]) -> Run {
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join(SCRIPT)).args(args);
        self.env(&mut cmd);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("reclaim-gcp-root.sh runs");
        Run {
            code: out.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.calls).unwrap_or_default()
    }

    /// Every name in the scratch /opt still present.
    fn present(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&self.opt)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn assert_nothing_removed(&self, text: &str) {
        for b in BACKUPS.iter().chain(KEPT.iter()) {
            assert!(
                self.opt.join(b).join("bin/boss-jobs-api").is_file(),
                "{b} was touched:\n{text}"
            );
        }
        assert!(
            self.capture().is_file() || self.capture().is_symlink(),
            "the capture was removed:\n{text}"
        );
        self.assert_not_ours_intact(text);
        assert!(
            !self.calls().contains("--vacuum-size"),
            "the journal was vacuumed:\n{text}"
        );
    }

    /// A refusal: exit 2, the needles in what it said, nothing removed.
    fn refused(&self, args: &[&str], extra: &[(&str, String)], needles: &[&str]) {
        let r = self.run_env(args, extra);
        let text = r.text();
        assert_eq!(r.code, 2, "{args:?} was not refused:\n{text}");
        contains_all(&text, needles, "the refusal");
        assert!(
            !r.err.contains("plan-sha256:"),
            "a refusal rendered a plan:\n{text}"
        );
        self.assert_nothing_removed(&text);
    }
}

fn contains_all(text: &str, needles: &[&str], what: &str) {
    for n in needles {
        assert!(text.contains(n), "{what}: expected `{n}` in:\n{text}");
    }
}

// ---------------------------------------------------------------------------
// The plan, and the signed write.
// ---------------------------------------------------------------------------

#[test]
fn dry_run_renders_a_plan_naming_its_own_hash_and_removes_nothing() {
    let c = Case::new("dry-run");
    let r = c.run(&["--dry-run"]);
    let text = r.text();
    assert_eq!(r.code, 0, "the dry run did not pass:\n{text}");
    c.assert_nothing_removed(&text);
    for b in BACKUPS {
        contains_all(
            &r.out,
            &[&format!("would remove {} (", c.opt.join(b).display())],
            "the plan",
        );
    }
    // Each candidate's top level is in the plan, for the approver to read.
    contains_all(
        &r.out,
        &[
            "), holding:\n  VERSION\n  bin\n",
            "would vacuum the journal to 1G",
            &c.capture_line(&c.capture(), CAPTURE_BYTES),
        ],
        "the plan",
    );
    // Nothing else under /var/backups is named, not even a capture's
    // compressed copy.
    for n in NOT_OURS {
        assert!(!r.out.contains(n), "the plan names {n}:\n{text}");
    }
    contains_all(
        &r.err,
        &["1 second-stack capture(s) (56 bytes)"],
        "the dry run's verdict",
    );
    for k in KEPT {
        assert!(
            !r.out
                .contains(&format!("would remove {} (", c.opt.join(k).display())),
            "the plan names the kept {k}:\n{text}"
        );
    }
    // Free space and journal usage move on their own: stderr, not the plan.
    contains_all(
        &r.err,
        &[
            "root free before:",
            "journal: Archived and active journals take up 4.1G",
            "DRY RUN",
        ],
        "the dry run's stderr",
    );
    assert!(!r.out.contains("root free") && !r.out.contains("4.1G"));
    assert_eq!(
        r.plan_sha(),
        sha256(&r.out),
        "the named hash is not the plan's"
    );
    // Every bound ran, the unit bound included.
    assert!(
        c.calls().contains("systemctl list-units"),
        "the unit bound did not run in the dry run:\n{text}"
    );
}

#[test]
fn two_renders_of_one_state_are_byte_identical() {
    let c = Case::new("deterministic");
    let a = c.run(&["--dry-run"]);
    let b = c.run(&["--dry-run"]);
    assert_eq!(a.code, 0, "{}", a.text());
    assert_eq!(a.out, b.out, "two renders differ");
}

#[test]
fn the_signed_plan_removes_exactly_the_backups_and_vacuums_to_1g() {
    let c = Case::new("for-real");
    let plan = c.run(&["--dry-run"]);
    let sha = plan.plan_sha();
    let r = c.run(&[&sha]);
    let text = r.text();
    assert_eq!(r.code, 0, "the signed run failed:\n{text}");
    assert!(
        r.out.starts_with(&plan.out),
        "the write did not print the approved plan first:\n{text}"
    );
    let mut kept: Vec<String> = KEPT.iter().map(|s| s.to_string()).collect();
    kept.sort();
    assert_eq!(c.present(), kept, "the survivors are wrong:\n{text}");
    for b in BACKUPS {
        contains_all(
            &text,
            &[&format!("removed {}", c.opt.join(b).display())],
            "the record",
        );
    }
    assert!(
        c.calls().contains("journalctl --vacuum-size=1G"),
        "the journal was not vacuumed to 1G:\n{}",
        c.calls()
    );
    contains_all(
        &text,
        &[
            "OK — removed 4 backup directories",
            "and 1 second-stack capture(s) (56 bytes)",
            &format!("removed {} (56 bytes, sha256 ", c.capture().display()),
        ],
        "the verdict",
    );
    // The capture went, LAST (after every backup directory), and nothing
    // else under /var/backups did.
    assert!(
        !c.capture().exists(),
        "the capture was not removed:\n{text}"
    );
    assert!(c.capture_dir().is_dir(), "the capture's directory went");
    c.assert_not_ours_intact(&text);
    let last_dir = text
        .rfind(&format!("removed {}", c.opt.display()))
        .expect("a removed directory");
    let capture_at = text
        .find(&format!("removed {}", c.capture().display()))
        .expect("the removed capture");
    assert!(
        last_dir < capture_at,
        "the capture was not removed last:\n{text}"
    );

    // At most once: the applied plan's backups are gone, so it no longer
    // hashes to the signature and a second run removes nothing.
    let again = c.run(&[&sha]);
    assert_eq!(
        again.code,
        2,
        "a second run was not refused:\n{}",
        again.text()
    );
    contains_all(&again.text(), &["not the approved"], "the second run");
}

#[test]
fn a_plan_that_moved_since_it_was_signed_removes_nothing() {
    let c = Case::new("moved");
    let sha = c.run(&["--dry-run"]).plan_sha();
    put(&c.opt.join("boss-dev-bak/late-arrival"), "new\n");
    let r = c.run(&[&sha]);
    let text = r.text();
    assert_eq!(r.code, 2, "a moved plan was not refused:\n{text}");
    contains_all(&text, &["not the approved", "late-arrival"], "the refusal");
    for b in BACKUPS {
        assert!(c.opt.join(b).is_dir(), "{b} was removed:\n{text}");
    }
    assert!(!c.calls().contains("--vacuum-size"));
}

// ---------------------------------------------------------------------------
// The bounds, each refusing with nothing removed.
// ---------------------------------------------------------------------------

#[test]
fn refuses_a_path_or_any_word_but_dry_run_or_a_hash() {
    let c = Case::new("mode");
    for args in [
        vec![],
        vec!["--for-real"],
        vec!["--for-real", "/var/backups/boss"],
        vec!["/var/backups/boss-cluster-pg"],
        vec!["/home/dauld"],
        vec!["--now"],
    ] {
        c.refused(&args, &[], &["--dry-run | <plan-sha256>"]);
    }
}

#[test]
fn refuses_a_symlinked_candidate_and_leaves_its_target() {
    let c = Case::new("symlink");
    let outside = c.root.join("var-backups-boss");
    put(&outside.join("second-stack.sql"), "data\n");
    std::os::unix::fs::symlink(&outside, c.opt.join("boss-binbak-evil")).unwrap();
    c.refused(&["--dry-run"], &[], &["boss-binbak-evil", "is a symlink"]);
    assert!(
        outside.join("second-stack.sql").is_file(),
        "the link's target was touched"
    );
}

/// The adversarial review's H1, measured: `cp -a` preserves mtimes, so a
/// rollback backup made TODAY from an old tree read as July to an mtime
/// bound and passed it. The bound is ctime, which a copy cannot carry.
#[test]
fn refuses_a_backup_copied_today_with_its_old_mtimes() {
    let c = Case::new("cp-a");
    for b in BACKUPS {
        std::fs::remove_dir_all(c.opt.join(b)).unwrap();
    }
    let ok = Command::new("cp")
        .arg("-a")
        .arg(c.opt.join("boss"))
        .arg(c.opt.join("boss-binbak-rollback-today"))
        .status()
        .expect("cp runs")
        .success();
    assert!(ok, "cp -a failed");
    // The copy carries July's mtimes...
    let mtime = std::fs::metadata(c.opt.join("boss-binbak-rollback-today/VERSION"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        SystemTime::now().duration_since(mtime).unwrap().as_secs() > 50 * 86400,
        "the fixture did not copy an old mtime"
    );
    // ...and at today's clock it is refused, by ctime.
    let r = c.run_env(
        &["--dry-run"],
        &[("BOSS_RECLAIM_NOW", epoch_now().to_string())],
    );
    let text = r.text();
    assert_eq!(
        r.code, 2,
        "a backup copied today passed the age bound:\n{text}"
    );
    contains_all(
        &text,
        &[
            "boss-binbak-rollback-today",
            "changed (ctime) in the last 30 days",
        ],
        "the refusal",
    );
    assert!(c.opt.join("boss-binbak-rollback-today/VERSION").is_file());
    // The control: sixty days on, the same copy is old, and planned.
    let later = c.run(&["--dry-run"]);
    assert_eq!(later.code, 0, "{}", later.text());
    assert!(later.out.contains("boss-binbak-rollback-today"));
}

// ---------------------------------------------------------------------------
// A checkout is KEPT, with its unpushed state as evidence (backlog d3c7eada,
// 2026-09-27). The first plan on boss-gcp (ops-request 8d334ca2) refused the
// WHOLE run because /opt/boss-dev-bak holds a .git, so the ~1.7 GB of
// boss-binbak-* never freed either. A checkout is now skipped with its
// reason and the rest of the plan stands; what the checkout carries that no
// remote has rides in the plan's bytes, so the passkey signs it.
// ---------------------------------------------------------------------------

/// Run git in `dir` with no user or system config, so a test box's own
/// git settings cannot shape the fixture.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Make /opt/boss-dev-bak a checkout carrying, measured against its own
/// remote-tracking refs: `main` two commits past `origin/main`, `feature`
/// one commit on no remote, `pushed` wholly on the remote, one modified
/// tracked file, two untracked files and one stash. `clean` leaves only
/// `main` and `pushed`, both at `origin/main`, with nothing else.
fn make_checkout(c: &Case, clean: bool) -> PathBuf {
    let r = c.opt.join("boss-dev-bak");
    git(&r, &["init", "-q"]);
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "july"]);
    let base = git(&r, &["rev-parse", "HEAD"]);
    git(&r, &["update-ref", "refs/remotes/origin/main", &base]);
    git(&r, &["branch", "pushed"]);
    if !clean {
        for n in ["one", "two"] {
            put(&r.join(format!("notes-{n}")), &format!("{n}\n"));
            git(&r, &["add", "-A"]);
            git(&r, &["commit", "-q", "-m", n]);
        }
        git(&r, &["checkout", "-q", "-b", "feature", &base]);
        put(&r.join("feature.txt"), "wip\n");
        git(&r, &["add", "-A"]);
        git(&r, &["commit", "-q", "-m", "feature"]);
        git(&r, &["checkout", "-q", "main"]);
        put(&r.join("VERSION"), "stashed\n");
        git(&r, &["stash", "push", "-q"]);
        put(&r.join("notes-one"), "edited\n");
        put(&r.join("notes-two"), "staged\n");
        git(&r, &["add", "notes-two"]);
        put(&r.join("scratch-a"), "a\n");
        put(&r.join("scratch-b"), "b\n");
    }
    age(&c.opt);
    r
}

fn ctime_of(p: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).unwrap();
    m.ctime() * 1_000_000_000 + m.ctime_nsec()
}

#[test]
fn a_checkout_is_kept_with_its_reason_and_the_backups_still_plan() {
    let c = Case::new("git-kept");
    let r = make_checkout(&c, false);
    let index = r.join(".git/index");
    let before = ctime_of(&index);
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "a checkout refused the whole run:\n{text}");
    // Reading the checkout wrote nothing into it: git status refreshes a
    // stale index unless optional locks are off, and that write would be
    // a plan verb mutating the host.
    assert_eq!(
        before,
        ctime_of(&index),
        "the dry run rewrote the checkout's index"
    );
    for b in &BACKUPS[..3] {
        contains_all(
            &plan.out,
            &[&format!("would remove {} (", c.opt.join(b).display())],
            "the plan",
        );
    }
    assert!(
        !plan
            .out
            .contains(&format!("would remove {} (", r.display())),
        "the plan removes the checkout:\n{text}"
    );
    contains_all(
        &plan.out,
        &[
            &format!("would keep {} — it holds a git checkout", r.display()),
            "this plan removes no checkout",
            &format!("  checkout {}:\n", r.display()),
            "    branches with commits on no remote: 2\n",
            "      feature: 1 commit(s)\n",
            "      main: 2 commit(s)\n",
            "    commits at HEAD on no branch and no remote: 0\n",
            "    staged changes not committed: 1\n",
            "    tracked files whose bytes differ from the index (read without filters): 1\n",
            "    untracked files (ignored ones not counted): 2\n",
            "    stashes: 1\n",
            "    not measured: tags, reflogs, ignored files and submodules; a branch squash-merged upstream still counts as unpushed. This plan removes no checkout.\n",
        ],
        "the plan's evidence",
    );
    assert!(
        !plan.out.contains("      pushed:"),
        "a branch wholly on the remote was reported as unpushed:\n{text}"
    );
    contains_all(
        &plan.err,
        &["DRY RUN", "3 backup directories", "1 checkout kept"],
        "the dry run's verdict",
    );
    // The evidence is part of the signed bytes, so it must be stable.
    assert_eq!(
        plan.out,
        c.run(&["--dry-run"]).out,
        "two renders of one checkout differ"
    );

    // The signed plan removes the three backups and leaves the checkout.
    let r2 = c.run(&[&plan.plan_sha()]);
    let t2 = r2.text();
    assert_eq!(r2.code, 0, "the signed run failed:\n{t2}");
    assert!(
        r.join(".git/HEAD").is_file() && r.join("scratch-a").is_file(),
        "the checkout was touched:\n{t2}"
    );
    for b in &BACKUPS[..3] {
        assert!(!c.opt.join(b).exists(), "{b} was not removed:\n{t2}");
    }
    contains_all(
        &t2,
        &["OK — removed 3 backup directories", "1 checkout kept"],
        "the verdict",
    );
}

#[test]
fn a_checkout_with_nothing_unpushed_is_still_kept_and_says_so() {
    let c = Case::new("git-clean");
    let r = make_checkout(&c, true);
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    contains_all(
        &plan.out,
        &[
            &format!("would keep {}", r.display()),
            "    branches with commits on no remote: 0\n",
            "    commits at HEAD on no branch and no remote: 0\n",
            "    staged changes not committed: 0\n",
            "    tracked files whose bytes differ from the index (read without filters): 0\n",
            "    untracked files (ignored ones not counted): 0\n",
            "    stashes: 0\n",
        ],
        "a clean checkout's evidence",
    );
    assert!(
        !plan
            .out
            .contains(&format!("would remove {} (", r.display()))
    );
}

/// The first reading's own fixture: a `.git` that is not a repository.
/// Its state cannot be read, so the plan says THAT — loudly, in the
/// signed bytes — keeps it, and still plans the backups. Git must not
/// walk up and answer for a repository above the candidate.
#[test]
fn a_checkout_whose_state_cannot_be_read_is_kept_and_says_so() {
    let c = Case::new("git-unreadable");
    put(
        &c.opt.join("boss-dev-bak/src/.git/HEAD"),
        "ref: refs/heads/main\n",
    );
    // A real repository ABOVE the candidate, which git must not answer
    // for — with a commit, because an unborn one fails `rev-list HEAD`
    // and so hid a missing ceiling (mutation-checked 2026-09-27).
    git(&c.opt, &["init", "-q"]);
    git(&c.opt, &["commit", "-q", "--allow-empty", "-m", "above"]);
    age(&c.opt);
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    let src = c.opt.join("boss-dev-bak/src");
    contains_all(
        &plan.out,
        &[
            &format!("would keep {}", c.opt.join("boss-dev-bak").display()),
            &format!("  checkout {}:\n", src.display()),
            "    its unpushed state could not be read:",
        ],
        "the plan",
    );
    assert!(
        !plan.out.contains("untracked files:"),
        "an unreadable checkout reported counts:\n{text}"
    );
    for b in &BACKUPS[..3] {
        contains_all(
            &plan.out,
            &[&format!("would remove {} (", c.opt.join(b).display())],
            "the plan",
        );
    }
    c.assert_nothing_removed(&text);
}

#[test]
fn refuses_a_backup_the_live_tree_resolves_into() {
    let c = Case::new("live-tree");
    // /opt/boss-cli is a symlink into a backup: a rollback that stayed.
    std::fs::remove_dir_all(c.opt.join("boss-cli")).unwrap();
    std::os::unix::fs::symlink(c.opt.join("boss-dev-bak"), c.opt.join("boss-cli")).unwrap();
    let r = c.run(&["--dry-run"]);
    let text = r.text();
    assert_eq!(r.code, 2, "a live backup was not refused:\n{text}");
    contains_all(&text, &["boss-cli", "boss-dev-bak", "LIVE"], "the refusal");
    assert!(c.opt.join("boss-dev-bak/bin/boss-jobs-api").is_file());
}

#[test]
fn refuses_a_backup_with_a_mount_inside_it() {
    let c = Case::new("mount");
    let inside = c.opt.join("boss-binbak-pre-latest-194355/bin");
    write_file(
        &c.mountinfo,
        &format!(
            "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
             41 22 8:1 /var/lib/boss {} rw,relatime shared:1 - ext4 /dev/sda1 rw\n",
            inside.display()
        ),
    );
    c.refused(&["--dry-run"], &[], &["is a mount point", "bind mount"]);
}

#[test]
fn refuses_a_backup_a_symlink_elsewhere_resolves_into() {
    let c = Case::new("link");
    std::os::unix::fs::symlink(
        c.opt
            .join("boss-binbak-pre-latest-194355/bin/boss-jobs-api"),
        c.links.join("boss-jobs-api"),
    )
    .unwrap();
    c.refused(&["--dry-run"], &[], &["still linked to"]);
}

#[test]
fn refuses_a_backup_a_running_process_executes_from() {
    let c = Case::new("proc");
    std::fs::create_dir_all(c.proc_dir.join("202")).unwrap();
    std::os::unix::fs::symlink(
        c.opt
            .join("boss-binbak-pre-pr73-0702-1801/bin/boss-jobs-api"),
        c.proc_dir.join("202/exe"),
    )
    .unwrap();
    write_file(&c.proc_dir.join("202/comm"), "boss-jobs-api\n");
    c.refused(&["--dry-run"], &[], &["process 202", "LIVE"]);
}

#[test]
fn refuses_a_backup_a_loaded_unit_names() {
    let c = Case::new("unit");
    let exec = c
        .opt
        .join("boss-dev-bak/bin/boss-jobs-api")
        .display()
        .to_string();
    c.refused(
        &["--dry-run"],
        &[("STUB_EXEC", exec)],
        &["boss-ops-runner.service", "LIVE"],
    );
}

/// The retired second stack's units are stopped and disabled — not
/// loaded — and kept on disk for a restore; a unit FILE, or a drop-in,
/// naming a backup still refuses it.
#[test]
fn refuses_a_backup_a_unit_file_on_disk_names() {
    for (name, file) in [
        ("unit-file", "boss-docs-api.service"),
        ("drop-in", "boss-views.service.d/override.conf"),
    ] {
        let c = Case::new(name);
        put(
            &c.units.join(file),
            &format!(
                "[Service]\nExecStart={}\n",
                c.opt
                    .join("boss-binbak-pre-pr5-20260701-032322/bin/boss-jobs-api")
                    .display()
            ),
        );
        c.refused(
            &["--dry-run"],
            &[],
            &["the unit file", file, "kept for a restore"],
        );
    }
}

#[test]
fn a_unit_bound_that_cannot_be_evaluated_is_a_refusal() {
    let c = Case::new("no-bus");
    c.refused(
        &["--dry-run"],
        &[("STUB_LIST_FAIL", "1".into())],
        &["could not list", "cannot be evaluated"],
    );
    c.refused(
        &["--dry-run"],
        &[("STUB_SHOW_FAIL", "1".into())],
        &["systemctl show", "cannot be judged"],
    );
}

/// The runner refuses a plan over OPS_OUTPUT_CAP (102400 bytes), and one
/// evidence line per branch reached 112,659 bytes at 1690 branches in
/// review — the reclaim blocked again by its own evidence. The plan names
/// the first twenty in C order and counts the rest.
#[test]
fn the_evidence_names_twenty_branches_and_counts_the_rest_under_the_runner_cap() {
    const OPS_OUTPUT_CAP: usize = 102_400;
    let c = Case::new("git-many");
    let r = make_checkout(&c, false);
    let tip = git(&r, &["rev-parse", "main"]);
    let refs: String = (0..1300)
        .map(|i| {
            format!(
                "create refs/heads/wip/an-unpushed-branch-with-a-deliberately-long-descriptive-name-{i:04} {tip}\n"
            )
        })
        .collect();
    let stdin = c.root.join("refs.txt");
    write_file(&stdin, &refs);
    let ok = Command::new("git")
        .current_dir(&r)
        .args(["update-ref", "--stdin"])
        .stdin(std::fs::File::open(&stdin).unwrap())
        .status()
        .expect("git update-ref runs")
        .success();
    assert!(ok, "could not create the branches");
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{}", &text[..text.len().min(4000)]);
    assert!(
        plan.out.len() < OPS_OUTPUT_CAP,
        "the plan is {} bytes, over the runner's cap",
        plan.out.len()
    );
    contains_all(
        &plan.out,
        &[
            "    branches with commits on no remote: 1302\n",
            "      feature: 1 commit(s)\n      main: 2 commit(s)\n",
            "descriptive-name-0017: 2 commit(s)\n      ...and 1282 more\n",
        ],
        "the bounded branch list",
    );
    let named = plan
        .out
        .lines()
        .filter(|l| l.starts_with("      ") && l.ends_with(" commit(s)"))
        .count();
    assert_eq!(named, 20, "the plan named {named} branches, not 20");
    assert_eq!(
        plan.out,
        c.run(&["--dry-run"]).out,
        "two renders of many branches differ"
    );
}

/// The adversarial review planted a clean filter in the checkout's
/// .git/config and the plan verb RAN it: `git status` re-hashes every
/// stat-dirty file through it, and a `cp -a` copy is stat-dirty
/// everywhere. Every command a repository's config can name is planted
/// here — clean, smudge and process filters, fsmonitor, textconv, an
/// external diff, gpg with log.showSignature, every hook — and neither the
/// plan nor the signed write may run one; the evidence is still counted.
#[test]
fn reading_a_checkout_runs_nothing_its_config_names() {
    let c = Case::new("git-no-exec");
    let r = make_checkout(&c, false);
    let marker = c.root.join("PWNED");
    let evil = c.root.join("evil.sh");
    write_exec(
        &evil,
        &format!("#!/bin/sh\necho \"$0 $*\" >> {}\ncat\n", marker.display()),
    );
    let e = evil.display();
    let hooks = c.root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for h in [
        "post-index-change",
        "reference-transaction",
        "post-checkout",
        "pre-auto-gc",
    ] {
        write_exec(&hooks.join(h), &format!("#!/bin/sh\n{e} hook-{h}\n"));
    }
    let cfg = std::fs::read_to_string(r.join(".git/config")).unwrap();
    write_file(
        &r.join(".git/config"),
        &format!(
            "{cfg}[filter \"evil\"]\n\tclean = {e} clean\n\tsmudge = {e} smudge\n\tprocess = {e} process\n\trequired = true\n\
             [core]\n\tfsmonitor = {e}\n\thooksPath = {}\n\tpager = {e}\n\
             [diff \"evil\"]\n\ttextconv = {e}\n[diff]\n\texternal = {e}\n\
             [log]\n\tshowSignature = true\n[gpg]\n\tprogram = {e}\n",
            hooks.display()
        ),
    );
    put(&r.join(".git/info/attributes"), "* filter=evil diff=evil\n");
    put(&r.join(".gitattributes"), "* filter=evil diff=evil\n");
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    assert!(
        !marker.exists(),
        "the plan ran a command the checkout's config names:\n{}\n{text}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
    contains_all(
        &plan.out,
        &[
            "    branches with commits on no remote: 2\n",
            "    tracked files whose bytes differ from the index (read without filters): 1\n",
            "    untracked files (ignored ones not counted): 3\n",
            "    stashes: 1\n",
        ],
        "the evidence, read without running anything",
    );
    let w = c.run(&[&plan.plan_sha()]);
    assert_eq!(w.code, 0, "{}", w.text());
    assert!(
        !marker.exists(),
        "the write ran a command the checkout's config names:\n{}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );
}

/// Plumbing is not command-free under a partial clone (re-review of
/// f5bd6eac, reproduced): with a promisor remote and a missing object,
/// rev-list and diff-index --cached HEAD lazily FETCH, and the fetch runs
/// the remote's `uploadpack` — as root, before the read failed closed. A
/// checkout whose config names a partial clone or a promisor remote is
/// not read at all: kept, and the plan says why.
#[test]
fn a_partial_clone_checkout_is_not_read_and_runs_nothing() {
    let c = Case::new("git-promisor");
    let r = make_checkout(&c, false);
    let tree = git(&r, &["rev-parse", "HEAD^{tree}"]);
    let marker = c.root.join("PWNED_lazy");
    let cfg = std::fs::read_to_string(r.join(".git/config")).unwrap();
    write_file(
        &r.join(".git/config"),
        &format!(
            "{cfg}[core]\n\trepositoryformatversion = 1\n[extensions]\n\tpartialClone = origin\n\
             [remote \"origin\"]\n\turl = {}\n\tpromisor = true\n\tuploadpack = \"sh -c 'touch {}; exit 1' --\"\n",
            c.root.join("nowhere").display(),
            marker.display()
        ),
    );
    std::fs::remove_file(r.join(".git/objects").join(&tree[..2]).join(&tree[2..])).unwrap();
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    assert!(
        !marker.exists(),
        "reading a partial clone ran its remote's uploadpack:\n{text}"
    );
    contains_all(
        &plan.out,
        &[
            &format!("would keep {}", r.display()),
            "    its unpushed state could not be read:",
            "partial clone",
        ],
        "the plan",
    );
    for b in &BACKUPS[..3] {
        contains_all(
            &plan.out,
            &[&format!("would remove {} (", c.opt.join(b).display())],
            "the plan",
        );
    }
}

/// Git never runs as root (re-review of f5bd6eac): a ROOT-owned checkout
/// is read as nobody (uid 65534), so whatever a path to a command turns
/// out to be, it runs with no authority. Observed through a file only
/// root can read: as nobody it cannot be hashed, so the state is "could
/// not be read" — and the checkout is kept either way. Root-only.
#[test]
fn as_root_a_root_owned_checkout_is_read_as_nobody() {
    let me = Command::new("id").arg("-u").output().expect("id runs");
    if String::from_utf8_lossy(&me.stdout).trim() != "0" {
        eprintln!("skipping: not root, so no root-owned checkout exists here");
        return;
    }
    let c = Case::new("git-root-owned");
    let r = make_checkout(&c, false);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        r.join("bin/boss-jobs-api"),
        std::fs::Permissions::from_mode(0o600), // mode-bits-ok: a file only root may read, not a script
    )
    .unwrap();
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    contains_all(
        &plan.out,
        &[
            &format!("would keep {}", r.display()),
            "    its unpushed state could not be read:",
        ],
        "a root-owned checkout read as nobody",
    );
    assert!(r.join("bin/boss-jobs-api").is_file());
}

/// A git variable in the runner's environment must not redirect the read
/// to another repository, index or config (review, LOW).
#[test]
fn a_git_environment_does_not_change_the_evidence() {
    let c = Case::new("git-env");
    make_checkout(&c, false);
    let base = c.run(&["--dry-run"]);
    assert_eq!(base.code, 0, "{}", base.text());
    let other = c.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "other"]);
    let skewed = c.run_env(
        &["--dry-run"],
        &[
            ("GIT_DIR", other.join(".git").display().to_string()),
            ("GIT_WORK_TREE", other.display().to_string()),
            (
                "GIT_INDEX_FILE",
                other.join(".git/index").display().to_string(),
            ),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "core.bare".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
        ],
    );
    assert_eq!(skewed.code, 0, "{}", skewed.text());
    assert_eq!(
        base.out, skewed.out,
        "a GIT_* variable changed the evidence"
    );
}

/// The verb runs as root; a checkout another account owns is read AS that
/// account (setpriv), so nothing in its config runs with more authority
/// than the account that wrote it. Root-only: the gate runs as uid 65534
/// and cannot switch accounts, so there it reads its own checkout (every
/// other case) and this one says it skipped.
#[test]
fn as_root_a_checkout_another_account_owns_is_read_as_its_owner() {
    let me = Command::new("id").arg("-u").output().expect("id runs");
    if String::from_utf8_lossy(&me.stdout).trim() != "0" {
        eprintln!("skipping: not root, so no checkout can be given to another account here");
        return;
    }
    let c = Case::new("git-owner");
    let r = make_checkout(&c, false);
    // Every ancestor must be traversable by the other account.
    let mut a = c.opt.clone();
    while let Some(p) = a.parent().map(Path::to_path_buf) {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        if mode & 0o001 == 0 {
            eprintln!(
                "skipping: {} is not traversable by another account",
                p.display()
            );
            return;
        }
        a = p;
    }
    // No CAP_CHOWN on the dev pod: the other account COPIES the checkout,
    // so the copy is its own (the as-gate-uid method).
    use std::os::unix::fs::PermissionsExt;
    let src = c.root.join("checkout-src");
    std::fs::rename(&r, &src).unwrap();
    std::fs::set_permissions(&c.opt, std::fs::Permissions::from_mode(0o777)).unwrap(); // mode-bits-ok: a fixture directory opened so uid 65534 can copy into it, not a script
    let ok = Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "--"])
        .args(["cp", "-a"])
        .arg(&src)
        .arg(&r)
        .status()
        .expect("setpriv runs")
        .success();
    std::fs::set_permissions(&c.opt, std::fs::Permissions::from_mode(0o755)).unwrap(); // mode-bits-ok: the fixture directory closed again, not a script
    assert!(ok, "uid 65534 could not copy the checkout");
    use std::os::unix::fs::MetadataExt;
    assert_eq!(std::fs::metadata(r.join(".git")).unwrap().uid(), 65534);
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    contains_all(
        &plan.out,
        &[
            "    branches with commits on no remote: 2\n",
            "    stashes: 1\n",
        ],
        "the evidence, read as its owner",
    );
    assert!(
        !plan.out.contains("could not be read"),
        "a checkout another account owns was not read as that account:\n{text}"
    );
}

/// The write re-renders the plan and keeps any candidate holding a .git;
/// a checkout appearing AFTER that render, before rm, is caught by the
/// last-moment check and nothing is removed from it (review, LOW).
#[test]
fn a_checkout_that_appears_after_the_render_is_not_removed() {
    let c = Case::new("git-late");
    let sha = c.run(&["--dry-run"]).plan_sha();
    let first = c.opt.join(BACKUPS[0]);
    let r = c.run_env(&[&sha], &[("STUB_PLANT_GIT", first.display().to_string())]);
    let text = r.text();
    assert_eq!(r.code, 2, "a late checkout was not refused:\n{text}");
    contains_all(&text, &[".git", "never removes a checkout"], "the refusal");
    assert!(
        first.join("bin/boss-jobs-api").is_file(),
        "the late checkout was removed:\n{text}"
    );
    assert!(!c.calls().contains("--vacuum-size"));
}

// ---------------------------------------------------------------------------
// The second-stack capture: the one exception under /var/backups (David,
// 2026-09-28, backlog f44ca628).
// ---------------------------------------------------------------------------

/// The signature binds the capture's BYTES, not its name or size: the
/// same length rewritten is a different plan, and a plan signed before
/// the rewrite removes nothing.
#[test]
fn the_plan_hash_moves_with_the_captures_bytes_and_a_stale_signature_removes_nothing() {
    let c = Case::new("capture-bytes");
    let before = c.run(&["--dry-run"]);
    assert_eq!(before.code, 0, "{}", before.text());
    let rewritten = CAPTURE_BYTES.replace("retired", "RETIRED");
    assert_eq!(
        rewritten.len(),
        CAPTURE_BYTES.len(),
        "the fixture moved size"
    );
    write_file(&c.capture(), &rewritten);
    let after = c.run(&["--dry-run"]);
    assert_eq!(after.code, 0, "{}", after.text());
    assert_ne!(
        before.plan_sha(),
        after.plan_sha(),
        "a capture rewritten at the same size hashed to the same plan"
    );
    contains_all(
        &after.out,
        &[&c.capture_line(&c.capture(), &rewritten)],
        "the re-rendered plan",
    );

    let r = c.run(&[&before.plan_sha()]);
    let text = r.text();
    assert_eq!(r.code, 2, "a stale signature was not refused:\n{text}");
    contains_all(&text, &["not the approved"], "the refusal");
    c.assert_nothing_removed(&text);
}

/// The last moment: the capture's bytes change AFTER the write re-rendered
/// the plan and before its rm. The re-hash beside the rm refuses it, and
/// the capture — removed last — is the thing still standing.
#[test]
fn a_capture_rewritten_after_the_render_is_not_removed() {
    let c = Case::new("capture-late");
    let sha = c.run(&["--dry-run"]).plan_sha();
    let r = c.run_env(
        &[&sha],
        &[("STUB_REWRITE", c.capture().display().to_string())],
    );
    let text = r.text();
    assert_eq!(r.code, 2, "a late rewrite was not refused:\n{text}");
    contains_all(
        &text,
        &[
            "hashes to",
            "not the planned",
            "already removed (4):",
            "the journal was not vacuumed",
        ],
        "the refusal",
    );
    assert_eq!(
        std::fs::read_to_string(c.capture()).unwrap(),
        "late bytes\n",
        "the rewritten capture was removed:\n{text}"
    );
    c.assert_not_ours_intact(&text);
    assert!(!c.calls().contains("--vacuum-size"));
}

#[test]
fn refuses_a_symlinked_capture_and_leaves_its_target() {
    let c = Case::new("capture-symlink");
    let target = c.backups.join("boss-cluster-pg/live.sql");
    put(&target, "a dump somebody still wants\n");
    std::fs::remove_file(c.capture()).unwrap();
    std::os::unix::fs::symlink(&target, c.capture()).unwrap();
    c.refused(&["--dry-run"], &[], &[CAPTURE, "is a symlink"]);
    assert!(target.is_file(), "the link's target was touched");
}

/// The directory, too: a capture directory that is a link is somewhere
/// else, and nothing is read through it.
#[test]
fn refuses_a_symlinked_capture_directory() {
    let c = Case::new("capture-dir-symlink");
    let elsewhere = c.root.join("elsewhere");
    std::fs::rename(c.capture_dir(), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, c.capture_dir()).unwrap();
    c.refused(&["--dry-run"], &[], &["second-stack", "is a symlink"]);
    assert!(elsewhere.join(CAPTURE).is_file());
}

/// A name retire-second-stack does not write is refused, loudly, with
/// nothing removed — its stamp is `%Y%m%dT%H%M%SZ`, so a 14-digit stamp
/// (the packet's first reading) is not a capture this verb was reviewed
/// to remove.
#[test]
fn refuses_a_capture_name_retire_second_stack_does_not_write() {
    for (case, name) in [
        ("capture-14-digit", "second-stack-20260915211123.sql"),
        ("capture-word", "second-stack-x.sql"),
        ("capture-space", "second-stack-20260915T211123Z .sql"),
    ] {
        let c = Case::new(case);
        put(&c.capture_dir().join(name), "odd\n");
        c.refused(&["--dry-run"], &[], &[name, "not ^second-stack-"]);
        assert!(c.capture_dir().join(name).is_file());
    }
}

#[test]
fn refuses_a_directory_by_a_captures_name() {
    let c = Case::new("capture-is-dir");
    let d = c.capture_dir().join("second-stack-20260916T000000Z.sql");
    put(&d.join("inside"), "x\n");
    c.refused(&["--dry-run"], &[], &["is not a regular file"]);
    assert!(d.join("inside").is_file());
}

/// A capture a process holds open is refused: it is being written (a
/// retire-second-stack run's pg_dump) or read (a restore), and removing
/// it frees nothing while that process lives.
#[test]
fn refuses_a_capture_a_process_holds_open() {
    let c = Case::new("capture-open");
    std::fs::create_dir_all(c.proc_dir.join("303/fd")).unwrap();
    std::os::unix::fs::symlink(c.capture(), c.proc_dir.join("303/fd/5")).unwrap();
    write_file(&c.proc_dir.join("303/comm"), "pg_dump\n");
    c.refused(&["--dry-run"], &[], &["process 303", "pg_dump", "open"]);
}

/// A process table that cannot be read is a bound that cannot be
/// evaluated — a refusal, never a pass — for BOTH readers of it: bound
/// 5(d), whether a backup is a running process's executable (it passed
/// silently until the adversarial review of 7bca1fee), and bound 7,
/// whether a process holds the capture open.
#[test]
fn a_process_table_that_cannot_be_read_is_a_refusal() {
    let c = Case::new("no-proc-backups");
    let gone = c.root.join("no-such-proc").display().to_string();
    c.refused(
        &["--dry-run"],
        &[("BOSS_RECLAIM_PROC_DIR", gone.clone())],
        &["running process", "cannot be read", "not passed"],
    );
    // With no backup directory to judge, the capture bound is the reader.
    let c = Case::new("no-proc-capture");
    for b in BACKUPS {
        std::fs::remove_dir_all(c.opt.join(b)).unwrap();
    }
    let gone = c.root.join("no-such-proc").display().to_string();
    let r = c.run_env(&["--dry-run"], &[("BOSS_RECLAIM_PROC_DIR", gone)]);
    let text = r.text();
    assert_eq!(r.code, 2, "an unreadable table passed the capture:\n{text}");
    contains_all(
        &text,
        &["holds", "open cannot be judged", "cannot be read"],
        "the refusal",
    );
    assert!(c.capture().is_file());
}

/// A partial process table — no pid 1 — is not the host's whole table,
/// and a holder could be among the missing.
#[test]
fn a_process_table_without_init_is_a_refusal() {
    let c = Case::new("capture-no-init");
    std::fs::remove_dir_all(c.proc_dir.join("1")).unwrap();
    c.refused(&["--dry-run"], &[], &["process 1", "not passed"]);
}

/// An fd that cannot be followed for any reason but "gone" (here a link
/// loop; on a host, EIO or E2BIG) leaves the bound unread: a refusal.
#[test]
fn an_fd_that_cannot_be_followed_is_a_refusal() {
    let c = Case::new("capture-fd-loop");
    let fd = c.proc_dir.join("101/fd/9");
    std::os::unix::fs::symlink(&fd, &fd).unwrap();
    c.refused(
        &["--dry-run"],
        &[],
        &["process 101", "cannot be followed", "not passed"],
    );
}

/// The adversarial review's MUST-FIX 1: a boss-cluster-pg dump hardlinked
/// in under a capture's name passed, the write removed the NAME, and the
/// OK line claimed bytes freed that were not — a false record under
/// David's passkey. A capture with more than one link is refused.
#[test]
fn refuses_a_hardlinked_capture() {
    let c = Case::new("capture-hardlink");
    let dump = c.backups.join(NOT_OURS[0]);
    std::fs::remove_file(c.capture()).unwrap();
    std::fs::hard_link(&dump, c.capture()).unwrap();
    c.refused(&["--dry-run"], &[], &[CAPTURE, "2 links"]);
    assert!(dump.is_file());
}

/// ...and at the last moment: a link made after the render is refused by
/// the write's own count, with the capture standing.
#[test]
fn a_capture_hardlinked_after_the_render_is_not_removed() {
    let c = Case::new("capture-late-hardlink");
    let sha = c.run(&["--dry-run"]).plan_sha();
    let twin = c.backups.join("boss-cluster-pg/twin.sql");
    let r = c.run_env(
        &[&sha],
        &[(
            "STUB_HARDLINK",
            format!("{} {}", c.capture().display(), twin.display()),
        )],
    );
    let text = r.text();
    assert_eq!(r.code, 2, "a late hardlink was not refused:\n{text}");
    contains_all(&text, &["2 links", "already removed (4):"], "the refusal");
    assert!(c.capture().is_file() && twin.is_file());
    assert!(!c.calls().contains("--vacuum-size"));
}

/// The write's own open-file check (review item 2): a process that opens
/// the capture after the render is caught before the rm.
#[test]
fn a_capture_opened_after_the_render_is_not_removed() {
    let c = Case::new("capture-late-open");
    let sha = c.run(&["--dry-run"]).plan_sha();
    let r = c.run_env(
        &[&sha],
        &[(
            "STUB_FD",
            format!(
                "{} {}",
                c.proc_dir.join("404/fd/3").display(),
                c.capture().display()
            ),
        )],
    );
    let text = r.text();
    assert_eq!(r.code, 2, "a late holder was not refused:\n{text}");
    contains_all(
        &text,
        &["process 404", "open now", "already removed (4):"],
        "the refusal",
    );
    assert!(c.capture().is_file());
}

/// The write's own realpath check (review item 2): a parent swapped for a
/// link after the render leaves the capture directory a real directory
/// and the capture a regular file, but not where the plan named it.
#[test]
fn a_capture_whose_path_moved_after_the_render_is_not_removed() {
    let c = Case::new("capture-late-swap");
    let sha = c.run(&["--dry-run"]).plan_sha();
    let r = c.run_env(
        &[&sha],
        &[("STUB_SWAP_DIR", c.backups.display().to_string())],
    );
    let text = r.text();
    assert_eq!(r.code, 2, "a moved capture was not refused:\n{text}");
    contains_all(&text, &["resolves to", "now, not itself"], "the refusal");
    assert!(
        c.backups
            .with_file_name("boss.moved")
            .join("second-stack")
            .join(CAPTURE)
            .is_file()
    );
}

/// Review item 3: the capture directory and its parent must be the
/// owner's (root on the host) and writable by nobody else — or a file
/// could be planted under the capture's name — and both facts ride in
/// the signed bytes.
#[test]
fn refuses_a_capture_directory_another_account_owns_or_can_write() {
    let c = Case::new("capture-owner");
    let other = (c.owner().parse::<u32>().unwrap() + 1).to_string();
    c.refused(
        &["--dry-run"],
        &[("BOSS_RECLAIM_CAPTURE_OWNER", other.clone())],
        &["owned by uid", &format!("not uid {other}")],
    );
    use std::os::unix::fs::PermissionsExt;
    for (dir, mode) in [(c.capture_dir(), 0o775), (c.backups.clone(), 0o757)] {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap(); // mode-bits-ok: a fixture directory made writable to prove the bound refuses it, not a script
        c.refused(&["--dry-run"], &[], &["group- or other-writable"]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap(); // mode-bits-ok: the fixture directory closed again, not a script
    }
    let plan = c.run(&["--dry-run"]);
    assert_eq!(plan.code, 0, "{}", plan.text());
    contains_all(
        &plan.out,
        &[&format!(
            "  capture directory {}: owner uid {}, mode 755; its parent {}: owner uid {}, mode 755\n",
            c.capture_dir().display(),
            c.owner(),
            c.backups.display(),
            c.owner()
        )],
        "the plan",
    );
}

/// Only the capture directory is in reach: a capture-named file anywhere
/// else under /var/backups is neither planned nor removed.
#[test]
fn a_capture_named_file_outside_the_capture_directory_is_not_reached() {
    let c = Case::new("capture-outside");
    let outside = c.backups.join("boss-cluster-pg").join(CAPTURE);
    put(&outside, CAPTURE_BYTES);
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    assert!(
        !plan.out.contains(&outside.display().to_string()),
        "the plan names a capture outside its directory:\n{text}"
    );
    let r = c.run(&[&plan.plan_sha()]);
    assert_eq!(r.code, 0, "{}", r.text());
    assert!(
        outside.is_file(),
        "a file outside the capture directory went"
    );
    assert!(!c.capture().exists());
    c.assert_not_ours_intact(&r.text());
}

/// After the capture is gone the verb still plans the rest; with no
/// capture directory at all it plans no capture and says so.
#[test]
fn no_capture_directory_plans_no_capture() {
    let c = Case::new("capture-none");
    std::fs::remove_dir_all(c.capture_dir()).unwrap();
    let plan = c.run(&["--dry-run"]);
    let text = plan.text();
    assert_eq!(plan.code, 0, "{text}");
    assert!(!plan.out.contains("second-stack"), "{text}");
    contains_all(
        &plan.err,
        &["no second-stack capture", "0 second-stack capture(s)"],
        "the dry run",
    );
}

// ---------------------------------------------------------------------------
// THROUGH THE RUNNER, with the real allowlist, as boss-gcp.
// ---------------------------------------------------------------------------

fn shipped_verbs(root: &Path) -> PathBuf {
    let dst = root.join("verbs");
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(repo_root().join("infra/ops/verbs")).expect("infra/ops/verbs/") {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "json") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

fn run_runner(
    c: &Case,
    verbs: &Path,
    verb: &str,
    args: &str,
) -> (String, Option<serde_json::Value>) {
    write_exec(
        &c.bin.join("curl"),
        &[
            "#!/bin/sh\n",
            boss_testing::ops_runner_stub::RECORD_STEP_METADATA,
            "cat \"$STUB_JOBS\"\n",
        ]
        .concat(),
    );
    write_file(
        &c.root.join("jobs.json"),
        &format!(
            r#"{{"data":[{{"id":"aaaaaaaa-0000-4000-8000-000000000000","status":"open","metadata":{{"host":"boss-gcp","verb":"{verb}","args":{args}}},"steps":[{{"id":"s-execute","spec_slug":"execute","status":"ready","metadata":{{"authority_role":"platform-admin"}}}}]}}]}}"#
        ),
    );
    let step_md = c.root.join("step-metadata.json");
    let _ = std::fs::remove_file(&step_md);
    let mut cmd = Command::new("sh");
    cmd.arg(repo_root().join("infra/ops/ops-runner.sh"));
    c.env(&mut cmd);
    let out = cmd
        .env("HOST_ID", "boss-gcp")
        .env("BOSS_JOBS_URL", "http://sor.invalid")
        .env("OPS_VERBS_DIR", verbs)
        .env("STUB_JOBS", c.root.join("jobs.json"))
        .env("STUB_STEP_METADATA", &step_md)
        .output()
        .expect("ops-runner.sh runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = boss_testing::ops_runner_stub::step_metadata_written(&step_md);
    (text, meta)
}

/// A packet cannot hand the write a path, and a well-formed hash with no
/// passkey approval behind it does not reach the script either.
#[test]
fn the_runner_refuses_a_path_or_an_unapproved_hash() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-refuses");
    let verbs = shipped_verbs(&c.root);
    let hash = "0".repeat(64);
    for args in [
        "[]".to_string(),
        r#"["/var/backups/boss"]"#.to_string(),
        r#"["--dry-run"]"#.to_string(),
        format!(r#"["{hash}","/home/dauld"]"#),
        format!(r#"["{hash}"]"#),
    ] {
        let (text, _meta) = run_runner(&c, &verbs, "reclaim-gcp-root", &args);
        assert!(
            !c.calls().contains("systemctl list-units"),
            "{args} reached the script:\n{text}"
        );
        c.assert_nothing_removed(&text);
    }
}

/// The plan verb is a plain read: answered on boss-gcp through the
/// runner, with the plan on the packet and nothing removed.
#[test]
fn the_runner_on_boss_gcp_answers_the_plan_verb() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-plan");
    let verbs = shipped_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, "plan-a-gcp-root-reclaim", "[]");
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the plan verb was not answered:\n{text}"
    );
    assert_eq!(meta["exit_code"], "0", "the plan did not exit 0:\n{text}");
    let output = meta["output"].as_str().unwrap_or_default();
    contains_all(
        output,
        &["would remove", "boss-dev-bak", "plan-sha256:"],
        "the packet's output",
    );
    c.assert_nothing_removed(&text);
}

/// The live failure, through the runner: plan-a-gcp-root-reclaim on a
/// host whose /opt/boss-dev-bak is a checkout was REFUSED (ops-request
/// 8d334ca2). It is answered now, keeping the checkout in the plan.
#[test]
fn the_runner_answers_the_plan_verb_when_a_candidate_is_a_checkout() {
    if !has("jq") {
        eprintln!("skipping: the ops-runner is sh + jq and this box has no jq");
        return;
    }
    let c = Case::new("runner-git");
    make_checkout(&c, false);
    let verbs = shipped_verbs(&c.root);
    let (text, meta) = run_runner(&c, &verbs, "plan-a-gcp-root-reclaim", "[]");
    let meta = meta.expect("the runner completed the execute step");
    assert_eq!(
        meta["disposition"], "answered",
        "the plan verb was not answered:\n{text}"
    );
    let output = meta["output"].as_str().unwrap_or_default();
    contains_all(
        output,
        &[
            "would keep",
            "branches with commits on no remote: 2",
            "boss-binbak-pre-pr73-0702-1801",
            "plan-sha256:",
        ],
        "the packet's output",
    );
    c.assert_nothing_removed(&text);
}
