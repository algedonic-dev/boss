//! A LEGACY STATION KEY NAMES ITS LIVE READER (backlog 8172b40d; David's
//! order 20261108T0517Z). A station row that has moved to the tree's
//! words (allocation, capacity, machines, parent) may keep a v1 key only while
//! a live reader still needs it, and the reader is NAMED. This file is
//! that naming: one table, read both ways.
//!
//! - A key the table names that no station row still carries is a stale
//!   entry: the key is retired, so drop its entry.
//! - A named reader whose line is gone from its file means the reader
//!   moved, so the key it kept alive is retired from every row that
//!   still carries it, in the same car.
//!
//! The landed review of 90c51017 found gates.toml carrying `department`
//! and `automaton` beside its v2 words with no reader named anywhere.
//! Removing them blind would break the readers below, and keeping them
//! unnamed leaves nothing to say when they may go.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A reader that keeps a legacy key alive: the file, and a line of it
/// that reads the key. The line is a code line, never a comment, so the
/// entry fails when the read moves rather than when prose is edited.
struct Reader {
    file: &'static str,
    line: &'static str,
    why: &'static str,
}

struct Legacy {
    key: &'static str,
    readers: &'static [Reader],
    retire_when: &'static str,
}

const LEGACY: &[Legacy] = &[Legacy {
    key: "automaton",
    readers: &[
        Reader {
            file: "crates/core/boss-jobs/src/step_executor.rs",
            line: "Some(b) if b.automaton != holder => refuse(",
            why: "the claim door refuses a holder that is not the station's operating automaton",
        },
        Reader {
            file: "crates/core/boss-jobs/src/station_lint.rs",
            line: "match spec.automaton.as_deref() {",
            why: "the station lint requires a station with machines to name its automaton",
        },
    ],
    retire_when: "a station's operator is read from the automaton rows that name it as their station (backlog 096a4354), so the claim check and the lint read that instead",
}];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Every station row of the platform bundle that carries `key`, by name.
fn rows_carrying(key: &str) -> Vec<String> {
    let dir = repo().join("infra/platform/stations");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the platform station rows read")
        .map(|e| e.expect("a station row entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).expect("a station row reads");
            let doc: toml::Value =
                toml::from_str(&text).unwrap_or_else(|e| panic!("{} parses: {e}", path.display()));
            doc.get("station")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|row| row.get(key).is_some())
                .map(|row| {
                    row.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("<unnamed>")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The table judged against the rows and the sources it is handed: one
/// line per entry that no longer holds. Pure, so its two failing legs are
/// pinned below on a table of their own.
fn failures(
    table: &[Legacy],
    rows_of: impl Fn(&str) -> Vec<String>,
    source_of: impl Fn(&str) -> String,
) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen = BTreeMap::new();
    for legacy in table {
        if seen.insert(legacy.key, ()).is_some() {
            failures.push(format!("`{}` is named twice", legacy.key));
        }
        if legacy.readers.is_empty() || legacy.retire_when.trim().is_empty() {
            failures.push(format!(
                "`{}` names no reader or no retirement condition",
                legacy.key
            ));
        }
        let rows = rows_of(legacy.key);
        if rows.is_empty() {
            failures.push(format!(
                "`{}` is on no station row any more: the key is retired, so drop its entry",
                legacy.key
            ));
            continue;
        }
        for reader in legacy.readers {
            let live = source_of(reader.file)
                .lines()
                .map(str::trim)
                .any(|l| !l.starts_with("//") && l == reader.line);
            if !live {
                failures.push(format!(
                    "`{}` was kept for {} ({}), whose read is gone: retire the key from {} (or name the reader that still needs it)",
                    legacy.key,
                    reader.file,
                    reader.why,
                    rows.join(", ")
                ));
            }
        }
    }
    failures
}

#[test]
fn every_legacy_station_key_names_a_live_reader_and_when_it_retires() {
    let found = failures(LEGACY, rows_carrying, |file| {
        std::fs::read_to_string(repo().join(file)).unwrap_or_else(|e| panic!("{file} reads: {e}"))
    });
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// Both failing legs, on a table of their own: a reader whose line moved
/// (or survives only as a comment) names the key and the rows to retire
/// it from, and a key no row carries names its stale entry.
#[test]
fn a_moved_reader_or_a_retired_key_is_named() {
    const TABLE: &[Legacy] = &[
        Legacy {
            key: "kept",
            readers: &[Reader {
                file: "reader.rs",
                line: "row.kept.as_deref()",
                why: "a reader",
            }],
            retire_when: "the reader moves",
        },
        Legacy {
            key: "gone",
            readers: &[Reader {
                file: "reader.rs",
                line: "row.gone",
                why: "a reader",
            }],
            retire_when: "never read",
        },
    ];
    let rows = |key: &str| {
        if key == "kept" {
            vec!["gates".to_owned()]
        } else {
            Vec::new()
        }
    };
    let held = failures(TABLE, rows, |_| {
        "fn read() {\n    row.kept.as_deref()\n}\n".into()
    });
    assert_eq!(
        held,
        vec!["`gone` is on no station row any more: the key is retired, so drop its entry"]
    );
    let moved = failures(TABLE, rows, |_| {
        "// row.kept.as_deref()\nlet a = tier(row)\n".into()
    });
    assert_eq!(moved.len(), 2, "{moved:?}");
    assert!(
        moved[0].starts_with("`kept` was kept for reader.rs (a reader), whose read is gone: retire the key from gates"),
        "{moved:?}"
    );
}
