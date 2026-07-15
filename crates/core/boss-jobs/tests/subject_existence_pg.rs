//! `PgSubjectExistence` — the uniform gate adapter over the
//! `subjects` identity table (R1). Every kind is checked the same
//! way; there are no fall-through kinds anymore.

#![cfg(feature = "postgres")]

use boss_core::job::Subject;
use boss_jobs::subject_existence::{
    PgSubjectExistence, SubjectExistenceCheck, SubjectExistenceError,
};
use boss_testing::TestDb;

#[tokio::test(flavor = "multi_thread")]
async fn known_identity_passes_unknown_fails_every_kind() {
    let db = TestDb::new().await;
    sqlx::query("INSERT INTO subjects (kind, id) VALUES ('account','acc-1'), ('vendor','vnd-1'), ('campaign','cmp-1')")
        .execute(&db.pool)
        .await
        .unwrap();
    let check = PgSubjectExistence::new(db.pool.clone());

    for (kind, id) in [
        ("account", "acc-1"),
        ("vendor", "vnd-1"),
        ("campaign", "cmp-1"),
    ] {
        check
            .check(&Subject::new(kind, id))
            .await
            .unwrap_or_else(|e| panic!("{kind}/{id} should exist: {e}"));
    }
    // vendor + campaign were the fall-through kinds of the old HTTP
    // prober — now they are checked like everything else.
    for (kind, id) in [
        ("vendor", "vnd-ghost"),
        ("campaign", "cmp-ghost"),
        ("account", "acc-ghost"),
    ] {
        match check.check(&Subject::new(kind, id)).await {
            Err(SubjectExistenceError::NotFound(_)) => {}
            other => panic!("{kind}/{id} must be NotFound, got {other:?}"),
        }
    }
}
