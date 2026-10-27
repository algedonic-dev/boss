use boss_core::{actor::ActorId, publisher::EventStamp};
use boss_jobs::port::{EstateNodeInput, JobsRepository};
use serde_json::json;

fn node(disk_gb: Option<i32>) -> EstateNodeInput {
    EstateNodeInput {
        id: "w-2".into(),
        label: "original identity".into(),
        address: "192.0.2.16".into(),
        role: "talos-worker".into(),
        roles: vec![],
        cpu: Some(4),
        memory_gb: Some(15),
        disk_gb,
        notes: Some("operator note remains".into()),
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("estate-seed".into()))
}

#[test]
fn declared_disk_capacity_is_positive_or_omitted() {
    for disk in [None, Some(1), Some(929)] {
        boss_jobs::port::validate_estate_node(&node(disk)).unwrap();
    }
    for disk in [0, -1] {
        let why = boss_jobs::port::validate_estate_node(&node(Some(disk)))
            .expect_err("zero and negative capacities must refuse");
        assert!(why.contains("w-2") && why.contains("disk_gb"), "{why}");
    }
}

async fn invalid_disk_refuses_the_whole_batch(repo: &impl JobsRepository) {
    repo.declare_estate_nodes(&[node(Some(110))], &stamp())
        .await
        .unwrap();
    for disk in [0, -1] {
        let mut new_node = node(Some(929));
        new_node.id = "new-node".into();
        let bad = [new_node, node(Some(disk))];
        let why = repo
            .declare_estate_nodes(&bad, &stamp())
            .await
            .expect_err("invalid batch must refuse before inserting its first valid row");
        assert!(why.to_string().contains("disk_gb"), "{why}");
        let rows = repo.list_estate_nodes().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].disk_gb, Some(110));
    }
}

async fn prior_direct_input_boundary_is_kept(repo: &impl JobsRepository) {
    // The file and HTTP shape validators are stricter than these ports.
    // N1 must not silently apply their unrelated restrictions here.
    let mut declared = node(None);
    declared.id = "legacy_node".into();
    declared.role = "Legacy Role".into();
    declared.label.clear();
    declared.address.clear();
    let inserted = repo
        .declare_estate_nodes(&[declared.clone()], &stamp())
        .await
        .unwrap();
    assert_eq!((inserted.inserted, inserted.updated), (1, 0));
    declared.disk_gb = Some(929);
    let changed = repo
        .declare_estate_nodes(&[declared.clone()], &stamp())
        .await
        .unwrap();
    assert_eq!(changed.updated, 1);
    let repeated = repo
        .declare_estate_nodes(&[declared.clone()], &stamp())
        .await
        .unwrap();
    assert_eq!(repeated.updated, 0);
    declared.disk_gb = None;
    let omitted = repo
        .declare_estate_nodes(&[declared], &stamp())
        .await
        .unwrap();
    assert_eq!(omitted.updated, 0);
    let rows = repo.list_estate_nodes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].disk_gb, Some(929));
    assert_eq!(
        (
            &*rows[0].id,
            &*rows[0].role,
            &*rows[0].label,
            &*rows[0].address
        ),
        ("legacy_node", "Legacy Role", "", "")
    );
}

#[tokio::test]
async fn direct_adapter_memory_keeps_its_prior_unrelated_input_boundary() {
    let repo = boss_jobs::InMemoryJobs::new();
    prior_direct_input_boundary_is_kept(&repo).await;
    assert_eq!(repo.recorded_events().len(), 2);
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn direct_adapter_postgres_keeps_its_prior_unrelated_input_boundary() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    prior_direct_input_boundary_is_kept(&repo).await;
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts, 2);
}

#[tokio::test]
async fn memory_refuses_invalid_disk_before_any_batch_change() {
    let repo = boss_jobs::InMemoryJobs::new();
    invalid_disk_refuses_the_whole_batch(&repo).await;
    assert_eq!(repo.recorded_events().len(), 1);
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn postgres_refuses_invalid_disk_before_any_batch_change() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    invalid_disk_refuses_the_whole_batch(&repo).await;
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts, 1);
}

#[test]
fn old_outcome_shape_defaults_to_no_disk_updates() {
    let old: boss_jobs::port::EstateBatchOutcome =
        serde_json::from_value(json!({"received":1,"inserted":0,"roles_inserted":1})).unwrap();
    assert_eq!(old.updated, 0, "the old API did not refresh capacity");
    let new: boss_jobs::port::EstateBatchOutcome =
        serde_json::from_value(json!({"received":1,"inserted":0,"updated":1,"roles_inserted":0}))
            .unwrap();
    assert_eq!(new.updated, 1);
}

#[test]
fn old_role_only_event_helper_does_not_claim_requested_capacity_landed() {
    let event = boss_jobs::port::node_declared_event(
        &stamp(),
        &node(Some(929)),
        false,
        &["ops-runner".into()],
    )
    .unwrap();
    assert_eq!(
        event.payload["disk_gb"], 929,
        "the old payload retains the request"
    );
    assert_eq!(
        event.payload["landed"],
        json!({"node":false,"roles":["ops-runner"]})
    );
    assert!(event.payload["landed"].get("disk_gb").is_none());
}

async fn conflicting_duplicate_ids_refuse(repo: &impl JobsRepository) {
    repo.declare_estate_nodes(&[node(Some(110))], &stamp())
        .await
        .unwrap();
    let ambiguous = [node(Some(929)), node(Some(110))];
    let result = repo.declare_estate_nodes(&ambiguous, &stamp()).await;
    let error = result.expect_err("the file loader already refuses duplicate IDs; a batch must not oscillate capacity on every retry");
    assert!(error.to_string().contains("declared twice"), "{error}");
    assert_eq!(
        repo.list_estate_nodes().await.unwrap()[0].disk_gb,
        Some(110)
    );
}

#[tokio::test]
async fn memory_refuses_ambiguous_duplicate_ids_before_any_change() {
    let repo = boss_jobs::InMemoryJobs::new();
    conflicting_duplicate_ids_refuse(&repo).await;
    assert_eq!(repo.recorded_events().len(), 1);
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn postgres_refuses_ambiguous_duplicate_ids_before_any_change() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    conflicting_duplicate_ids_refuse(&repo).await;
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts, 1);
}

async fn refresh_contract(repo: &impl JobsRepository) {
    repo.declare_estate_nodes(&[node(Some(110))], &stamp())
        .await
        .unwrap();
    let mut later = node(Some(929));
    later.label = "a requested label that must not replace identity".into();
    later.address = "192.0.2.99".into();
    later.role = "forge".into();
    later.cpu = Some(64);
    later.memory_gb = Some(128);
    later.notes = Some("a requested note that must not replace operator notes".into());
    let out = repo
        .declare_estate_nodes(&[later.clone()], &stamp())
        .await
        .unwrap();
    let rows = repo.list_estate_nodes().await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        row.disk_gb,
        Some(929),
        "existing declared disk capacity must refresh"
    );
    assert_eq!(
        (row.label.as_str(), row.address.as_str(), row.role.as_str()),
        ("original identity", "192.0.2.16", "talos-worker")
    );
    assert_eq!((row.cpu, row.memory_gb), (Some(4), Some(15)));
    assert_eq!(row.notes.as_deref(), Some("operator note remains"));
    assert!(!row.retired);
    assert_eq!(
        serde_json::to_value(out).unwrap(),
        json!({"received":1,"inserted":0,"updated":1,"roles_inserted":0})
    );
    let again = repo
        .declare_estate_nodes(&[later.clone()], &stamp())
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(again).unwrap(),
        json!({"received":1,"inserted":0,"updated":0,"roles_inserted":0})
    );
    later.disk_gb = None;
    let omitted = repo.declare_estate_nodes(&[later], &stamp()).await.unwrap();
    assert_eq!(serde_json::to_value(omitted).unwrap()["updated"], 0);
    assert_eq!(
        repo.list_estate_nodes().await.unwrap()[0].disk_gb,
        Some(929)
    );
}

#[tokio::test]
async fn memory_refreshes_only_explicit_disk_capacity_and_records_only_landed_fields() {
    let repo = boss_jobs::InMemoryJobs::new();
    refresh_contract(&repo).await;
    let facts: Vec<_> = repo
        .recorded_events()
        .into_iter()
        .filter(|event| event.kind == "node.declared")
        .collect();
    assert_eq!(
        facts.len(),
        2,
        "repeat and omitted disk produce no additional fact"
    );
    assert_eq!(facts[1].payload["declared_by"], "automation:estate-seed");
    assert_eq!(
        facts[1].payload["landed"],
        json!({"node":false,"roles":[],"disk_gb":{"before":110,"after":929}})
    );
    assert_eq!(facts[0].payload["landed"], json!({"node":true,"roles":[]}));
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn postgres_refreshes_only_explicit_disk_capacity_in_the_same_fact_transaction() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    refresh_contract(&repo).await;
    let payloads: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(payloads.len(), 2);
    assert!(
        payloads.iter().any(|p| p["landed"]
            == json!({"node":false,"roles":[],"disk_gb":{"before":110,"after":929}}))
    );
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_identical_declarations_land_one_disk_change_and_keep_retirement() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    repo.declare_estate_nodes(&[node(Some(110))], &stamp())
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET retired_at = now() WHERE id = 'w-2'")
        .execute(&db.pool)
        .await
        .unwrap();
    // Hold the row outside both declarations so both statements reach
    // the comparison before either can update it. A call barrier cannot
    // force that ordering and passed with the production lock removed.
    let mut held = db.pool.begin().await.unwrap();
    let _: String = sqlx::query_scalar("SELECT id FROM nodes WHERE id = 'w-2' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let call = |pool| {
        tokio::spawn(async move {
            boss_jobs::PgJobs::new(pool)
                .declare_estate_nodes(&[node(Some(929))], &stamp())
                .await
                .unwrap()
        })
    };
    let a = call(db.pool.clone());
    let b = call(db.pool.clone());
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    held.commit().await.unwrap();
    let (a, b) = (a.await.unwrap(), b.await.unwrap());
    let mut updates = [
        serde_json::to_value(a).unwrap()["updated"]
            .as_u64()
            .unwrap(),
        serde_json::to_value(b).unwrap()["updated"]
            .as_u64()
            .unwrap(),
    ];
    updates.sort();
    assert_eq!(updates, [0, 1]);
    let rows = repo.list_estate_nodes().await.unwrap();
    assert_eq!(rows[0].disk_gb, Some(929));
    assert!(
        rows[0].retired,
        "declaration must not reactivate a retired node"
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE kind = 'node.declared' AND payload->'landed' ? 'disk_gb'")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(count, 1);
    let subject: String =
        sqlx::query_scalar("SELECT label FROM subjects WHERE kind = 'node' AND id = 'w-2'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(subject, "original identity");
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn outbox_failure_rolls_back_disk_and_new_role_together() {
    let db = boss_testing::TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    repo.declare_estate_nodes(&[node(Some(110))], &stamp())
        .await
        .unwrap();
    // Isolated fixture refusal at the existing atomic outbox seam.
    sqlx::query("ALTER TABLE event_outbox ADD CONSTRAINT refuse_capacity_fact CHECK (NOT (payload->'landed' ? 'disk_gb'))")
        .execute(&db.pool).await.unwrap();
    let mut change = node(Some(929));
    change.roles = vec!["ops-runner".into()];
    let error = repo
        .declare_estate_nodes(&[change], &stamp())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("refuse_capacity_fact"),
        "{error}"
    );
    let rows = repo.list_estate_nodes().await.unwrap();
    assert_eq!(rows[0].disk_gb, Some(110));
    assert!(rows[0].roles.is_empty());
    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'node.declared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(facts, 1);
}
