//! Every TRUNCATE in production code either proves the log complete
//! first or is on the exemption list below with its reason (design
//! b046f510, backlog d6656496).
//!
//! A truncate-and-replay run while a committed write's fact still sits
//! in `event_outbox` deletes that row and has no fact to replay it
//! from. `outbox::lock_and_assert_log_complete` refuses that run; this
//! pin is what stops the next rebuilder omitting the call — the same
//! shape backlog 1d7fb51e asks of the rebuild lock. A wipe passed to
//! the shared driver `replay::replay_projection` is covered by the
//! driver, which calls the helper itself; the pin checks that too.
//!
//! "Production code" is every `.rs` under `crates/` outside a `tests/`
//! directory, read line by line; a comment line is skipped, so a doc
//! that mentions a truncate is not one.
//!
//! tree-wide pin — it scans every crate's sources, which no changed-file
//! map can attribute to boss-events, so a scoped gate must run it anyway.

use std::path::{Path, PathBuf};

const HELPER: &str = "lock_and_assert_log_complete(";
const DRIVER: &str = "replay_projection(";
const DRIVER_FILE: &str = "crates/core/boss-events/src/replay.rs";

/// How far below a `replay_projection(` call its wipe argument may sit:
/// the pool and the lock key come first, then the wipe list.
const WIPE_ARG_SPAN: usize = 8;

/// Truncates that replay nothing from `audit_log` over rows a live
/// writer owns, each with the reason it cannot lose a committed write.
const EXEMPT: &[(&str, &str)] = &[
    (
        "crates/core/boss-search/src/rebuild.rs",
        "search_index has no live writer: an undrained fact lands one ten-minute \
         reindex late, never lost, and locking event_outbox every ten minutes \
         would stall every writer (design b046f510)",
    ),
    (
        "crates/core/boss-views/src/rebuild_event_facts.rs",
        "event_facts has no live writer and its catch-up replays past the \
         audit_log watermark, so a fact copied later lands late, never lost \
         (design b046f510)",
    ),
    (
        "crates/modules/boss-ledger/src/rebuild.rs",
        "the gl_account_daily rollup re-aggregates gl_journal_lines, a committed \
         table, not audit_log",
    ),
    (
        "crates/modules/boss-ledger/src/rebuild_payroll.rs",
        "payroll replays financial_facts, a committed table, not audit_log",
    ),
    (
        "crates/core/boss-jobs/src/postgres.rs",
        "the demo epoch reset truncates event_outbox itself and clears payroll \
         the synthesize endpoint writes directly; it replays nothing",
    ),
];

fn production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name != "tests" && name != "target" {
                production_sources(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Lines that are code, with their 1-based numbers.
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
        .map(|(i, l)| (i + 1, l))
        .collect()
}

#[test]
fn every_truncating_rebuilder_proves_the_log_complete() {
    let root = boss_testing::repo_root();
    let mut files = Vec::new();
    production_sources(&root.join("crates"), &mut files);
    files.sort();
    assert!(
        files.len() > 100,
        "the walk found {} source files under {} — it is reading the wrong tree",
        files.len(),
        root.display()
    );

    let mut offenders = Vec::new();
    let mut truncating = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(path).unwrap();
        let lines = code_lines(&text);
        let truncates: Vec<usize> = lines
            .iter()
            .filter(|(_, l)| l.contains("\"TRUNCATE "))
            .map(|(n, _)| *n)
            .collect();
        if truncates.is_empty() {
            continue;
        }
        truncating.push(rel.clone());
        if EXEMPT.iter().any(|(f, _)| *f == rel) {
            continue;
        }
        if lines.iter().any(|(_, l)| l.contains(HELPER)) {
            continue;
        }
        let driver_calls: Vec<usize> = lines
            .iter()
            .filter(|(_, l)| l.contains(DRIVER))
            .map(|(n, _)| *n)
            .collect();
        for n in truncates {
            let a_driver_wipe = driver_calls
                .iter()
                .any(|call| n > *call && n <= call + WIPE_ARG_SPAN);
            if !a_driver_wipe {
                offenders.push(format!("{rel}:{n}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these TRUNCATEs run without proving the log holds every committed \
         write — call boss_events::outbox::{HELPER}..) in the step's transaction \
         before the wipe, or add the file to EXEMPT with the reason it cannot \
         lose one (design b046f510):\n  {}",
        offenders.join("\n  ")
    );

    // An exemption outlives its reason silently unless it must still
    // name a truncating file.
    for (file, reason) in EXEMPT {
        assert!(
            truncating.iter().any(|t| t == file),
            "EXEMPT names {file} ({reason}), which no longer truncates — drop it"
        );
    }

    // The driver's wipes are covered only while the driver takes the check.
    let driver = std::fs::read_to_string(root.join(DRIVER_FILE)).unwrap();
    assert!(
        code_lines(&driver).iter().any(|(_, l)| l.contains(HELPER)),
        "{DRIVER_FILE} no longer calls {HELPER}..), so no wipe passed to \
         replay_projection proves the log complete"
    );
}
