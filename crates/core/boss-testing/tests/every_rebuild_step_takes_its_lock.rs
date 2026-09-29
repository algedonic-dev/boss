//! Every `boss-rebuild-all` step takes its projection's advisory lock
//! (backlog 1d7fb51e, 2026-09-28).
//!
//! The header of `crates/orchestrators/boss-rebuild/src/main.rs` says
//! every step holds its projection's advisory lock under
//! `boss_core::rebuild::lock_key`. That sentence was false for six steps
//! until backlog 8d5ac7c5, and nothing but prose held it true after: a
//! new `step!` whose rebuilder takes no lock failed nothing. This pin
//! reads the `step!` names out of main.rs and refuses, naming the step,
//! when no `lock_key("<key>")` call under any crate's `src/` carries the
//! step's key.
//!
//! WHY A TABLE AND NOT A RENAME. Three steps lock under a key that is not
//! their step name (measured on origin/main e901e2739, run 5c847a5e):
//! `ledger-journal` under `ledger`, `search` under `search-index`,
//! `agent-runs` under `agent_runs`. Renaming the keys would make the name
//! the only definition, but an advisory lock excludes only holders of the
//! SAME key: during a rollout an old binary holding `ledger` and a new
//! one holding `ledger-journal` would both enter, and `ledger` is also
//! the key the ledger's replay verifier serializes on
//! (`boss-ledger/src/replay_check.rs`). So the mapping is one table here,
//! [`KEY_OF_STEP`], and a step absent from it locks under its own name.
//! The table is pinned too: an entry whose step main.rs no longer runs,
//! or whose key equals its step name, fails — a table nobody needs is a
//! second copy of main.rs waiting to drift (CLAUDE.md §9a).
//!
//! The triage also listed `content` -> `content-files`; that key belongs
//! to `boss_content::files::rebuild`, which rebuild-all does not run, so
//! `content` locks under its own name and needs no entry.
//!
//! tree-wide pin — it scans a tree no changed-file map can attribute
//! to this crate, so every scoped gate runs it whatever its scope
//! (`tree_wide_pins` in infra/gate.sh; backlog c87ad472).

use boss_testing::repo_root;
use std::path::{Path, PathBuf};

/// The steps whose advisory-lock key is not their step name — the ONE
/// place that says so. Every other step locks under its own name.
const KEY_OF_STEP: &[(&str, &str)] = &[
    ("ledger-journal", "ledger"),
    ("search", "search-index"),
    ("agent-runs", "agent_runs"),
];

const MAIN_RS: &str = "crates/orchestrators/boss-rebuild/src/main.rs";

/// The `step!` names in main.rs, in order: the first string literal after
/// each `step!(` (the name sits on the next line when rustfmt wraps).
fn step_names(main_rs: &str) -> Vec<String> {
    main_rs
        .split("step!(")
        .skip(1)
        .filter_map(|rest| {
            let open = rest.find('"')?;
            // Only the macro's own first argument: nothing but
            // whitespace may sit between `step!(` and the literal.
            if !rest[..open].trim().is_empty() {
                return None;
            }
            let lit = &rest[open + 1..];
            Some(lit[..lit.find('"')?].to_string())
        })
        .collect()
}

fn key_of(step: &str) -> &str {
    KEY_OF_STEP
        .iter()
        .find(|(s, _)| *s == step)
        .map_or(step, |(_, k)| k)
}

/// Every `.rs` under `crates/<tier>/<crate>/src/`.
fn crate_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    let crates = root.join("crates");
    for tier in std::fs::read_dir(&crates).expect("read crates/").flatten() {
        let Ok(members) = std::fs::read_dir(tier.path()) else {
            continue;
        };
        for member in members.flatten() {
            walk(&member.path().join("src"), &mut out);
        }
    }
    out
}

/// The keys named by a `lock_key("…")` call on a code line (comments
/// mention keys too, and a mention takes no lock).
fn lock_keys(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(|l| {
            l.split("lock_key(\"")
                .skip(1)
                .filter_map(|rest| rest.split('"').next().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What is wrong with this tree, one line per offending step or entry.
fn findings(steps: &[String], keys: &[String]) -> Vec<String> {
    let unlocked = steps.iter().filter_map(|step| {
        let key = key_of(step);
        (!keys.iter().any(|k| k == key)).then(|| {
            format!(
                "step `{step}` in {MAIN_RS}: no lock_key(\"{key}\") call under any crate's src/ — \
                 its rebuilder must take its projection's advisory lock, or KEY_OF_STEP must \
                 name the key it does take"
            )
        })
    });
    let stale = KEY_OF_STEP.iter().filter_map(|(step, key)| {
        if !steps.iter().any(|s| s == step) {
            Some(format!(
                "KEY_OF_STEP names step `{step}`, which {MAIN_RS} no longer runs — delete the entry"
            ))
        } else if step == key {
            Some(format!(
                "KEY_OF_STEP maps `{step}` to its own name — delete the entry"
            ))
        } else {
            None
        }
    });
    unlocked.chain(stale).collect()
}

#[test]
fn every_rebuild_all_step_takes_a_lock_under_its_key() {
    let root = repo_root();
    let main_rs = std::fs::read_to_string(root.join(MAIN_RS)).expect("read boss-rebuild main.rs");
    let steps = step_names(&main_rs);
    // A parser that finds nothing would pass vacuously; main.rs held 26
    // steps on 2026-09-28.
    assert!(
        steps.len() >= 20,
        "read only {} step! names from {MAIN_RS} — the parser no longer matches the file: {steps:?}",
        steps.len()
    );
    let keys: Vec<String> = crate_sources(&root)
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .flat_map(|t| lock_keys(&t))
        .collect();
    let found = findings(&steps, &keys);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_wrapped_step_name_is_read() {
    let src = "macro_rules! step {}\n step!(\"a\", f());\n step!(\n        \"b-c\",\n        g()\n    );\n";
    assert_eq!(step_names(src), vec!["a".to_string(), "b-c".to_string()]);
}

#[test]
fn a_step_whose_key_no_call_carries_is_named() {
    let steps = vec!["catalog".to_string(), "search".to_string()];
    let keys = lock_keys(
        "const K: i64 = boss_core::rebuild::lock_key(\"catalog\");\n// lock_key(\"search-index\")\n",
    );
    let found = findings(&steps, &keys);
    let unlocked: Vec<&String> = found.iter().filter(|f| f.starts_with("step ")).collect();
    // The comment's mention of search-index takes no lock; catalog's call does.
    assert_eq!(unlocked.len(), 1, "{found:?}");
    assert!(unlocked[0].contains("step `search`"), "{found:?}");
    assert!(
        unlocked[0].contains("lock_key(\"search-index\")"),
        "{found:?}"
    );
}
