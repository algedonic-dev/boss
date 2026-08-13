//! The Operating System map — the company as a network of executors.
//!
//! BOSS claims to be the software layer of a state machine whose CPUs
//! are humans and agents. Every other surface renders the *work*; this
//! renders the *machine*: who the processors are and what moves
//! between them. Design + decisions:
//! `docs/architecture-decisions.md` (§the flow canvas), which also
//! records this view's retirement as a PAGE while the LAG-pairing SQL
//! below survives as the traffic layer, re-keyed from department to
//! station. The pre-network `operating-system-view.md` it used to cite
//! was dropped by the framing convergence.
//!
//! The shape here is what that review settled, and each choice is a
//! decision rather than a default:
//!
//! - **Nodes are departments** (Q1). Individual actors do not
//!   aggregate — 3,838 distinct edges across ~200 actors is a
//!   hairball — but 176 employees collapse to 15 departments, which
//!   is a graph a person can read. Departments also let geography be
//!   represented later without a second concept.
//! - **Edges are step handoffs** (Q2): consecutive step completions on
//!   the same Job by actors in different departments. Self-edges are
//!   kept deliberately, because an intra-departmental handoff is real
//!   work moving between people and the reviewer asked to see it.
//! - **The dispatcher is a node** (Q4), not hidden. It is the single
//!   busiest executor in the system, and a map of job traffic that
//!   omitted it would be a map of a different company.
//! - **Simulated traffic is counted separately** (Q5) so the surface
//!   can colour it apart rather than blending real and synthetic
//!   executors into one number someone might staff against.
//!
//! Reads `event_facts`, not `audit_log`: it carries the same payload,
//! is indexed for this access pattern, and a timer keeps it level with
//! the log — which is what makes a *live* instrument (Q3) honest
//! rather than a picture of whenever someone last rebuilt.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ViewsError;

/// What kind of executor a node stands for. Open-ended on purpose:
/// `Department` is the tenant's own vocabulary and a new one appears
/// without a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeKind {
    /// A department of the org — the default executor grouping.
    Department,
    /// Automation: rules, schedulers, bus subscribers. One node, not
    /// one per rule; `/it/dispatcher` is the drill-down for what is
    /// inside it.
    Dispatcher,
    /// Agent sessions — the LLM CPUs (`ActorId::Agent`, wire form
    /// `<mode>:<model>`). One node, like the dispatcher: the map is
    /// about traffic between executor classes, and `claude:opus-5` vs
    /// `claude:fable` is a drill-down, not a node.
    Agent,
    /// An actor we could not resolve to either. Rendered rather than
    /// dropped, because a silently missing executor is how a map
    /// starts lying.
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsMapNode {
    pub id: String,
    pub label: String,
    pub kind: NodeKind,
    /// Handoffs this node participated in, either direction. The
    /// weight a layout should size it by.
    pub touched: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsMapEdge {
    pub source: String,
    pub target: String,
    pub handoffs: i64,
    /// How many of `handoffs` came from simulated executors. Kept
    /// alongside rather than as a separate edge so the surface can
    /// render one edge with a real/simulated split.
    pub simulated: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsMap {
    pub nodes: Vec<OsMapNode>,
    pub edges: Vec<OsMapEdge>,
    /// Rows considered, so a caller can tell an empty map ("nothing
    /// moved") from a truncated one.
    pub handoffs_considered: i64,
    /// The `event_facts` watermark this was built from. A live view
    /// polls; this is how it knows whether anything actually advanced.
    pub high_water: i64,
}

/// Reading the executor network. A port because the aggregation is a
/// storage concern — the surface wants a graph, not SQL.
#[async_trait]
pub trait OsMapRepo: Send + Sync {
    /// Build the map from the most recent `limit` step completions.
    ///
    /// Bounded by recency rather than a wall-clock window on purpose:
    /// `occurred_at` runs on the sim clock, which moves at warp, so
    /// "the last hour" is a wildly different amount of work depending
    /// on the warp factor. "The last N handoffs" means the same thing
    /// at any speed.
    async fn os_map(&self, limit: i64) -> Result<OsMap, ViewsError>;
}

/// Default recency window. Large enough that every department appears
/// on a busy system, small enough to stay a picture of *now*.
pub const DEFAULT_LIMIT: i64 = 5_000;

/// Assemble nodes from the edge list.
///
/// Nodes are derived rather than queried separately: an executor that
/// has not handed anything off in the window is not on the map, which
/// is the honest rendering of "who is moving work right now".
pub fn nodes_from_edges(
    edges: &[OsMapEdge],
    label_of: impl Fn(&str) -> (String, NodeKind),
) -> Vec<OsMapNode> {
    use std::collections::BTreeMap;
    let mut touched: BTreeMap<&str, i64> = BTreeMap::new();
    for e in edges {
        *touched.entry(e.source.as_str()).or_default() += e.handoffs;
        // A self-edge is one node's traffic, not two nodes' worth.
        if e.source != e.target {
            *touched.entry(e.target.as_str()).or_default() += e.handoffs;
        }
    }
    touched
        .into_iter()
        .map(|(id, n)| {
            let (label, kind) = label_of(id);
            OsMapNode {
                id: id.to_string(),
                label,
                kind,
                touched: n,
            }
        })
        .collect()
}

/// Label + kind for a node id. `dispatcher`, `agent` and `unresolved`
/// are the reserved ids; everything else is a department code the
/// tenant owns.
pub fn classify(id: &str) -> (String, NodeKind) {
    match id {
        "dispatcher" => ("Dispatcher".to_string(), NodeKind::Dispatcher),
        "agent" => ("Agent".to_string(), NodeKind::Agent),
        "unresolved" => ("Unresolved".to_string(), NodeKind::Unresolved),
        other => {
            let mut label = String::with_capacity(other.len());
            for (i, part) in other.split(['-', '_']).enumerate() {
                if i > 0 {
                    label.push(' ');
                }
                let mut chars = part.chars();
                if let Some(first) = chars.next() {
                    label.extend(first.to_uppercase());
                    label.push_str(chars.as_str());
                }
            }
            (label, NodeKind::Department)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(s: &str, t: &str, n: i64, sim: i64) -> OsMapEdge {
        OsMapEdge {
            source: s.into(),
            target: t.into(),
            handoffs: n,
            simulated: sim,
        }
    }

    #[test]
    fn nodes_are_derived_from_the_edges_they_appear_in() {
        let edges = vec![
            edge("production", "qa", 10, 10),
            edge("qa", "warehouse", 4, 0),
        ];
        let nodes = nodes_from_edges(&edges, classify);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["production", "qa", "warehouse"]);
        // qa is on both edges, so it carries both weights.
        let qa = nodes.iter().find(|n| n.id == "qa").unwrap();
        assert_eq!(qa.touched, 14);
    }

    /// An intra-departmental handoff is one department's traffic. If
    /// a self-edge counted twice, the busiest departments — the ones
    /// that mostly hand off internally — would be inflated exactly
    /// where the map is trying to tell the truth about load.
    #[test]
    fn a_self_edge_counts_once() {
        let nodes = nodes_from_edges(&[edge("production", "production", 10, 10)], classify);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].touched, 10);
    }

    #[test]
    fn the_dispatcher_is_its_own_kind_not_a_department() {
        let nodes = nodes_from_edges(&[edge("dispatcher", "warehouse", 3, 3)], classify);
        let d = nodes.iter().find(|n| n.id == "dispatcher").unwrap();
        assert_eq!(d.kind, NodeKind::Dispatcher);
        assert_eq!(d.label, "Dispatcher");
        let w = nodes.iter().find(|n| n.id == "warehouse").unwrap();
        assert_eq!(w.kind, NodeKind::Department);
        assert_eq!(w.label, "Warehouse");
    }

    /// The registry owns department names, so a label supplied by it
    /// must win over anything derived from the code. `classify` alone
    /// renders `qa` as "Qa" and `it` as "It", which is why deriving
    /// was the wrong default.
    #[test]
    fn a_registry_label_beats_the_derived_one() {
        let registry: std::collections::HashMap<&str, &str> =
            [("qa", "QA"), ("it", "IT")].into_iter().collect();
        let nodes = nodes_from_edges(
            &[edge("qa", "it", 5, 0), edge("dispatcher", "qa", 2, 2)],
            |id| match registry.get(id) {
                Some(display) => ((*display).to_string(), NodeKind::Department),
                None => classify(id),
            },
        );
        let label = |id: &str| {
            nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| n.label.clone())
                .unwrap()
        };
        assert_eq!(label("qa"), "QA");
        assert_eq!(label("it"), "IT");
        // Reserved ids have no Class row and still resolve.
        assert_eq!(label("dispatcher"), "Dispatcher");
    }

    /// Agents are the third class of CPU (see `boss_core::actor`), and
    /// the map claims to render "who the processors are". Folding them
    /// into `dispatcher` would make the `/it/dispatcher` drill-down
    /// lie; leaving them in `unresolved` is the silently-missing
    /// executor this module's docs call out.
    #[test]
    fn agents_are_their_own_kind_not_the_dispatcher_and_not_unresolved() {
        let nodes = nodes_from_edges(&[edge("agent", "qa", 7, 0)], classify);
        let a = nodes.iter().find(|n| n.id == "agent").unwrap();
        assert_eq!(a.kind, NodeKind::Agent);
        assert_eq!(a.label, "Agent");
        assert_ne!(a.kind, NodeKind::Dispatcher);
        assert_ne!(a.kind, NodeKind::Unresolved);
    }

    #[test]
    fn multiword_department_codes_read_as_labels() {
        assert_eq!(classify("field-service").0, "Field Service");
        assert_eq!(classify("people_ops").0, "People Ops");
    }
}
