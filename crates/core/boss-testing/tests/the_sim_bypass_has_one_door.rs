//! The sim bypass has one door (backlog 85e7f10f, 2026-09-25).
//!
//! Until that day the policy bypass for simulator traffic was spelled
//! twelve times: five service binaries wrapped their policy client in
//! `SimBypassPolicyClient::new` with no condition, and seven
//! operator-tier handlers wrote `let sim = is_in_sim_chain(); if !(sim
//! || tier_ok) { 403 }` inline. Each trusted the `x-sim-origin` header
//! alone, so any caller that reached a service port without passing the
//! gateway passed every check it guarded — on prod, whose sim had been
//! parked for three weeks.
//!
//! The fix put the decision in ONE place, `boss-policy-client`: the
//! wrapper is built only through `SimBypassPolicyClient::from_env` (the
//! inner client untouched unless `BOSS_SIM_ENABLED` is on), and an
//! inline door asks `sim_bypass_allowed(&user)` (sim on, sim chain, sim
//! identity). Two rules, pinned across every crate, so a thirteenth
//! spelling cannot come back in silence:
//!
//!   1. No Rust source outside boss-policy-client names the wrapper
//!      except as `SimBypassPolicyClient::from_env` or `::wrap` (the
//!      test door, whose switch is explicit at the call).
//!   2. The chain flag, `is_in_sim_chain()`, is read only by the files
//!      below — each for PROVENANCE (stamping `_simulated`, the partition
//!      at admission, carrying the header to the next hop), none for
//!      authorization. A new reader is a new line here, with its reason.
//!   3. The `x-sim-origin` header itself — `SIM_ORIGIN_HEADER`,
//!      `header_is_truthy(`, or a `.get("x-sim-origin")` — is read only
//!      by the files in HEADER_READERS (backlog d65bd066, from the review
//!      of the car above): rules 1 and 2 matched two spellings, so a
//!      handler that parsed the header for itself walked around both.
//!      Senders (`.header("x-sim-origin", …)`) are not readers.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// Every file allowed to read the chain flag, and what it reads it for.
const CHAIN_READERS: &[(&str, &str)] = &[
    ("crates/core/boss-core/src/sim_origin.rs", "the definition"),
    (
        "crates/core/boss-core/src/publisher.rs",
        "stamps _simulated on every emit",
    ),
    (
        "crates/core/boss-policy-client/src/lib.rs",
        "the one authorization predicate",
    ),
    (
        "crates/core/boss-jobs/src/http/jobs.rs",
        "decides a packet's partition at admission",
    ),
    (
        "crates/core/boss-jobs/src/escalation.rs",
        "carries the chain to boss-messages",
    ),
    (
        "crates/core/boss-dispatcher/src/dispatcher.rs",
        "carries the chain to the next hop",
    ),
    (
        "crates/orchestrators/boss-dispatcher-handlers/src/handlers/common.rs",
        "carries the chain to the next hop",
    ),
];

/// Every file allowed to read the `x-sim-origin` header, and why.
const HEADER_READERS: &[(&str, &str)] = &[
    ("crates/core/boss-core/src/sim_origin.rs", "the definition"),
    (
        "crates/core/boss-policy-client/src/lib.rs",
        "scopes the chain flag, only on a sim instance",
    ),
    (
        "crates/core/boss-gateway/src/proxy.rs",
        "strips the header from every public request",
    ),
];

/// Built in halves so this file's own text is not an instance.
const CHAIN_READ: &str = concat!("is_in_sim", "_chain()");
const WRAPPER: &str = concat!("SimBypass", "PolicyClient::");
const HEADER_READS: &[&str] = &[
    concat!("SIM_ORIGIN", "_HEADER"),
    concat!("header_is", "_truthy("),
    concat!("get(\"x-sim", "-origin\")"),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Code, not prose: a comment explaining the rule is not an instance.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .map(|(i, l)| (i + 1, l))
}

fn sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(&root).ok()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).ok()?;
            Some((rel, text))
        })
        .collect()
}

/// A wrapper use that is not `from_env(` or `wrap(`.
fn unconditional_wrappers(text: &str) -> Vec<usize> {
    code_lines(text)
        .filter(|(_, l)| {
            l.match_indices(WRAPPER).any(|(i, _)| {
                let rest = &l[i + WRAPPER.len()..];
                !(rest.starts_with("from_env(") || rest.starts_with("wrap("))
            })
        })
        .map(|(n, _)| n)
        .collect()
}

fn chain_reads(text: &str) -> Vec<usize> {
    code_lines(text)
        .filter(|(_, l)| l.contains(CHAIN_READ))
        .map(|(n, _)| n)
        .collect()
}

fn header_reads(text: &str) -> Vec<usize> {
    code_lines(text)
        .filter(|(_, l)| HEADER_READS.iter().any(|t| l.contains(t)))
        .map(|(n, _)| n)
        .collect()
}

/// The rule every allow-list here shares: no reader outside `allowed`,
/// and no entry in `allowed` that no longer reads — a stale entry is an
/// allowance nobody needs, and the next reader would inherit it.
fn readers_outside(
    allowed: &[(&str, &str)],
    reads: fn(&str) -> Vec<usize>,
) -> (Vec<String>, Vec<String>) {
    let allowed: Vec<&str> = allowed.iter().map(|(f, _)| *f).collect();
    let all = sources();
    let offenders = all
        .iter()
        .filter(|(rel, _)| !allowed.contains(&rel.as_str()))
        .flat_map(|(rel, text)| reads(text).into_iter().map(move |n| format!("{rel}:{n}")))
        .collect();
    let stale = allowed
        .iter()
        .filter(|f| {
            !all.iter()
                .any(|(rel, text)| rel == *f && !reads(text).is_empty())
        })
        .map(|f| f.to_string())
        .collect();
    (offenders, stale)
}

#[test]
fn the_scanners_read_code_and_not_prose() {
    let wrapped = format!(
        "let a = {WRAPPER}from_env(inner);\nlet b = {WRAPPER}wrap(inner, true);\n// {WRAPPER}new(x) in prose\nlet c = {WRAPPER}new(inner);\n"
    );
    assert_eq!(unconditional_wrappers(&wrapped), vec![4]);
    let reads =
        format!("// {CHAIN_READ} in prose\nlet sim = boss_core::sim_origin::{CHAIN_READ};\n");
    assert_eq!(chain_reads(&reads), vec![2]);
    let [name, parse, get] = HEADER_READS else {
        panic!("three header spellings")
    };
    let header = format!(
        "// {name} in prose\nlet h = req.headers().{get};\nlet on = {parse}v);\nlet n = boss_core::sim_origin::{name};\nb.header(\"x-sim-origin\", \"true\");\n"
    );
    assert_eq!(header_reads(&header), vec![2, 3, 4]);
}

#[test]
fn the_bypass_is_built_only_from_the_deployments_switch() {
    let offenders: Vec<String> = sources()
        .iter()
        .filter(|(rel, _)| !rel.starts_with("crates/core/boss-policy-client/"))
        .flat_map(|(rel, text)| {
            unconditional_wrappers(text)
                .into_iter()
                .map(move |n| format!("{rel}:{n}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "the sim bypass is installed here without the deployment's switch — build it with \
         SimBypassPolicyClient::from_env(inner), which returns the inner client unless \
         BOSS_SIM_ENABLED is on (backlog 85e7f10f):\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_chain_flag_is_read_for_provenance_and_never_to_authorize() {
    let (offenders, stale) = readers_outside(CHAIN_READERS, chain_reads);
    assert!(
        offenders.is_empty(),
        "these lines read the sim-chain flag directly. To authorize, ask \
         boss_policy_client::sim_bypass_allowed(&user) — the header alone is not a caller \
         (backlog 85e7f10f); for provenance, add the file to CHAIN_READERS with its reason:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "CHAIN_READERS names files that no longer read the flag: {stale:?}"
    );
}

#[test]
fn the_sim_origin_header_is_read_only_by_its_named_readers() {
    let (offenders, stale) = readers_outside(HEADER_READERS, header_reads);
    assert!(
        offenders.is_empty(),
        "these lines read the x-sim-origin header themselves. A service learns it is in a sim \
         chain from boss_policy_client's request-context middleware, which honours the header \
         only on a sim instance (backlog 85e7f10f, d65bd066); a new reader goes in \
         HEADER_READERS with its reason:\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "HEADER_READERS names files that no longer read the header: {stale:?}"
    );
}
