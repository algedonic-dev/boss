//! Integration test: parse the real design docs in the repo and
//! assert the parser's behavior matches what we expect for each
//! doc's current state.

use boss_docs::{DocStatus, parse_doc};

#[test]
fn parses_framing_doc_with_zero_open_questions() {
    // human-powered-state-machine.md is the canonical framing doc —
    // approved, no implementation work, no open questions. Stable
    // fixture: it's load-bearing prose that won't get purged or
    // migrated, so the parser test stays valid as design docs come
    // and go.
    let path = "../../../docs/design/human-powered-state-machine.md";
    let md = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let parsed = parse_doc("docs/design/human-powered-state-machine.md", &md);

    assert_eq!(
        parsed.title,
        "Design: BOSS as a Human-Powered State Machine"
    );
    assert_eq!(parsed.status, DocStatus::Approved);
    assert_eq!(
        parsed.questions.len(),
        0,
        "framing doc has no open questions"
    );
    assert!(
        parsed.unresolved_questions.is_empty(),
        "expected 0 unresolved question titles, got {:?}",
        parsed.unresolved_questions
    );
}

#[test]
fn parses_correctness_protocol_doc() {
    // correctness-protocol.md is a stable Bucket-A pattern doc
    // referenced by CLAUDE.md as the load-bearing invariant
    // system. Good fixture for parser stability — it's not going
    // anywhere.
    let path = "../../../docs/design/correctness-protocol.md";
    let md = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let parsed = parse_doc("docs/design/correctness-protocol.md", &md);

    assert!(
        !parsed.title.is_empty(),
        "correctness-protocol.md must have a title heading",
    );
    assert!(
        parsed.questions.is_empty(),
        "correctness-protocol.md is a settled pattern doc; expected 0 open questions, got {:?}",
        parsed
            .questions
            .iter()
            .map(|q| &q.anchor)
            .collect::<Vec<_>>()
    );
}
