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
//!    refused outside `crates/core/boss-testing/src/proxy.rs`. So is a
//!    code line that QUOTES one of the names at all, outside the files
//!    `SETS_A_PROXY_ON_PURPOSE` names with why: those two call shapes
//!    alone passed a loop over an inline list and a list kept under any
//!    other name (review ea6cbc2f of car a0e06a85, backlog 9a12c070).
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

/// The files that quote a proxy variable's name on purpose, each with
/// why. Every one SETS a proxy, or the exemption from one, to read what a
/// sender does under it — or judges the name itself and sets nothing. A
/// file named here may quote the names, and nothing else may (backlog
/// 9a12c070). It is exempt from that one rule and no other: a hand
/// `env_remove("<name>")` is refused in it as anywhere, and the files of
/// it that are in MEASURED must still strip through the helper. An entry
/// that no longer quotes a name is refused too, so the list cannot keep
/// an allowance for a file that stopped needing it.
const SETS_A_PROXY_ON_PURPOSE: &[(&str, &str)] = &[
    (
        "crates/core/boss-core/tests/a_machine_client_follows_no_ambient_proxy.rs",
        "points six proxy variables at a second loopback, and removes the two \
         exemptions, to prove the machine client follows none of them",
    ),
    (
        "crates/orchestrators/boss-dispatcher-handlers/tests/an_estate_credential_follows_no_ambient_proxy.rs",
        "the same proof for the estate credential client",
    ),
    (
        "crates/orchestrators/boss-dispatcher-handlers/src/handlers/managed_collector_tls/observer/tests.rs",
        "points the https and all-scheme proxies at a dark port, so the TLS \
         child it spawns must connect direct",
    ),
    (
        "crates/orchestrators/boss-cli/src/gate/reservation/native_tests.rs",
        "exempts loopback and the jobs host from a proxy for the child it runs",
    ),
    (
        "crates/orchestrators/boss-cli/src/harness.rs",
        "judges the name of the http proxy variable as one a harness may pass \
         on; it sets nothing",
    ),
    (
        "crates/core/boss-testing/tests/secret_header_sh.rs",
        "its table sets each proxy variable in turn to read the host rule's \
         verdict on it",
    ),
    (
        "crates/core/boss-testing/tests/ops_runner_sh.rs",
        "sets an http proxy to prove the runner credential is withheld from a \
         proxied request",
    ),
    (
        "crates/core/boss-testing/tests/runner_credential_recv_sh.rs",
        "the same proof for the receiving side",
    ),
    (
        "crates/core/boss-testing/tests/runner_credential_deposit_sh.rs",
        "sets an all-scheme proxy for the same proof, and the exemption so the \
         stub's unstamped control reads still reach it",
    ),
];

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

/// Every line of one file the pin refuses, as `rel:line: shape`. Pure,
/// so the fixtures below can hand it a planted file without writing one
/// into the tree.
fn hand_lines(rel: &str, src: &str) -> Vec<String> {
    if rel == HELPER_HOME {
        return Vec::new();
    }
    let shapes = hand_shapes();
    let names: Vec<String> = PROXY_VARS.iter().map(|v| quoted(v)).collect();
    let on_purpose = SETS_A_PROXY_ON_PURPOSE.iter().any(|(f, _)| *f == rel);
    let mut by_hand = Vec::new();
    for (n, line) in src.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some(s) = shapes.iter().find(|s| line.contains(s.as_str())) {
            by_hand.push(format!("{rel}:{}: {s}", n + 1));
        } else if !on_purpose && let Some(s) = names.iter().find(|s| line.contains(s.as_str())) {
            // Any quoted name, not one call shape: a loop over an inline
            // list, or a list under a name other than PROXY_VARS, is the
            // same hand copy (review ea6cbc2f finding 1, backlog 9a12c070).
            by_hand.push(format!("{rel}:{}: {s}", n + 1));
        }
    }
    by_hand
}

/// Quoted names on a file's code lines — what an allowance is FOR.
fn quotes_a_proxy_name(src: &str) -> bool {
    code_lines(src).any(|l| PROXY_VARS.iter().any(|v| l.contains(&quoted(v))))
}

#[test]
fn every_file_allowed_to_quote_a_proxy_name_still_does() {
    let root = repo_root();
    let mut stale = Vec::new();
    for (rel, why) in SETS_A_PROXY_ON_PURPOSE {
        let src = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e} — an allowed file moved; follow it"));
        assert!(!why.is_empty(), "{rel}: an allowance names why");
        if !quotes_a_proxy_name(&src) {
            stale.push(*rel);
        }
    }
    assert!(
        stale.is_empty(),
        "{stale:?} no longer quote a proxy variable's name, so their allowance in \
         SETS_A_PROXY_ON_PURPOSE allows nothing but the next hand list — delete the \
         entry (backlog 9a12c070)."
    );
}

/// A proxy name as a Rust string literal, built at run time from the
/// one list, so the fixtures below never quote a name in this file and
/// the walk does not refuse the pin that runs it.
fn quoted(name: &str) -> String {
    format!("\"{name}\"")
}

/// The nth of the eight, quoted: 0 http, 2 https, 4 all, 6 no (lower
/// case; each odd index is the upper-case spelling of the one before).
fn q(n: usize) -> String {
    quoted(PROXY_VARS[n])
}

/// A test file under no allowance, for the fixtures.
const PLANTED: &str = "crates/core/boss-testing/tests/a_planted_shell_test.rs";

/// The shape the review of car a0e06a85 found on origin/main at
/// machine_gate_value_header_sh.rs:123-134 (review ea6cbc2f, finding 1):
/// a loop over an inline list, which names no `env_remove("<name>")` and
/// no `PROXY_VARS`, so the first walk let it through.
#[test]
fn a_loop_over_an_inline_list_of_proxy_names_is_refused() {
    let src = format!(
        "fn run(cmd: &mut Command) {{\n    for proxy in [{}, {}] {{\n        cmd.env_remove(proxy);\n    }}\n}}\n",
        q(0),
        q(2)
    );
    let refused = hand_lines(PLANTED, &src);
    assert_eq!(refused.len(), 1, "{refused:#?}");
    assert!(
        refused[0].starts_with(&format!("{PLANTED}:2: ")),
        "{refused:#?}"
    );
}

/// The same drifted subset kept under any name but `PROXY_VARS`.
#[test]
fn a_private_list_under_another_name_is_refused() {
    let src = format!("const STRIP: [&str; 2] = [{}, {}];\n", q(4), q(5));
    let refused = hand_lines(PLANTED, &src);
    assert_eq!(refused.len(), 1, "{refused:#?}");
}

/// The controls: a comment may name a proxy variable, the helper's own
/// file holds the one list, and a file that sets a proxy on purpose may
/// quote one — but even there a hand `env_remove("<name>")` is refused.
#[test]
fn a_comment_the_helper_and_an_allowed_file_pass_but_not_a_hand_strip() {
    let line = format!("const STRIP: [&str; 1] = [{}];\n", q(6));
    assert!(hand_lines(PLANTED, &format!("// {line}")).is_empty());
    assert!(hand_lines(HELPER_HOME, &line).is_empty());
    let (allowed, _) = SETS_A_PROXY_ON_PURPOSE[0];
    let sets = format!("    .env({}, proxy_url)\n", q(0));
    assert!(hand_lines(allowed, &sets).is_empty(), "{allowed}");
    let strip = format!("    cmd.env_remove({});\n", q(0));
    assert_eq!(hand_lines(allowed, &strip).len(), 1, "{allowed}");
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
    let mut by_hand = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        by_hand.extend(hand_lines(&rel, &src));
    }
    assert!(
        by_hand.is_empty(),
        "a proxy variable named by hand: {by_hand:#?}. Call \
         `boss_testing::without_proxy(&mut cmd)` instead — it removes all eight, \
         and a hand list is a copy that drifts (backlog 50708d76, F-b). A file \
         that sets a proxy on purpose is named, with why, in \
         SETS_A_PROXY_ON_PURPOSE (backlog 9a12c070)."
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
