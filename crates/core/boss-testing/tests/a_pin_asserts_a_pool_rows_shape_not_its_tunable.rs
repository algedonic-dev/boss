//! A PIN NEVER ASSERTS A TUNABLE VALUE (backlog f399458f ask 3, David
//! 2026-11-08; CLAUDE.md §9a — a fact that lives twice gets an equality
//! test, and a tunable that lives twice is the fact in question).
//!
//! A pool amount — `robots:opus`'s 6 slots, `robots:sol`'s 2 — is a
//! tunable: it lives in its row, `infra/platform/pools/<name>.toml`, and
//! since the live pools door (ask 1 of the same item) it changes through
//! `boss pool publish` with no code car at all. Until this pin, one Rust
//! test read the real bundle and asserted the NUMBER —
//! `crates/core/boss-jobs/tests/station_tree.rs` held
//! `[("robots:opus", "6"), ("robots:sol", "2")]` — so the value lived
//! twice, and raising it (item 89b17d21, 4 -> 6) had to ride a code car,
//! a gate and a train to keep the pin green. A publish through the door
//! would have left main's test suite red for a change that carried no
//! code. The test now asserts the SHAPE: configured, a positive integer,
//! the models the row declares, the pool under its station.
//!
//! WHAT THIS PIN READS. Every Rust file under `crates/` that reads the
//! real pool bundle — it names `platform_pools_path()` or calls
//! `load_pools(` — is a reader of the tree's tunables. In such a file no
//! line may pair a pool's NAME with a QUOTED number — `("robots:opus",
//! "6")` — which is how an amount is spelled on the wire and the one
//! shape the tree had. Two near shapes are deliberately not read, each
//! measured on the first run of this pin: `pool("robots:door", 1)` pairs
//! a name with a VERSION, an unquoted integer (resource_pools.rs, eight
//! lines); and `["amount"], "2"` with no pool name on its line compares
//! a fixture's own amount in a file that also loads the bundle for
//! another test (claims_console_history.rs:2009), which a line cannot
//! tell from a pin. A fixture that builds its own rows may say `row(2,
//! "6")` as it likes: that 6 is the fixture's, not the row's.
//!
//! tree-wide pin — it reads every Rust source under crates/, which no
//! changed-file map attributes to this crate.

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

/// A pool's name beside a quoted number: `("robots:opus", "6")`.
fn tunable_literal() -> Regex {
    Regex::new(r#"\("[a-z_]+:[a-z0-9_.-]+",\s*&?"[0-9]+"\)"#).expect("the pattern compiles")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The lines of `src` that pin a tunable, each as `line: text`, with
/// comment lines skipped: a comment that says "reads 6 since David
/// raised it" is prose about the row, not an assertion of it.
fn tunable_lines(src: &str) -> Vec<String> {
    let re = tunable_literal();
    src.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| re.is_match(line))
        .map(|(n, line)| format!("{}: {}", n + 1, line.trim()))
        .collect()
}

#[test]
fn no_reader_of_the_pool_bundle_asserts_an_amount() {
    let root = repo_root();
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    let mut found = Vec::new();
    // This file names `load_pools(` in prose and carries the retired
    // line as its control, so it is not a reader.
    let own = Path::new(file!())
        .file_name()
        .expect("this file has a name");
    for path in files.into_iter().filter(|p| p.file_name() != Some(own)) {
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if !(src.contains("platform_pools_path()") || src.contains("load_pools(")) {
            continue;
        }
        for line in tunable_lines(&src) {
            found.push(format!(
                "{}:{line}",
                path.strip_prefix(&root).unwrap_or(&path).display()
            ));
        }
    }
    assert!(
        found.is_empty(),
        "a test that reads the real pool bundle asserts a tunable's VALUE; assert its shape \
         (configured, a positive integer, the models the row declares) and leave the number to \
         infra/platform/pools/<name>.toml, where `boss pool publish` changes it (backlog \
         f399458f, CLAUDE.md §9a):\n  {}",
        found.join("\n  ")
    );
}

/// The control, in the shape the tree actually had: station_tree.rs:995
/// as origin/main 59fe53092 spelled it, and a call that pins one name
/// to one quoted number. Both are caught; the shape assertions that
/// replace them, a comment that names the number, a name beside its
/// version, and a host's cpu count (an estate fact, no colon) are not.
#[test]
fn the_pattern_catches_the_line_the_tree_had_and_not_its_replacement() {
    let had = r#"    for (name, amount) in [("robots:opus", "6"), ("robots:sol", "2")] {"#;
    let call = r#"        assert_amount("robots:sol", "2");"#;
    let shape = r#"        assert_eq!(row["pool"]["capacity"]["state"], "configured", "{row}");
        assert!(amount > 0, "{row}");
        assert_eq!(models, spec.models, "{row}");
    // robots:opus reads 6 since David raised it on 2026-11-08 (backlog 89b17d21).
    let mut second = pool("robots:door", 2);
    let served: Vec<(String, i64)> = [("boss-gcp", 4), ("cp-1", 8)].into_iter().collect();"#;
    assert_eq!(tunable_lines(had).len(), 1, "{had}");
    assert_eq!(tunable_lines(call).len(), 1, "{call}");
    assert_eq!(tunable_lines(shape), Vec::<String>::new());
}
