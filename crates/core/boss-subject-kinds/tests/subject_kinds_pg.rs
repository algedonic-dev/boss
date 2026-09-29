//! The SubjectKind registry's metadata door against the real database
//! (backlog abc2e9d5): the replayed log rebuilds a patched row, and the
//! rebuild takes its lock.
//!
//! What the door itself promises — the UPDATE and its
//! `subject_kind.updated` commit together, a restatement writes and
//! stages nothing, a reserved key is refused, and the fact alone
//! replays the row — is stated once for BOTH adapters in
//! `the_adapters_agree_on_the_subject_kind_registry_pg.rs`, which
//! absorbed the four patch tests that lived here, two of them
//! hand-rolled comparisons of the two adapters (backlog be459ab9).

use boss_core::publisher::EventStamp;
use boss_subject_kinds::PgSubjectKinds;
use boss_subject_kinds::port::{SUBJECT_KIND_UPDATED, SubjectKindRepository};
use boss_subject_kinds::rebuild::rebuild_subject_kinds;
use boss_testing::TestDb;
use serde_json::{Map, Value, json};

fn stamp() -> EventStamp {
    EventStamp::new(
        "subject-kinds",
        boss_core::actor::ActorId::Automation("tenant-seed".into()),
    )
}

fn patch(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("a patch is an object")
}

/// Copy the staged facts into audit_log, where the relay would put
/// them — the log a rebuild reads.
async fn relay_outbox(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
         SELECT event_id, kind, source, timestamp, payload FROM event_outbox \
          WHERE kind = $1 ORDER BY id",
    )
    .bind(SUBJECT_KIND_UPDATED)
    .execute(pool)
    .await
    .expect("relay");
}

/// MED-2 of the review (closure): a fresh database — the row as the
/// migrations seed it, plus the replayed log — reproduces the patched
/// row; a second rebuild writes nothing; a row changed outside the door
/// is drift, named and rolled back rather than overwritten.
#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_log_rebuilds_a_patched_row_onto_the_seeded_one() {
    let db = TestDb::new().await;
    let repo = PgSubjectKinds::new(db.pool.clone());
    let seeded = repo
        .get("shipment")
        .await
        .unwrap()
        .expect("seeded")
        .metadata;
    repo.patch_metadata("shipment", &patch(json!({"module": "x"})), &stamp())
        .await
        .unwrap();
    let patched = repo
        .patch_metadata("shipment", &patch(json!({"module": "shipping"})), &stamp())
        .await
        .unwrap()
        .expect("shipment exists")
        .metadata;
    relay_outbox(&db.pool).await;

    // The fresh database: the row back where the migrations left it.
    sqlx::query("UPDATE subject_kinds SET metadata = $1 WHERE kind = 'shipment'")
        .bind(&seeded)
        .execute(&db.pool)
        .await
        .unwrap();
    let report = rebuild_subject_kinds(&db.pool).await.expect("rebuild");
    assert_eq!(report.events_processed, 2);
    assert_eq!(report.kinds_written, 1);
    assert_eq!(
        repo.get("shipment").await.unwrap().unwrap().metadata,
        patched
    );

    let again = rebuild_subject_kinds(&db.pool).await.expect("re-run");
    assert_eq!((again.kinds_written, again.kinds_already_current), (0, 1));

    // A change the log never heard of: drift, and nothing is written.
    let foreign = json!({"module": "somewhere-else"});
    sqlx::query("UPDATE subject_kinds SET metadata = $1 WHERE kind = 'shipment'")
        .bind(&foreign)
        .execute(&db.pool)
        .await
        .unwrap();
    let err = rebuild_subject_kinds(&db.pool).await.unwrap_err();
    assert!(err.contains("shipment") && err.contains("drift"), "{err}");
    assert_eq!(
        repo.get("shipment").await.unwrap().unwrap().metadata,
        foreign
    );
}

/// `boss-rebuild-all` says every step holds its projection's
/// `pg_advisory_xact_lock` under `lock_key(<step>)`; `subject-kinds`
/// took none (backlog 8d5ac7c5). Holding `lock_key("subject-kinds")` on
/// another session must hold the rebuild back until it is released.
#[tokio::test(flavor = "multi_thread")]
async fn the_rebuild_waits_on_the_subject_kinds_rebuild_lock() {
    let db = TestDb::new().await;
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(boss_core::rebuild::lock_key("subject-kinds"))
        .execute(&mut *holder)
        .await
        .unwrap();

    let pool = db.pool.clone();
    let rebuild = tokio::spawn(async move { rebuild_subject_kinds(&pool).await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !rebuild.is_finished(),
        "the rebuild ran while another session held the subject-kinds rebuild lock"
    );

    holder.rollback().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), rebuild)
        .await
        .expect("the rebuild finishes once the lock is released")
        .unwrap()
        .expect("rebuild");
}
