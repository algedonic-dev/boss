//! A step plugin writes and reads only statuses a step can have.
//!
//! Backlog e639899a (3), the adversarial review of claim-door car 2
//! (6ef4a36b, 2026-09-27). `StepStatus` is kebab-case with no aliases —
//! pending, ready, active, completed, skipped — and three plugins spoke
//! another vocabulary: `checklist.js` completed with `'done'`,
//! `diagnostic-call.js` waived with `'waived'` and closed with `'done'`,
//! and `sr-triage.js` completed with `'done'`. The step PUT answers each
//! with 400 before any handler code runs (serde refuses the body), so
//! those buttons could never succeed, and the read side (`isDone =
//! step.status === 'done'`) never saw a finished step as finished. The
//! plugin README taught the same word (`status: "done"` to complete),
//! which is where a new plugin copies it from, so the README is scanned
//! with the bundles.
//!
//! THE RULE, over `infra/step-plugins/`: a quoted word that a status is
//! written as (`status: '…'`, `save('…')`) or compared to (`status ===
//! '…'`) is one of `StepStatus`'s wire names, derived from the enum
//! itself, so a status added there is admitted here without an edit
//! (CLAUDE.md §9a). The web's own step surfaces are typed against
//! `StepStatus` in TypeScript and svelte-check refuses a stray word
//! there; plugins are plain JS, and nothing else reads them.
//!
//! And a plugin's Save-draft keeps the claim (backlog d88d9601 (a)):
//! a draft sent the snapshot's `status` and `assignee_id`, which is the
//! release body when someone claimed the step after the page loaded.
//! The fix landed with car 6ef4a36b (both bundles dropped the drawn
//! status and holder before the PUT), and a test here held it. It was
//! retired with backlog e39a9d2a (Stage 2 car 3), which took the rule a
//! step further: a draft is now a merge-door PATCH alone, and every
//! step PUT a bundle sends carries the status and nothing else, so no
//! holder can ride one at all.
//! `apps/web/src/steps/a-step-plugin-put-carries-no-metadata.test.ts`
//! holds that, over every bundle and this directory's README.

use boss_core::job::StepStatus;
use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// Every status a step can have, spelled as the API spells it — read
/// off the enum's own serialisation. The match has no wildcard, so a
/// variant added to `StepStatus` fails to compile here until it is
/// listed, and is then admitted by its own wire name.
fn step_statuses() -> Vec<String> {
    use StepStatus::{Active, Completed, Pending, Ready, Skipped};
    [Pending, Ready, Active, Completed, Skipped]
        .into_iter()
        .map(|s| {
            match s {
                Pending | Ready | Active | Completed | Skipped => {}
            }
            serde_json::to_value(s)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| panic!("{s:?} does not serialise to a string"))
        })
        .collect()
}

/// A quoted word in a status position: written (`status: '…'`,
/// `status = '…'`, `save('…')`) or compared (`status === '…'`).
fn status_literal() -> Regex {
    Regex::new(r#"(?:\bstatus\s*(?:===|!==|==|!=|:|=)\s*|\bsave\(\s*)['"]([a-z][a-z_-]*)['"]"#)
        .unwrap()
}

/// The lines of `text` whose status literals name no step status.
fn stray_statuses(text: &str, statuses: &[String]) -> Vec<(usize, String)> {
    let re = status_literal();
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            re.captures_iter(line)
                .any(|c| !statuses.iter().any(|s| s == &c[1]))
        })
        .map(|(i, line)| (i + 1, line.trim().to_string()))
        .collect()
}

/// The plugin bundles and their README, in a stable order.
fn plugin_files() -> Vec<PathBuf> {
    let dir = repo_root().join("infra/step-plugins");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "js" || e == "md")
        })
        .collect();
    files.sort();
    // A wrong path answers "clean" instead of erroring (CLAUDE.md
    // §Doors), so an empty scope is a failure, not a pass.
    assert!(
        files
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "js")),
        "{} holds no .js bundle — the scope moved",
        dir.display()
    );
    files
}

fn rel(path: &Path) -> String {
    let root = repo_root();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn every_status_a_step_plugin_names_is_a_step_status() {
    let statuses = step_statuses();
    let mut offenders = Vec::new();
    for file in plugin_files() {
        let text = std::fs::read_to_string(&file).unwrap();
        for (line, src) in stray_statuses(&text, &statuses) {
            offenders.push(format!("{}:{line}: {src}", rel(&file)));
        }
    }
    assert!(
        offenders.is_empty(),
        "a step's status is one of {statuses:?} — the step PUT refuses any other word \
         with 400 (StepStatus has no aliases: complete with 'completed', waive with \
         'skipped'):\n  {}",
        offenders.join("\n  ")
    );
}

/// The scan can fail: each shape the stray words took is caught, and
/// each status the API knows is not.
#[test]
fn the_scan_catches_every_shape_a_stray_status_took() {
    let statuses = step_statuses();
    assert_eq!(
        statuses,
        ["pending", "ready", "active", "completed", "skipped"],
        "the wire names are the enum's own"
    );
    for stray in [
        "body: JSON.stringify({ ...fresh, job_id: jobId, status: 'done' }),",
        "completeBtn.addEventListener('click', () => save('done'));",
        "waiveBtn.addEventListener('click', () => save('waived'));",
        "const isDone = step.status === 'done' || step.status === 'waived';",
        "ended_at: status === 'done'",
        "yourself (set `status: \"done\"` to complete the step), then call",
        "// ask the host to refetch. Set status='done' to complete.",
    ] {
        assert_eq!(
            stray_statuses(stray, &statuses).len(),
            1,
            "not caught: {stray}"
        );
    }
    for known in [
        "body: JSON.stringify({ ...fresh, job_id: jobId, status: 'completed' }),",
        "waiveBtn.addEventListener('click', () => save('skipped'));",
        "const isDone = step.status === 'completed' || step.status === 'skipped';",
        "saveDraftBtn.addEventListener('click', () => save(null));",
        "if (step.status !== 'ready') return;",
    ] {
        assert!(
            stray_statuses(known, &statuses).is_empty(),
            "flagged a step status: {known}"
        );
    }
}
