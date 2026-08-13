//! Integration test: parse the real design docs in the repo and
//! assert the parser's behavior matches what we expect for each
//! doc's current state.

use boss_docs::{DocStatus, parse_doc};

#[test]
fn parses_framing_doc_with_zero_open_questions() {
    // human-powered-state-machine.md is a stable fixture in the sense
    // that matters — it stays in the repo, approved, with no open
    // questions. It is NOT stable prose: the framing convergence
    // retitled it to "the execution lens" and this test, which pinned
    // the old title as a literal, went red on the train carrying that
    // rename (CI run 64). The title lived twice — in the doc and here —
    // and drifted the first time anyone edited the doc (CLAUDE.md §9a).
    //
    // So assert what this test is actually for: that the parser lifts
    // the H1 the file really has. Derive the expectation from the same
    // bytes the parser was handed, and the assertion survives every
    // rewording while still failing if the parser stops finding titles.
    let path = "../../../docs/design/human-powered-state-machine.md";
    let md = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let parsed = parse_doc("docs/design/human-powered-state-machine.md", &md);

    let h1 = md
        .lines()
        .find_map(|l| l.strip_prefix("# "))
        .expect("framing doc has an H1 heading")
        .trim();
    assert_eq!(
        parsed.title, h1,
        "parser did not lift the doc's own H1 as the title"
    );
    assert!(!parsed.title.is_empty(), "framing doc must have a title");
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
