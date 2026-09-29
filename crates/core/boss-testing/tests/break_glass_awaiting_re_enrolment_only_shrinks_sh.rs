//! `infra/lint/break-glass-awaiting-re-enrolment-only-shrinks.sh` is RUN,
//! not read — against fixture repositories with a trunk, so the ratchet
//! is shown to refuse a planted row rather than assumed to.
//!
//! THE HOLE (backlog edf403aa, from the adversarial review of car
//! 1c4c100a, 2026-09-28). boss-gateway's
//! `the_break_glass_records_answer_the_door_they_serve` holds every
//! committed break-glass record to the door `BOSS_PUBLIC_URL` names,
//! except the rows of `infra/lint/break-glass-awaiting-re-enrolment.txt`
//! — records KNOWN to be bound to the door that was abandoned on
//! 2026-09-16 (793a3c33). That test fails when a listed record is
//! replaced, so the list cannot outlive its reason; nothing stopped it
//! GROWING. When the door next moves, a car that also adds `(primary,
//! boss.algedonic.dev)` and `(backup, boss.algedonic.dev)` satisfies
//! both of its checks — each row differs from the new door, each names
//! a real record — and goes green while no key opens the door. The lint
//! refuses any row the trunk does not already carry.
//!
//! What is pinned: a row the trunk carries is clean; deleting a row is
//! clean (the re-enrolment car's whole diff); a PLANTED ADDED ROW on the
//! real file is refused by name, committed or merely dirty; rewording a
//! row's reason is not an addition; a file the trunk once held and
//! deleted gives no fresh start; the car that first introduces the file
//! passes and says so; and a git that cannot answer exits 3, never
//! `clean`.

use boss_testing::{git_config_isolated, repo_root, scratch};
use std::path::PathBuf;
use std::process::Command;

const LINT: &str = "infra/lint/break-glass-awaiting-re-enrolment-only-shrinks.sh";
const LIST: &str = "infra/lint/break-glass-awaiting-re-enrolment.txt";

/// The trunk every synthetic fixture starts from: two rows.
const TWO_ROWS: &str = "\
# label  rp_id  cutover  incident
primary  playground.example  793a3c33  1c4c100a
backup   playground.example  793a3c33  1c4c100a
";

struct Repo(PathBuf);

impl Repo {
    /// A git repo with the lint, its libraries and nothing else, on
    /// `main`. Nothing is committed yet.
    fn empty(tag: &str) -> Repo {
        let root = scratch::scratch_dir(&format!("break-glass-only-shrinks-{tag}"));
        boss_testing::copy_lint_libs(&root);
        let body = std::fs::read_to_string(repo_root().join(LINT))
            .unwrap_or_else(|e| panic!("read {LINT}: {e}"));
        scratch::write_exec(&root.join(LINT), &body);
        let repo = Repo(root);
        repo.git(&["init", "-q", "-b", "main", "."]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        repo
    }

    /// A trunk carrying `list`, and a car branched from it.
    fn with_trunk(tag: &str, list: &str) -> Repo {
        let repo = Repo::empty(tag);
        repo.write(list);
        repo.commit("trunk");
        repo.git(&["checkout", "-q", "-b", "car"]);
        repo
    }

    fn git(&self, args: &[&str]) {
        let out = git_config_isolated(&mut Command::new("git"))
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "fixture git {args:?} failed in {}: {}{}",
            self.0.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, list: &str) {
        scratch::write_file(&self.0.join(LIST), list);
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", msg]);
    }

    fn run(&self) -> (i32, String) {
        self.run_with(&[])
    }

    fn run_with(&self, env: &[(&str, &str)]) -> (i32, String) {
        let mut cmd = Command::new("bash");
        git_config_isolated(&mut cmd)
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            // The fixture has only `main`; naming it keeps the test
            // independent of any remote-tracking ref.
            .env("BOSS_TRUNK_REF", "main");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap_or_else(|e| panic!("spawn {LINT}: {e}"));
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn assert_clean(code: i32, out: &str, why: &str) {
    assert_eq!(code, 0, "{why}: the lint must pass:\n{out}");
    assert!(
        out.contains("break-glass-awaiting-re-enrolment-only-shrinks: clean"),
        "{why}: a pass says `clean`:\n{out}"
    );
}

#[test]
fn a_row_the_trunk_carries_is_clean() {
    let repo = Repo::with_trunk("unchanged", TWO_ROWS);
    let (code, out) = repo.run();
    assert_clean(code, &out, "an unchanged list");
}

#[test]
fn deleting_a_row_is_clean() {
    // The re-enrolment car's whole diff to this file: one line gone.
    let repo = Repo::with_trunk("delete", TWO_ROWS);
    repo.write(
        "# label  rp_id  cutover  incident\nbackup   playground.example  793a3c33  1c4c100a\n",
    );
    repo.commit("primary re-enrolled");
    let (code, out) = repo.run();
    assert_clean(code, &out, "a deleted row");
}

/// THE PIN, red with a planted row: the next cutover's waiver, appended
/// to the REAL file, so the fixture reads the shape the tree commits.
#[test]
fn a_planted_row_on_the_real_list_is_refused_by_name() {
    let real = std::fs::read_to_string(repo_root().join(LIST))
        .unwrap_or_else(|e| panic!("read {LIST}: {e}"));
    let repo = Repo::with_trunk("planted", &real);
    repo.write(&format!(
        "{real}primary  boss.algedonic.dev  0123abcd  deadbeef\n"
    ));
    repo.commit("waive the next cutover");
    let (code, out) = repo.run();
    assert_eq!(code, 1, "an added row must be refused:\n{out}");
    assert!(
        out.contains("primary") && out.contains("boss.algedonic.dev"),
        "the refusal names the row it refused:\n{out}"
    );
    assert!(
        !out.contains("only-shrinks: clean"),
        "a refusal never also says clean:\n{out}"
    );
}

#[test]
fn an_uncommitted_row_is_refused_before_the_commit() {
    // The pre-flight runs before a builder commits; it must see the
    // same verdict the gate will.
    let repo = Repo::with_trunk("dirty", TWO_ROWS);
    repo.write(&format!(
        "{TWO_ROWS}backup  other.example  0123abcd  deadbeef\n"
    ));
    let (code, out) = repo.run();
    assert_eq!(code, 1, "a dirty added row must be refused:\n{out}");
    assert!(out.contains("other.example"), "names the row:\n{out}");
}

#[test]
fn rewording_a_rows_reason_is_not_an_addition() {
    // A row is the record it excuses — label and relying party. Its
    // justification may be corrected without reading as a new waiver.
    let repo = Repo::with_trunk("reword", TWO_ROWS);
    repo.write(
        "# label  rp_id  cutover  incident\n\
         primary  playground.example  793a3c33d  1c4c100a\n\
         backup   playground.example  793a3c33d  1c4c100a\n",
    );
    repo.commit("longer sha");
    let (code, out) = repo.run();
    assert_clean(code, &out, "a reworded reason");
}

#[test]
fn a_list_the_trunk_deleted_gives_no_fresh_start() {
    let repo = Repo::empty("redeleted");
    repo.write(TWO_ROWS);
    repo.commit("list");
    repo.git(&["rm", "-q", LIST]);
    repo.commit("list retired");
    repo.git(&["checkout", "-q", "-b", "car"]);
    repo.write(TWO_ROWS);
    repo.commit("list back");
    let (code, out) = repo.run();
    assert_eq!(
        code, 1,
        "a list the trunk once held and deleted has an EMPTY baseline, so every \
         row is an addition:\n{out}"
    );
}

#[test]
fn the_car_that_introduces_the_list_passes_and_says_so() {
    let repo = Repo::empty("introduce");
    scratch::write_file(&repo.0.join("README"), "x\n");
    repo.commit("trunk");
    repo.git(&["checkout", "-q", "-b", "car"]);
    repo.write(TWO_ROWS);
    repo.commit("introduce the list");
    let (code, out) = repo.run();
    assert_clean(code, &out, "the introducing car");
    assert!(
        out.contains("introduced"),
        "the pass says the list is new on this branch, not that it was compared:\n{out}"
    );
}

#[test]
fn a_git_that_cannot_answer_is_a_refusal_not_a_verdict() {
    let repo = Repo::with_trunk("foreign", TWO_ROWS);
    let (code, out) = repo.run_with(&[("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1")]);
    assert_eq!(code, 3, "git refusing must exit 3:\n{out}");
    // The verdict line, not the word: a refusal's own remediation text
    // explains that `clean` cannot be claimed.
    assert!(
        !out.contains("only-shrinks: clean"),
        "never clean on a tree nothing read:\n{out}"
    );
}
