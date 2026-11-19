//! The `file-a-fold-item-when-a-design-reaches-its-fold` rule — a
//! decided design files its own fold build (backlog 77ac4f30; David,
//! 2026-11-18: keep canonical documentation fresh).
//!
//! Measured 2026-11-17: 71 decided designs sat unfolded, some since
//! September, because a design's `fold` step reached a queue only when a
//! coordinator noticed it and filed a fold car by hand. The rule files
//! ONE backlog-item, about the design's own id, the moment the fold step
//! goes ready — on the review's completion, or at once for a design that
//! asked no question.
//!
//! Read FROM the rule's file under `infra/dispatcher/rules/`, and the
//! step it watches is read off the shipped design-doc Workflow, so a
//! renamed or re-kinded fold step reds this file rather than silently
//! retiring the filing.

use boss_dispatcher::rules::expr::{EvalError, HelperResolver, Value};
use boss_dispatcher::rules::helpers_inventory::InventoryHelpers;
use boss_dispatcher::rules::registry::match_event;
use boss_jobs::registry::seedable_platform_workflows;

mod common;

const RULE: &str = "file-a-fold-item-when-a-design-reaches-its-fold";
const DESIGN: &str = "02894662-99a0-4c1d-b194-933e0077e803";
const TITLE: &str = "Finance shows the live Stripe balance";

/// `open_job_exists` answering a fixed value and recording what it was
/// asked; every other helper (`concat`, `left`) is the real resolver's.
struct Stub {
    open_item: bool,
    asked: std::sync::Mutex<Vec<(String, String)>>,
    real: InventoryHelpers,
}

impl Stub {
    fn new(open_item: bool) -> Self {
        Self {
            open_item,
            asked: std::sync::Mutex::new(Vec::new()),
            real: InventoryHelpers::new("http://unused.invalid", "http://unused.invalid"),
        }
    }
}

impl HelperResolver for Stub {
    fn call(&self, name: &str, args: &[Value]) -> Result<Value, EvalError> {
        if name != "open_job_exists" {
            return self.real.call(name, args);
        }
        if let (Some(Value::String(kind)), Some(Value::String(subject))) =
            (args.first(), args.get(1))
        {
            self.asked
                .lock()
                .expect("stub lock")
                .push((kind.clone(), subject.clone()));
        }
        Ok(Value::Bool(self.open_item))
    }
}

/// A `step.ready.task` marker in the shape `build_step_ready_event`
/// emits: `workflow_kind`, `spec_slug` and `job_title` at the root.
fn ready(workflow_kind: &str, spec_slug: &str) -> serde_json::Value {
    serde_json::json!({
        "job_id": DESIGN,
        "step_id": "5f0d1b2c-0000-4000-8000-000000000001",
        "kind": "task",
        "subject_kind": "custom",
        "subject_id": "finance-stripe-balance",
        "workflow_kind": workflow_kind,
        "spec_slug": spec_slug,
        "job_title": TITLE,
        "assignee_id": null,
        "metadata": { "procedure": "State what CURRENT TRUTH gains" },
    })
}

fn hits(payload: &serde_json::Value, stub: &Stub) -> Vec<Vec<(String, Value)>> {
    let outcome = match_event(
        &common::authored_rule(RULE),
        "step.ready.task",
        payload,
        stub,
    );
    assert!(
        outcome.skipped.is_empty(),
        "the predicate and every arg must evaluate, not skip: {:?}",
        outcome.skipped
    );
    outcome
        .matched
        .iter()
        .map(|m| {
            assert_eq!(m.invocations.len(), 1);
            assert_eq!(m.invocations[0].handler, "jobs.spawn");
            m.invocations[0].args.clone()
        })
        .collect()
}

fn get(args: &[(String, Value)], k: &str) -> Value {
    args.iter()
        .find(|(name, _)| name == k)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| panic!("arg {k} missing: {args:?}"))
}

fn text(args: &[(String, Value)], k: &str) -> String {
    match get(args, k) {
        Value::String(s) => s,
        other => panic!("arg {k} is not a string: {other:?}"),
    }
}

/// THE FILING: a design's fold going ready files ONE backlog-item about
/// the design's own id — the key the dedup reads back — titled so a
/// person knows which design, on the roadmap channel, area docs, with a
/// description naming the design, the record and the precedent cars.
#[test]
fn a_design_reaching_its_fold_files_one_fold_item() {
    let stub = Stub::new(false);
    let fired = hits(&ready("design-doc", "fold"), &stub);
    assert_eq!(fired.len(), 1, "one item per design reaching its fold");
    let args = &fired[0];
    assert_eq!(get(args, "kind"), Value::String("backlog-item".into()));
    assert_eq!(get(args, "subject_kind"), Value::String("custom".into()));
    assert_eq!(get(args, "subject"), Value::String(DESIGN.into()));
    assert_eq!(
        text(args, "title"),
        format!("Fold design 02894662 ({TITLE}) into the decision record")
    );
    assert_eq!(text(args, "metadata.input_channel"), "roadmap");
    assert_eq!(text(args, "metadata.area"), "docs");
    assert_eq!(text(args, "metadata.design"), DESIGN);
    let description = text(args, "metadata.description");
    for must in [
        DESIGN,
        TITLE,
        "docs/architecture-decisions.md",
        "e848e857",
        "027b8784",
        "one section per car",
        "built or decided and not built",
        "boss fold",
    ] {
        assert!(
            description.contains(must),
            "the description must name {must:?}: {description}"
        );
    }
    assert_eq!(
        stub.asked.lock().unwrap().clone(),
        vec![("backlog-item".to_string(), DESIGN.to_string())],
        "the dedup asks about the design's own id, the subject it files"
    );
}

/// ONCE PER DESIGN: a fold item already open for the design files
/// nothing more, so a redelivered ready marker is a no-op.
#[test]
fn an_open_fold_item_for_the_design_files_nothing_more() {
    let stub = Stub::new(true);
    assert!(hits(&ready("design-doc", "fold"), &stub).is_empty());
}

/// Every other task step on the shared topic is left alone — and the
/// dedup read is never asked for one, because AND short-circuits on
/// the workflow and the slug first.
#[test]
fn another_workflows_step_or_another_design_step_files_nothing() {
    for (workflow_kind, spec_slug) in [
        ("backlog-item", "fold"),
        ("design-doc-review", "fold"),
        ("design-doc", "published"),
        ("ship-a-change", "build"),
    ] {
        let stub = Stub::new(false);
        assert!(
            hits(&ready(workflow_kind, spec_slug), &stub).is_empty(),
            "{workflow_kind}.{spec_slug} must not file a fold item"
        );
        assert!(
            stub.asked.lock().unwrap().is_empty(),
            "{workflow_kind}.{spec_slug} must not cost a dedup read"
        );
    }
}

/// A marker without the title (a jobs API older than the payload
/// field) is SKIPPED by name, never filed with a hole in its title.
#[test]
fn a_marker_without_the_title_skips_rather_than_filing_a_hole() {
    let mut payload = ready("design-doc", "fold");
    payload.as_object_mut().expect("object").remove("job_title");
    let outcome = match_event(
        &common::authored_rule(RULE),
        "step.ready.task",
        &payload,
        &Stub::new(false),
    );
    assert!(outcome.matched.is_empty(), "nothing filed without a title");
    assert_eq!(outcome.skipped.len(), 1);
    assert_eq!(outcome.skipped[0].rule, RULE);
}

/// The step the rule watches is the shipped design-doc's `fold`, a
/// `task` (the topic the rule rides), and the step before the terminal
/// — so its going ready IS "every question answered, or none asked".
#[test]
fn the_step_the_rule_watches_is_the_live_design_doc_fold() {
    let spec = seedable_platform_workflows()
        .into_iter()
        .find(|w| w.kind == "design-doc")
        .expect("design-doc is a shipped platform Workflow");
    let fold = spec
        .steps
        .iter()
        .find(|s| s.title == "fold")
        .expect("design-doc has a `fold` step");
    assert_eq!(fold.kind, "task", "the rule rides step.ready.task");
    assert!(fold.terminal.is_none(), "fold is work, not a terminal");
    let published = spec
        .steps
        .iter()
        .find(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()) == Some("published"))
        .expect("design-doc publishes");
    assert_eq!(
        published.ready_when, "steps.fold.done",
        "the fold is the last step before the design publishes"
    );
}
