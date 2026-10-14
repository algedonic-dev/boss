//! A test keeps the suite-runner's proxy off its children through ONE
//! helper, `boss_testing::without_proxy`, and nowhere by hand.
//!
//! WHY (backlog 50708d76, F-b of review 0a666c59). The machine-token
//! host rule withholds both secrets from a request curl would send
//! through a proxy, and curl sends a loopback request through one like
//! any other. So a shell test whose child inherits a proxy from the shell
//! that ran the suite reads "withheld" where it expects "stamped", or
//! never reaches its stub at all — a red the script did not cause, seen
//! only by whoever runs the suite by hand, because the gate runner sets
//! no proxy. Measured 2026-10-10 by agent-run 71ba06b3: every
//! boss-testing binary run with the six proxy variables pointed at a
//! dark loopback port, then again with none.
//!
//! Two halves, both tree-wide (they walk every Rust file under crates/,
//! so a change in any crate can break them and no changed-file map
//! attributes that to this crate):
//!
//! 1. NO HAND LIST. Before this, seven files each removed their own
//!    subset of the eight variables, and the subsets had drifted (two of
//!    six, four of six, five of six, three private eight-entry lists). A code line that
//!    removes one of the names by hand, or a private `PROXY_VARS`, is
//!    refused outside `crates/core/boss-testing/src/proxy.rs`.
//! 2. EVERY MEASURED RED ADOPTS IT. The files the measurement found red
//!    under a proxy and green without one are named below and must call
//!    the helper. A list typed here is a pin, not a derivation (CLAUDE.md
//!    §9a): what it holds is that the measured set cannot quietly lose
//!    its strip, and the walk in half 1 is the one that catches the next
//!    file written by hand.
//!
//! tree-wide pin — it walks every Rust file under crates/, so a hand
//! list added in any crate can break it and no changed-file map
//! attributes that to this crate (`tree_wide_pins` in infra/gate.sh).

use boss_testing::{PROXY_VARS, repo_root};
use std::path::{Path, PathBuf};

const HELPER_HOME: &str = "crates/core/boss-testing/src/proxy.rs";

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

/// The hand-written shapes, built at run time so this file does not
/// match itself.
fn hand_shapes() -> Vec<String> {
    let mut shapes: Vec<String> = PROXY_VARS
        .iter()
        .map(|v| format!("{}(\"{v}\")", "env_remove"))
        .collect();
    shapes.push(format!("const {}:", "PROXY_VARS"));
    shapes
}

#[test]
fn no_test_removes_a_proxy_variable_by_hand() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    assert!(
        files.len() > 100,
        "the walk found {} Rust files under crates/ — it is broken, so its silence \
         means nothing",
        files.len()
    );
    let shapes = hand_shapes();
    let mut by_hand = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        if rel == HELPER_HOME {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in src.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(s) = shapes.iter().find(|s| line.contains(s.as_str())) {
                by_hand.push(format!("{rel}:{}: {s}", n + 1));
            }
        }
    }
    assert!(
        by_hand.is_empty(),
        "a proxy variable removed by hand: {by_hand:#?}. Call \
         `boss_testing::without_proxy(&mut cmd)` instead — it removes all eight, \
         and a hand list is a copy that drifts (backlog 50708d76, F-b)."
    );
}

/// The files the 2026-10-10 measurement found red under a proxy and green
/// without one, plus the files that carried a hand list before the
/// collapse. Each must call the helper.
const MEASURED: &[&str] = &[
    // Red under a proxy, green without one (2026-10-10, run 71ba06b3).
    "crates/core/boss-testing/tests/a_chore_stamps_every_request_it_sends.rs",
    "crates/core/boss-testing/tests/a_killed_sender_leaves_no_header_behind.rs",
    "crates/core/boss-testing/tests/a_lint_that_scanned_nothing_is_red.rs",
    "crates/core/boss-testing/tests/a_probe_rehearsal_reads_through_the_pods_door.rs",
    "crates/core/boss-testing/tests/admission_readers_present_the_machine_token.rs",
    "crates/core/boss-testing/tests/boss_api_sh.rs",
    "crates/core/boss-testing/tests/codebase_metrics_files_from_a_file.rs",
    "crates/core/boss-testing/tests/codebase_metrics_sh.rs",
    "crates/core/boss-testing/tests/conservation_invariants_sweep_the_sor.rs",
    "crates/core/boss-testing/tests/consist_clone.rs",
    "crates/core/boss-testing/tests/dev_memory_watch_sh.rs",
    "crates/core/boss-testing/tests/dev_scratch_reclaim_sh.rs",
    "crates/core/boss-testing/tests/gate_runner_report_retry.rs",
    "crates/core/boss-testing/tests/host_senders_present_the_machine_token.rs",
    "crates/core/boss-testing/tests/machine_token_deposit_sh.rs",
    "crates/core/boss-testing/tests/machine_token_push_sh.rs",
    "crates/core/boss-testing/tests/mounted_filesystem_evidence.rs",
    "crates/core/boss-testing/tests/probe_reader_deposit_sh.rs",
    "crates/core/boss-testing/tests/protocol_drift_sh.rs",
    "crates/core/boss-testing/tests/registry_token_deposit_sh.rs",
    "crates/core/boss-testing/tests/run_inference_batch_sh.rs",
    "crates/core/boss-testing/tests/runner_credential_recv_sh.rs",
    "crates/core/boss-testing/tests/runner_credential_send_sh.rs",
    "crates/core/boss-testing/tests/surface_usage_sh.rs",
    "crates/core/boss-testing/tests/the_chore_runs_through_a_sor_that_answers_badly.rs",
    "crates/core/boss-testing/tests/the_converge_files_the_registry_drift_it_finds.rs",
    "crates/core/boss-testing/tests/wait_out_a_converge_sh.rs",
    // Carried a hand list before the collapse.
    "crates/core/boss-clock-client/tests/machine_transport.rs",
    "crates/core/boss-testing/tests/credential_deposit_sh.rs",
    "crates/core/boss-testing/tests/forge_token_audit.rs",
    "crates/core/boss-testing/tests/machine_gate_value_header_sh.rs",
    "crates/core/boss-testing/tests/runner_credential_deposit_sh.rs",
    "crates/core/boss-testing/tests/secret_header_sh.rs",
];

#[test]
fn every_file_measured_red_under_a_proxy_strips_it_through_the_helper() {
    let root = repo_root();
    let mut missing = Vec::new();
    for rel in MEASURED {
        let src = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e} — a measured file moved; follow it"));
        if !code_lines(&src).any(|l| l.contains("without_proxy(")) {
            missing.push(*rel);
        }
    }
    assert!(
        missing.is_empty(),
        "{missing:?} ran red with a proxy exported and green without one, and no \
         longer calls `boss_testing::without_proxy` (backlog 50708d76, F-b)."
    );
}
