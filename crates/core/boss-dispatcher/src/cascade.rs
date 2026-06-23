//! Static cascade metadata for the dispatcher-rules visualization.
//!
//! `rules.toml` declares the reactive layer as `trigger event → rule →
//! handler(s)`. To render the full *cascade* — the feedback loops that
//! make the state machine self-drive — the graph also needs two facts
//! that aren't in `rules.toml`:
//!
//!   1. **What each handler causes to be emitted** downstream (by the
//!      API it calls). A loop closes wherever an emitted kind matches
//!      some rule's `on_event` (e.g. `inventory.parts.consume` emits
//!      `inventory.item.consumed`, which re-triggers `spawn-restock`).
//!   2. **The jobs-api / external consequences** that re-enter the rule
//!      set but are NOT dispatcher rules themselves (a new Job's steps
//!      becoming ready; a completed step emitting its `done` topic; an
//!      invoice later being paid by a counterparty). Rendered distinctly
//!      so dispatcher rules stay visually separable from system/external
//!      wiring.
//!
//! Both are authored here — a documented, central "what each side-effect
//! causes" — and served by `GET /api/dispatcher/rules`. Kept in sync
//! with the handlers in `rules::handlers::*`; the
//! `dispatcher_rules`-registry migration will fold this into first-class
//! rule/handler metadata. `cascade_handlers_match_rules` (tests) guards
//! against a rule referencing a handler this map forgot.

use serde::Serialize;
use std::collections::BTreeMap;

/// Event kind(s) each handler causes to be emitted downstream, keyed by
/// the handler's registered name (the `handler = "..."` in `rules.toml`).
/// An empty list is a pure sink (notifier / webhook — emits nothing).
pub fn handler_emits() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        (
            "inventory.parts.consume",
            vec!["inventory.item.consumed", "inventory.labor.absorbed"],
        ),
        ("commerce.invoice.issue", vec!["commerce.invoice.created"]),
        (
            "products.produce",
            vec!["products.produced", "products.inventory.upserted"],
        ),
        (
            "products.consume",
            vec!["products.consumed", "products.inventory.upserted"],
        ),
        (
            "inventory.po.place",
            vec!["inventory.purchase_order.upserted"],
        ),
        (
            "inventory.receive",
            vec!["inventory.item.received", "inventory.po.status_changed"],
        ),
        (
            "inventory.bill.approve",
            vec!["inventory.vendor_invoice.approved"],
        ),
        ("ledger.bill.approve", vec!["ledger.bill.approved"]),
        (
            "inventory.bill.payment_batch",
            vec!["inventory.vendor_invoice.paid"],
        ),
        ("ledger.bill.payment_batch", vec!["ledger.bill.paid"]),
        ("ledger.tax.accrue", vec!["ledger.tax.accrued"]),
        ("ledger.tax.remit", vec!["ledger.tax.remitted"]),
        ("ledger.payroll.run.submit", vec!["ledger.payroll.run"]),
        ("people.hire", vec!["people.employee.created"]),
        ("people.terminate", vec!["people.employee.updated"]),
        ("shipping.create", vec!["shipping.shipment.created"]),
        ("jobs.spawn", vec!["jobs.job.created"]),
        ("jobs.complete_step", vec!["jobs.step.completed"]),
        ("jobs.subjob_resolve", vec!["jobs.step.completed"]),
        ("gate.resolve", vec!["jobs.step.completed"]),
        ("messages.notify", vec![]),
        ("webhook.notify", vec![]),
    ])
}

/// A cascade edge that closes a loop but is NOT a dispatcher rule — a
/// consequence of jobs-api's DAG mechanics or of an external
/// counterparty. The viz renders these with a distinct style so the
/// dispatcher's own rules stay legible.
#[derive(Debug, Clone, Serialize)]
pub struct SystemEdge {
    /// Emitted event the edge starts from.
    pub from: &'static str,
    /// Event/topic it leads to (often a `*` wildcard topic).
    pub to: &'static str,
    /// `"jobs-api"` (DAG mechanics) or `"external"` (a counterparty).
    pub kind: &'static str,
    /// Human-readable reason the edge exists.
    pub label: &'static str,
}

/// The non-rule edges that complete the cascade. Small + fixed: jobs-api
/// step-lifecycle mechanics + the AR collections loop.
pub fn system_edges() -> Vec<SystemEdge> {
    vec![
        SystemEdge {
            from: "jobs.job.created",
            to: "step.ready.*",
            kind: "jobs-api",
            label: "a new Job's entry steps become ready",
        },
        SystemEdge {
            from: "jobs.step.completed",
            to: "step.done.*",
            kind: "jobs-api",
            label: "a completed step emits its done topic",
        },
        SystemEdge {
            from: "jobs.step.completed",
            to: "step.ready.*",
            kind: "jobs-api",
            label: "completing a step readies its dependents",
        },
        SystemEdge {
            from: "commerce.invoice.created",
            to: "commerce.invoice.paid",
            kind: "external",
            label: "the counterparty settles the invoice",
        },
        SystemEdge {
            from: "commerce.invoice.created",
            to: "commerce.invoice.past_due",
            kind: "external",
            label: "the invoice goes unpaid past terms",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::registry::parse_raw;

    #[test]
    fn emits_and_system_edges_present() {
        let emits = handler_emits();
        // The load-bearing loop: parts.consume → inventory.item.consumed.
        assert!(
            emits["inventory.parts.consume"].contains(&"inventory.item.consumed"),
            "restock loop edge must be present"
        );
        assert!(emits["messages.notify"].is_empty(), "notifier is a sink");
        assert!(!system_edges().is_empty());
    }

    /// Drift guard: every handler the shipped `rules.toml` references must
    /// have a `handler_emits` entry, so the cascade graph never silently
    /// drops a handler. Reads the real rules file via CARGO_MANIFEST_DIR so
    /// it tracks the deployed registry, not a fixture.
    #[test]
    fn cascade_handlers_match_rules() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../infra/dispatcher/rules.toml"
        );
        let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let raw = parse_raw(&src).expect("parse rules.toml");
        let emits = handler_emits();
        for rule in &raw.rules {
            for step in &rule.do_steps {
                assert!(
                    emits.contains_key(step.handler.as_str()),
                    "handler {:?} (rule {:?}) is missing from handler_emits()",
                    step.handler,
                    rule.name
                );
            }
        }
    }
}
