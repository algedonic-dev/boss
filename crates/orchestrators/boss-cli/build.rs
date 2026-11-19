//! Two jobs. (1) Stamp the binary with the commit it was built from,
//! so the operator's own tool can say when it is stale (895c9a3b). (2)
//! Generate the platform workflow roster `boss tenant check` compiles in,
//! from the directory `infra/platform/workflows/` — see
//! `platform_workflow_kinds` below. (The ops allowlist `boss ops` reads
//! was a third, until backlog 7aef2405 moved its one copy to boss-jobs'
//! build.rs, where the jobs API's file-the-request door reads it too.)
//!
//! On the dev pod `boss` is whatever `cargo build -p boss-cli` last
//! produced, and NOTHING refreshes it: measured 2026-09-10, the binary
//! running every gate launch was eleven hours and two trains behind
//! main, so a guard that had landed (`--park-*` refusing without an
//! item) was unenforced on the one box that gates, silently — a removed
//! flag fails loudly, changed behaviour behind the same flag just does
//! the old thing. The floor is that the binary SAYS what it is: this
//! script reads `git rev-parse HEAD` at build time and, where there is
//! no git (an image build passes the sha as `BOSS_BUILD_COMMIT`, the
//! same variable every service already reports on /health), falls back
//! to that, and to `unknown` — never to a guess.

use std::path::{Path, PathBuf};
use std::process::Command;
mod workflow_roster;

fn main() {
    platform_workflow_kinds();
    let from_git = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let sha = from_git
        .or_else(|| std::env::var("BOSS_BUILD_COMMIT").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=BOSS_CLI_BUILT_FROM={sha}");
    println!("cargo:rerun-if-env-changed=BOSS_BUILD_COMMIT");
    // RERUN EVERY BUILD. This script used to name the checkout's HEAD
    // file as its one rerun key, and that was wrong twice over. On a
    // branch, HEAD is `ref: refs/heads/x` and a commit moves the ref
    // file, not HEAD, so the stamp lagged every commit. And worktrees
    // sharing one target dir share this script's unit: the key the LAST
    // worktree wrote (`/work/boss/.git/worktrees/gaterebase/HEAD`) is
    // what cargo checks when the NEXT worktree builds, so on 2026-09-12
    // the pod's `boss --version` answered 47a31701 for a binary just
    // built from a7c3bf4f — the freshness door (895c9a3b) saying
    // "current" about a stale stamp, which is the one thing it exists
    // not to do. A path that never exists makes cargo run this script
    // on every build; it costs one `git rev-parse` (milliseconds), and
    // boss-cli itself recompiles only when the sha it is handed changes.
    println!(
        "cargo:rerun-if-changed={}",
        workflow_roster::IDENTITY_RERUN_TRIGGER
    );
}

/// `PLATFORM_WORKFLOW_KINDS: &[&str]` — every
/// `infra/platform/workflows/*.toml`, by kind, written to OUT_DIR for
/// `src/tenant.rs` to `include!`.
///
/// THE DIRECTORY IS THE ROSTER, the same collapse boss-jobs' `verb_files`
/// makes (CLAUDE.md §9a). `boss tenant check` judges a tenant directory
/// with no network and no product tree beside it — the converge runs it
/// from the image against a `tenant_repo` checkout — so the only honest
/// offline answer to "is this a workflow kind the tenant can open" is
/// the tenant's own `seeds/workflows.toml` plus the kinds the PRODUCT
/// ships, and the product's are known at build time or not at all. A
/// hand-kept list in `tenant.rs` would be the second copy §9a forbids;
/// the test `the_platform_workflow_roster_is_the_directory` pins the
/// compiled set to the directory as it stands at test time.
///
/// The kind IS the file name — the rule `infra/gcp/publish-workflow.sh`
/// applies when it resolves `infra/platform/workflows/$kind.toml`, and
/// the same test holds each file's own `kind` to its name. Only
/// `*.toml` is read; the README beside them is prose. An empty
/// directory is a build-time panic, not an empty roster that would let
/// every dangling reference through.
fn platform_workflow_kinds() {
    let crate_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    // crates/orchestrators/boss-cli -> repo root
    let dir = crate_dir
        .join("../../../infra/platform/workflows")
        .canonicalize()
        .expect("infra/platform/workflows must exist relative to the crate root");
    println!("cargo:rerun-if-changed={}", dir.display());

    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let out = workflow_roster::render(&names)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

    let dest = Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"))
        .join("platform_workflow_kinds.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
}
