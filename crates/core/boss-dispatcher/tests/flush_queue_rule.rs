//! The `design-decision-flush-queue` rule (migration 109) — a
//! recorded decision queues its doc's flush (cea82de0, link 1).
//!
//! Pins the shipped rule against the expr engine: no `when` (every
//! decision event fires it), and the S1 payload's flat `doc_path`
//! rides to the handler via the event payload, not args.

use boss_dispatcher::rules::expr::NoHelpers;
use boss_dispatcher::rules::registry::{Registry, match_event};

const RULE: &str = r#"
[[rule]]
name = "design-decision-flush-queue"
on_event = "docs.design.decision_recorded"
[[rule.do]]
handler = "docs.flush_queue"
"#;

#[test]
fn every_decision_event_queues_a_flush() {
    let reg = Registry::from_toml(RULE).expect("rule parses");
    let payload = serde_json::json!({
        "doc_path": "docs/design/stations.md",
        "anchor": "Q1",
        "kind": "override",
        "resolution": "Table is good",
        "decided_by": "emp-bootstrap-admin",
    });
    let hits =
        match_event(&reg, "docs.design.decision_recorded", &payload, &NoHelpers).expect("eval");
    assert_eq!(hits.len(), 1, "unconditional: every decision fires it");
    assert_eq!(hits[0].invocations[0].handler, "docs.flush_queue");
}
