//! `infra/lint/a-secret-header-rides-in-a-file.sh` is RUN, not read —
//! against synthetic trees, so every property below is one the lint
//! actually has.
//!
//! THE CLASS (backlog 5f3ad356, 2026-09-27; the sweep on cfa678e9,
//! 2026-09-28). A secret header handed to curl as `-H "Name: $value"` is
//! in curl's command line, readable by every local user in `ps` and
//! /proc/<pid>/cmdline while curl runs: 28 machine-token sites in 13
//! files, and four more secrets under variable names (`TOK`,
//! `auth_header`) that a grep for `*TOKEN*` misses. The lint pins the
//! HEADER NAME, so the variable's name cannot hide it; the repair is
//! `infra/lib/secret-header.sh` (pinned by `secret_header_sh.rs`).
//!
//! WHY THIS TEST AND NOT ONLY THE LINT'S `--self-test`: the self-test
//! owns "the scanner still matches" (mawk silently matches nothing for an
//! interval or a `\s`), and runs on every invocation; this file owns the
//! VERDICT on a tree — the repaired shapes exit 0 and say how much they
//! read, and the defect exits 1 naming each site and the way out.

use boss_testing::repo_root;
use boss_testing::scratch;
use std::path::PathBuf;
use std::process::{Command, Output};

const LINT: &str = "infra/lint/a-secret-header-rides-in-a-file.sh";

/// A synthetic repository: the lint at the path its own
/// `cd "$(dirname "$0")/../.."` resolves from, its libs, and whatever
/// files under infra/ a test plants.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Tree {
        let root = scratch::scratch_dir(&format!("secret-header-lint-{tag}"));
        scratch::create_dir(&root.join("infra/lint"));
        let body = std::fs::read_to_string(repo_root().join(LINT)).expect("read the lint");
        scratch::write_exec(&root.join(LINT), &body);
        boss_testing::copy_lint_libs(&root);
        Tree(root)
    }

    fn file(&self, rel: &str, body: &str) -> &Tree {
        let path = self.0.join(rel);
        scratch::create_dir(path.parent().expect("a parent"));
        scratch::write_file(&path, body);
        self
    }

    fn run(&self) -> Output {
        Command::new("bash")
            .arg(self.0.join(LINT))
            .current_dir(&self.0)
            .output()
            .unwrap_or_else(|e| panic!("run the lint in {}: {e}", self.0.display()))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The scanner proves itself; this test only insists it is run and says so.
#[test]
fn the_scanner_proves_itself_on_every_invocation() {
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
    assert!(
        text(&out).contains("self-test ok"),
        "the self-test must SAY it ran:\n{}",
        text(&out)
    );
}

/// The repaired shapes pass: the lib's call and its `${VAR:+-H "$VAR"}`,
/// a caller's own `-H @file`, the printf-to-file shape four forge
/// scripts already used, and prose (a comment, a README) that names the
/// old shape.
#[test]
fn the_repaired_shapes_are_clean() {
    let tree = Tree::new("clean");
    tree.file(
        "infra/forge/publish.sh",
        "\
#!/usr/bin/env bash
. \"$(dirname \"$0\")/../lib/secret-header.sh\"
# was: ${BOSS_MACHINE_TOKEN:+-H \"x-boss-machine-token: $BOSS_MACHINE_TOKEN\"}
secret_header MT_HDR ${BOSS_MACHINE_TOKEN:+\"x-boss-machine-token: $BOSS_MACHINE_TOKEN\"}
curl -fsS -H \"x-boss-user: $BOSS_USER\" ${MT_HDR:+-H \"$MT_HDR\"} \"$BASE/api/jobs\"
",
    )
    .file(
        "infra/forge/github.sh",
        "\
(umask 077 && printf 'Authorization: Bearer %s\\n' \"$token\" >\"$WORK/auth\")
curl -sS -H \"@$WORK/auth\" \"$api\"
",
    )
    .file(
        "infra/forge/README.md",
        "Never `curl -H \"Authorization: Bearer $TOKEN\"` — use the lib.\n",
    );
    let out = tree.run();
    assert!(
        out.status.success(),
        "the repaired shapes must exit 0; got {:?}:\n{}",
        out.status.code(),
        text(&out)
    );
    // Exit 0 with the README's argv-shaped line in the tree is the proof
    // that prose is not judged; the count proves something was.
    assert!(
        text(&out).contains("file(s) under infra/") && text(&out).contains("ok — no file"),
        "the lint must say how much it read, and that it is clean:\n{}",
        text(&out)
    );
}

/// The runner's own credential is a secret header like the machine token
/// (design f623e425 option A; backlog 6c9183de): the jobs API believes a
/// declared writer only on it, so in argv it is the forgery the design
/// exists to refuse, readable by any local user. Named like the others;
/// the lib's call beside it passes.
#[test]
fn the_runner_credential_is_a_secret_header_too() {
    let tree = Tree::new("runner-credential");
    tree.file(
        "infra/ops/runner.sh",
        "\
#!/bin/sh
secret_header RC_HDR \"x-boss-runner-credential: $rc_value\"
curl -fsS ${RC_HDR:+-H \"$RC_HDR\"} \"$BASE/api/jobs\"
curl -fsS -H \"x-boss-runner-credential: $rc_value\" \"$BASE/api/jobs\"
rc_header=\"X-Boss-Runner-Credential: $rc_value\"
",
    );
    let out = tree.run();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let msg = text(&out);
    for expect in [
        "infra/ops/runner.sh:4: argv",
        "infra/ops/runner.sh:5: built",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
    for clean in ["infra/ops/runner.sh:2:", "infra/ops/runner.sh:3:"] {
        assert!(
            !msg.contains(clean),
            "the lib's shape passes, {clean}:\n{msg}"
        );
    }
}

/// The defect in the shapes the sweep found, including the two a
/// variable-name grep misses (the pod door's `$TOK`, and a header built
/// in `auth_header` then passed as `-H "$auth_header"`): each is named by
/// file:line, and the refusal names the way out.
#[test]
fn a_secret_header_in_argv_is_named_by_its_site() {
    let tree = Tree::new("defect");
    tree.file(
        "infra/ops/runner.sh",
        "\
#!/bin/sh
curl -fsS \\
    ${BOSS_MACHINE_TOKEN:+-H \"x-boss-machine-token: $BOSS_MACHINE_TOKEN\"} \\
    \"$BASE/api/jobs\"
",
    )
    .file(
        "infra/dev/door",
        "\
TOK=$(cat /etc/boss/machine-token)
[ -n \"$TOK\" ] && args+=( -H \"X-Boss-Machine-Token: $TOK\" )
",
    )
    .file(
        "infra/forge/report.sh",
        "\
auth_header=\"Authorization: Bearer $token\"
curl -H \"$auth_header\" \"$url\"
",
    );
    let out = tree.run();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a secret header in argv must exit 1:\n{}",
        text(&out)
    );
    let msg = text(&out);
    for expect in [
        "infra/ops/runner.sh:3: argv",
        "infra/dev/door:2: argv",
        "infra/forge/report.sh:1: built",
        "infra/lib/secret-header.sh",
    ] {
        assert!(
            msg.contains(expect),
            "the verdict must name {expect:?}:\n{msg}"
        );
    }
}
