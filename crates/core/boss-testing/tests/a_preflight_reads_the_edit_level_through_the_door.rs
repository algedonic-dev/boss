//! The dev pod's pre-flight reads the instance's edit level THROUGH THE
//! DOOR and hands it to the one lint that needs it, so that lint sends
//! no tokenless request of its own. `infra/gate.sh` is RUN against a
//! synthetic tree, so the property is one the pre-flight has, not one
//! its comments claim.
//!
//! MEASURED (backlog 37742794 R2, part of urgent 2710c8fc). A gate's
//! launcher hands the level to its Job as `BOSS_EDIT_LEVEL_ANSWER`
//! (backlog 934ccad1), and the conductor hands it to the consist worker
//! (car cee7d4bb). A pre-flight has no launcher, so
//! `a-car-stays-under-the-edit-level` read `GET /api/tenant/edit-level`
//! itself, with no machine token, from the dev container: 15
//! would-refuse facts in the 24 h to 2026-10-09T05:30Z, and 7 more from
//! the dev pod (10.244.3.189) between 06:00Z and 13:48Z. Each keeps the
//! machine gate's clean window dirty, so the enforce flip cannot read
//! clean, and under enforce each is refused (the lint's exit 3, a
//! warning in `--quick` and a refusal in `--lint`). The dev container
//! already holds the token for its doors (infra/dev/machine-token-dir),
//! so the pre-flight asks the door once — `boss-api`, which stamps it —
//! and hands the answer exactly as a launcher does.
//!
//! WHAT THIS PINS. The door is asked only in a pre-flight mode, only
//! when nothing was handed, only when the roster carries the lint that
//! reads the level, only when the run names no instance of its own
//! (`BOSS_JOBS_URL`) and declares no estate state (`BOSS_ESTATE`) —
//! the door stamps loopback, so a caller-chosen address never gets the
//! token from a pre-flight — and only when a door is on PATH. Anything
//! else, and any door answer that is not the edit-level object, hands
//! nothing and the lint reads the level itself, as it always did.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The lint the door's answer is for. Planted as a stub that reports
/// what it was handed, because what is under test is gate.sh's hand-off,
/// not the lint's judgement (pinned elsewhere:
/// a_lint_that_reads_the_api_waits_out_a_roll.rs).
const EDIT_LEVEL_LINT: &str = "infra/lint/a-car-stays-under-the-edit-level.sh";
/// The door's own answer, as the live jobs API gave it on 2026-10-09.
const DOOR_ANSWER: &str = r#"{"edit_level":null,"manifest":"/opt/boss/tenant/seeds/tenant.toml"}"#;

struct Tree {
    dir: PathBuf,
    tree: PathBuf,
}

/// How the planted door answers.
#[derive(Clone, Copy)]
enum Door {
    /// No `boss-api` anywhere on PATH.
    Absent,
    /// The edit-level object, exit 0.
    Answers,
    /// `HTTP:503`, exit 1 — the door's own non-2xx shape.
    Refuses,
    /// Exit 0 with something that is not the edit-level object.
    Junk,
    /// Exit 0 with an empty body — the input jq-1.6's `-e` reads as a
    /// pass (backlog d96e38ab).
    Silent,
}

impl Tree {
    fn new(tag: &str, with_lint: bool) -> Tree {
        let dir = boss_testing::scratch_dir(&format!("preflight-door-{tag}"));
        let tree = dir.join("tree");
        boss_testing::create_dir(&tree.join("infra/lint/lib"));
        boss_testing::copy_gate_sh(&tree);
        boss_testing::write_file(
            &tree.join("infra/lint/workspace-declares-what-it-runs.sh"),
            "#!/usr/bin/env bash\nexit 0\n",
        );
        if with_lint {
            boss_testing::write_exec(
                &tree.join(EDIT_LEVEL_LINT),
                &format!(
                    "#!/usr/bin/env bash\nprintf '%s' \"${{BOSS_EDIT_LEVEL_ANSWER:-unset}}\" > '{}'\nexit 0\n",
                    dir.join("lint-saw").display()
                ),
            );
        }
        git(&tree, &["init", "-q", "-b", "main"]);
        git(&tree, &["add", "."]);
        git(
            &tree,
            &[
                "-c",
                "user.email=preflight-door@test",
                "-c",
                "user.name=preflight-door",
                "commit",
                "-q",
                "-m",
                "the gate under test",
            ],
        );
        let bin = dir.join("bin");
        boss_testing::create_dir(&bin);
        for tool in ["cargo", "bun"] {
            boss_testing::write_exec(&bin.join(tool), "#!/usr/bin/env bash\nexit 0\n");
        }
        for web in ["apps/web", "apps/simulator", "libs/web-kit"] {
            boss_testing::create_dir(&tree.join(web));
        }
        boss_testing::write_exec(
            &dir.join("df"),
            "#!/usr/bin/env bash\n\
             echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n\
             echo '/dev/fake 1 1 943718400 1% /'\n",
        );
        Tree { dir, tree }
    }

    fn door(&self, door: Door) -> &Tree {
        let calls = self.dir.join("door-calls");
        let (out, err, code) = match door {
            Door::Absent => return self,
            Door::Answers => (DOOR_ANSWER, "HTTP:200", 0),
            Door::Refuses => ("", "HTTP:503", 1),
            Door::Junk => ("<html>not the door</html>", "HTTP:200", 0),
            Door::Silent => ("", "HTTP:200", 0),
        };
        boss_testing::write_exec(
            &self.dir.join("bin/boss-api"),
            &format!(
                "#!/usr/bin/env bash\n\
                 echo \"$* wait=${{BOSS_SOR_WAIT_SECONDS:-unset}}\" >> '{}'\n\
                 printf '%s' '{out}'\n\
                 echo '{err}' >&2\n\
                 exit {code}\n",
                calls.display()
            ),
        );
        self
    }

    /// Every call the planted door received, one per line.
    fn door_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("door-calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// What the planted lint was handed, or `None` when it never ran.
    fn lint_saw(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("lint-saw")).ok()
    }

    /// gate.sh with `args`, `env` added and every PATH directory that
    /// carries a real `boss-api` left out — the dev pod's own door must
    /// never answer a test.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let outer = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
        let path = std::iter::once(self.dir.join("bin").display().to_string())
            .chain(
                outer
                    .split(':')
                    .filter(|d| !d.is_empty() && !Path::new(d).join("boss-api").exists())
                    .map(str::to_string),
            )
            .collect::<Vec<_>>()
            .join(":");
        let mut cmd = Command::new("bash");
        cmd.arg(self.tree.join("infra/gate.sh"))
            .args(args)
            .current_dir(&self.tree)
            .env("PATH", path)
            .env("BOSS_GATE_DF_CMD", self.dir.join("df"))
            .env("BOSS_GATE_MIN_FREE_GB", "12")
            .env("BOSS_GATE_RECEIPT", self.dir.join("receipt.json"))
            .env("BOSS_GATE_TRUNK", "HEAD")
            .env("GIT_CEILING_DIRECTORIES", "")
            .env_remove("BOSS_JOBS_URL")
            .env_remove("BOSS_ESTATE")
            .env_remove("BOSS_EDIT_LEVEL_ANSWER")
            .env_remove("BOSS_SOR_WAIT_SECONDS");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output()
            .unwrap_or_else(|e| panic!("run gate.sh {args:?} in {}: {e}", self.tree.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// THE DEFECT, BOTH PRE-FLIGHT MODES. With a door on PATH and nothing
/// handed, the door is asked ONCE for the edit level, with the lint's
/// own sixty-second roll window, and the lint is handed the door's
/// answer whole — so it makes no request of its own. The pre-flight
/// says which reader answered.
#[test]
fn a_preflight_hands_the_lint_the_doors_answer() {
    for mode in ["--quick", "--lint"] {
        let tree = Tree::new(&format!("answers{mode}"), true);
        tree.door(Door::Answers);
        let out = tree.run(&[mode], &[]);
        let said = text(&out);
        assert!(out.status.success(), "{mode}: {said}");
        assert_eq!(
            tree.door_calls(),
            vec!["GET /api/tenant/edit-level wait=60".to_string()],
            "{mode}: the door is asked once, for the level, with the lint's window: {said}"
        );
        assert_eq!(
            tree.lint_saw().as_deref(),
            Some(DOOR_ANSWER),
            "{mode}: the lint is handed the door's answer whole: {said}"
        );
        assert!(
            said.contains("read through the door (boss-api,"),
            "{mode}: the pre-flight says which reader answered: {said}"
        );
    }
}

/// An operator's own roll window is the door's too.
#[test]
fn the_operators_wait_window_reaches_the_door() {
    let tree = Tree::new("window", true);
    tree.door(Door::Answers);
    let out = tree.run(&["--quick"], &[("BOSS_SOR_WAIT_SECONDS", "0")]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(
        tree.door_calls(),
        vec!["GET /api/tenant/edit-level wait=0".to_string()]
    );
}

/// A door that refuses, or answers something that is not the
/// edit-level object, hands NOTHING: the lint reads the level itself,
/// as it always did, and the pre-flight says why in one line. Never a
/// level guessed, never the junk handed on.
#[test]
fn a_door_that_cannot_answer_hands_nothing() {
    for (tag, door, why) in [
        ("refuses", Door::Refuses, "HTTP:503"),
        ("junk", Door::Junk, "not the edit-level object"),
        ("silent", Door::Silent, "not the edit-level object"),
    ] {
        let tree = Tree::new(tag, true);
        tree.door(door);
        let out = tree.run(&["--quick"], &[]);
        let said = text(&out);
        assert!(out.status.success(), "{tag}: {said}");
        assert_eq!(tree.door_calls().len(), 1, "{tag}: {said}");
        assert_eq!(tree.lint_saw().as_deref(), Some("unset"), "{tag}: {said}");
        assert!(
            said.contains("the lint reads it itself") && said.contains(why),
            "{tag}: the pre-flight names the door's failure: {said}"
        );
    }
}

/// Where the door must NOT be asked: the gate proper (a car's branch,
/// handed its level by its launcher), a run already handed the level, a
/// run that names its own instance or declares its estate state, a
/// roster without the lint, and a box with no door. Each leaves the
/// lint exactly as it was before this car.
#[test]
fn the_door_is_asked_only_by_a_preflight_that_needs_it() {
    let handed = r#"{"edit_level":"core"}"#;
    let cases: [(&str, &[&str], &[(&str, &str)], bool, Door, Option<&str>); 6] = [
        ("gate-proper", &[], &[], true, Door::Answers, Some("unset")),
        (
            "handed",
            &["--quick"],
            &[("BOSS_EDIT_LEVEL_ANSWER", handed)],
            true,
            Door::Answers,
            Some(handed),
        ),
        (
            "named-instance",
            &["--quick"],
            &[("BOSS_JOBS_URL", "http://[::1]:9")],
            true,
            Door::Answers,
            Some("unset"),
        ),
        (
            "estate-declared",
            &["--quick"],
            &[("BOSS_ESTATE", "none")],
            true,
            Door::Answers,
            Some("unset"),
        ),
        ("no-lint", &["--quick"], &[], false, Door::Answers, None),
        (
            "no-door",
            &["--quick"],
            &[],
            true,
            Door::Absent,
            Some("unset"),
        ),
    ];
    for (tag, args, env, with_lint, door, saw) in cases {
        let tree = Tree::new(tag, with_lint);
        tree.door(door);
        let out = tree.run(args, env);
        let said = text(&out);
        assert!(
            tree.door_calls().is_empty(),
            "{tag}: the door was asked: {:?}\n{said}",
            tree.door_calls()
        );
        assert_eq!(tree.lint_saw().as_deref(), saw, "{tag}: {said}");
    }
}
