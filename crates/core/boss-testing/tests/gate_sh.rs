//! The rust gate has ONE definition: `infra/gate.sh`.
//!
//! On the 2026-08-10 train (PR #226) the gate's definition lived twice —
//! once in `.github/workflows/ci.yml`, once in whatever the agent ran
//! locally before pushing a car — and drifted twice in one day: a car
//! gated with named test files missed a lib-suite pin, and a car gated
//! with full crate suites missed a shell lint only CI ran. CLAUDE.md
//! §9a: collapse the pair, and pin what cannot collapse.
//!
//! The collapse: ci.yml's rust job invokes `infra/gate.sh` instead of
//! inlining cargo commands and lint scripts, so CI and a local run are
//! the same definition. What cannot collapse is pinned here:
//! - ci.yml must actually call the script, and must not grow a second
//!   inline definition beside it (a new `run: infra/lint/...` line in
//!   the rust job is the pair reopening);
//! - the script must keep covering the checks the gate exists to run —
//!   a trimmed roster is exactly the under-covering gate that let both
//!   #226 failures through.
//!
//! Both tests name the offending entry when they fail.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repo root resolves")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The rust-job slice of ci.yml: from the `rust:` job key to the next
/// top-level job key.
fn rust_job() -> String {
    let ci = read(".github/workflows/ci.yml");
    let start = ci.find("\n  rust:").expect("ci.yml has a rust job");
    let rest = &ci[start + 1..];
    let end = rest.find("\n  web:").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// The `test`-job slice of the Forgejo workflow — the job that carries
/// the Postgres service, and so the only one that can run the gate's
/// DB-backed test phase. `test` is the last job in the file, so the
/// slice runs to the end; a job appended after it would be swept in,
/// which only ever makes the no-second-definition check stricter.
fn forge_test_job() -> String {
    let ci = read(".forgejo/workflows/ci.yml");
    let start = ci
        .find("\n  test:")
        .expect(".forgejo/workflows/ci.yml has a test job");
    ci[start + 1..].to_string()
}

#[test]
fn ci_rust_job_invokes_the_gate_script() {
    let job = rust_job();
    assert!(
        job.contains("infra/gate.sh"),
        "ci.yml's rust job does not invoke infra/gate.sh — the gate's \
         definition has forked away from the script"
    );
}

#[test]
fn ci_rust_job_has_no_inline_second_definition() {
    let job = rust_job();
    // Environment setup stays in ci.yml (toolchain, cache, schema
    // apply); checks do not. An inline check line beside the script
    // call is the two-definition state this test exists to prevent.
    let inline_checks = [
        "run: cargo clippy",
        "run: cargo test",
        "run: cargo fmt",
        "run: infra/lint/",
    ];
    for needle in inline_checks {
        assert!(
            !job.contains(needle),
            "ci.yml's rust job inlines `{needle}` beside infra/gate.sh — \
             the gate now has two definitions again; move the check into \
             the script"
        );
    }
}

/// The forge workflow is the one that actually gates a train — since
/// the 2026-08-12 cutover, `.github/workflows/ci.yml` runs on the
/// public mirror while every car lands through Forgejo. It ran
/// locomotive + fmt + clippy + migrate + build + test and NOT the
/// script, so the whole lint roster was unenforced in production for a
/// day and thirteen trains landed green over a real `no-wallclock`
/// violation. The pin above only ever knew about the GitHub file,
/// which is why nothing caught it. It knows about both now.
#[test]
fn forge_test_job_invokes_the_gate_script() {
    let job = forge_test_job();
    assert!(
        job.contains("infra/gate.sh"),
        ".forgejo/workflows/ci.yml's test job does not invoke \
         infra/gate.sh — the workflow that gates every train has \
         forked away from the gate's definition"
    );
}

#[test]
fn forge_test_job_has_no_inline_second_definition() {
    let job = forge_test_job();
    // Same rule as the GitHub job: environment setup (services, schema
    // apply) stays in the workflow, checks live in the script. The
    // `fast` job's fmt + clippy are deliberately outside this slice —
    // they are a duplicated fast-signal loop, not a second definition.
    let inline_checks = [
        "run: cargo clippy",
        "run: cargo test",
        "run: cargo build",
        "run: cargo fmt",
        "run: infra/lint/",
    ];
    for needle in inline_checks {
        assert!(
            !job.contains(needle),
            ".forgejo/workflows/ci.yml's test job inlines `{needle}` \
             beside infra/gate.sh — the gate now has two definitions \
             again; move the check into the script"
        );
    }
}

#[test]
fn gate_script_covers_the_checks() {
    let gate = read("infra/gate.sh");
    // The four cargo phases, with the flags that made each one catch a
    // real bug class (see ci.yml history for the provenance of each).
    let cargo_phases = [
        "cargo clippy",
        "-D warnings",
        "cargo build --workspace",
        "--all-features",
        "cargo fmt -- --check",
    ];
    // The lint roster. Listed HERE (not globbed from infra/lint/)
    // because the directory legitimately holds non-gate scripts —
    // nightly prod invariants, their systemd units. Trimming an entry
    // from gate.sh without removing it here is the under-covering
    // gate that shipped two red runs on PR #226.
    let lints = [
        "seed-bypass-smell.sh",
        "no-todo-citation.sh",
        "no-step-kind-match.sh",
        "api-path-bypass-smell.sh",
        "dispatcher-actor-stamp.sh",
        "sim-boundary-audit.sh",
        "tier-import-audit.sh",
        "no-wallclock.sh",
        "outbox-migration-ratchet.sh",
        "idempotence-ratchet.sh",
    ];
    for needle in cargo_phases.iter().chain(lints.iter()) {
        assert!(
            gate.contains(needle),
            "infra/gate.sh no longer runs `{needle}` — the gate \
             under-covers what it existed to cover"
        );
    }
}
