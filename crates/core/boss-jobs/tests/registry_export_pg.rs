//! The registries' recovery export over Postgres (backlog c26aacaa,
//! design 942dc0ac Q2) — the adapters the export binary reads with.
//!
//! The in-memory half lives beside the module
//! (`boss_jobs::registry_export` tests). This half pins what only the
//! database can: that each Pg adapter names every lineage its table
//! holds — the migrated rows a fresh deployment starts with, and a
//! lineage an operator retired, which `list_active` would miss and a
//! restore without it would let the next bootstrap re-insert.

use boss_core::actor::ActorId;
use boss_jobs::cadence::{CadenceRegistry, PgCadence};
use boss_jobs::pools::PgPools;
use boss_jobs::registry::WorkflowStatus;
use boss_jobs::registry_export::{Registries, RegistryExport, diff, export};
use boss_jobs::{PgStations, PgWorkflows};
use boss_testing::TestDb;
use chrono::{DateTime, Utc};

#[tokio::test(flavor = "multi_thread")]
async fn a_migrated_database_exports_every_lineage_and_names_what_moved_after() {
    let db = TestDb::new().await;
    let workflows = PgWorkflows::new(db.pool.clone());
    let stations = PgStations::new(db.pool.clone());
    let pools = PgPools::new(db.pool.clone());
    let cadence = PgCadence::new(db.pool.clone());
    let registries = Registries {
        workflows: &workflows,
        stations: &stations,
        pools: &pools,
        cadence: &cadence,
    };
    let now = DateTime::<Utc>::UNIX_EPOCH;

    let before = export(&registries, "testdb", now).await.expect("export");
    let reconcile = before
        .cadence_rules
        .iter()
        .find(|r| r.name() == "train-reconcile" && r.status == WorkflowStatus::Active)
        .expect("the migrated train-reconcile rule is exported")
        .version;
    assert!(
        before.stations.iter().any(|s| s.name == "loading-dock"),
        "the migrated loading-dock station is exported"
    );

    let again = export(
        &registries,
        "another label",
        now + chrono::Duration::days(1),
    )
    .await
    .expect("export again");
    assert_eq!(
        before.digest, again.digest,
        "an unchanged database exports to one digest"
    );

    let operator = ActorId::Human("emp-operator".into());
    cadence
        .retire("train-reconcile", &operator, now)
        .await
        .expect("retire")
        .expect("the rule was active");
    let after = export(&registries, "testdb", now)
        .await
        .expect("export after");
    let retired = after
        .cadence_rules
        .iter()
        .find(|r| r.name() == "train-reconcile" && r.version == reconcile)
        .expect("a RETIRED lineage is still exported");
    assert_eq!(retired.status, WorkflowStatus::Retired);
    let lines: Vec<String> = diff(&before, &after)
        .expect("diff")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        lines,
        vec![format!(
            "cadence_rules train-reconcile v{reconcile}: differs on status"
        )]
    );

    let text = serde_json::to_string(&after).expect("encode");
    let read: RegistryExport = serde_json::from_str(&text).expect("decode");
    let read = read.checked().expect("the file checks");
    assert!(diff(&read, &after).expect("diff").is_empty());
}
