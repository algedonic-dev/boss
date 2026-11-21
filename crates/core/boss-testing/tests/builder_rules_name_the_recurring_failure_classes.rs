//! The builder rules name the failure classes that cost held cars a
//! CHANGES round, so a builder checks each one before the first gate.
//!
//! MEASURED 2026-11-20 (backlog f1e9b091, approved by David). Five held
//! cars took two or three CHANGES rounds each — car D's writer
//! b17066eb, the seated pull 43a1a813, the worker reap 97683c12, the
//! ops-request 357fc272, the website seat 403eeea5 — at about an hour
//! and a half a round, and every finding fell into one of eight classes
//! that no rule stated. They live ONCE, as builder rule 21; the reviewer
//! rules point at that rule rather than copy it (CLAUDE.md §9a: a list
//! written twice drifts).
//!
//! Class 8 is the pre-flight's own gap. It REPLACES the sentence that
//! used to follow rule 4 ("A change in those directories still runs the
//! relevant targeted pins locally"), which named the infra pins and
//! missed the web unit tests and crate lib tests that read infra files
//! (136d6b5b); one statement, not two beside each other.

use boss_testing::repo_root;

const BUILDER: &str = "infra/platform/documents/builder-rules.md";
const REVIEWER: &str = "infra/platform/documents/reviewer-rules.md";

/// The class names, in rule 21's order. Each is the line's lead, so a
/// reworded test question keeps the pin green and a dropped class fails
/// it by name.
const CLASSES: [&str; 8] = [
    "UNBOUNDED RECORDS",
    "A DENIED OR EMPTY READ IS NOT A FACT",
    "LOCAL BELIEF IS NOT THE RECORD",
    "WIRE COMPATIBILITY DURING ROLLOUT",
    "LOST UPDATE",
    "IDENTITY OF WHAT YOU SIGNAL OR WRITE",
    "PARTIAL FAILURE NAMES WHAT IT LEFT",
    "THE PRE-FLIGHT DOES NOT RUN EVERY TEST THAT READS YOUR CHANGE",
];

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Rule 21: from its number at the start of a line to the end of the
/// document or the next rule's number.
fn rule_21(text: &str) -> String {
    let start = text
        .find("\n21. ")
        .unwrap_or_else(|| panic!("{BUILDER} has no rule 21 (backlog f1e9b091)"))
        + 1;
    let end = text[start..]
        .find("\n22. ")
        .map_or(text.len(), |e| e + start);
    text[start..end].to_string()
}

#[test]
fn rule_21_names_every_recurring_failure_class_with_its_test_question() {
    let rule = rule_21(&read(BUILDER));
    for (n, class) in CLASSES.iter().enumerate() {
        let lead = format!("{}. {class}: ", n + 1);
        let line = rule
            .lines()
            .find(|l| l.trim_start().starts_with(&lead))
            .unwrap_or_else(|| {
                panic!("rule 21 of {BUILDER} must carry `{lead}` as one line: {rule}")
            });
        assert!(
            line.contains('?'),
            "class {} of rule 21 asks its test question: {line}",
            n + 1
        );
    }
}

#[test]
fn class_8_replaces_the_partial_sentence_after_rule_4() {
    let text = read(BUILDER);
    assert!(
        !text.contains("still runs the relevant targeted pins locally"),
        "{BUILDER}: class 8 of rule 21 replaces the infra-pins sentence after rule 4"
    );
    let rule = rule_21(&text);
    for reader in ["web unit", "lib test"] {
        assert!(
            rule.contains(reader),
            "class 8 names the {reader} tests the pre-flight does not run (136d6b5b): {rule}"
        );
    }
}

#[test]
fn the_reviewer_rules_point_at_rule_21_and_do_not_copy_it() {
    let text = read(REVIEWER);
    assert!(
        text.contains("rule 21 of [builder-rules.md](builder-rules.md)"),
        "{REVIEWER} points at the one list of failure classes"
    );
    for class in CLASSES {
        assert!(
            !text.contains(class),
            "{REVIEWER} copies `{class}`; point at builder rule 21 instead"
        );
    }
}
