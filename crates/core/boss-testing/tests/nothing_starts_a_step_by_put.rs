//! Nothing in the tree's step writers moves a step to `active` by hand.
//!
//! Design 611fbffd ("A step becomes Active only through a claim",
//! answered by David 2026-09-26), clause (b) of backlog 6ef4a36b item
//! (2). Ten web step surfaces (Generic, Intake, Scheduling, Handoff,
//! Inspection, Procurement, Receiving, Repair, ProductionConsume,
//! Shipment), four step plugins (checklist, review-design, sr-triage,
//! diagnostic-call) and the sim's workforce took a step to Active with a
//! step PUT carrying `status: active` — so the record could not tell "X
//! took this work" from "someone assigned X and started the clock", and
//! "release, then claim" held only as far as every writer chose to
//! honour it. They now start through the claim door (`startStep` in
//! `apps/web/src/steps/stepWrite.ts`; `Workforce::post_claim` in the
//! sim), and a plugin's save no longer flips a pending step at all.
//!
//! This pin is what lets the step PUT refuse the move by name (the
//! second half of clause b): it holds the writers to that, so the
//! refusal cannot strand a caller the tree still carries. THE RULE, over
//! the step writers below: the quoted word `active` appears only as the
//! thing a status is COMPARED to (`=== 'active'`, `!= "active"`, a Rust
//! match arm `"active" =>`, a JS `case 'active':`) — never as a value
//! written, passed or returned. That is coarse on purpose: a step writer
//! has no other reason to spell the word.
//!
//! The Rust callers outside this scope already claim through the door
//! — `boss dispatch`, the car's build claim (`boss_jobs::car`) and the
//! ops runner (`infra/ops/ops-runner.sh`) — and their fixtures spell
//! `"status": "active"` as stored rows, which a word scan cannot tell
//! from a write; the server-side refusal is what judges them.

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// The step writers: where a step's status is written from.
const ROOTS: [(&str, &[&str]); 3] = [
    ("apps/web/src/steps", &["ts", "svelte"]),
    ("infra/step-plugins", &["js"]),
    ("crates/orchestrators/boss-sim/src", &["rs"]),
];

/// A quoted `active` in a comparison: after `===`/`!==`/`==`/`!=`, after
/// `case `, or before a Rust match arm's `=>`.
fn allowed() -> Regex {
    Regex::new(r#"(?:(?:===|!==|==|!=)\s*|case\s+)['"]active['"]|['"]active['"]\s*(?:\|[^=]*)?=>"#)
        .unwrap()
}

fn quoted() -> Regex {
    Regex::new(r#"['"]active['"]"#).unwrap()
}

/// The lines of `text` that spell `active` other than as a comparison.
fn offending_lines(text: &str) -> Vec<(usize, String)> {
    let (all, ok) = (quoted(), allowed());
    text.lines()
        .enumerate()
        .filter(|(_, line)| all.find_iter(line).count() > ok.find_iter(line).count())
        .map(|(i, line)| (i + 1, line.trim().to_string()))
        .collect()
}

/// A test file is not a writer: `*.test.ts`, `*.testkit.ts`, and a Rust
/// file's `#[cfg(test)]` tail, whose fixtures spell stored rows.
fn writer_text(path: &Path, text: &str) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".test.ts") || name.ends_with(".testkit.ts") {
        return None;
    }
    if name.ends_with(".rs") {
        return Some(text.split("#[cfg(test)]").next().unwrap_or("").to_string());
    }
    Some(text.to_string())
}

fn files_under(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, exts, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

#[test]
fn no_step_writer_moves_a_step_to_active_by_hand() {
    let root = repo_root();
    let mut offenders = Vec::new();
    for (dir, exts) in ROOTS {
        let mut files = Vec::new();
        files_under(&root.join(dir), exts, &mut files);
        // A wrong path answers "clean" instead of erroring (CLAUDE.md
        // §Doors), so an empty scope is a failure, not a pass.
        assert!(
            !files.is_empty(),
            "{dir} holds no .{exts:?} file — the scope moved"
        );
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            let Some(writer) = writer_text(&file, &text) else {
                continue;
            };
            for (line, src) in offending_lines(&writer) {
                let rel = file.strip_prefix(&root).unwrap_or(&file).display();
                offenders.push(format!("{rel}:{line}: {src}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a step becomes Active only through the claim door (design 611fbffd) — \
         start it with startStep (apps/web/src/steps/stepWrite.ts) or POST \
         /api/jobs/{{id}}/steps/{{step_id}}/claim, never a status of `active`:\n  {}",
        offenders.join("\n  ")
    );
}

/// The scan can fail: each shape a Start took before this car is caught,
/// and each comparison the writers legitimately make is not.
#[test]
fn the_scan_catches_every_shape_a_start_took() {
    for written in [
        "onclick={() => persist('active')}",
        "onclick={() => save('active')}",
        "persist(isPending(step.status) ? 'active' : undefined)",
        "body: JSON.stringify({ job_id: jobId, status: 'active' }),",
        "if (step.status === 'pending') await putStep({}, 'active');",
        "save(step.status === 'pending' ? 'active' : step.status)",
        r#"&json!({ "status": "active", "assignee_id": emp }),"#,
    ] {
        assert_eq!(offending_lines(written).len(), 1, "not caught: {written}");
    }
    for compared in [
        "{#if !terminal && step.status === 'active'}",
        "return step.status === 'active' && (step.assignee_id ?? '').trim() !== '';",
        r#"            "active" => {"#,
        r#"if status != "active" {"#,
        "case 'active':",
        "(step.status === 'ready' || step.status === 'active'),",
    ] {
        assert!(
            offending_lines(compared).is_empty(),
            "flagged a comparison: {compared}"
        );
    }
}

/// A Rust writer's test tail is fixtures, not writes; its body is not.
#[test]
fn a_rust_writers_test_tail_is_not_scanned() {
    let src = "fn claim() {}\n#[cfg(test)]\nmod tests { const ROW: &str = r#\"{\"status\": \"active\"}\"#; }\n";
    let body = writer_text(Path::new("workforce.rs"), src).unwrap();
    assert!(offending_lines(&body).is_empty(), "{body}");
    assert_eq!(offending_lines(src).len(), 1);
    assert!(writer_text(Path::new("holder.test.ts"), "status: 'active'").is_none());
}
