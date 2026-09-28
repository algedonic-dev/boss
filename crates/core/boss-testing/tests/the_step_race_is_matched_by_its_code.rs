//! THE STEP RACE IS MATCHED BY ITS CODE, AND ITS WORDS LIVE ONCE
//! (backlog 2aa2b19e; CLAUDE.md §9a).
//!
//! The step PUT refuses, 409, a write computed from a read another
//! write has since moved (car 88123ae0), and two runners answer exactly
//! that refusal by sending the same status-only body once more. They
//! used to recognise it by its `error` WORDS, copied byte for byte into
//! `infra/gate-runner/run.sh`, `infra/ops/ops-runner.sh` and two test
//! harnesses beside `boss_jobs::step_metadata_write::STEP_CHANGED_ERROR`
//! — measured at eight files on 2026-09-27, held equal by nothing, and
//! wrong the day the compare widened from metadata to the whole row
//! (backlog 428332da: the words still said "its metadata").
//!
//! Collapsed where it can be: the 409 carries a machine `code`,
//! [`STEP_CHANGED_CODE`], the test harnesses and `boss dispatch` speak
//! the Rust definitions themselves, and the current words are a
//! person's again, free to move. What still lives twice is what the two
//! shell runners cannot read at run time — the code, and for ONE
//! RELEASE the words a server from before the code sent
//! ([`STEP_CHANGED_ERROR_BEFORE_THE_CODE`], the review of car 24eb9471)
//! — so this pins each runner's copy to its definition, naming the
//! file that drifts, and refuses the words anywhere else.

use boss_jobs::step_metadata_write::{
    STEP_CHANGED_CODE, STEP_CHANGED_ERROR, STEP_CHANGED_ERROR_BEFORE_THE_CODE,
};
use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The runners that resend on the step race.
const RUNNERS: &[&str] = &["infra/gate-runner/run.sh", "infra/ops/ops-runner.sh"];

/// The one file allowed to spell the words: their definition.
const DEFINITION: &str = "crates/core/boss-jobs/src/step_metadata_write.rs";

fn runner(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Each runner assigns `name` exactly once, as `name='value'`.
fn assigned_once(rel: &str, src: &str, name: &str, value: &str, definition: &str) {
    let want = format!("{name}='{value}'");
    let spelled: Vec<&str> = src
        .lines()
        .filter(|l| l.starts_with(&format!("{name}=")))
        .collect();
    assert_eq!(
        spelled,
        vec![want.as_str()],
        "{rel} must assign {name} exactly once, as `{want}` — {definition} is the \
         definition, and a runner holding any other value misses the refusal it resends on"
    );
}

#[test]
fn each_runner_spells_the_code_the_server_sends() {
    for rel in RUNNERS {
        assigned_once(
            rel,
            &runner(rel),
            "STEP_CHANGED_CODE",
            STEP_CHANGED_CODE,
            "boss_jobs::step_metadata_write::STEP_CHANGED_CODE",
        );
    }
}

/// DELETE WITH `STEP_CHANGED_ERROR_BEFORE_THE_CODE`, after one release.
#[test]
fn each_runner_spells_the_old_words_as_their_definition_does() {
    for rel in RUNNERS {
        assigned_once(
            rel,
            &runner(rel),
            "STEP_CHANGED_WORDS_BEFORE_THE_CODE",
            STEP_CHANGED_ERROR_BEFORE_THE_CODE,
            "boss_jobs::step_metadata_write::STEP_CHANGED_ERROR_BEFORE_THE_CODE",
        );
    }
}

#[test]
fn no_runner_matches_the_current_words() {
    for rel in RUNNERS {
        assert!(
            !runner(rel).contains(STEP_CHANGED_ERROR),
            "{rel} carries the step race's CURRENT words — match the 409's `code` \
             (STEP_CHANGED_CODE) instead; the words are a person's and are free to move"
        );
    }
}

/// Nowhere in the tree but their definition spells the words — and the
/// runners only as their one pinned assignment of the old ones. A copy
/// in a harness or a surface is the drift this collapsed.
#[test]
fn the_words_live_only_in_their_definition() {
    let opening = opening_clause();
    let root = repo_root();
    let mut found = Vec::new();
    for top in ["crates", "infra", "apps", "libs", "docs"] {
        walk(&root.join(top), &mut |path| {
            let Ok(src) = std::fs::read_to_string(path) else {
                return;
            };
            let rel = path.strip_prefix(&root).unwrap_or(path).to_path_buf();
            if rel == Path::new(DEFINITION) {
                return;
            }
            let pinned = RUNNERS.iter().any(|r| rel == Path::new(r));
            let stray: Vec<&str> = src
                .lines()
                .filter(|l| l.contains(opening))
                .filter(|l| !(pinned && l.starts_with("STEP_CHANGED_WORDS_BEFORE_THE_CODE=")))
                .collect();
            if !stray.is_empty() {
                found.push(format!("{}: {stray:?}", rel.display()));
            }
        });
    }
    assert!(
        found.is_empty(),
        "the step race's words ({opening:?}) are spelled outside {DEFINITION}: {found:?} — \
         use boss_jobs::step_metadata_write's constants, or match STEP_CHANGED_CODE"
    );
}

/// The opening clause both spellings share, read off the constants —
/// never typed here, or this file would be the copy it refuses.
fn opening_clause() -> &'static str {
    let new = STEP_CHANGED_ERROR
        .split(" — ")
        .next()
        .unwrap_or(STEP_CHANGED_ERROR);
    assert!(
        STEP_CHANGED_ERROR_BEFORE_THE_CODE.starts_with(new) && new.len() > 20,
        "the refusal's opening clause is too short, or the two spellings no longer share it: {new:?}"
    );
    new
}

fn walk(dir: &Path, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            "target" | "node_modules" | ".git" | "dist" | "build" | ".svelte-kit"
        ) {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&path, visit),
            Ok(t) if t.is_file() => visit(&path),
            _ => {}
        }
    }
}
