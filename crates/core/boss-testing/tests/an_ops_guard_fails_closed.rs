//! `infra/lint/an-ops-guard-fails-closed.sh` is RUN, not read — against
//! synthetic trees and against the repository, so every property below is
//! one the lint actually has.
//!
//! THE CLASS (design 3036296f mechanism B, David 2026-09-27; backlog
//! fdbb447e). The day's reds and review holds were mostly one defect
//! class — silence that reads as success — and in the host scripts it has
//! three spellings: a read whose failure is swallowed into an empty guard
//! variable (`x=$(cmd || true)`), a failed read recorded as a plausible
//! number (`$(cat f || echo 0)`), and a MUTATING verb's script run
//! without `pipefail`, where a pipeline's failed producer is invisible.
//! The adversarial review caught every instance on the host-mutating
//! verbs; the gate caught none. This lint is the gate's half.
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the classifier still tells each shape from its safe twin"; this
//! file owns the VERDICT on a tree — which files are read (infra/forge,
//! infra/ops, and every script a MUTATING verb names wherever it lives),
//! that a finding names file and line, that an allowance excuses exactly
//! its count and must carry a reason, and that a machine without jq says
//! it cannot answer rather than certifying the tree.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LINT: &str = "infra/lint/an-ops-guard-fails-closed.sh";
const ALLOW: &str = "infra/lint/an-ops-guard-fails-closed-allow.txt";

/// The two files the lint runs to derive the MUTATING roster — the same
/// derivation the ops runner uses (infra/ops/verbs/README.md), copied
/// from the working tree so the fixture exercises this branch.
const ROSTER_HELPERS: [&str; 2] = ["infra/ops/verbs-allowlist.sh", "infra/lib/jq.sh"];

const MUTATING_ABOUT: &str = "MUTATING: a fixture verb; authorized by David for this test";

struct Tree(PathBuf);

impl Tree {
    /// A git repository holding the lint, its libraries, the roster
    /// derivation and an EMPTY allowlist — so a fixture is judged by the
    /// rule alone unless a test writes an allowance.
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("an-ops-guard-fails-closed-{tag}"));
        boss_testing::copy_lint_libs(&root);
        for rel in std::iter::once(LINT).chain(ROSTER_HELPERS) {
            let body = std::fs::read_to_string(repo_root().join(rel))
                .unwrap_or_else(|e| panic!("read {rel}: {e}"));
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                scratch::create_dir(parent);
            }
            scratch::write_exec(&path, &body);
        }
        let tree = Tree(root);
        git(&tree.0, &["init", "-q", "-b", "main"]);
        tree.file(ALLOW, "# fixture allowlist: empty\n");
        git(&tree.0, &["add", "."]);
        tree
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            scratch::create_dir(parent);
        }
        scratch::write_file(&path, body);
        git(&self.0, &["add", rel]);
        self
    }

    /// A verb file under infra/ops/verbs whose argv[0] is `script`.
    fn verb(&self, name: &str, about: &str, script: &str) -> &Tree {
        self.file(
            &format!("infra/ops/verbs/{name}.json"),
            &format!(
                "{{\"about\": \"{about}\", \"hosts\": [\"forge\"], \"argv\": [\"{script}\"], \"params\": []}}\n"
            ),
        )
    }

    fn run(&self) -> Output {
        self.run_with(&[])
    }

    fn run_with(&self, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg(self.0.join(LINT))
            .current_dir(&self.0)
            .env("GIT_CEILING_DIRECTORIES", "");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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

/// A MUTATING verb whose script is fail-closed in every way the lint
/// reads — the base most fixtures below add one defect to.
fn fail_closed_tree(tag: &str) -> Tree {
    let tree = Tree::new(tag);
    tree.verb("mut", MUTATING_ABOUT, "infra/forge/mut.sh");
    tree.file(
        "infra/forge/mut.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\n# a comment may quote x=$(y || true) and $(cat f || echo 0)\nn=$(grep -c . \"$1\") || [ $? -eq 1 ]\nrm -f \"$1.tmp\" || true\n",
    );
    tree
}

/// The classifier proves itself on every invocation and SAYS so.
#[test]
fn the_lint_proves_itself_on_every_invocation() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .arg("--self-test")
        .output()
        .expect("run the lint's self-test");
    assert!(
        out.status.success(),
        "the lint's --self-test must pass:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("self-test ok"), "{}", text(&out));
}

/// BEHAVIOUR 1 — a fail-closed tree is clean, says how many files it read
/// and how many MUTATING scripts it held to pipefail; the safe twins of
/// each refused shape pass: the grep-count idiom that tolerates exit 1
/// ONLY, a bare command's `|| true` (no guard variable is set), a
/// sentinel word rather than a number, a failure recorded in a flag, and
/// a READ-ONLY verb's script with no `set` line at all.
#[test]
fn a_fail_closed_tree_is_clean_and_counts_what_it_read() {
    let tree = fail_closed_tree("clean");
    tree.verb("ro", "read-only: prints disk usage", "infra/forge/ro.sh");
    tree.file("infra/forge/ro.sh", "#!/bin/sh\ndf -h\n");
    tree.file(
        "infra/ops/helper.sh",
        "#!/usr/bin/env bash\nset -uo pipefail\nlast=$(cat \"$1\" 2>/dev/null || echo none)\ndeclared=$(list_them) || unreadable=1\n",
    );
    tree.file("infra/forge/NOTES.md", "never write x=$(y || true)\n");
    let out = tree.run();
    let said = text(&out);
    assert!(
        out.status.success(),
        "a fail-closed tree must exit 0; got {:?}:\n{said}",
        out.status.code()
    );
    assert!(
        said.contains("an-ops-guard-fails-closed: scanned "),
        "the scanned line is the verdict on how much was read:\n{said}"
    );
    assert!(
        said.contains("1 MUTATING verb script(s)"),
        "the pipefail roster is the MUTATING verbs only, not the read-only one:\n{said}"
    );
}

/// BEHAVIOUR 2 — a read whose failure is swallowed into the variable it
/// sets is refused by file and line, in every spelling of the swallow;
/// a comment is prose and a failure recorded in ANOTHER variable is the
/// fail-closed idiom, so neither is named.
#[test]
fn a_swallowed_read_is_refused_by_file_and_line() {
    let tree = fail_closed_tree("swallowed");
    tree.file(
        "infra/forge/a.sh",
        "#!/usr/bin/env bash\nn=$(grep -c . \"$f\" || true)\nv=$(cmd) || :\nw=$(cmd) || w=\"\"\nu=$(cmd) || failed=1\n# z=$(cmd || true)\n",
    );
    let out = tree.run();
    let said = text(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a swallowed read is a violation:\n{said}"
    );
    for line in [
        "infra/forge/a.sh:2:",
        "infra/forge/a.sh:3:",
        "infra/forge/a.sh:4:",
    ] {
        assert!(said.contains(line), "{line} must be named:\n{said}");
    }
    for line in ["infra/forge/a.sh:5:", "infra/forge/a.sh:6:"] {
        assert!(
            !said.contains(line),
            "{line} is not a swallowed read:\n{said}"
        );
    }
    assert!(
        said.contains("swallowed-read"),
        "the rule is named:\n{said}"
    );
}

/// BEHAVIOUR 3 — a failed read recorded as a number (the `|| echo 0` the
/// design names, and its assignment spelling) is refused by file and
/// line.
#[test]
fn a_failed_read_recorded_as_a_number_is_refused() {
    let tree = fail_closed_tree("number");
    tree.file(
        "infra/ops/b.sh",
        "#!/usr/bin/env bash\ndark=$(( $(cat \"$S\" 2>/dev/null || echo 0) + 1 ))\nm=$(stat -c %Y \"$f\") || m=0\n",
    );
    let out = tree.run();
    let said = text(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(
        said.contains("infra/ops/b.sh:2:"),
        "|| echo 0 is named:\n{said}"
    );
    assert!(
        said.contains("infra/ops/b.sh:3:"),
        "|| m=0 is named:\n{said}"
    );
    assert!(
        said.contains("failed-read-as-number"),
        "the rule is named:\n{said}"
    );
}

/// BEHAVIOUR 4 — the MUTATING roster reaches a script WHEREVER its verb
/// points (boss-gcp's verbs live under infra/gcp): one without pipefail
/// is refused naming the verb, and one that has it is still read for the
/// other two shapes.
#[test]
fn a_mutating_script_without_pipefail_is_refused_wherever_it_lives() {
    let tree = fail_closed_tree("pipefail");
    tree.verb("retire", MUTATING_ABOUT, "infra/gcp/retire.sh");
    tree.file(
        "infra/gcp/retire.sh",
        "#!/usr/bin/env bash\nset -eu\nsystemctl stop x\n",
    );
    tree.verb("other", MUTATING_ABOUT, "infra/gcp/other.sh");
    tree.file(
        "infra/gcp/other.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\nx=$(systemctl is-active y || true)\n",
    );
    // Not named by a MUTATING verb and outside infra/forge and infra/ops:
    // not this lint's to read.
    tree.file(
        "infra/gcp/unrelated.sh",
        "#!/usr/bin/env bash\nq=$(cmd || true)\n",
    );
    let out = tree.run();
    let said = text(&out);
    assert_eq!(out.status.code(), Some(1), "{said}");
    assert!(
        said.contains("infra/gcp/retire.sh") && said.contains("no-pipefail"),
        "the script without pipefail is named with its rule:\n{said}"
    );
    assert!(said.contains("retire"), "the verb is named:\n{said}");
    assert!(
        said.contains("infra/gcp/other.sh:3:"),
        "a MUTATING script outside forge/ops is still read for swallowed reads:\n{said}"
    );
    assert!(
        !said.contains("infra/gcp/unrelated.sh"),
        "a script no MUTATING verb names, outside forge/ops, is not read:\n{said}"
    );
}

/// BEHAVIOUR 5 — an allowance is a written decision: it excuses EXACTLY
/// its count, it must carry a reason, and it must name a file that
/// exists. Each failure is exit 1 — the list is wrong, the tree was read.
#[test]
fn an_allowance_excuses_exactly_its_count_and_carries_a_reason() {
    let site = "#!/usr/bin/env bash\nset -euo pipefail\nn=$(grep -c . \"$f\" || true)\n";

    let tree = fail_closed_tree("allow-exact");
    tree.file("infra/forge/a.sh", site);
    tree.file(
        ALLOW,
        "# rule path count reason\nswallowed-read infra/forge/a.sh 1 the count is printed, never tested\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "an exact allowance with a reason excuses the site:\n{}",
        text(&out)
    );

    let tree = fail_closed_tree("allow-high");
    tree.file("infra/forge/a.sh", site);
    tree.file(
        ALLOW,
        "swallowed-read infra/forge/a.sh 2 the count is printed, never tested\n",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("lower it"), "{}", text(&out));

    let tree = fail_closed_tree("allow-no-reason");
    tree.file("infra/forge/a.sh", site);
    tree.file(ALLOW, "swallowed-read infra/forge/a.sh 1\n");
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("reason"), "{}", text(&out));

    let tree = fail_closed_tree("allow-gone");
    tree.file(
        ALLOW,
        "swallowed-read infra/forge/gone.sh 1 a file that was deleted\n",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("infra/forge/gone.sh"), "{}", text(&out));
}

/// BEHAVIOUR 6 — without jq the MUTATING roster cannot be derived: exit 3
/// naming what was missing. Never 0.
#[test]
fn without_jq_the_lint_says_it_cannot_answer() {
    let tree = fail_closed_tree("no-jq");
    let out = tree.run_with(&[("LINT_JQ", "a-jq-this-machine-does-not-have")]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "no jq is a fact about the machine (exit 3):\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("a-jq-this-machine-does-not-have"),
        "{}",
        text(&out)
    );
}

/// The live tree: every site the lint finds today is in the allowlist
/// with its reason and its exact count, and every MUTATING verb's script
/// runs under pipefail.
#[test]
fn the_repository_itself_fails_closed() {
    let out = Command::new("bash")
        .arg(repo_root().join(LINT))
        .current_dir(repo_root())
        .output()
        .expect("run the lint on the repository");
    assert!(
        out.status.success(),
        "the repository must pass its own lint:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("scanned "), "{}", text(&out));
}
