//! A file a change ADDS does not use a retired executor word.
//!
//! The factory taxonomy (design 4348b857, folded into docs/design/factory-taxonomy-v2.md)
//! retired `mover`, `territory` and `machinery`, and David replaced
//! `machine` as the deterministic executor kind with `automaton` on
//! 2026-11-02. infra/lint/factory-vocabulary.sh is the taxonomy's
//! mechanism M5: it refuses those words in files ADDED against the trunk,
//! leaves older files as history, and lets `machine token`, `state
//! machine` and the machine door through. The station tree (T4, backlog
//! 3c5115ee; docs/design/factory-taxonomy-v2.md) adds four words refused
//! in added CODE under `crates/` and `apps/` only: `equipment` as a
//! station kind, `bay`, `automation` as a group, and a literal
//! `/algedonic/…` address. Registry rows and docs keep v1's words while the
//! bundle is additive. Pinned on fixture repos this process owns — the
//! lint and its libraries copied out of the working tree — for each side
//! the lint must tell apart, then on the real tree, which must be clean.
//!
//! tree-wide pin — it reads every file the change adds, which no
//! changed-file map attributes to this crate.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINT: &str = "factory-vocabulary";

fn copy(from: &Path, to: &Path) {
    boss_testing::create_dir(to.parent().expect("parent"));
    std::fs::copy(from, to).unwrap_or_else(|e| panic!("copy {}: {e}", from.display()));
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// A repo whose base commit is on branch `base`; `origin/main` names
    /// that commit when `with_trunk`, and the branch `car` is checked out
    /// on top of it for the change under test.
    fn new(case: &str, base_files: &[(&str, &str)], with_trunk: bool) -> Self {
        let dir = boss_testing::scratch_dir(&format!("factory-vocabulary-{case}"));
        let root = repo_root();
        copy(
            &root.join("infra/lint").join(format!("{LINT}.sh")),
            &dir.join("infra/lint").join(format!("{LINT}.sh")),
        );
        for lib in [
            "git-answer.sh",
            "trunk-ref.sh",
            "public-mirror.sh",
            "scanned.sh",
        ] {
            copy(
                &root.join("infra/lint/lib").join(lib),
                &dir.join("infra/lint/lib").join(lib),
            );
        }
        let me = Fixture { dir };
        me.git(&["init", "-q", "-b", "base", "."]);
        me.git(&["config", "user.email", "t@t"]);
        me.git(&["config", "user.name", "t"]);
        for (rel, body) in base_files {
            me.write(rel, body);
        }
        me.git(&["add", "-A"]);
        me.git(&["commit", "-q", "-m", "base"]);
        if with_trunk {
            me.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        }
        me.git(&["checkout", "-q", "-b", "car"]);
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
        let path = self.dir.join(rel);
        boss_testing::create_dir(path.parent().expect("parent"));
        boss_testing::write_file(&path, body);
    }

    /// Commit everything on `car` and run the lint: (exit code, stdout +
    /// stderr).
    fn lint(&self) -> (i32, String) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "car"]);
        let out = boss_testing::git_config_isolated(&mut Command::new("bash"))
            .arg(self.dir.join("infra/lint").join(format!("{LINT}.sh")))
            .current_dir(&self.dir)
            // The public mirror runs this suite with BOSS_TRUNK=none
            // declared (backlog 2d28df78), under which the lint classes no
            // file as added; these cases are about a car that has a trunk.
            .env_remove("BOSS_TRUNK")
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
fn an_added_file_using_a_retired_word_is_refused_and_named() {
    let f = Fixture::new("refused", &[("README.md", "base\n")], true);
    f.write("crates/a.rs", "let mover = Mover::new();\n");
    f.write("apps/b.ts", "// the Territory panel draws IT's regions\n");
    f.write("docs/c.md", "The deploy machinery shares a pool.\n");
    f.write("crates/d.rs", "let kind = ExecutorKind::Machine;\n");
    f.write("infra/e.toml", "kind = \"machine\"\n");
    f.write("docs/f.md", "A machine executor signs the claim.\n");
    f.write(
        "crates/g.sql",
        "UPDATE agents SET executor_kind = 'machine';\n",
    );
    let (code, out) = f.lint();
    assert_eq!(code, 1, "{out}");
    for named in [
        "crates/a.rs:1:",
        "apps/b.ts:1:",
        "docs/c.md:1:",
        "crates/d.rs:1:",
        "infra/e.toml:1:",
        "docs/f.md:1:",
        "crates/g.sql:1:",
    ] {
        assert!(out.contains(named), "{named} must be named: {out}");
    }
    assert!(out.contains("automaton"), "the remedy is named: {out}");
    assert!(
        out.contains("7 added"),
        "the scan says what it classed as added: {out}"
    );
}

#[test]
fn the_other_meanings_of_machine_pass_in_an_added_file() {
    let f = Fixture::new("legal", &[("README.md", "base\n")], true);
    f.write(
        "crates/h.rs",
        "// the machine token opens the machine door; the state machine is BOSS\n\
         let token = machine_token(); let door = MachineToken::door();\n",
    );
    // v1's station words are legal outside code while the station tree is
    // additive: a registry row still carries them for the build a rollback
    // lands on, and a doc quotes them as history.
    f.write(
        "infra/platform/stations/gates.toml",
        "kind = \"equipment\" # operated by an automaton\nbays = 2\n",
    );
    f.write("docs/g.md", "A gate is an equipment station with bays.\n");
    let (code, out) = f.lint();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("3 added"), "{out}");
    assert!(out.contains("1 of them code"), "{out}");
    assert!(
        out.contains("scanned"),
        "a scanning lint says what it scanned: {out}"
    );
}

/// Station tree T4 (backlog 3c5115ee): an added CODE file may not use
/// `equipment` as a station kind, `bay`, `automation` as a group, or a
/// literal `/algedonic/…` address, and each is named by file and line.
#[test]
fn an_added_code_file_using_a_v2_retired_word_is_refused_and_named() {
    let f = Fixture::new("v2-refused", &[("README.md", "base\n")], true);
    f.write("crates/a.toml", "kind = \"equipment\"\n");
    f.write("crates/b.rs", "let k = StationKind::Equipment;\n");
    f.write(
        "crates/c.rs",
        "// the gates station has two equipment stations beside it\n",
    );
    f.write("apps/d.ts", "const bays = 2;\n");
    f.write("apps/e.ts", "const lane = 'gate-bay-0';\n");
    f.write("crates/f.rs", "if lane == \"automation\" { draw() }\n");
    f.write("apps/g.ts", "// the automation group sits at the root\n");
    f.write("apps/h.ts", "const family = 'automation';\n");
    f.write(
        "crates/i.rs",
        "let at = \"/algedonic/engineering/gates\";\n",
    );
    f.write("apps/j.ts", "const root = '/algedonic';\n");
    let (code, out) = f.lint();
    assert_eq!(code, 1, "{out}");
    for named in [
        "crates/a.toml:1:",
        "crates/b.rs:1:",
        "crates/c.rs:1:",
        "apps/d.ts:1:",
        "apps/e.ts:1:",
        "crates/f.rs:1:",
        "apps/g.ts:1:",
        "apps/h.ts:1:",
        "crates/i.rs:1:",
        "apps/j.ts:1:",
    ] {
        assert!(out.contains(named), "{named} must be named: {out}");
    }
    assert!(
        out.contains("read the station from its row"),
        "the remedy is named: {out}"
    );
    assert!(out.contains("10 of them code"), "{out}");
}

/// The neighbours each v2 rule must let through in added code: the
/// `automation:` actor prefix, an identifier with `bay` inside it, the
/// noun `equipment`, and a path or URL that merely contains `algedonic`.
#[test]
fn the_neighbours_of_the_v2_words_pass_in_added_code() {
    let f = Fixture::new("v2-legal", &[("README.md", "base\n")], true);
    f.write(
        "crates/k.rs",
        "let id = \"automation:gate-runner\"; // an automaton's id, not a group\n\
         let n = COMPILED_GATE_BAYS + bay_count; // identifiers, not the word\n\
         // the forge's equipment is Operations' to keep\n\
         let repo = \"https://forge.example/algedonic-dev/boss\";\n\
         let file = \"/work/algedonic/handoff.md\";\n\
         let automations = load(); // a plural noun, not quoted\n",
    );
    let (code, out) = f.lint();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1 of them code"), "{out}");
}

#[test]
fn a_file_older_than_the_cutover_is_history_even_when_edited() {
    let f = Fixture::new(
        "history",
        &[
            ("README.md", "base\n"),
            (
                "crates/old.rs",
                "let mover = 1; // executor_kind = 'machine'\n",
            ),
        ],
        true,
    );
    f.write(
        "crates/old.rs",
        "let mover = 1; // executor_kind = 'machine'\nlet territory = 2;\n",
    );
    let (code, out) = f.lint();
    assert_eq!(
        code, 0,
        "an edited old file is history, not an added file: {out}"
    );
    assert!(out.contains("0 added"), "{out}");
}

#[test]
fn a_moved_file_is_an_added_file_and_is_cleaned_when_it_moves() {
    let f = Fixture::new(
        "moved",
        &[
            ("README.md", "base\n"),
            ("crates/old.rs", "let mover = 1;\n"),
        ],
        true,
    );
    f.git(&["mv", "crates/old.rs", "crates/new.rs"]);
    let (code, out) = f.lint();
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("crates/new.rs:1:"),
        "the moved file is named under its new path: {out}"
    );
}

#[test]
fn no_trunk_ref_skips_out_loud_rather_than_claiming_clean() {
    let f = Fixture::new("no-trunk", &[("README.md", "base\n")], false);
    f.write("crates/a.rs", "let mover = 1;\n");
    let (code, out) = f.lint();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("no trunk ref"), "{out}");
    assert!(out.contains("skipped"), "{out}");
    assert!(!out.contains("ok —"), "a skip is not an ok: {out}");
}

/// The live tree: nothing this checkout adds against the trunk uses a
/// retired word, or the lint names the file.
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
