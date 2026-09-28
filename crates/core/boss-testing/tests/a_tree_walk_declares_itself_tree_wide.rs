//! A test that walks a top-level tree declares itself tree-wide
//! (backlog bc978312, 2026-09-26).
//!
//! A scoped gate runs the crates its changed files map to, and a test
//! that `read_dir`s `infra/`, `crates/`, `apps/` or `libs/` reads files
//! no such map can attribute to it. Car c87ad472 closed that hole for
//! the pins that SAY so — a `//! tree-wide pin` header on a `tests/`
//! file, or a `// tree-wide pin` line above one test under `src/` — and
//! `tree_wide_pins` in infra/gate.sh runs them on every scoped gate. But
//! the declaration was by hand, so the next tree-walking test without
//! it would be skipped on every scoped gate again, silently: measured
//! the day this landed, two already were (boss-testing's
//! `a_lint_that_cannot_read_does_not_say_clean.rs`, which walks every
//! shell under `infra/`, and the `test_db` unit test that walks
//! `crates/`), and the gateway's route pin — the fifth unrouted fetch,
//! cb2157f9 — was the same class in another crate.
//!
//! WHAT COUNTS AS A WALK. A file that calls `read_dir` (or `WalkDir`)
//! AND joins a top-level directory literal onto a base —
//! `.join("infra")`, `.join("crates/")`, `.join("../../../apps")`.
//! Measured over every `.rs` under `crates/` on 2026-09-26: 15 files
//! matched, 12 already marked, two true misses (above) and ONE file that
//! reads `crates/` only to find a single named crate's directory
//! (gate_sh.rs), which opts out with its reason. The broader shape — any
//! DIRECTORY literal under those trees, `infra/ops/verbs` and the like —
//! matched 56 files and is mostly tests of the directory they sit beside,
//! so it is not the rule; a subtree walk that crosses crates is marked
//! by hand, as the gateway's `apps/web/src` pin is.
//!
//! THE OPT-OUT is a line `// not a tree-wide pin: <reason>` in the file,
//! and the reason is required: an opt-out nobody can read is a skip.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The top-level trees whose files no scoped gate maps back to a test
/// that reads them.
const TREES: [&str; 4] = ["infra", "crates", "apps", "libs"];

/// Where the opt-out's reason starts.
const OPT_OUT: &str = "// not a tree-wide pin:";

/// What a file says about itself.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Walks no top-level tree.
    NoWalk,
    /// Walks one, and declares itself tree-wide.
    Marked,
    /// Walks one, and says why it is not a tree-wide pin.
    OptedOut,
    /// Walks one and says nothing — the hole this file exists to close.
    Silent,
    /// Opts out with no reason.
    ReasonlessOptOut,
}

/// Does this code line join a top-level tree onto a base? The literal
/// after `.join("`, with any `../` climbing to the root stripped and one
/// trailing `/` allowed, is exactly one of `TREES`.
fn joins_a_top_level_tree(line: &str) -> bool {
    line.split(".join(\"").skip(1).any(|rest| {
        let lit = rest.split('"').next().unwrap_or("");
        let mut lit = lit;
        while let Some(up) = lit.strip_prefix("../") {
            lit = up;
        }
        let lit = lit.strip_suffix('/').unwrap_or(lit);
        TREES.contains(&lit)
    })
}

fn classify(text: &str) -> Verdict {
    let code: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let reads_dirs = code
        .iter()
        .any(|l| l.contains("read_dir") || l.contains("WalkDir"));
    if !reads_dirs || !code.iter().any(|l| joins_a_top_level_tree(l)) {
        return Verdict::NoWalk;
    }
    let marked = text.lines().any(|l| {
        l.starts_with("//! tree-wide pin") || l.trim_start().starts_with("// tree-wide pin")
    });
    if marked {
        return Verdict::Marked;
    }
    match text
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(OPT_OUT))
    {
        Some(reason) if reason.trim().len() >= 10 => Verdict::OptedOut,
        Some(_) => Verdict::ReasonlessOptOut,
        None => Verdict::Silent,
    }
}

/// Every `.rs` under `crates/*/*/tests/` and `crates/*/*/src/`, the two
/// places a test lives.
fn test_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    for tier in std::fs::read_dir(root.join("crates"))
        .expect("crates/ is readable")
        .filter_map(Result::ok)
    {
        for krate in std::fs::read_dir(tier.path())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            for part in ["src", "tests"] {
                walk(&krate.path().join(part), &mut files);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn the_classifier_tells_a_silent_walk_from_a_declared_one() {
    let walk = "fn t() {\n    for e in std::fs::read_dir(root.join(\"infra\")) {}\n}\n";
    assert_eq!(classify(walk), Verdict::Silent);
    assert_eq!(
        classify(&format!("//! tree-wide pin — reasons\n{walk}")),
        Verdict::Marked
    );
    assert_eq!(
        classify(&format!("    // tree-wide pin — reasons\n{walk}")),
        Verdict::Marked
    );
    assert_eq!(
        classify(&format!(
            "{OPT_OUT} reads crates/ only to find one crate by name\n{walk}"
        )),
        Verdict::OptedOut
    );
    assert_eq!(
        classify(&format!("{OPT_OUT}\n{walk}")),
        Verdict::ReasonlessOptOut
    );
    // Climbing to the root is the same tree; a trailing slash too.
    assert_eq!(
        classify("let d = std::fs::read_dir(m.join(\"../../../apps/\"));\n"),
        Verdict::Silent
    );
    // A subdirectory is not a top-level tree, a walk in a comment is
    // prose, and a join with no directory read is not a walk.
    assert_eq!(
        classify("let d = std::fs::read_dir(root.join(\"infra/lint\"));\n"),
        Verdict::NoWalk
    );
    assert_eq!(
        classify("// std::fs::read_dir(root.join(\"crates\"))\n"),
        Verdict::NoWalk
    );
    assert_eq!(
        classify("let p = root.join(\"crates\");\n"),
        Verdict::NoWalk
    );
}

#[test]
fn every_test_that_walks_a_top_level_tree_declares_itself_tree_wide() {
    let root = repo_root();
    let files = test_sources(&root);
    assert!(
        files.len() > 500,
        "found only {} .rs files under crates/*/*/{{src,tests}} — the walk broke, and a \
         walk that finds nothing pins nothing",
        files.len()
    );
    let mut walks = 0;
    let mut offences = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
        let rel = path.strip_prefix(&root).unwrap_or(path).display();
        match classify(&text) {
            Verdict::NoWalk => {}
            Verdict::Marked | Verdict::OptedOut => walks += 1,
            Verdict::Silent => {
                walks += 1;
                offences.push(format!(
                    "{rel}: walks a top-level tree and does not say whether it is tree-wide"
                ));
            }
            Verdict::ReasonlessOptOut => {
                walks += 1;
                offences.push(format!("{rel}: `{OPT_OUT}` with no reason after it"));
            }
        }
    }
    // The control: the pins marked on the day this landed walk a
    // top-level tree, so a classifier that stopped seeing walks would
    // pass every file about nothing.
    assert!(
        walks >= 10,
        "only {walks} file(s) classified as walking a top-level tree — 15 did when this \
         landed, so the classifier has gone blind"
    );
    assert!(
        offences.is_empty(),
        "a test that walks infra/, crates/, apps/ or libs/ reads files no changed-file map \
         attributes to its crate, so a scoped gate skips it unless it says so (backlog \
         bc978312). Put `//! tree-wide pin — <why>` in a tests/ file's header, or \
         `// tree-wide pin — <why>` on the line above one test under src/, and every scoped \
         gate runs it (`tree_wide_pins` in infra/gate.sh); or, if its verdict cannot change \
         with a file outside its crate, `{OPT_OUT} <why>`.\n  {}",
        offences.join("\n  ")
    );
}
