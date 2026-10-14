//! A FACT THAT LIVES TWICE (CLAUDE.md §9a): what a test reads of cargo's
//! environment at RUN time, and what `infra/cargo-test-wide.sh` exports
//! in cargo's place when the gate runs test binaries several at a time
//! (backlog 2d234222).
//!
//! Under `cargo test`, cargo starts each test binary with its package's
//! variables set; under the gate's wide runner the runner does. A test
//! that reads one the runner does not set sees a different world under
//! the gate than on a builder's `cargo test` — exactly the shape of
//! difference the gate exists to rule out. `boss_testing::test_db` is
//! the live case: it reads CARGO_MANIFEST_DIR at run time to find the
//! schema directory of the checkout under test.
//!
//! tree-wide pin — it scans crates/ for runtime reads of cargo's
//! environment, a tree no changed-file map can attribute to this crate.

use boss_testing::repo_root;
use std::collections::BTreeSet;
use std::fs;

const SCRIPT: &str = "infra/cargo-test-wide.sh";

/// Every std::env var / var_os call naming a `CARGO*` variable outside a
/// build script (where cargo sets them for the BUILD, not the run) is a
/// runtime read, and each must be one the runner exports.
#[test]
fn every_runtime_read_of_cargos_environment_is_one_the_runner_sets() {
    let script = fs::read_to_string(repo_root().join(SCRIPT)).expect("read the runner");
    let mut reads = BTreeSet::new();
    let mut stack = vec![repo_root().join("crates")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs")
                || path.file_name().is_some_and(|n| n == "build.rs")
            {
                continue;
            }
            let src = fs::read_to_string(&path).unwrap_or_default();
            for call in ["env::var(\"", "env::var_os(\""] {
                for (i, _) in src.match_indices(call) {
                    let rest = &src[i + call.len()..];
                    let name: String = rest.chars().take_while(|c| *c != '"').collect();
                    if name.starts_with("CARGO") {
                        reads.insert((name, path.display().to_string()));
                    }
                }
            }
        }
    }
    assert!(
        reads.iter().any(|(n, _)| n == "CARGO_MANIFEST_DIR"),
        "the scan found no runtime read of CARGO_MANIFEST_DIR, which boss_testing::test_db makes \
         — the scan is broken: {reads:?}"
    );
    let unset: Vec<_> = reads
        .iter()
        .filter(|(n, _)| !script.contains(&format!("{n}=")))
        .collect();
    assert!(
        unset.is_empty(),
        "these runtime reads of cargo's environment are not set by {SCRIPT}, so the test sees \
         them under `cargo test` and not under the gate — export each one there: {unset:#?}"
    );
}
