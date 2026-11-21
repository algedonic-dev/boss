//! A test that sends a child a signal it can catch or ignore — TERM, INT
//! or HUP — starts that child through `boss_testing::signals`, and
//! nowhere by hand.
//!
//! WHY (backlog 5f6a2cde, after b43f68bb). GitHub's runner hands its test
//! processes an ignored or blocked INT or HUP. `env_clear` does not reset
//! a disposition, and bash can neither trap nor reset a signal ignored
//! when it starts, so a test that signals a script measured its parent
//! rather than the script: move_forgejo_data_sh read exit 0 on the
//! public mirror while the forge gates passed the same tree.
//! move_volume_replica_to_disk_sh's TERM case read red the same way when
//! run from a parent that ignores TERM (measured in the worker,
//! 2026-11-17). Every file that signals now starts its child through the
//! helper; this pin refuses the next one written without it.
//!
//! What counts as signalling, on a code line: a `kill`/`killpg` naming
//! TERM, INT or HUP, or a signal or pid held in a variable (a stub's
//! `kill -s "$STUB_SIGNAL"`, a `kill "-$1"`); a `"-TERM"`-shaped
//! argument; a `kill` program run with no signal (TERM by default). A
//! KILL or `-9` is exempt: it cannot be ignored or blocked. The grain is
//! the file, as `without_proxy`'s pin reads it.

use boss_testing::repo_root;
use regex::Regex;
use std::path::{Path, PathBuf};

const TESTS: &str = "crates/core/boss-testing/tests";

/// This file names the shapes it looks for.
const SELF: &str = "a_test_that_signals_a_child_starts_it_with_default_signals.rs";

/// Files that match a shape and signal nothing, each with why. An entry
/// that no longer matches is refused, so the list cannot keep an
/// allowance for a file that stopped needing it.
const SIGNALS_NOTHING: &[(&str, &str)] = &[(
    "the_gate_image_carries_the_native_client.rs",
    "asserts the TEXT of the gate image's build-time cancellation check \
     (a /bin/kill -TERM in its Dockerfile); it starts and signals nothing",
)];

fn shapes() -> Vec<Regex> {
    [
        r#"\bkill(pg)?\b.*(-s\s*\\?"?|-)(TERM|INT|HUP)\b"#,
        r#"\bkill\s+(-s\s+)?\\?"?-?\$"#,
        r#"\bkill(pg)?\(.*signal\.SIG(TERM|INT|HUP)\b"#,
        r#""-(TERM|INT|HUP)""#,
        r#"Command::new\("kill"\)"#,
    ]
    .iter()
    .map(|s| Regex::new(s).unwrap())
    .collect()
}

/// Every line of one file that signals a child, as `line: text`. Pure,
/// so the fixtures below hand it planted text.
fn signalling_lines(src: &str) -> Vec<String> {
    let shapes = shapes();
    let uncatchable = Regex::new(r"-KILL\b|-9\b|SIGKILL").unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        // A `kill` program's signal is on the lines its builder chains.
        let reach = if line.contains("Command::new(\"kill\")") {
            lines[i..lines.len().min(i + 4)].join("\n")
        } else {
            line.to_string()
        };
        if uncatchable.is_match(&reach) {
            continue;
        }
        if shapes.iter().any(|s| s.is_match(line)) {
            found.push(format!("{}: {}", i + 1, line.trim()));
        }
    }
    found
}

/// The file starts a child through the helper, on a code line.
fn starts_through_the_helper(src: &str) -> bool {
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .any(|l| l.contains("with_default_signals(") || l.contains("DEFAULT_SIGNALS_ARGV"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_test_that_signals_a_child_starts_it_with_default_signals() {
    let dir = repo_root().join(TESTS);
    let mut files = Vec::new();
    rust_files(&dir, &mut files);
    assert!(
        files.len() > 100,
        "the walk found {} files under {TESTS} — it is broken, so its silence means nothing",
        files.len()
    );
    let mut signalling = Vec::new();
    let mut missing = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(&path)
            .display()
            .to_string();
        if rel == SELF || SIGNALS_NOTHING.iter().any(|(f, _)| *f == rel) {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let lines = signalling_lines(&src);
        if lines.is_empty() {
            continue;
        }
        signalling.push(rel.clone());
        if !starts_through_the_helper(&src) {
            missing.push(format!("{rel}:{lines:#?}"));
        }
    }
    // The control: the files measured on 2026-11-17 are found, or the
    // shapes have drifted from how this tree signals and the silence
    // above proves nothing.
    for known in [
        "move_forgejo_data_sh.rs",
        "move_volume_replica_to_disk_sh.rs",
        "the_chore_runs_through_a_sor_that_answers_badly.rs",
    ] {
        assert!(
            signalling.iter().any(|f| f == known),
            "{known} signals a child and the walk did not see it: {signalling:#?}"
        );
    }
    assert!(
        missing.is_empty(),
        "a test signals a child it did not start with default dispositions: {missing:#?}. \
         Start the child with `boss_testing::with_default_signals(program)` (or put \
         `boss_testing::signals::DEFAULT_SIGNALS_ARGV` before it in an argv you build): \
         a runner that hands its tests an ignored or blocked signal otherwise decides \
         what the test reads (backlog 5f6a2cde). A file that matches and signals \
         nothing is named, with why, in SIGNALS_NOTHING."
    );
}

#[test]
fn every_file_allowed_to_signal_nothing_still_matches() {
    let dir = repo_root().join(TESTS);
    for (rel, why) in SIGNALS_NOTHING {
        assert!(!why.is_empty(), "{rel}: an allowance names why");
        let src = std::fs::read_to_string(dir.join(rel))
            .unwrap_or_else(|e| panic!("{rel}: {e} — an allowed file moved; follow it"));
        assert!(
            !signalling_lines(&src).is_empty(),
            "{rel} no longer matches a signalling shape, so its allowance in \
             SIGNALS_NOTHING allows nothing but the next test written without the \
             helper — delete the entry"
        );
    }
}

/// Each shape the tree uses is seen: a stub's TERM to its parent, a
/// signal held in a variable, a Rust `kill` program, Python's `killpg`.
#[test]
fn each_way_this_tree_sends_a_catchable_signal_is_seen() {
    for planted in [
        "    : > \"$S/termed\"; kill -TERM \"$PPID\"",
        "then kill -s \"$STUB_SIGNAL\" \"$PPID\"; fi",
        "        .args([\"-c\", \"kill \\\"-$1\\\" -- \\\"$2\\\"\", \"kill\", signal])",
        "    os.killpg(child.pid, signal.SIGHUP)",
        "        && Command::new(\"kill\")\n            .args([\"-INT\", &pid])",
        "        let _ = Command::new(\"kill\").arg(pid).status();",
    ] {
        assert!(!signalling_lines(planted).is_empty(), "{planted}");
        assert!(!starts_through_the_helper(planted), "{planted}");
    }
}

/// The controls: KILL and `-9` cannot be ignored or blocked, so they need
/// no helper; a comment signals nothing; and a file that does call the
/// helper passes.
#[test]
fn kill_a_comment_and_a_helped_start_pass() {
    for planted in [
        "        .args([\"-c\", \"kill -KILL -- \\\"-$1\\\"\", \"kill-group\"])",
        "        .arg(\"kill -9 \\\"-$1\\\"\")",
        "        Command::new(\"kill\")\n            .args([\"-KILL\", pid.trim()])",
        "        let _ = child.kill();",
        "// kill -TERM \"$PPID\" from the stub",
    ] {
        assert!(signalling_lines(planted).is_empty(), "{planted}");
    }
    let helped = "    let mut c = with_default_signals(\"bash\");\n    kill -TERM \"$PPID\"\n";
    assert!(starts_through_the_helper(helped));
    assert!(!starts_through_the_helper(
        "    // with_default_signals(\"bash\") is only named here\n"
    ));
}
