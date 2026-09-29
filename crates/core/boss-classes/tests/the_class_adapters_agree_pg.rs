//! The Class registry's two adapters, held to one statement of the door
//! contract (`boss_testing::adapters_agree!`, design 3036296f mechanism C).
//!
//! Backlog 6c2aa86c. A door fact — an edit or a retirement — on a Class
//! the log does not declare is what switched off the backfill's only
//! check on a change made outside the doors (the stamp guard ran only on
//! an EMPTY log), so after it the bodiless backfill could declare an
//! out-of-doors edit as the Class's birth. The doors now record the birth
//! first, in the same transaction, for a Class whose row says nothing
//! outside the doors touched it — and each adapter must say so the same
//! way. Asked through the port alone: once a door has written, the
//! backfill answers `AlreadyDeclared`, which it gives only for a log that
//! declares the Class AND replays from nothing to the live row.
//!
//! Each adapter starts holding the same two undeclared rows, as a
//! migration or the pre-2026-09-17 batch door left them: the row, no fact.
//! The refusal (a row whose stamps show a change outside the doors) is
//! the Postgres adapter's alone — the in-memory double has no path that
//! writes a row outside its doors — and is in `classes_pg.rs`.

use boss_classes::InMemoryClasses;
use boss_classes::port::{Backfill, ClassRepository};
use boss_classes::postgres::PgClasses;
use boss_core::primitives::{Class, ClassRef};
use boss_core::publisher::EventStamp;

fn stamp() -> EventStamp {
    EventStamp::new(
        "classes",
        boss_core::actor::ActorId::Automation("tenant-seed".into()),
    )
}

/// An undeclared row, as both adapters hold it before any door wrote.
fn undeclared(code: &str) -> Class {
    Class {
        subject_kind: "employee".into(),
        code: code.into(),
        display_name: code.into(),
        parent_code: None,
        member_attribute: Some("department".into()),
        metadata: serde_json::json!({}),
        sort_order: 60,
        retired_at: None,
    }
}

const CODES: [&str; 3] = ["pre-door-edit", "pre-door-retire", "pre-door-restated"];

async fn postgres_holding_the_undeclared_rows() -> (PgClasses, boss_testing::TestDb) {
    let db = boss_testing::TestDb::new().await;
    for code in CODES {
        let c = undeclared(code);
        sqlx::query(
            "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&c.subject_kind)
        .bind(&c.code)
        .bind(&c.display_name)
        .bind(&c.member_attribute)
        .bind(c.sort_order)
        .execute(&db.pool)
        .await
        .unwrap();
    }
    (PgClasses::new(db.pool.clone()), db)
}

/// An edit through the door of an untouched undeclared Class leaves the
/// edited row AND a log that declares it and replays to that row.
async fn a_door_edit_declares_an_untouched_undeclared_class<R: ClassRepository>(
    repo: &R,
    adapter: &str,
) {
    let key = ClassRef::new("employee", "pre-door-edit");
    let mut edited = undeclared("pre-door-edit");
    edited.display_name = "Edited through the door".into();
    edited.metadata = serde_json::json!({"department": "ops"});
    assert!(repo.update(&edited, &stamp()).await.unwrap(), "{adapter}");
    assert_eq!(repo.get(&key).await.unwrap(), Some(edited), "{adapter}");
    assert_eq!(
        repo.backfill_declared(&key, &stamp()).await.unwrap(),
        Backfill::AlreadyDeclared,
        "{adapter}: the door declared the birth before its edit"
    );
}

/// A retirement through the door, the same.
async fn a_door_retire_declares_an_untouched_undeclared_class<R: ClassRepository>(
    repo: &R,
    adapter: &str,
) {
    let key = ClassRef::new("employee", "pre-door-retire");
    assert!(repo.retire(&key, &stamp()).await.unwrap(), "{adapter}");
    assert!(!repo.exists_active(&key).await.unwrap(), "{adapter}");
    assert_eq!(
        repo.backfill_declared(&key, &stamp()).await.unwrap(),
        Backfill::AlreadyDeclared,
        "{adapter}: the door declared the birth before its retirement"
    );
}

/// A PUT that restates the row changes nothing, so it records nothing —
/// not even the birth: the Class is still undeclared, and the bodiless
/// backfill records it as the row it is.
async fn a_restatement_through_the_door_declares_nothing<R: ClassRepository>(
    repo: &R,
    adapter: &str,
) {
    let held = undeclared("pre-door-restated");
    assert!(repo.update(&held, &stamp()).await.unwrap(), "{adapter}");
    assert_eq!(
        repo.backfill_declared(&ClassRef::new("employee", "pre-door-restated"), &stamp())
            .await
            .unwrap(),
        Backfill::Recorded(held),
        "{adapter}"
    );
}

boss_testing::adapters_agree! {
    adapters {
        in_memory => (
            InMemoryClasses::new(CODES.iter().map(|c| undeclared(c)).collect()),
            (),
        ),
        postgres => postgres_holding_the_undeclared_rows().await,
    }
    cases {
        a_door_edit_declares_an_untouched_undeclared_class,
        a_door_retire_declares_an_untouched_undeclared_class,
        a_restatement_through_the_door_declares_nothing,
    }
}
