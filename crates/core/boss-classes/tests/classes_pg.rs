//! The classes.subject_kind FK (subject-model audit residual, closed
//! 2026-07-29): a Class belongs to a registered SubjectKind by
//! definition, so a row naming an unregistered kind aborts at the
//! database — and the batch adapter surfaces WHICH kind offended
//! instead of a generic storage error.

#![cfg(feature = "postgres")]

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
