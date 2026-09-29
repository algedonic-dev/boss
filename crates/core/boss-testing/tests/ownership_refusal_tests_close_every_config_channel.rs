//! Every test that asks git for an ownership refusal closes every
//! channel a host could use to switch that refusal off.
//!
//! MEASURED 2026-09-27 (backlog a8d8c956, filed as 3bef4198). GitHub's
//! ubuntu runner image appends `[safe] directory = *` to /etc/gitconfig,
//! so a test that sets `GIT_TEST_ASSUME_DIFFERENT_OWNER=1` and leaves the
//! system file open runs a HEALTHY git there: the script under test reads
//! the fixture, answers it, and the assertion that it must refuse fails
//! on the public mirror while the cluster gate stays green. Seven tests
//! went red that way in one file. The fix was four env lines, and two
//! files each carried their own copy; the one copy is now
//! `boss_testing::git_config_isolated`, and this pin holds every file
//! that plants the foreign owner to it — so the next such test cannot
//! quietly close three channels of four.
//!
//! The rule is per FILE, not per call: a file may also plant a host's
//! config on purpose (the trusting-host case in
//! `a_lint_that_cannot_read_does_not_say_clean.rs` does, to prove the
//! plant is the runner's condition), and that is a deliberate choice
//! the file makes beside the isolated cases, never instead of them.
//!
//! tree-wide pin — it walks every Rust file under crates/, so a new
//! ownership test in any crate can break it and no changed-file map
//! attributes that to this crate.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The literal a test sets, spelled in two halves so this file does not
/// match itself.
fn planted() -> String {
    format!("\"{}{}\"", "GIT_TEST_ASSUME_", "DIFFERENT_OWNER")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "target" && name != "node_modules" {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A line that is code, not a `//` comment.
fn code_lines(src: &str) -> impl Iterator<Item = &str> {
    src.lines().filter(|l| !l.trim_start().starts_with("//"))
}

#[test]
fn every_file_that_plants_a_foreign_owner_isolates_git_config_through_the_one_helper() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    let needle = planted();

    let mut planting = Vec::new();
    let mut unisolated = Vec::new();
    for path in files {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !code_lines(&src).any(|l| l.contains(&needle)) {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if !code_lines(&src).any(|l| l.contains("git_config_isolated(")) {
            unisolated.push(rel.clone());
        }
        planting.push(rel);
    }

    // The control: the two files the item names DO plant the owner, so
    // an empty scan is a broken walk, not a clean tree.
    for known in [
        "crates/core/boss-testing/tests/the_gate_reads_a_foreign_owned_checkout.rs",
        "crates/core/boss-testing/tests/a_lint_that_cannot_read_does_not_say_clean.rs",
    ] {
        assert!(
            planting.iter().any(|p| p == known),
            "the scan did not find {known}, which plants the foreign owner — the walk \
             is broken, so its silence about every other file means nothing. Found: \
             {planting:?}"
        );
    }
    assert!(
        unisolated.is_empty(),
        "{unisolated:?} set {needle} without `boss_testing::git_config_isolated`. On a \
         host whose /etc/gitconfig says `[safe] directory = *` (every GitHub ubuntu \
         runner) that test runs a healthy git and its refusal case fails — or passes \
         vacuously. Close all four channels through the helper (backlog 3bef4198)."
    );
}
