//! A decision that reads `git diff --name-only` says `--no-renames`.
//!
//! Porcelain `git diff` detects renames by default, so a file moved across
//! a boundary is listed only under its NEW path. On 2026-11-02 two cars by
//! one author carried that blind spot in one day: the dock's `read_base`
//! listed main's changes that way, so a gate input moved out of
//! infra/gate-runner/ classed `Unchanged` and the car boarded without the
//! re-gate it was owed (item 38fc5084), and the K2 car's freshness read
//! had the same shape. A rule two cars got wrong is a lint, not a rule
//! (CLAUDE.md §9a): infra/lint/a-name-only-diff-lists-the-old-path.sh.
//!
//! Pinned on a fixture repo this process owns — the lint and its library
//! copied out of the working tree — with the four shapes that must be told
//! apart, then on the real tree, which must be clean.
//!
//! tree-wide pin — it reads infra/lint and every boss-cli source through
//! the lint, which no changed-file map attributes to this crate.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINT: &str = "a-name-only-diff-lists-the-old-path";

fn copy(from: &Path, to: &Path) {
    boss_testing::create_dir(to.parent().expect("parent"));
    std::fs::copy(from, to).unwrap_or_else(|e| panic!("copy {}: {e}", from.display()));
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let dir = boss_testing::scratch_dir(&format!("name-only-diff-{case}"));
        let root = repo_root();
        copy(
            &root.join("infra/lint").join(format!("{LINT}.sh")),
            &dir.join("infra/lint").join(format!("{LINT}.sh")),
        );
        for lib in ["git-answer.sh", "pattern-scan.sh", "scanned.sh"] {
            copy(
                &root.join("infra/lint/lib").join(lib),
                &dir.join("infra/lint/lib").join(lib),
            );
        }
        boss_testing::create_dir(&dir.join("crates/orchestrators/boss-cli/src"));
        boss_testing::create_dir(&dir.join("infra/forge"));
        let me = Fixture { dir };
        me.git(&["init", "-q", "-b", "main", "."]);
        me.git(&["config", "user.email", "t@t"]);
        me.git(&["config", "user.name", "t"]);
        me
    }

    fn git(&self, args: &[&str]) {
        let out = boss_testing::git_config_isolated(&mut Command::new("git"))
            .args(args)
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, body: &str) {
        boss_testing::write_file(&self.dir.join(rel), body);
    }

    /// Commit everything and run the lint: (exit code, stdout + stderr).
    fn lint(&self) -> (i32, String) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "fixture"]);
        let out = Command::new("bash")
            .arg(self.dir.join("infra/lint").join(format!("{LINT}.sh")))
            .current_dir(&self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run the lint");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }
}

#[test]
fn a_name_only_diff_without_no_renames_is_refused_and_named() {
    let f = Fixture::new("refused");
    f.write(
        "crates/orchestrators/boss-cli/src/a.rs",
        "fn a() { git(&[\"diff\", \"--name-only\", base, tip]); }\n",
    );
    f.write(
        "infra/forge/b.sh",
        "#!/usr/bin/env bash\nchanged=$(git diff --name-status \"$base\" \"$head\")\n",
    );
    f.write(
        "crates/orchestrators/boss-cli/src/c.rs",
        "fn c() {\n    let args = vec![\n        \"git\",\n        \"log\",\n        \"-1\",\n        \"--name-only\",\n        \"origin/main\",\n    ];\n}\n",
    );
    let (code, out) = f.lint();
    assert_eq!(code, 1, "{out}");
    for named in [
        "crates/orchestrators/boss-cli/src/a.rs:1:",
        "infra/forge/b.sh:2:",
        "crates/orchestrators/boss-cli/src/c.rs:6:",
    ] {
        assert!(out.contains(named), "{named} must be named: {out}");
    }
    assert!(out.contains("--no-renames"), "the remedy is named: {out}");
}

#[test]
fn no_renames_plumbing_and_prose_are_clean() {
    let f = Fixture::new("clean");
    f.write(
        "crates/orchestrators/boss-cli/src/a.rs",
        "fn a() { git(&[\"diff\", \"--no-renames\", \"--name-only\", base, tip]); }\n\
         fn b() { git(&[\"diff-tree\", \"-r\", \"--name-only\", a, b]); }\n\
         // the consist's changed files (`git diff --name-only base..head`) are read\n\
         fn c() {\n    let args = vec![\n        \"git\",\n        \"log\",\n        \"-1\",\n        \"--no-renames\",\n        \"--name-only\",\n    ];\n}\n",
    );
    f.write(
        "infra/forge/b.sh",
        "#!/usr/bin/env bash\n# git diff --name-only is how it used to read the tree\nchanged=$(git diff --no-renames --name-only \"$base\" \"$head\")\nfiles=$(git diff-index --cached --name-only HEAD)\n",
    );
    let (code, out) = f.lint();
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("scanned"),
        "a scanning lint says what it scanned: {out}"
    );
}

/// The live tree: every decision under boss-cli and infra says
/// --no-renames, or the lint names the one that does not.
#[test]
fn the_tree_is_clean() {
    let out = Command::new("bash")
        .arg(repo_root().join("infra/lint").join(format!("{LINT}.sh")))
        .current_dir(repo_root())
        .output()
        .expect("run the lint");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
