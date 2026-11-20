//! The dispatcher nominates nobody (backlog e1eaf72b, 2026-11-11).
//!
//! David, 2026-11-11 ~23:25Z: "Work is supposed to sit at the admission
//! queue of a station now, not at a name or robot or executor." Measured
//! the same hour on the system of record: about 700 READY backlog-item
//! steps carried a named `assignee_id` (agent-claude about 560,
//! agent-codex about 144) although `backlog-item.toml`'s build step
//! declares no assignee. Every packet's events showed the same shape:
//! `step.ready` with `assignee_id: null`, then `automation:dispatcher`
//! PUT a name with `automatic_assignment: true` and emitted
//! `step.assigned`. The writer was the matchmaker in `boss-dispatcher`,
//! the `dispatcher-steps` consumer on `jobs.step.>`: a capability pick,
//! an environment executor and a role hash, each naming an agent. The
//! role hash read no model, so 125 v17 build steps landed on agent-codex
//! while `boss dispatch --next` refused codex every one of them.
//!
//! The matchmaker was retired outright. A ready step now stands at its
//! station unassigned, and the executor that pulls it (`boss dispatch
//! --next`) is filtered by role and model class at the claim. Two rules,
//! pinned across the dispatcher's crates so the push model cannot come
//! back in silence:
//!
//!   1. No source writes the jobs API's `automatic_assignment` key — the
//!      flag the step PUT reads to mark a nomination made by automation.
//!   2. No source binds the `dispatcher-steps` consumer.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The dispatcher's crates: the core one and its handlers.
const DISPATCHER_CRATES: &[&str] = &[
    "crates/core/boss-dispatcher",
    "crates/orchestrators/boss-dispatcher-handlers",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `(path relative to the repo, line number, line)` for every line of
/// the dispatcher's crates that holds `needle`.
fn lines_holding(needle: &str) -> Vec<(String, usize, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for krate in DISPATCHER_CRATES {
        rust_files(&root.join(krate), &mut files);
    }
    assert!(
        files.len() > 10,
        "the dispatcher's crates were read: {} files under {DISPATCHER_CRATES:?}",
        files.len()
    );
    files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string();
            text.lines()
                .enumerate()
                .filter(|(_, line)| line.contains(needle))
                .map(|(i, line)| (rel.clone(), i + 1, line.trim().to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn no_dispatcher_source_writes_an_automatic_assignment() {
    // The key as a JSON key, quoted: the 409 code
    // `automatic_assignment_superseded`, which a handler names in prose,
    // is not a write of it.
    let writes = lines_holding("\"automatic_assignment\"");
    assert!(
        writes.is_empty(),
        "a ready step stands at its station unassigned (backlog e1eaf72b); the \
         dispatcher nominates nobody, and these lines write a nomination:\n{writes:#?}"
    );
}

#[test]
fn no_dispatcher_source_binds_the_assignment_consumer() {
    let binds = lines_holding("\"dispatcher-steps\"");
    assert!(
        binds.is_empty(),
        "the `dispatcher-steps` consumer was the matchmaker, retired by backlog \
         e1eaf72b; these lines bind it again:\n{binds:#?}"
    );
}
