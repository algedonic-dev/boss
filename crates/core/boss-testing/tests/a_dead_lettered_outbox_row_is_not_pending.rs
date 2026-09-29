//! Every reader of the outbox's pending count uses the relay's own
//! predicate (backlog e22b692e).
//!
//! Since e4019cbc the relay sets aside a row the bus refuses
//! (`dead_lettered_at`) and never selects it again, so "pending" — rows
//! the relay still owes a publish — is `delivered_at IS NULL AND
//! dead_lettered_at IS NULL`. Three readers kept the old
//! `delivered_at IS NULL`: infra/cluster/export-log-copy.sh (twice),
//! boss-ledger-replay-check and infra/forge/tenant-census.sh. One dead
//! letter made the first two wait out their whole bound and then fail
//! naming a stuck relay — the wrong cause — and the census reported it
//! as relay lag. The replay check now calls the relay's own
//! `pending_count`, which cannot drift from itself; the shell readers
//! cannot call Rust, so this pins them (CLAUDE.md §9a): a line that
//! says `delivered_at IS NULL` must say `dead_lettered_at` too.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// Files that read `delivered_at IS NULL` for a question other than
/// "what does the relay still owe": the file, EXACTLY how many such
/// lines it holds, and the reason those lines are right. A count, not
/// a whole-file exemption (review L2): a third line added to the file
/// is a finding like any other.
const NOT_A_PENDING_COUNT: [(&str, usize, &str); 1] = [(
    "crates/core/boss-jobs/src/plugin_version_repair.rs",
    2,
    "CANDIDATES and HISTORY read staged rows NOT YET in audit_log (each with a NOT EXISTS \
     on audit_log), and a dead letter is always in audit_log — the relay logs before it \
     publishes",
)];

/// The lines of `text` that count outbox rows as pending with the old
/// predicate. A comment is not a query: the history of this very
/// defect is told in comments that quote the old predicate.
fn findings(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            !t.starts_with("//") && !t.starts_with('#')
        })
        .filter(|l| l.contains("delivered_at IS NULL") && !l.contains("dead_lettered_at"))
        .map(|l| l.trim().to_string())
        .collect()
}

fn walk(dir: &Path, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !matches!(name.as_ref(), "target" | "node_modules" | ".git") {
                walk(&path, keep, out);
            }
        } else if keep(&path) {
            out.push(path);
        }
    }
}

#[test]
fn the_old_predicate_is_a_finding_and_the_relays_is_not() {
    // The control: the exact line export-log-copy.sh carried.
    let old = r#"n=$(psql_boss "SELECT count(*) FROM event_outbox WHERE delivered_at IS NULL")"#;
    assert_eq!(findings(old).len(), 1);
    let new = "WHERE delivered_at IS NULL AND dead_lettered_at IS NULL";
    assert!(findings(new).is_empty());
    for comment in [
        "/// `delivered_at IS NULL`, so one dead letter held the check",
        "# `delivered_at IS NULL` was the old predicate",
    ] {
        assert!(findings(comment).is_empty(), "a comment is not a query");
    }
}

#[test]
fn every_outbox_reader_counts_a_dead_letter_apart_from_pending() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(
        &root.join("infra"),
        &|p| p.extension().is_some_and(|e| e == "sh"),
        &mut files,
    );
    walk(
        &root.join("crates"),
        &|p| {
            p.extension().is_some_and(|e| e == "rs")
                && p.components().any(|c| c.as_os_str() == "src")
        },
        &mut files,
    );
    assert!(
        files.len() > 100,
        "the walk found {} files — it is not reading the tree",
        files.len()
    );
    let mut bad = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let found = findings(&text);
        let allowed = NOT_A_PENDING_COUNT
            .iter()
            .find(|(f, _, _)| *f == rel)
            .map(|(_, n, _)| *n);
        match allowed {
            Some(n) if found.len() == n => {}
            Some(n) => bad.push(format!(
                "{rel}: {} lines read `delivered_at IS NULL` alone, and NOT_A_PENDING_COUNT \
                 vouches for exactly {n} — judge the new one, then fix it or the count:\n  {}",
                found.len(),
                found.join("\n  ")
            )),
            None => bad.extend(found.into_iter().map(|l| format!("{rel}: {l}"))),
        }
    }
    assert!(
        bad.is_empty(),
        "these lines count event_outbox rows as pending with `delivered_at IS NULL` alone, so \
         a dead-lettered row — one the relay will never publish — reads as relay lag. Use \
         `delivered_at IS NULL AND dead_lettered_at IS NULL` (Rust: \
         boss_events::outbox::pending_count) and name dead letters separately \
         (dead_lettered_count):\n{}",
        bad.join("\n")
    );
    // The allowlist names files that exist: a count of zero would be
    // caught above only if the file were still walked.
    for (file, _, why) in NOT_A_PENDING_COUNT {
        assert!(
            root.join(file).is_file(),
            "{file} ({why}) is allowlisted and gone — drop it from NOT_A_PENDING_COUNT"
        );
    }
}
