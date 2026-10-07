//! Every platform protocol names the department whose work it is.
//!
//! A department's protocols are the kinds whose ACTIVE row declares it
//! (`boss_jobs::department::kinds_declaring`) — the join behind both
//! `GET /api/departments/<code>/readiness` and the kind half of
//! `GET /api/jobs?department=<code>`. Until backlog a8458043 the
//! platform bundle declared no department at all, so IT — the
//! department that runs every one of these protocols — read
//! `protocols.kinds = []` on its readiness while finance, sales and
//! marketing each read two (measured live 2026-10-01: 64 of 71 active
//! rows declared nothing; the 7 that did were tenant rows). The estate
//! kinds, the pipeline kinds and the backlog were invisible to IT's
//! own view unless a packet happened to carry the word itself.
//!
//! The rule is two-armed, and the second arm is data, not a list in
//! this file: a row either DECLARES its department, or its
//! `metadata_schema` REQUIRES every packet to name one. The second arm
//! is the kinds every department has — `department-retro` and
//! `page-audit` — whose packets each carry their own department
//! (backlog 481d7939). Declaring `it` on those would put every
//! department's retros and audits on IT's readiness count, so a row
//! doing both is refused too: two answers to "whose work is this".

use boss_jobs::department::{declared, kinds_declaring};
use boss_jobs::registry::{WorkflowSpec, platform_bundle_path};
use boss_jobs::seed_loader::load_workflows;

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

/// Whether the row's schema requires each packet to name its own
/// department.
fn packets_name_their_own(spec: &WorkflowSpec) -> bool {
    spec.metadata_schema
        .get("required")
        .and_then(|r| r.as_array())
        .is_some_and(|r| r.iter().any(|k| k == "department"))
}

#[test]
fn every_platform_row_declares_a_department_or_requires_its_packets_to() {
    let rows = bundle();
    assert!(rows.len() >= 20, "an empty bundle would prove nothing");
    let silent: Vec<&str> = rows
        .iter()
        .filter(|s| declared(s).is_none() && !packets_name_their_own(s))
        .map(|s| s.kind.as_str())
        .collect();
    assert!(
        silent.is_empty(),
        "platform rows that name no department, so no department's readiness or jobs view \
         holds them (declare `department` in the row's `metadata`, or require it on every \
         packet in `metadata_schema`): {silent:?}"
    );
    let both: Vec<&str> = rows
        .iter()
        .filter(|s| declared(s).is_some() && packets_name_their_own(s))
        .map(|s| s.kind.as_str())
        .collect();
    assert!(
        both.is_empty(),
        "rows that declare a department AND require every packet to name its own — the \
         declaration would count every department's packets as one department's: {both:?}"
    );
}

/// The kinds the packet named as IT's and found undeclared: the estate
/// chain, the pipeline, the backlog, and the two the drift audit
/// (93b508fd) decided on by name.
#[test]
fn it_reads_its_estate_pipeline_and_backlog_kinds() {
    let it = kinds_declaring(&bundle(), "it");
    for kind in [
        "maintenance-estate-observe-host",
        "maintenance-estate-observe-units",
        "maintenance-cluster-watchdog",
        "maintenance-cluster-converge",
        "ops-request",
        "maintenance-protocol-drift",
        "gate-run",
        "pr-train",
        "backlog-item",
    ] {
        assert!(
            it.iter().any(|k| k == kind),
            "`{kind}` does not declare department `it`; IT declares {it:?}"
        );
    }
}

/// The two kinds whose packets carry their own department stay
/// undeclared, so a sales retro is not counted as IT's work.
#[test]
fn the_kinds_every_department_has_declare_none() {
    let rows = bundle();
    for kind in ["department-retro", "page-audit"] {
        let spec = rows
            .iter()
            .find(|s| s.kind == kind)
            .unwrap_or_else(|| panic!("`{kind}` is in the platform bundle"));
        assert_eq!(declared(spec), None, "`{kind}` declares a department");
        assert!(
            packets_name_their_own(spec),
            "`{kind}` requires its packets to name their department"
        );
    }
}

/// Ledger recognition and replay are Finance's work (b7263ac5), even
/// though the platform supplies their maintenance protocols. The
/// broader IT declaration sweep must preserve this specific assignment.
#[test]
fn finance_reads_its_ledger_recognition_and_replay_kinds() {
    let rows = bundle();
    let finance = kinds_declaring(&rows, "finance");
    let it = kinds_declaring(&rows, "it");
    for kind in ["maintenance-ledger-recognize", "maintenance-ledger-replay"] {
        assert!(
            finance.iter().any(|k| k == kind),
            "`{kind}` does not declare Finance; Finance declares {finance:?}"
        );
        assert!(
            !it.iter().any(|k| k == kind),
            "`{kind}` is counted as IT's work as well as Finance's"
        );
    }
}
