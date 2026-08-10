//! The `design-review-spawn` rule (migration 107) — reviews create
//! themselves (dogfooding arc `e556c000`, S2).
//!
//! Pins the exact `when` the migration ships against the expr engine:
//! flat identifiers over the S1 `docs.design.indexed` payload
//! (`open_questions`, `path` — the binder resolves no dotted paths),
//! and the `open_review_exists` dedup helper gating re-fires. Every
//! review Job before this rule was opened by hand, including one
//! opened before its doc existed.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::registry::{Registry, match_event};

const RULE: &str = r#"
[[rule]]
name = "design-review-spawn"
on_event = "docs.design.indexed"
when = "open_questions > 0 AND NOT open_review_exists(path)"
[[rule.do]]
handler = "jobs.spawn"
args = { kind = "\"design-doc-review\"", subject_kind = "\"custom\"", subject = "path", title = "title", "metadata.doc_path" = "path", "metadata.doc_title" = "title" }
"#;

/// `open_review_exists` answering a fixed value; records nothing else.
struct StubReviews(bool);

impl HelperResolver for StubReviews {
    fn call(&self, name: &str, _args: &[Value]) -> Result<Value, EvalError> {
        match name {
            "open_review_exists" => Ok(Value::Bool(self.0)),
            other => Err(EvalError::UnknownHelper(other.to_string())),
        }
    }
}

fn indexed_payload(open: i64) -> serde_json::Value {
    serde_json::json!({
        "path": "docs/design/idm-kanidm.md",
        "title": "Kanidm IDM — the front door",
        "status": "draft",
        "open_questions": open,
        "resolved_questions": 0,
        "first_index": true,
    })
}

#[test]
fn a_doc_with_open_questions_and_no_review_spawns_one() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "docs.design.indexed",
        &indexed_payload(5),
        &StubReviews(false),
    )
    .expect("eval");
    assert_eq!(hits.len(), 1, "open questions + no review → spawn");
    let args = &hits[0].invocations[0].args;
    let get = |k: &str| {
        args.iter()
            .find(|(name, _)| name == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("missing arg {k}: {args:?}"))
    };
    assert_eq!(get("kind"), Value::String("design-doc-review".into()));
    assert_eq!(
        get("subject"),
        Value::String("docs/design/idm-kanidm.md".into()),
        "the doc path IS the subject — the dedup helper keys on it"
    );
    assert_eq!(
        get("title"),
        Value::String("Kanidm IDM — the front door".into()),
        "the review carries the doc's title, not an auto-spawn label"
    );
    assert_eq!(
        get("metadata.doc_path"),
        Value::String("docs/design/idm-kanidm.md".into()),
        "the review-design step surface reads doc_path from metadata"
    );
}

#[test]
fn an_existing_open_review_suppresses_the_spawn() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "docs.design.indexed",
        &indexed_payload(5),
        &StubReviews(true),
    )
    .expect("eval");
    assert!(
        hits.is_empty(),
        "indexed re-fires on every question-count change; the dedup must hold"
    );
}

#[test]
fn a_doc_with_no_open_questions_stays_silent() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let hits = match_event(
        &reg,
        "docs.design.indexed",
        &indexed_payload(0),
        &StubReviews(false),
    )
    .expect("eval");
    assert!(
        hits.is_empty(),
        "a fully-resolved doc needs no review — nothing to answer"
    );
}
