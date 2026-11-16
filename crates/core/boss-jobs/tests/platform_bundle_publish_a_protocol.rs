//! `infra/platform/workflows/publish-a-protocol.toml` keeps its decided
//! shape (design b1f319bc, decided by David 2026-11-05: "a workflow
//! publish that needs a person is a packet on that person"; backlog
//! 7955a488). One pin file per kind file (see `platform_bundle.rs`).
//!
//! What the decision fixes, and so what is pinned: ONE approve step, a
//! presence (passkey) sign-off whose signed content carries every fact
//! of the one publish — kind, file, sha256, the live version the diff
//! was rendered against, the tree commit, the diff and the dry-run lint
//! receipt — each filled by the filer, so the owner types nothing; a
//! machine `publish` step behind an approved decision; and the
//! terminals a reader can tell apart. The row declares no writer and no
//! executor, so the kind itself is not held out of the drift publish
//! by default (infra/gcp/workflow-holds.py).

use boss_core::job::{Assurance, FilledBy};
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundled(kind: &str) -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == kind)
        .unwrap_or_else(|| panic!("{kind} ships in the platform bundle"))
}

/// Since backlog f399458f the packet names a REGISTRY ROW, so the first
/// two facts are which registry and which row; `protocol_kind` is the
/// protocol road's own key and is no longer signed on approve.
const SIGNED_FACTS: [&str; 8] = [
    "registry",
    "row_name",
    "spec_path",
    "spec_sha256",
    "live_version",
    "tree_sha",
    "dry_run",
    "diff",
];

#[test]
fn the_owner_signs_one_publish_with_a_passkey_over_every_fact_of_it() {
    let lane = bundled("publish-a-protocol");
    let approves: Vec<_> = lane.steps.iter().filter(|s| s.kind == "sign-off").collect();
    assert_eq!(approves.len(), 1, "ONE approve step");
    let approve = approves[0];
    assert_eq!(approve.title, "approve");
    assert_eq!(approve.assurance_required, Some(Assurance::Presence));
    assert_eq!(approve.sign_offs_required, vec!["platform-admin"]);
    let md = &approve.metadata_defaults;
    assert_eq!(md["human_only"], serde_json::json!(true));
    for key in SIGNED_FACTS {
        assert_eq!(
            md[key],
            serde_json::json!(format!("{{metadata.{key}}}")),
            "`{key}` is rendered from the job onto the signed step"
        );
        assert!(
            approve
                .fields
                .iter()
                .any(|f| f.name == key && f.required && f.filled_by == FilledBy::Filer),
            "`{key}` is filled by the filer, never typed by the signer"
        );
    }
    assert!(
        approve
            .fields
            .iter()
            .any(|f| f.name == "decision" && f.required),
        "the decision is the signer's"
    );
    let schema = &lane.metadata_schema;
    for key in SIGNED_FACTS {
        assert!(
            schema["required"]
                .as_array()
                .is_some_and(|r| r.iter().any(|k| k == key)),
            "a packet cannot be filed without `{key}`"
        );
    }
}

#[test]
fn the_machine_publish_waits_for_an_approval_and_the_terminals_are_distinct() {
    let lane = bundled("publish-a-protocol");
    let publish = lane
        .steps
        .iter()
        .find(|s| s.title == "publish")
        .expect("a `publish` step");
    assert_eq!(publish.kind, "task");
    assert_eq!(
        publish.ready_when,
        "steps.approve.done AND steps.approve.metadata.decision = \"approved\" AND job.metadata.registry != \"pool\""
    );
    // One automaton step per registry door (backlog f399458f), each
    // behind the same approval and recording the same read-back.
    let pool = lane
        .steps
        .iter()
        .find(|s| s.title == "publish-pool")
        .expect("a `publish-pool` step");
    assert_eq!(pool.kind, "task");
    assert_eq!(
        pool.ready_when,
        "steps.approve.done AND steps.approve.metadata.decision = \"approved\" AND job.metadata.registry = \"pool\""
    );
    for step in [publish, pool] {
        for key in ["version", "from_version", "read_back"] {
            assert!(
                step.fields.iter().any(|f| f.name == key && f.required),
                "`{}` records `{key}` of what went live",
                step.title
            );
        }
    }
    let published = lane
        .steps
        .iter()
        .find(|s| s.title == "published")
        .expect("a `published` terminal");
    assert_eq!(
        published.ready_when,
        "steps.publish.done OR steps.publish-pool.done"
    );
    assert_eq!(
        lane.metadata_schema["properties"]["registry"]["enum"],
        serde_json::json!(["workflow", "pool"]),
        "a station row has no door to drive yet"
    );
    let terminals: Vec<&str> = lane
        .steps
        .iter()
        .filter_map(|s| s.terminal.as_ref().map(|t| t.outcome.as_str()))
        .collect();
    assert_eq!(terminals, vec!["published", "declined", "superseded"]);
    // No writer, no executor: the kind is not default-held.
    for s in &lane.steps {
        assert!(
            s.executor.is_none(),
            "step {} declares an executor",
            s.title
        );
        assert!(
            s.fields.iter().all(|f| f.writer.is_none()),
            "step {} declares a field writer",
            s.title
        );
    }
}
