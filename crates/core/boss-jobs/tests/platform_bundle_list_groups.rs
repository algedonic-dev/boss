//! Phase1 classification is authored registry data, independent of kind spelling.
//! The two ledger rows remain REQUIRED by follow-up bb282d4c-21b0-4d05-82af-36bd4b1133d0
//! after ledger car b6d244b8 scheduled native whole-blob proof. Their absence
//! is a temporary sequencing dependency, never permanent expected behavior.

#[test]
fn the_machine_activity_rows_declare_the_reviewed_group() {
    let specs = boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("bundle");
    let reviewed = [
        "agent-run",
        "gate-run",
        "maintenance-audit-integrity",
        "maintenance-backup",
        "maintenance-boss-gcp-converge",
        "maintenance-break-glass-deposit",
        "maintenance-cluster-converge",
        "maintenance-cluster-watchdog",
        "maintenance-codebase-metrics",
        "maintenance-conservation-invariants",
        "maintenance-dev-scratch-reclaim",
        "maintenance-disk-floor-sweep",
        "maintenance-estate-observe-host",
        "maintenance-estate-observe-units",
        "maintenance-files-gc",
        "maintenance-forge-backup",
        "maintenance-forge-converge",
        "maintenance-forge-token-audit",
        "maintenance-messages-purge",
        "maintenance-ml-inference-batch",
        "maintenance-playground-crawl",
        "maintenance-protocol-drift",
        "maintenance-reap-ci-jobs",
        "maintenance-recovery-sheet",
        "maintenance-search-reindex",
        "maintenance-surface-usage",
        "maintenance-sweep",
        "maintenance-views-catchup",
        "ops-request",
    ];
    for kind in reviewed {
        let spec = specs
            .iter()
            .find(|spec| spec.kind == kind)
            .expect("reviewed workflow");
        assert_eq!(
            spec.metadata["list_group"],
            serde_json::json!({"code": "machine", "label": "Machine activity"}),
            "{kind}"
        );
    }
    for kind in ["backlog-item", "page-audit", "design-doc"] {
        let spec = specs
            .iter()
            .find(|spec| spec.kind == kind)
            .expect("work workflow");
        assert!(
            spec.metadata.get("list_group").is_none(),
            "work is unclassified until explicitly declared: {kind}"
        );
    }
}
