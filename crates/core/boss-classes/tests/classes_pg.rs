//! The classes.subject_kind FK (subject-model audit residual, closed
//! 2026-07-29): a Class belongs to a registered SubjectKind by
//! definition, so a row naming an unregistered kind aborts at the
//! database — and the batch adapter surfaces WHICH kind offended
//! instead of a generic storage error.

use boss_classes::port::{ClassError, ClassRepository};
use boss_classes::postgres::PgClasses;
use boss_testing::TestDb;

fn class_row(subject_kind: &str, code: &str) -> boss_core::primitives::Class {
    boss_core::primitives::Class {
        subject_kind: subject_kind.to_string(),
        code: code.to_string(),
        display_name: code.to_string(),
        parent_code: None,
        member_attribute: None,
        metadata: serde_json::json!({}),
        sort_order: 0,
        retired_at: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn class_for_registered_kind_lands_and_unregistered_aborts_with_the_kind_named() {
    let db = TestDb::new().await;
    let repo = PgClasses::new(db.pool.clone());

    // Registered kind (platform seed) → lands.
    repo.batch_upsert(&[class_row("employee", "test-role")])
        .await
        .expect("registered kind must land");

    // Unregistered kind → aborts with the kind named, not Storage.
    let err = repo
        .batch_upsert(&[class_row("made-up-kind", "whatever")])
        .await
        .expect_err("unregistered kind must abort");
    match err {
        ClassError::UnregisteredKind(kind) => assert_eq!(kind, "made-up-kind"),
        other => panic!("expected UnregisteredKind, got {other:?}"),
    }
}

/// The retire path against the real adapter: stamp once, hold the
/// stamp on a repeat, refuse nothing that exists, 404 what doesn't —
/// and the read primitives agree (`exists_active` refuses, `get`
/// still returns the row).
#[tokio::test(flavor = "multi_thread")]
async fn retire_stamps_once_and_the_read_primitives_agree() {
    let db = TestDb::new().await;
    let repo = PgClasses::new(db.pool.clone());
    repo.batch_upsert(&[class_row("employee", "retire-me")])
        .await
        .expect("seed row");
    let cref = boss_core::primitives::ClassRef::new("employee", "retire-me");

    assert!(
        repo.retire(&cref).await.expect("retire"),
        "existing row retires"
    );
    assert!(
        !repo.exists_active(&cref).await.expect("exists_active"),
        "a retired code is refused for new use"
    );
    let row = repo.get(&cref).await.expect("get").expect("row stays");
    let first_stamp = row.retired_at.expect("stamped");

    assert!(
        repo.retire(&cref).await.expect("repeat retire"),
        "repeat is a no-op success"
    );
    let row = repo.get(&cref).await.expect("get").expect("row stays");
    assert_eq!(
        row.retired_at.expect("still stamped"),
        first_stamp,
        "a repeat call must not move when it was withdrawn"
    );

    let missing = boss_core::primitives::ClassRef::new("employee", "never-was");
    assert!(
        !repo.retire(&missing).await.expect("missing"),
        "no row, no retire"
    );
}
