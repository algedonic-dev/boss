//! A host declares the ops credentials it holds (backlog f371c749;
//! design 835c0c9c, decision 3).
//!
//! WHY. `infra/estate/ops-credentials.sh` checked the forge's set — a
//! talosconfig and an admin kubeconfig — on every host that ran it, so
//! boss-gcp's host reading said `not ready: talosconfig:absent
//! kubeconfig:absent` on 2026-09-27, -28 and -29, and the estate alarm
//! told David to place both there by hand. Both halves were wrong for
//! the public edge: a talosconfig has no scoped form and must never be
//! placed there, and its kubeconfig is the SCOPED break-glass one, a
//! derivable credential the broker delivers (backlog 7336cb5f), which
//! no human places.
//!
//! THE ONE SPELLING is the `[ops_credentials.<host>]` tables of
//! infra/estate/estate.toml. Two parsers read it — the shell check here
//! (sed, because the hosts read the tree with the shell) and
//! `estate.compare` in Rust (the toml crate, compiled in). A fact read
//! twice by two parsers can drift in the READING even when the file
//! cannot, so the last test here holds the two readings equal for every
//! host the file declares (CLAUDE.md §9a).

use boss_testing::{repo_root, scratch_dir, scratch_path};
use std::process::Command;

const LIB: &str = "infra/estate/ops-credentials.sh";
const ESTATE: &str = "infra/estate/estate.toml";

/// Run `script` after sourcing the check, under `sh -eu` as observe-host.sh
/// runs it, with the tree's estate declaration and an ops directory a
/// test may read.
fn sh(script: &str, ops_dir: &std::path::Path, estate: Option<&std::path::Path>) -> (i32, String) {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "set -eu\n. '{}'\n{script}\n",
            repo_root().join(LIB).display()
        ))
        .env("BOSS_OPS_DIR", ops_dir);
    match estate {
        Some(e) => cmd.env("BOSS_ESTATE_SOURCE", e),
        None => cmd.env_remove("BOSS_ESTATE_SOURCE"),
    };
    let out = cmd.output().expect("sh runs");
    let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str(&format!(" [stderr: {}]", err.trim()));
    }
    (out.status.code().unwrap_or(-1), text)
}

fn tree_estate() -> std::path::PathBuf {
    repo_root().join(ESTATE)
}

#[test]
fn the_public_edge_is_checked_for_its_scoped_kubeconfig_only() {
    let ops = scratch_path("edge-creds-absent");
    let (rc, out) = sh("ops_credentials_state boss-gcp", &ops, Some(&tree_estate()));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: kubeconfig:absent");
}

#[test]
fn the_forge_is_still_checked_for_both_root_credentials() {
    let ops = scratch_path("forge-creds-absent");
    let (rc, out) = sh("ops_credentials_state forge", &ops, Some(&tree_estate()));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: talosconfig:absent kubeconfig:absent");
}

#[test]
fn a_reading_with_no_declared_set_is_undeclared_never_present() {
    // An empty set would make the check's loop run zero times and print
    // `present` — the quiet wrong answer. Every way to reach an empty set
    // is refused by name: no host named, a host the file declares no set
    // for, no declaration named, and one that cannot be read.
    let ops = scratch_dir("undeclared-creds");
    let missing = scratch_path("no-such-estate").join("estate.toml");
    for (script, estate) in [
        ("ops_credentials_state", Some(tree_estate())),
        ("ops_credentials_state cp-1", Some(tree_estate())),
        ("ops_credentials_state boss-gcp", None),
        ("ops_credentials_state boss-gcp", Some(missing.clone())),
        ("ops_credentials_state 'boss-gcp]/x'", Some(tree_estate())),
    ] {
        let (rc, out) = sh(script, &ops, estate.as_deref());
        assert_eq!(rc, 0, "it reports, the caller decides: {script}: {out}");
        assert!(
            out.starts_with("undeclared: "),
            "{script} with {estate:?}: {out}"
        );
    }
}

#[test]
fn the_host_observation_checks_the_set_its_host_declares() {
    // observe-host.sh for real, as boss-gcp's unit runs it, against an
    // unreachable jobs API so the observation lands in the spool.
    let root = scratch_dir("edge-creds-observe");
    let spool = root.join("spool");
    let out = Command::new("sh")
        .arg(repo_root().join("infra/estate/observe-host.sh"))
        .env("HOST_ID", "boss-gcp")
        .env("JOBS_API", "http://127.0.0.1:1")
        .env("ADDRESS", "127.0.0.1")
        .env("SPOOL_DIR", &spool)
        .env("BOSS_OPS_DIR", root.join("boss-ops"))
        .env_remove("BOSS_ESTATE_SOURCE")
        .output()
        .expect("observe-host runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let spooled: Vec<_> = std::fs::read_dir(&spool)
        .unwrap_or_else(|e| panic!("the observation was spooled ({e}): {text}"))
        .filter_map(Result::ok)
        .collect();
    assert_eq!(spooled.len(), 1, "{text}");
    let body = std::fs::read_to_string(spooled[0].path()).expect("read");
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        v["nodes"][0]["ops_credentials"]["state"], "not ready: kubeconfig:absent",
        "{body}"
    );
}

#[test]
fn the_converge_checks_the_set_its_node_declares() {
    let ops = scratch_path("edge-creds-converge");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". '{}'\ninstall_cluster_operator boss-gcp",
            repo_root()
                .join("infra/estate/install-cluster-operator.sh")
                .display()
        ))
        .env("INSTALL_TALOSCTL", "0")
        .env("BOSS_OPS_DIR", &ops)
        .env_remove("BOSS_ESTATE_SOURCE")
        .output()
        .expect("bash runs");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("credentials not ready: kubeconfig:absent"),
        "{text}"
    );
    assert!(!text.contains("talosconfig"), "{text}");
}

#[test]
fn both_callers_name_the_host_they_check() {
    // The node id each converge already reads its roles with, and the
    // observer's HOST_ID — never a guess from a hostname.
    for (caller, call) in [
        (
            "infra/gcp/boss-gcp-converge.sh",
            "install_cluster_operator \"$NODE_ID\"",
        ),
        (
            "infra/forge/install.sh",
            "install_cluster_operator \"${BOSS_NODE_ID:-forge}\"",
        ),
        (
            "infra/estate/observe-host.sh",
            "ops_credentials_state \"$HOST_ID\"",
        ),
    ] {
        let text = std::fs::read_to_string(repo_root().join(caller))
            .unwrap_or_else(|e| panic!("{caller}: {e}"));
        assert!(text.contains(call), "{caller} must call `{call}`");
    }
}

#[test]
fn the_shell_and_the_comparator_read_one_declaration_the_same_way() {
    // The Rust reading: the toml crate, as estate.compare parses it.
    let text = std::fs::read_to_string(tree_estate()).expect("estate.toml");
    let table: toml::Table = toml::from_str(&text).expect("estate.toml parses");
    let sets = table
        .get("ops_credentials")
        .and_then(toml::Value::as_table)
        .expect("estate.toml declares [ops_credentials.<host>] tables");
    assert!(sets.len() >= 2, "the forge and boss-gcp at least: {sets:?}");
    let ops = scratch_path("pin-creds-absent");
    for (host, creds) in sets {
        let mut want: Vec<String> = creds
            .as_table()
            .unwrap_or_else(|| panic!("[ops_credentials.{host}] is a table"))
            .keys()
            .cloned()
            .collect();
        want.sort();
        let (rc, out) = sh(
            &format!("ops_credentials_declared {host}"),
            &ops,
            Some(&tree_estate()),
        );
        assert_eq!(rc, 0, "{host}: {out}");
        let mut got: Vec<String> = out.split_whitespace().map(str::to_string).collect();
        got.sort();
        assert_eq!(
            got, want,
            "the shell and the comparator read [ops_credentials.{host}] differently"
        );
    }
}
