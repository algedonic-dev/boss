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
    for needle in cargo_phases.iter() {
        assert!(
            gate.contains(needle),
            "infra/gate.sh no longer runs `{needle}` — the gate \
             under-covers what it existed to cover"
        );
    }

    // The lint roster used to be hand-listed here, and by 2026-08-13 it
    // had drifted to a strict subset: dispatcher-rules-ratchet,
    // schema-converge and no-secrets all ran in gate.sh while this test
    // said nothing about them, so any of the three — including the
    // secret scanner — could have been deleted from the gate silently.
    // That is the same under-covering shape PR #226 shipped twice, just
    // one level up, so the roster is derived instead of restated
    // (CLAUDE.md §9a).
    //
    // Every executable check in infra/lint/ must appear in gate.sh
    // unless it is listed below with a reason. The directory does hold
    // legitimate non-gate scripts — that was the original objection to
    // globbing — but "which ones and why" is a decision that should be
    // written down once, here, rather than expressed as absence.
    let not_gated: &[(&str, &str)] = &[
        (
            "conservation-invariants.sh",
            "live-DB sweep on a systemd timer, not a static check",
        ),
        (
            "audit-ordering.sh",
            "live-DB sweep; needs a populated audit_log to say anything",
        ),
        (
            "no-snapshot-arrays.sh",
            "needs a built workspace (boss-ports-list) — gating it is \
             proposed separately; it is the check that would have caught \
             the stale _generated/ports.ts",
        ),
    ];

    let mut missing = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("infra/lint")).expect("read infra/lint") {
        let path = entry.expect("dir entry").path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.ends_with(".sh") => n.to_string(),
            _ => continue,
        };
        if not_gated.iter().any(|(n, _)| *n == name) {
            continue;
        }
        if !gate.contains(&name) {
            missing.push(name);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "infra/lint/ holds check(s) that infra/gate.sh does not run: {missing:?}. \
         Either add them to the gate, or add them to `not_gated` in this test \
         with the reason they are exempt."
    );
}
