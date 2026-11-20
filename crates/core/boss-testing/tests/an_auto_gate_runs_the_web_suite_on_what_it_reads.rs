//! An `--auto` gate runs the web app's build and mocked suite on every
//! car that changes a file they read, and every web unit suite on every
//! car (backlog a244c6e7).
//!
//! MEASURED on origin/main@e2ee2f8ff (2026-10-19). `web_touched()` in
//! infra/gate.sh grepped the changed paths for apps/web, apps/simulator
//! and libs/web-kit, and in `--auto` the whole web phase — install,
//! web-kit unit, apps/web unit, build, mocked — ran only on its yes. The
//! mocked specs also load infra/step-plugins (17 reads), the estate's
//! dev-door declaration, and the bundle imports docs/architecture; the
//! unit tests read further still (estate.test.ts reads Rust handlers,
//! gate-matches-ci.test.ts reads ci.yml). A car that changed only a step
//! plugin went green without the specs that render it, and the conductor
//! gates trains with `--auto` too: the forge's web job was the first run
//! those specs got, after the train gate had already said green.
//!
//! So, measured on the dev pod the same day (the gate's own 4-wide unit
//! runner): the unit suites are cheap — web install 0.04 s warm,
//! web-kit unit 0.2 s, simulator typecheck+unit+build 6 s, apps/web
//! test:unit 14-15 s — and run on EVERY tree now, whatever they read.
//! The build and mocked suite are not (420 s here; 257-425 s for the
//! whole web-suite check in the gate receipts of 2026-10-18), so they
//! still run in `--auto` only when the car changes what they read — and
//! that answer is DERIVED from their own files by
//! apps/web/scripts/web-suite-reads.ts, never a second list (CLAUDE.md
//! 9a). Its own pin, web-suite-reads.test.ts, holds the derivation; this
//! one holds the gate's wiring of it.
//!
//! The derivation runs for real here, on this tree, so a car whose
//! derivation regressed reds this pin as well as its own. It needs bun,
//! which the gate's image carries, so it refuses rather than skips where
//! bun is absent — no evidence is not a pass.

use boss_testing::{repo_root, scratch_dir, write_file};
use std::process::Command;

const SUITE: &str = "web-suite (unit+build+mocked)";
const UNITS: [&str; 4] = [
    "web install",
    "web-kit unit",
    "simulator (typecheck+unit+build)",
    "web unit",
];

fn gate() -> String {
    let path = repo_root().join("infra/gate.sh");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// gate.sh's web phase: from the `web_touched() {` definition through
/// the `fi` that closes it, right before `run_preflight`.
fn web_phase(gate: &str) -> &str {
    let start = gate
        .find("\nweb_touched() {\n")
        .expect("infra/gate.sh defines web_touched()");
    let end = gate[start..]
        .find("\nfi\n\nrun_preflight\n")
        .map(|i| start + i + "\nfi\n".len())
        .expect("infra/gate.sh closes its web phase with `fi` right before run_preflight");
    &gate[start..end]
}

fn require_bun() {
    let bun = Command::new("bun").arg("--version").output();
    assert!(
        bun.as_ref().is_ok_and(|o| o.status.success()),
        "bun is not runnable here ({bun:?}); this pin runs the gate's own derivation of what \
         the web suite reads, which needs bun as the gate image (infra/forge/boss-ci) carries \
         it — no evidence is not a pass"
    );
}

/// Run the lifted web phase on this tree with `check` stubbed to record
/// names, and return the checks it called, in order. `bun_reads` is the
/// stand-in for bun: the real derivation, or one that cannot answer.
fn checks_called(auto: u8, changed: &[&str], bun_reads: &str) -> Vec<String> {
    let dir = scratch_dir("web-suite-reads-phase");
    let log = dir.join("checks.log");
    let changed_file = dir.join("changed.txt");
    write_file(&changed_file, &format!("{}\n", changed.join("\n")));
    let script = dir.join("phase.sh");
    write_file(
        &script,
        &format!(
            "set -uo pipefail\n\
             AUTO={auto}\n\
             CHECK_STATUS=0\n\
             BOSS_GATE_RUNTIME_EVIDENCE='{ev}'\n\
             changed_paths() {{ cat '{changed}'; }}\n\
             check() {{ printf '%s\\n' \"$1\" >> '{log}'; CHECK_STATUS=0; }}\n\
             web_timings_begin() {{ :; }}\n\
             keep_error_context() {{ :; }}\n\
             python3() {{ :; }}\n\
             bun() {{ {bun_reads}; }}\n\
             {phase}\n",
            ev = dir.join("evidence").display(),
            changed = changed_file.display(),
            log = log.display(),
            phase = web_phase(&gate()),
        ),
    );
    let out = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run bash");
    assert!(
        out.status.success(),
        "the lifted web phase exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The real derivation; every other bun call in the phase is a no-op.
const REAL: &str = "case \"$1\" in scripts/web-suite-reads.ts) command bun \"$@\";; *) :;; esac";

#[test]
fn an_auto_gate_on_a_step_plugin_runs_the_specs_that_load_it() {
    require_bun();
    for read in [
        "infra/step-plugins/sign-off.js",
        "infra/estate/dev-door.json",
        "docs/architecture/02-service-map.mmd",
        "bun.lock",
    ] {
        let checks = checks_called(1, &[read, "crates/core/boss-jobs/src/lib.rs"], REAL);
        assert!(
            checks.iter().any(|c| c == SUITE),
            "an --auto gate on a car that changes {read}, which the web suite reads, skipped \
             the {SUITE} check — the forge's run on the train would be the first run of those \
             specs (a244c6e7): {checks:?}"
        );
    }
}

#[test]
fn an_auto_gate_on_no_web_read_runs_every_unit_suite_and_not_the_browser() {
    require_bun();
    let checks = checks_called(
        1,
        &[
            "crates/core/boss-jobs/src/lib.rs",
            "infra/gate.sh",
            ".forgejo/workflows/ci.yml",
        ],
        REAL,
    );
    assert_eq!(
        checks,
        UNITS.map(str::to_string).to_vec(),
        "an --auto gate on a car that changes nothing the web suite reads must still run every \
         web unit suite — they read ci.yml, Rust handlers, workflows and seeds, and cost ~21 s \
         — and must not run the browser"
    );
}

#[test]
fn a_gate_that_runs_the_web_suite_runs_the_units_once() {
    require_bun();
    for (auto, changed) in [
        (1, "apps/web/src/router.ts"),
        (1, "libs/web-kit/src/links.ts"),
        (0, "crates/core/boss-jobs/src/lib.rs"),
    ] {
        let checks = checks_called(auto, &[changed], REAL);
        let mut want: Vec<String> = UNITS[..3].iter().map(|s| s.to_string()).collect();
        want.push(SUITE.to_string());
        assert_eq!(
            checks, want,
            "AUTO={auto} on {changed} must run the web suite, whose test:unit already carries \
             apps/web's unit files — so not the separate `web unit` check as well"
        );
    }
}

#[test]
fn a_derivation_that_cannot_answer_runs_the_web_suite() {
    // bun absent, refusing, or printing something that is neither yes
    // nor no: the gate cannot judge, so it runs the suite rather than
    // skip it.
    for stand_in in ["return 127", "echo maybe", ":"] {
        let checks = checks_called(1, &["crates/core/boss-jobs/src/lib.rs"], stand_in);
        assert!(
            checks.iter().any(|c| c == SUITE),
            "with bun standing in as `{stand_in}`, the gate skipped the web suite on an answer \
             it never got: {checks:?}"
        );
    }
}
