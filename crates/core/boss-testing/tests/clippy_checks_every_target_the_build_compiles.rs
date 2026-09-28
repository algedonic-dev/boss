//! Every clippy the gate runs, and the one the builder rules hand a
//! builder, checks every target `cargo build` compiles — lib AND bins in
//! their normal cfg — not only the test harnesses.
//!
//! MEASURED 2026-09-27 (backlog 7e535a67). `--tests` checks a binary
//! target only as a test harness, where dev-dependencies resolve, so a
//! crate a bin needs as a NORMAL dependency but carries only as a
//! dev-dependency passes clippy and fails the real build. It happened on
//! a car: boss-policy-client was only a dev-dependency of
//! boss-dispatcher-handlers, rule 2's `--tests` clippy was clean, and
//! only the whole handlers suite caught it before the push. Reproduced
//! by planting `use http_body_util as _;` (a boss-clock dev-dependency)
//! in the boss-clock-api bin: `--tests` exited 0, `--all-targets`
//! refused with E0432. On a warm tree the two cost the same within
//! noise (boss-jobs after a lib.rs touch: 16-20 s either way).
//!
//! CLAUDE.md §9a: the flags are not retyped here. The `--lint` door's
//! clippy line in `infra/gate.sh` is the definition — its own comment
//! says it is "the SAME invocation the gate runs in car mode" — and this
//! test holds the gate's other two clippy lines and rule 2 of the
//! builder rules to it, so a flag changed in one place fails HERE.

use boss_testing::repo_root;

/// The flags after the package scope on every `check "clippy"` line of
/// `infra/gate.sh`, in file order: `(line, flags)`.
fn gate_clippy_flags() -> Vec<(String, String)> {
    let gate = std::fs::read_to_string(repo_root().join("infra/gate.sh")).expect("infra/gate.sh");
    let lines: Vec<(String, String)> = gate
        .lines()
        .filter(|l| l.trim_start().starts_with("check \"clippy\""))
        .map(|l| {
            let (_, rest) = l
                .split_once("cargo clippy ")
                .unwrap_or_else(|| panic!("a clippy check runs `cargo clippy`: {l}"));
            // The scope is one word: `--workspace`, `"${SCOPE[@]}"` or
            // `"${LINT_SCOPE[@]}"`; what follows it is the flag set.
            let (_, flags) = rest
                .split_once(' ')
                .unwrap_or_else(|| panic!("a clippy check names a scope, then flags: {l}"));
            (l.trim().to_string(), flags.trim().to_string())
        })
        .collect();
    assert!(
        lines.len() >= 3,
        "infra/gate.sh runs clippy in --lint, full and scoped modes; found {} lines",
        lines.len()
    );
    lines
}

#[test]
fn every_gate_clippy_checks_bins_in_their_normal_cfg() {
    for (line, flags) in gate_clippy_flags() {
        assert!(
            flags.split_whitespace().any(|f| f == "--all-targets"),
            "`{line}` must pass --all-targets: under --tests alone a bin is checked \
             as a test harness, where dev-dependencies resolve, so a bin missing a \
             normal dependency passes clippy and fails the build (backlog 7e535a67)"
        );
    }
}

#[test]
fn every_gate_clippy_runs_the_same_flags() {
    let all = gate_clippy_flags();
    let (first_line, first) = &all[0];
    for (line, flags) in &all[1..] {
        assert_eq!(
            flags, first,
            "`{line}` and `{first_line}` must run clippy with the same flags — the \
             --lint door predicts the gate only if it is the same invocation"
        );
    }
}

#[test]
fn rule_two_hands_a_builder_the_gates_clippy_flags() {
    let rules =
        std::fs::read_to_string(repo_root().join("infra/platform/documents/builder-rules.md"))
            .expect("the builder rules are readable");
    let (_, flags) = &gate_clippy_flags()[0];
    let spelled = format!("wt-cargo clippy -p <crate> {flags}");
    assert!(
        rules.contains(&spelled),
        "the builder rules must hand a builder the gate's own clippy, `{spelled}` — \
         rule 2's `--tests` spelling missed a bin's missing dependency on 2026-09-27 \
         (backlog 7e535a67)"
    );
}
