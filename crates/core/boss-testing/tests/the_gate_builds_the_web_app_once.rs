//! The gate installs and builds the web app ONCE, and each of those is a
//! timed check on the receipt (backlog efd7f75c).
//!
//! MEASURED 2026-10-15 on origin/main@e0f709f77. `infra/gate-runner/run.sh`
//! ran a warm-up before the gate —
//! `(cd apps/web && bun install --frozen-lockfile >/dev/null 2>&1 && bun run build >/dev/null 2>&1) || true`
//! — and `infra/gate.sh` then installed again (`check "web install"`) and
//! built again (`check "web-suite (unit+build+mocked)"`). The warm-up's
//! output went to /dev/null and its exit was discarded, so the record held
//! neither what it took nor whether it failed: a reduction before storing
//! (CLAUDE.md §Diagnosis). It also installed WITHOUT
//! `PUPPETEER_SKIP_DOWNLOAD`, the one flaky download every other install
//! in the gate skips.
//!
//! Its reason (2026-08-23: a cold `bun src/dev-server.ts` bundle overran
//! the mocked suite's webServer boot timeout) is held elsewhere now, by
//! the "durable fix" its own comment pointed to: `tests/run-mocked.ts`
//! owns readiness with a 240 s deadline it controls (backlog 63a242ca),
//! and the suite's own build runs, timed, immediately before
//! `test:mocked`, so the page cache is warm when the dev-server bundles.
//!
//! So the pin is on what RUNS: across the runner and the gate, the web
//! app's install and its build each appear once, and each is the command
//! of a `check` — which is what puts its duration and its exit on the
//! receipt.

use boss_testing::repo_root;

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The lines of a shell file that run: comment lines dropped, because
/// the comments explaining this very history name the commands.
fn running_lines(rel: &str) -> Vec<(String, String)> {
    read(rel)
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .map(|(i, l)| (format!("{rel}:{}", i + 1), l.to_string()))
        .collect()
}

/// Every running line, in the runner and the gate, that does `what` in
/// `apps/web`.
fn web_lines(what: &str) -> Vec<(String, String)> {
    ["infra/gate-runner/run.sh", "infra/gate.sh"]
        .iter()
        .flat_map(|rel| running_lines(rel))
        .filter(|(_, l)| l.contains("cd apps/web") && l.contains(what))
        .collect()
}

fn assert_once_and_checked(what: &str) {
    let found = web_lines(what);
    assert_eq!(
        found.len(),
        1,
        "the web app's `{what}` must run exactly once per gate, as a timed check; found {}:\n{}",
        found.len(),
        found
            .iter()
            .map(|(at, l)| format!("  {at}: {}", l.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (at, line) = &found[0];
    assert!(
        line.trim_start().starts_with("check \""),
        "the web app's `{what}` at {at} is not the command of a `check`, so the receipt \
         holds neither its time nor its failure: {}",
        line.trim()
    );
}

#[test]
fn the_web_app_is_installed_once_and_the_install_is_on_the_receipt() {
    assert_once_and_checked("bun install");
}

#[test]
fn the_web_app_is_built_once_and_the_build_is_on_the_receipt() {
    assert_once_and_checked("bun run build");
}

#[test]
fn the_runner_runs_no_bun_of_its_own() {
    // Anything the runner does with bun happens before gate.sh starts
    // its receipt, so nothing it does could be timed or judged there.
    let ran: Vec<String> = running_lines("infra/gate-runner/run.sh")
        .into_iter()
        .filter(|(_, l)| {
            let t = l.trim_start();
            t.contains("bun install")
                || t.contains("bun run ")
                || t.starts_with("bun ")
                || t.contains("&& bun ")
                || t.contains("(cd apps/web")
        })
        .map(|(at, l)| format!("  {at}: {}", l.trim()))
        .collect();
    assert!(
        ran.is_empty(),
        "infra/gate-runner/run.sh runs bun outside infra/gate.sh's checks:\n{}",
        ran.join("\n")
    );
}
