//! `infra/dev/wt-web` — the web half of `wt-cargo`: from inside a
//! worktree it gives the worktree its OWN `node_modules` (the gate's
//! frozen install, hardlinked out of bun's cache), proves Playwright can
//! start there, then runs the given command.
//!
//! A worktree has no `node_modules`, so every web check a builder ran
//! there failed on the environment before it could fail on the change
//! (backlog ef4394e1). The first answer symlinked the operator
//! checkout's four `node_modules` in, and that answer failed silently
//! (backlog 44f1cd1e, 2026-09-27): the operator's `apps/web/node_modules/.bin`
//! had been rewritten without `playwright`, so the mocked runner's
//! `bun x playwright` fetched the registry's latest (1.63.0) while every
//! spec imported the linked 1.61.0 — "two different versions of
//! @playwright/test" before any spec ran, in every worktree at once,
//! while builders reported web checks they could not have run. A link
//! shares one mutable tree among every worktree and answers for a
//! lockfile that is not the worktree's own; the gate's install answers
//! for exactly the tree under test and, warm, took 128 ms here.
//!
//! Pinned against a scratch git repository and a stub `bun` on PATH
//! that plays both the install and `bun x` (falling back, as the real
//! one does, to a DIFFERENT Playwright when the worktree has no
//! `playwright` bin), so nothing touches /work/boss or the registry.
//! The smoke must never ask `bun x` at all — asking is what fetches the
//! second copy, and `--no-install` still answered the cached latest when
//! measured — so every case below also holds that the stub's `x` was
//! never called:
//!
//!   * it runs the gate's own install command in `apps/web`, the same
//!     text `infra/gate.sh` runs (§9a: a fact that lives twice gets an
//!     equality test), and the modules it leaves are gitignored;
//!   * links an older wt-web made are removed — the LINK, never its
//!     target — before the install, which would otherwise write through
//!     them into the operator's checkout;
//!   * Playwright that cannot start is REFUSED, with NOT RUN, and the
//!     command never runs: a missing bin, or a bin answering another
//!     version than the one the specs import;
//!   * a failed install is refused the same way;
//!   * outside a git worktree it refuses (exit 2) and installs nothing;
//!   * argv passes through untouched; with no argv it only prepares.

use boss_testing::{repo_root, scratch_dir, write_exec, write_file};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "infra/dev/wt-web";
/// Where the older wt-web left symlinks into the operator's checkout.
const LINKS: [&str; 4] = [
    "node_modules",
    "apps/web/node_modules",
    "libs/web-kit/node_modules",
    "apps/simulator/node_modules",
];

/// The stub `bun`. `install` leaves the layout a frozen install leaves
/// (in its cwd, apps/web, and the workspace root) — or fails, or omits
/// the bin, as the test asks. `x playwright` is bun x's lookup: the
/// cwd's `node_modules/.bin` first, and otherwise the registry's latest,
/// which is a SECOND Playwright — the 2026-09-27 failure.
const BUN_STUB: &str = r#"#!/usr/bin/env bash
echo "bun $* cwd=$PWD skip=${PUPPETEER_SKIP_DOWNLOAD:-unset}" >> "$STUB_LOG"
case "$1" in
  install)
    if [ "${STUB_INSTALL_FAILS:-}" = 1 ]; then
      echo "error: lockfile had changes, but lockfile is frozen" >&2
      exit 1
    fi
    mkdir -p node_modules/@playwright/test ../../node_modules/.bun \
      ../simulator/node_modules ../../libs/web-kit/node_modules
    printf '{\n  "name": "@playwright/test",\n  "version": "1.61.0"\n}\n' \
      > node_modules/@playwright/test/package.json
    if [ "${STUB_NO_BIN:-}" != 1 ]; then
      mkdir -p node_modules/.bin
      printf '#!/usr/bin/env bash\necho "Version %s"\n' "${STUB_BIN_VERSION:-1.61.0}" \
        > node_modules/.bin/playwright
      chmod +x node_modules/.bin/playwright
    fi
    echo "164 packages installed"
    ;;
  x)
    [ "$2" = playwright ] || exit 9
    shift 2
    if [ -x node_modules/.bin/playwright ]; then exec node_modules/.bin/playwright "$@"; fi
    echo "Version 1.63.0"
    ;;
  *) exit 9 ;;
esac
"#;

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = scratch_dir(&format!("wt-web-{name}"));
        let bin = root.join("bin");
        boss_testing::create_dir(&bin);
        // The command under test hands off to: print what it was given
        // and where it ran. The real bun / svelte-check is never reached.
        write_exec(
            &bin.join("webcheck"),
            "#!/usr/bin/env bash\n\
             echo \"argv=$*\"\n\
             echo \"cwd=$PWD\"\n",
        );
        write_exec(&bin.join("bun"), BUN_STUB);
        let log = root.join("bun.log");
        Self { root, bin, log }
    }

    /// A real git repository carrying the repo's own root `.gitignore`
    /// (so the test reads the pattern that will judge the modules in a
    /// live worktree) and an `apps/web/package.json` to install for.
    fn worktree(&self, name: &str) -> PathBuf {
        let wt = self.root.join("trees").join(name);
        boss_testing::create_dir(&wt);
        let out = Command::new("git")
            .args(["init", "-q"])
            .arg(&wt)
            .output()
            .expect("git init");
        assert!(
            out.status.success(),
            "git init {}: {}",
            wt.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        let ignore = std::fs::read_to_string(repo_root().join(".gitignore")).expect(".gitignore");
        write_file(&wt.join(".gitignore"), &ignore);
        boss_testing::create_dir(&wt.join("apps/web"));
        write_file(&wt.join("apps/web/package.json"), "{}\n");
        wt
    }

    fn run(&self, worktree: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new(repo_root().join(SCRIPT));
        cmd.args(args)
            .current_dir(worktree)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("STUB_LOG", &self.log)
            .env_remove("PUPPETEER_SKIP_DOWNLOAD");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run wt-web");
        let merged = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), merged)
    }

    fn bun_log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// `git check-ignore` for EVERY path, one at a time — with several
    /// paths it answers "one or more", which is not the question.
    fn ignored(&self, worktree: &Path, rels: &[&str]) -> bool {
        rels.iter().all(|rel| {
            Command::new("git")
                .args(["check-ignore", "-q", rel])
                .current_dir(worktree)
                .output()
                .expect("git check-ignore")
                .status
                .success()
        })
    }
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[test]
fn the_script_is_in_the_tree_and_executable() {
    use std::os::unix::fs::PermissionsExt;
    let path = repo_root().join(SCRIPT);
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "{SCRIPT} must be executable: the pod's /work/tools/bin/wt-web is a symlink to it"
    );
}

/// ONE INSTALL, SPELLED TWICE (§9a). The worktree is prepared by the
/// command the gate's `web install` check runs, or a builder's web
/// check answers for a different node_modules than the gate's does —
/// which is the defect this door was rebuilt to close.
#[test]
fn it_runs_the_gates_own_web_install() {
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("read gate.sh");
    let line = gate
        .lines()
        .find(|l| l.contains("check \"web install\""))
        .expect("infra/gate.sh has a `web install` check");
    let install = line
        .split("cd apps/web && ")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .expect("the web install check runs `cd apps/web && <install>`");
    let wt_web = std::fs::read_to_string(repo_root().join(SCRIPT)).expect("read wt-web");
    assert!(
        wt_web.contains(install),
        "{SCRIPT} must run the gate's web install verbatim, `{install}` (from infra/gate.sh), \
         in apps/web"
    );
}

/// The happy path: the gate's install runs in apps/web, what it leaves
/// is real (not linked) and gitignored, Playwright is proven to start,
/// and the command runs in the worktree with its argv intact.
#[test]
fn installs_the_worktrees_own_modules_and_execs_its_argv() {
    let f = Fixture::new("installs");
    let wt = f.worktree("agent-web");

    let (rc, out) = f.run(&wt, &["webcheck", "test", "src/it/yard/"], &[]);
    assert_eq!(rc, 0, "wt-web failed: {out}");

    let log = f.bun_log();
    assert!(
        log.contains(&format!(
            "bun install --frozen-lockfile cwd={} skip=1",
            wt.join("apps/web").display()
        )),
        "the frozen install must run in apps/web with puppeteer's download skipped: {log}"
    );
    for rel in LINKS {
        let p = wt.join(rel);
        assert!(
            p.is_dir() && !is_symlink(&p),
            "{rel} must be the worktree's own directory"
        );
    }
    assert!(
        f.ignored(&wt, &LINKS),
        "the modules must be ignored by the root .gitignore, or every builder worktree \
         shows untracked paths after its first web check"
    );
    assert!(
        out.contains("playwright 1.61.0 starts"),
        "the Playwright smoke must be reported: {out}"
    );
    assert!(
        !log.contains("bun x"),
        "the smoke must run the tree's bin by path, never ask bun x: {log}"
    );
    assert!(
        out.contains("argv=test src/it/yard/"),
        "the command's argv must pass through: {out}"
    );
    assert!(
        out.contains(&format!("cwd={}", wt.display())),
        "the command must run where wt-web was run: {out}"
    );
}

/// A worktree an older wt-web prepared holds symlinks into the
/// operator's checkout. An install through them would write INTO that
/// checkout, so each is removed first — the link, never its target.
#[test]
fn links_an_older_wt_web_made_are_removed_and_their_targets_left_alone() {
    let f = Fixture::new("old-links");
    let wt = f.worktree("agent-linked");
    let source = f.root.join("operator");
    for rel in LINKS {
        let dir = source.join(rel);
        boss_testing::create_dir(&dir);
        write_file(&dir.join("marker.txt"), rel);
        let dst = wt.join(rel);
        boss_testing::create_dir(dst.parent().expect("parent"));
        std::os::unix::fs::symlink(&dir, &dst).expect("plant the old link");
    }

    let (rc, out) = f.run(&wt, &[], &[]);
    assert_eq!(rc, 0, "wt-web failed: {out}");

    for rel in LINKS {
        assert!(
            !is_symlink(&wt.join(rel)),
            "{rel} must no longer be a link: {out}"
        );
        assert!(
            out.contains(&format!("removed the link {rel}")),
            "each removed link must be said, by name: {out}"
        );
        assert_eq!(
            std::fs::read_to_string(source.join(rel).join("marker.txt")).expect("marker"),
            rel,
            "the operator's {rel} must be untouched"
        );
    }
    assert!(
        !source.join("apps/web/node_modules/@playwright").exists(),
        "the install wrote through a link into the operator's checkout"
    );
}

/// The 2026-09-27 failure: no `playwright` bin, so `bun x playwright`
/// would run the registry's latest while the specs import the installed
/// one. Refused before the command, naming the missing bin, NOT RUN —
/// and without asking bun x, which would have fetched that latest.
#[test]
fn a_missing_playwright_bin_is_refused_as_not_run() {
    let f = Fixture::new("no-bin");
    let wt = f.worktree("agent-nobin");

    let (rc, out) = f.run(&wt, &["webcheck"], &[("STUB_NO_BIN", "1")]);
    assert_eq!(
        rc, 4,
        "a Playwright that cannot start must refuse with 4: {out}"
    );
    let bin = wt.join("apps/web/node_modules/.bin/playwright");
    assert!(
        out.contains("NOT RUN")
            && out.contains("1.61.0")
            && out.contains(&format!("{} is missing", bin.display())),
        "the refusal must say NOT RUN, name the missing bin and the version the specs import: {out}"
    );
    assert!(!out.contains("argv="), "the command must not run: {out}");
    assert!(
        !f.bun_log().contains("bun x"),
        "a missing bin must be refused by path, never by asking bun x: {}",
        f.bun_log()
    );
}

/// A bin that is there but answers for another version is the same
/// two-copies failure, and refused the same way.
#[test]
fn a_bin_of_another_version_is_refused_as_not_run() {
    let f = Fixture::new("other-version");
    let wt = f.worktree("agent-drift");

    let (rc, out) = f.run(&wt, &["webcheck"], &[("STUB_BIN_VERSION", "1.60.0")]);
    assert_eq!(rc, 4, "must refuse with 4: {out}");
    assert!(
        out.contains("NOT RUN") && out.contains("1.60.0"),
        "must say NOT RUN and name what the bin answered: {out}"
    );
    assert!(!out.contains("argv="), "the command must not run: {out}");
}

/// An install that fails (a lockfile the tree's package.json no longer
/// matches, a registry that is down) leaves nothing to check with.
#[test]
fn a_failed_install_is_refused_as_not_run() {
    let f = Fixture::new("install-fails");
    let wt = f.worktree("agent-frozen");

    let (rc, out) = f.run(&wt, &["webcheck"], &[("STUB_INSTALL_FAILS", "1")]);
    assert_eq!(rc, 3, "a failed install must refuse with 3: {out}");
    assert!(
        out.contains("NOT RUN") && out.contains("lockfile is frozen"),
        "the refusal must say NOT RUN and carry bun's own error: {out}"
    );
    assert!(!out.contains("argv="), "the command must not run: {out}");
}

/// With no argv there is nothing to exec: it prepares, reports, exits 0.
#[test]
fn with_no_arguments_it_only_prepares() {
    let f = Fixture::new("noargs");
    let wt = f.worktree("agent-prepare");

    let (rc, out) = f.run(&wt, &[], &[]);
    assert_eq!(rc, 0, "wt-web failed: {out}");
    assert!(!out.contains("argv="), "nothing must be exec'd: {out}");
    assert!(
        out.contains("164 packages installed") && out.contains("playwright 1.61.0 starts"),
        "the install and the smoke must be printed: {out}"
    );
}

/// Outside a git worktree there is no root to install into; the script
/// refuses rather than installing into whatever cwd is.
#[test]
fn outside_a_worktree_it_refuses() {
    let f = Fixture::new("nogit");
    let nowhere = f.root.join("plain-dir");
    boss_testing::create_dir(&nowhere);

    let (rc, out) = f.run(&nowhere, &["webcheck"], &[]);
    assert_eq!(rc, 2, "must refuse with exit 2: {out}");
    assert!(out.contains("not in a git worktree"), "must say why: {out}");
    assert!(!out.contains("argv="), "the command must not run: {out}");
    assert!(
        f.bun_log().is_empty(),
        "nothing may be installed outside a worktree"
    );
}
