//! No test reads the process's live machine token: outside
//! `machine_token.rs`'s own tests, no `#[cfg(test)]` item and no file
//! under a `tests/` directory names `machine_token::shared` or
//! `Source::watch` (backlog 2ee29275, F2).
//!
//! WHAT WAS MEASURED (review of car d36d3478, run d356afce, 2026-09-29).
//! 63 test sites in boss-cli built their client with
//! `gate::machine_client()`, which read `machine_token::shared()` — the
//! process-wide watched source over the mounted Secret. A failing test
//! prints the request head it captured, so the day design 6805c764 car 4
//! mounts the Secret where a gate runs, the estate token would be in the
//! gate's log. The fix sits inside `gate::machine_client` itself (under
//! `cfg(test)` it holds `Source::fixed(None)`), so none of the 63 needed
//! an edit; this pin holds the opposite line — a test that reaches for
//! the shared source BY NAME is refused, naming its file and line.
//!
//! WHAT IT DOES NOT HOLD, said so it is not assumed. It is a NAME scan:
//! a test that builds a client through a production constructor that
//! reads the shared source inside it (`machine_token::Client::build`,
//! the gateway's `MachineClient::build`) is not seen here — that is why
//! boss-cli's constructor switches under `cfg(test)` rather than relying
//! on this pin. A `#[cfg(test)] mod x;` declaration's file is not
//! followed. Comments are stripped first, so a note naming the source
//! is not a read of it.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::production_source::{production_text, without_comments};
use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// The one file whose tests may read the shared source: they are the
/// tests OF it, and they watch scratch dirs, never the mount.
const OWN_TESTS: &str = "crates/core/boss-core/src/machine_token.rs";

/// This pin's own file, whose fixtures spell what it refuses.
const SELF: &str = "crates/core/boss-testing/tests/no_test_reads_the_live_machine_token.rs";

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The ways code names the shared source: the path (a call, or a `use`
/// of it), a grouped `use machine_token::{…, shared, …}`, and a new
/// watcher over any dir (`Source::watch`, `Source::watch_every`).
fn readers() -> Vec<Regex> {
    [
        r"machine_token::shared\b",
        r"machine_token::\{[^}]*\bshared\b",
        r"\bSource::watch",
    ]
    .iter()
    .map(|r| Regex::new(r).unwrap())
    .collect()
}

/// The 1-based lines of `src` that are TEST code and name the shared
/// source. `whole_file_is_test` for a file under a `tests/` directory.
fn reads_in_tests(src: &str, whole_file_is_test: bool) -> Result<Vec<usize>, syn::Error> {
    let code = without_comments(src);
    let prod = if whole_file_is_test {
        String::new()
    } else {
        production_text(&code)?
    };
    let prod_lines: Vec<&str> = prod.lines().collect();
    let res = readers();
    Ok(code
        .lines()
        .enumerate()
        // A test line is one production_text blanked.
        .filter(|(i, l)| prod_lines.get(*i).copied().unwrap_or("") != *l)
        .filter(|(_, l)| res.iter().any(|r| r.is_match(l)))
        .map(|(i, _)| i + 1)
        .collect())
}

fn offenders(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(&root.join("crates"), &mut files);
    let mut out: Vec<String> = files
        .iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            if rel == OWN_TESTS || rel == SELF {
                return None;
            }
            let src = std::fs::read_to_string(p).ok()?;
            let in_tests_dir = rel.split('/').any(|c| c == "tests");
            let lines = reads_in_tests(&src, in_tests_dir)
                .unwrap_or_else(|e| panic!("{rel} does not parse as Rust: {e}"));
            (!lines.is_empty()).then(|| format!("{rel}:{lines:?}"))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn no_test_reads_the_live_machine_token() {
    let found = offenders(&repo_root());
    assert!(
        found.is_empty(),
        "these tests read the process's live machine token — the shared source over the \
         mounted Secret — so a failing one prints the estate token into the gate's log \
         (backlog 2ee29275). Build the client with \
         `build_with_source(builder, Arc::new(Source::fixed(None)))`, or a fixed test value:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_scan_sees_a_test_read_and_not_a_production_one() {
    // Controls: a scan that saw nothing would pass the pin on any tree.
    let shared = concat!("machine_token::", "shared()");
    let watch = concat!("Source::", "watch(dir)");
    let grouped = concat!("use boss_core::machine_token::{Client, ", "shared};");
    let file = format!(
        "fn prod() {{ let _ = {shared}; }}\n\
         #[cfg(test)]\n\
         mod tests {{\n\
         \x20   {grouped}\n\
         \x20   fn t() {{ let _ = {shared}; }}\n\
         \x20   fn u() {{ let _ = {watch}; }}\n\
         \x20   // {shared} in a comment is not a read\n\
         }}\n"
    );
    assert_eq!(reads_in_tests(&file, false).unwrap(), vec![4, 5, 6]);
    // Under tests/, every line is test code.
    assert_eq!(reads_in_tests(&file, true).unwrap(), vec![1, 4, 5, 6]);
    // A fixed source is not a read of the live one.
    assert!(
        reads_in_tests(
            "#[cfg(test)]\nmod t { fn f() { let _ = Source::fixed(None); } }\n",
            false
        )
        .unwrap()
        .is_empty()
    );
}
