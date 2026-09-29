//! The classes.subject_kind FK (subject-model audit residual, closed
//! 2026-07-29): a Class belongs to a registered SubjectKind by
//! definition, so a row naming an unregistered kind aborts at the
//! database — and the batch adapter surfaces WHICH kind offended
//! instead of a generic storage error.

use boss_classes::port::{
    CLASS_DECLARED, CLASS_RETIRED, CLASS_UPDATED, ClassError, ClassRepository,
};
use boss_classes::postgres::PgClasses;
use boss_core::publisher::EventStamp;
use boss_testing::TestDb;

fn stamp() -> EventStamp {
    EventStamp::new(
        "classes",
        boss_core::actor::ActorId::Automation("tenant-seed".into()),
    )
}

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
    repo.batch_upsert(&[class_row("employee", "test-role")], &stamp())
        .await
        .expect("registered kind must land");

    // Unregistered kind → aborts with the kind named, not Storage.
    let err = repo
        .batch_upsert(&[class_row("made-up-kind", "whatever")], &stamp())
        .await
        .expect_err("unregistered kind must abort");
    match err {
        ClassError::UnregisteredKind(kind) => assert_eq!(kind, "made-up-kind"),
        other => panic!("expected UnregisteredKind, got {other:?}"),
    }

    // The fact rides the insert's transaction (backlog d9409039): the
    // row that landed staged one `class.declared` on the outbox, the
    // refused row staged nothing, and a re-run of the landed row —
    // kept, not inserted — stages nothing either.
    repo.batch_upsert(&[class_row("employee", "test-role")], &stamp())
        .await
        .expect("a re-run is a no-op");
    let staged: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT source, payload FROM event_outbox WHERE kind = $1")
            .bind(CLASS_DECLARED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(staged.len(), 1, "{staged:?}");
    assert_eq!(staged[0].0, "classes");
    assert_eq!(staged[0].1["code"], "test-role");
    assert_eq!(staged[0].1["declared_by"], "automation:tenant-seed");
}

/// The retire path against the real adapter: stamp once, hold the
/// stamp on a repeat, refuse nothing that exists, 404 what doesn't —
/// and the read primitives agree (`exists_active` refuses, `get`
/// still returns the row).
#[tokio::test(flavor = "multi_thread")]
async fn retire_stamps_once_and_the_read_primitives_agree() {
    let db = TestDb::new().await;
    let repo = PgClasses::new(db.pool.clone());
    repo.batch_upsert(&[class_row("employee", "retire-me")], &stamp())
        .await
        .expect("seed row");
    let cref = boss_core::primitives::ClassRef::new("employee", "retire-me");

    assert!(
        repo.retire(&cref, &stamp()).await.expect("retire"),
        "existing row retires"
    );
    assert!(
        !repo.exists_active(&cref).await.expect("exists_active"),
        "a retired code is refused for new use"
    );
    let row = repo.get(&cref).await.expect("get").expect("row stays");
    let first_stamp = row.retired_at.expect("stamped");

    assert!(
        repo.retire(&cref, &stamp()).await.expect("repeat retire"),
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
        !repo.retire(&missing, &stamp()).await.expect("missing"),
        "no row, no retire"
    );

    // The fact rides the stamp's own transaction (backlog 10dabe13):
    // ONE `class.retired` on the outbox, for the call that moved the
    // stamp — none for the repeat, none for the missing code.
    let staged: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(CLASS_RETIRED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(staged.len(), 1, "{staged:?}");
    assert_eq!(staged[0]["code"], "retire-me");
    assert_eq!(staged[0]["retired_by"], "automation:tenant-seed");
    assert_eq!(
        staged[0]["retired_at"],
        serde_json::to_value(first_stamp).unwrap(),
        "the fact carries the stamp the row holds"
    );
}

/// The edit path against the real adapter (backlog 10dabe13): the
/// UPDATE and its `class.updated` commit together, the fact names the
/// changed fields before and after and who changed them, and a PUT
/// that restates the held row changes nothing and stages nothing.
#[tokio::test(flavor = "multi_thread")]
async fn update_stages_one_updated_event_for_a_change_and_none_for_a_restatement() {
    let db = TestDb::new().await;
    let repo = PgClasses::new(db.pool.clone());
    repo.batch_upsert(&[class_row("employee", "edit-me")], &stamp())
        .await
        .expect("seed row");

    let mut edited = class_row("employee", "edit-me");
    edited.display_name = "Edited".to_string();
    edited.sort_order = 7;
    assert!(repo.update(&edited, &stamp()).await.expect("update"));
    assert!(
        repo.update(&edited, &stamp()).await.expect("restatement"),
        "a restatement still names an existing row"
    );
    let held = repo
        .get(&boss_core::primitives::ClassRef::new("employee", "edit-me"))
        .await
        .expect("get")
        .expect("row stays");
    assert_eq!(held.display_name, "Edited");
    assert_eq!(held.sort_order, 7);

    let missing = class_row("employee", "never-was");
    assert!(
        !repo.update(&missing, &stamp()).await.expect("missing"),
        "no row, no update"
    );

    let staged: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(CLASS_UPDATED)
            .fetch_all(&db.pool)
            .await
            .expect("outbox reads");
    assert_eq!(staged.len(), 1, "{staged:?}");
    assert_eq!(staged[0]["code"], "edit-me");
    assert_eq!(
        staged[0]["changed"],
        serde_json::json!(["display_name", "sort_order"])
    );
    assert_eq!(
        staged[0]["before"],
        serde_json::json!({"display_name": "edit-me", "sort_order": 0})
    );
    assert_eq!(
        staged[0]["after"],
        serde_json::json!({"display_name": "Edited", "sort_order": 7})
    );
    assert_eq!(staged[0]["updated_by"], "automation:tenant-seed");
}

/// Copy `from`'s staged class facts into `to`'s audit_log, in order —
/// where the relay would have put them, and what a log-rooted rebuild
/// (the epoch / DR path: migrations, then the log imported) reads.
async fn import_log(from: &sqlx::PgPool, to: &sqlx::PgPool) {
    type Row = (
        String,
        String,
        String,
        chrono::DateTime<chrono::Utc>,
        serde_json::Value,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT event_id::text, kind, source, timestamp, payload FROM event_outbox \
          WHERE kind LIKE 'class.%' ORDER BY id",
    )
    .fetch_all(from)
    .await
    .expect("outbox reads");
    for (event_id, kind, source, ts, payload) in rows {
        sqlx::query(
            "INSERT INTO audit_log (event_id, kind, source, timestamp, payload) \
             VALUES ($1::uuid, $2, $3, $4, $5)",
        )
        .bind(event_id)
        .bind(kind)
        .bind(source)
        .bind(ts)
        .bind(payload)
        .execute(to)
        .await
        .expect("import");
    }
}

/// Backlog 9d345f9b (closure): a Class the batch door inserted before it
/// staged `class.declared` (2026-09-17), and no migration seeds, has no
/// birth in the log. Measured live 2026-09-28: ten such Classes, four
/// already retired through the door. On a fresh database the retirement
/// replays onto no row and the WHOLE rebuild stops as drift. Once the
/// birth is backfilled — as the row before its retirement, recorded
/// after it and read first — migrations plus the log reproduce both
/// rows, the live table is already at the log's head, and a repeat
/// backfill records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_class_born_before_the_declare_door_rebuilds_once_its_birth_is_backfilled() {
    use boss_classes::port::{Backfill, is_backfill};
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    // Born the way the batch door made rows before 2026-09-17: the row,
    // and no fact.
    for code in ["pre-door-hosting", "pre-door-agent"] {
        sqlx::query(
            "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
             VALUES ('employee', $1, $1, 'department', 60)",
        )
        .bind(code)
        .execute(&live.pool)
        .await
        .unwrap();
    }
    let hosting = ClassRef::new("employee", "pre-door-hosting");
    let agent = ClassRef::new("employee", "pre-door-agent");
    // Retired as the door retired before it declared first (backlog
    // 6c2aa86c): the door now records the birth before its retirement,
    // so this shape is the historical one the live Classes are in.
    retire_as_the_door_did_before_it_declared(&live.pool, "pre-door-hosting").await;

    // The defect: the log holds the retirement and not the birth.
    let before = TestDb::new().await;
    import_log(&live.pool, &before.pool).await;
    let err = rebuild_classes(&before.pool).await.unwrap_err();
    assert!(
        err.contains("drift") && err.contains("pre-door-hosting"),
        "{err}"
    );

    // The backfill: each birth once, the retired one as it was before
    // its retirement; a repeat and a missing code record nothing.
    let mut born = repo.get(&hosting).await.unwrap().expect("the live row");
    born.retired_at = None;
    assert_eq!(
        repo.backfill_declared(&hosting, &stamp()).await.unwrap(),
        Backfill::Recorded(born)
    );
    assert!(matches!(
        repo.backfill_declared(&agent, &stamp()).await.unwrap(),
        Backfill::Recorded(_)
    ));
    for key in [&hosting, &agent] {
        assert_eq!(
            repo.backfill_declared(key, &stamp()).await.unwrap(),
            Backfill::AlreadyDeclared,
            "{key:?}"
        );
    }
    assert_eq!(
        repo.backfill_declared(&ClassRef::new("employee", "never-was"), &stamp())
            .await
            .unwrap(),
        Backfill::NotFound
    );
    let staged: Vec<serde_json::Value> =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY id")
            .bind(CLASS_DECLARED)
            .fetch_all(&live.pool)
            .await
            .unwrap();
    assert_eq!(staged.len(), 2, "{staged:?}");
    assert!(staged.iter().all(is_backfill), "{staged:?}");
    assert_eq!(staged[0]["declared_by"], "automation:tenant-seed");

    // Migrations plus the log now reproduce both rows — the backfill
    // was appended after the retirement it precedes, and is read first.
    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    let report = rebuild_classes(&fresh.pool).await.expect("rebuild");
    assert_eq!(report.classes_written, 2, "{report:?}");
    let rebuilt = PgClasses::new(fresh.pool.clone());
    for key in [&hosting, &agent] {
        let want = repo.get(key).await.unwrap().expect("live row");
        assert_eq!(rebuilt.get(key).await.unwrap(), Some(want), "{key:?}");
    }
    // And the live table is already where its log arrives.
    import_log(&live.pool, &live.pool).await;
    let in_place = rebuild_classes(&live.pool).await.expect("in place");
    assert_eq!(
        (in_place.classes_written, in_place.classes_already_current),
        (0, 2),
        "{in_place:?}"
    );
}

/// Review of car ce5ce2de, HIGH-1: a migration-seeded row the OLD edit
/// door changed (a bare UPDATE that set `updated_at = now()` and left
/// no fact, until 10dabe13) has no fact to un-apply, so its live body
/// would be recorded as its birth — a declare the migration's seed
/// contradicts on every fresh database, forever, since facts are
/// immutable. With no fact and `updated_at` moved off `created_at`,
/// the row was changed by something the log never heard of: 409 drift,
/// nothing staged, and a fresh rebuild still succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn a_seeded_row_edited_outside_the_doors_is_refused_not_backfilled() {
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    sqlx::query(
        "UPDATE classes SET display_name = 'Chief', updated_at = now() \
          WHERE subject_kind = 'employee' AND code = 'ceo'",
    )
    .execute(&live.pool)
    .await
    .unwrap();
    let err = repo
        .backfill_declared(&ClassRef::new("employee", "ceo"), &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, ClassError::Drift(_)), "{err}");
    assert!(err.to_string().contains("ceo"), "{err}");
    let staged: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
        .bind(CLASS_DECLARED)
        .fetch_one(&live.pool)
        .await
        .unwrap();
    assert_eq!(staged, 0);

    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    rebuild_classes(&fresh.pool)
        .await
        .expect("no false birth in the log, so a fresh rebuild does not stop");
}

/// A row the batch door inserted with no fact, given `gl_account` by a
/// bare UPDATE — the LLC's three invoice Classes, measured 2026-09-28
/// (backlog 93f361af). Insert it with the born body and move
/// `updated_at` off `created_at`, as that UPDATE did.
async fn insert_then_edit_outside_the_doors(pool: &sqlx::PgPool, code: &str) {
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
         VALUES ('employee', $1, 'Hosted instance', 'revenue_category', 30)",
    )
    .bind(code)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE classes SET metadata = '{\"gl_account\": \"4400\"}', \
                updated_at = created_at + interval '10 days' \
          WHERE subject_kind = 'employee' AND code = $1",
    )
    .bind(code)
    .execute(pool)
    .await
    .unwrap();
}

fn born_as(code: &str) -> boss_core::primitives::Class {
    boss_core::primitives::Class {
        display_name: "Hosted instance".into(),
        member_attribute: Some("revenue_category".into()),
        sort_order: 30,
        ..class_row("employee", code)
    }
}

/// `born` named with the one field the edit outside the doors changed.
fn named(born: boss_core::primitives::Class, source: &str) -> boss_classes::port::NamedBirth {
    boss_classes::port::NamedBirth {
        born,
        changed: vec!["metadata".into()],
        source: source.into(),
    }
}

/// The row's two stamps, read the way the backfill reads them.
async fn stamps_of(
    pool: &sqlx::PgPool,
    code: &str,
) -> (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) {
    sqlx::query_as(
        "SELECT created_at, updated_at FROM classes \
          WHERE subject_kind = 'employee' AND code = $1",
    )
    .bind(code)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Backlog 93f361af: the bodiless backfill refuses a row with no fact
/// whose `updated_at` moved (HIGH-1), and that refusal left three live
/// Classes with no birth, so no fresh rebuild reproduced them. Named
/// with its pre-edit body and its source, the birth AND the edit are
/// staged, both marked; migrations plus the log then rebuild the live
/// row, the live table is already at the log's head, and a repeat —
/// named or not — records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_class_edited_outside_the_doors_rebuilds_once_its_named_birth_and_edit_are_backfilled() {
    use boss_classes::port::{
        BACKFILL_SOURCE, BORN_AT, Backfill, EDITED_AT, EDITED_BY, is_backfill,
    };
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    insert_then_edit_outside_the_doors(&live.pool, "pre-door-revenue").await;
    let key = ClassRef::new("employee", "pre-door-revenue");
    let held = repo.get(&key).await.unwrap().expect("the live row");
    let err = repo.backfill_declared(&key, &stamp()).await.unwrap_err();
    assert!(matches!(err, ClassError::Drift(_)), "{err}");

    let source = "algedonic-llc 28a7ac1^ seeds/classes.json";
    let born = born_as("pre-door-revenue");
    let (inserted, edited) = stamps_of(&live.pool, "pre-door-revenue").await;
    let Backfill::RecordedWithEdit {
        born: recorded,
        change,
    } = repo
        .backfill_edited(&key, &named(born.clone(), source), &stamp())
        .await
        .unwrap()
    else {
        panic!("a named birth for a row edited outside the doors is recorded");
    };
    assert_eq!(recorded, born);
    assert_eq!(change.changed, vec!["metadata"]);
    assert_eq!(
        repo.get(&key).await.unwrap(),
        Some(held.clone()),
        "no row write"
    );
    assert_eq!(
        repo.backfill_edited(&key, &named(born.clone(), source), &stamp())
            .await
            .unwrap(),
        Backfill::AlreadyDeclared
    );
    assert_eq!(
        repo.backfill_declared(&key, &stamp()).await.unwrap(),
        Backfill::AlreadyDeclared
    );
    let staged: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT kind, payload FROM event_outbox ORDER BY id")
            .fetch_all(&live.pool)
            .await
            .unwrap();
    let kinds: Vec<&str> = staged.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, vec![CLASS_DECLARED, CLASS_UPDATED], "{staged:?}");
    for (_, payload) in &staged {
        assert!(is_backfill(payload), "{payload}");
        assert_eq!(payload[BACKFILL_SOURCE], source);
    }
    // Review of car 8778f12f, finding 2: the row's stamps, read under the
    // lock, ride on the facts — born 10 days before the edit, as the row
    // says — and the edit is credited to no one.
    assert_ne!(inserted, edited);
    assert_eq!(staged[0].1[BORN_AT], serde_json::json!(inserted));
    assert_eq!(staged[1].1[EDITED_AT], serde_json::json!(edited));
    assert_eq!(staged[1].1.get(EDITED_BY), Some(&serde_json::Value::Null));

    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    let report = rebuild_classes(&fresh.pool).await.expect("rebuild");
    assert_eq!(report.classes_written, 1, "{report:?}");
    let rebuilt = PgClasses::new(fresh.pool.clone());
    assert_eq!(rebuilt.get(&key).await.unwrap(), Some(held));
    import_log(&live.pool, &live.pool).await;
    let in_place = rebuild_classes(&live.pool).await.expect("in place");
    assert_eq!(
        (in_place.classes_written, in_place.classes_already_current),
        (0, 1),
        "{in_place:?}"
    );
}

/// What a named birth may not claim, each staging nothing of its own: an
/// edit of a row whose stamps say nothing edited it, and — the loophole
/// triage refused — a birth named AFTER a door fact (PUT metadata {}
/// first), which would record two edits that never happened. The door
/// now refuses that PUT itself (backlog 6c2aa86c); a log that holds a
/// fact from before it did is refused by the named door's own guard, so
/// the birth is the row before that fact, not a body the caller names.
#[tokio::test(flavor = "multi_thread")]
async fn a_named_birth_is_refused_for_an_unedited_row_and_after_a_door_fact() {
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
         VALUES ('employee', 'pre-door-untouched', 'Hosted instance', 'revenue_category', 30)",
    )
    .execute(&live.pool)
    .await
    .unwrap();
    let mut claimed = born_as("pre-door-untouched");
    claimed.metadata = serde_json::json!({"gl_account": "0000"});
    let err = repo
        .backfill_edited(
            &ClassRef::new("employee", "pre-door-untouched"),
            &named(claimed, "a commit"),
            &stamp(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClassError::Drift(_)) && err.to_string().contains("not changed"),
        "{err}"
    );

    // The loophole's first step — PUT metadata {} through the door onto
    // a row edited outside it — is refused at the door since backlog
    // 6c2aa86c: the log does not declare the Class and its stamps show
    // the outside edit, so no door fact may land on it.
    insert_then_edit_outside_the_doors(&live.pool, "pre-door-laundered").await;
    let key = ClassRef::new("employee", "pre-door-laundered");
    let mut emptied = repo.get(&key).await.unwrap().expect("the row");
    emptied.metadata = serde_json::json!({});
    let err = repo.update(&emptied, &stamp()).await.unwrap_err();
    assert!(err.to_string().contains("backfill-declared"), "{err}");

    // Review of car 8778f12f, finding 1: a log that already holds a fact
    // (a retirement the door made before it declared) is refused by the
    // guard's own words, with a birth that differs from the row. (A
    // birth EQUAL to the row was refused as "no edit to record", which
    // passed with the guard deleted.)
    retire_as_the_door_did_before_it_declared(&live.pool, "pre-door-laundered").await;
    let mut born = born_as("pre-door-laundered");
    born.metadata = serde_json::json!({"x": 1});
    let err = repo
        .backfill_edited(&key, &named(born, "a commit"), &stamp())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClassError::Drift(_)) && err.to_string().contains("holds 1 fact(s)"),
        "{err}"
    );
    let staged: Vec<String> = sqlx::query_scalar("SELECT kind FROM event_outbox ORDER BY id")
        .fetch_all(&live.pool)
        .await
        .unwrap();
    assert_eq!(
        staged,
        vec![CLASS_RETIRED],
        "only the retirement's own fact"
    );
}

/// A row as the batch door inserted it before 2026-09-17: the row, and
/// no fact — both stamps one NOW().
async fn insert_undeclared(pool: &sqlx::PgPool, code: &str) {
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
         VALUES ('employee', $1, $1, 'department', 60)",
    )
    .bind(code)
    .execute(pool)
    .await
    .unwrap();
}

/// A bare UPDATE, as the two doors that bypass the log do (backlog
/// 99539bd3): the body changes and `updated_at` moves, and no fact.
async fn change_outside_the_doors(pool: &sqlx::PgPool, code: &str) {
    sqlx::query(
        "UPDATE classes SET display_name = 'Changed outside the doors', \
                updated_at = clock_timestamp() \
          WHERE subject_kind = 'employee' AND code = $1",
    )
    .bind(code)
    .execute(pool)
    .await
    .unwrap();
}

/// The retire door as it stood before this car: stamp `retired_at` and
/// `updated_at` with one NOW() and stage the `class.retired`, with no
/// birth — the shape the four live Classes employee/it, node/legacy-stack,
/// marketing-asset/retro and marketing-asset/brief-body are in (measured
/// 2026-09-28, backlog 6c2aa86c).
async fn retire_as_the_door_did_before_it_declared(pool: &sqlx::PgPool, code: &str) {
    let mut tx = pool.begin().await.unwrap();
    let at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "UPDATE classes SET retired_at = now(), updated_at = now() \
          WHERE subject_kind = 'employee' AND code = $1 RETURNING retired_at",
    )
    .bind(code)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let row = boss_core::primitives::Class {
        retired_at: Some(at),
        ..class_row("employee", code)
    };
    boss_events::outbox::record_event_in_tx(
        &mut tx,
        &boss_classes::port::retired_event(&stamp(), &row),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

/// Every class fact staged so far, as `(kind, code)`, in order.
async fn staged(pool: &sqlx::PgPool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT kind, payload->>'code' FROM event_outbox WHERE kind LIKE 'class.%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

fn pair(kind: &str, code: &str) -> (String, String) {
    (kind.to_string(), code.to_string())
}

/// Backlog 6c2aa86c (a): an edit through the door of a Class the log
/// does not declare, whose stamps say nothing outside the doors touched
/// it, records the birth first — the row as inserted, marked as a
/// backfill — then the edit, in ONE transaction: an edit whose commit
/// fails (a parent the deferred FK refuses at commit) leaves neither.
/// Migrations plus the log then reproduce the row.
#[tokio::test(flavor = "multi_thread")]
async fn a_door_edit_of_an_untouched_undeclared_class_stages_its_birth_then_the_edit_in_one_transaction()
 {
    use boss_classes::port::is_backfill;
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    insert_undeclared(&live.pool, "pre-door-edited").await;
    let key = ClassRef::new("employee", "pre-door-edited");
    let born = repo.get(&key).await.unwrap().expect("the row");

    let mut orphaned = born.clone();
    orphaned.parent_code = Some("no-such-parent".into());
    repo.update(&orphaned, &stamp())
        .await
        .expect_err("the deferred parent FK refuses the commit");
    assert!(staged(&live.pool).await.is_empty(), "neither fact");
    assert_eq!(repo.get(&key).await.unwrap(), Some(born.clone()));

    let mut edited = born.clone();
    edited.display_name = "Edited through the door".into();
    assert!(repo.update(&edited, &stamp()).await.unwrap());
    assert_eq!(
        staged(&live.pool).await,
        vec![
            pair(CLASS_DECLARED, "pre-door-edited"),
            pair(CLASS_UPDATED, "pre-door-edited")
        ]
    );
    let declared: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM event_outbox WHERE kind = $1")
            .bind(CLASS_DECLARED)
            .fetch_one(&live.pool)
            .await
            .unwrap();
    assert!(is_backfill(&declared), "{declared}");
    assert_eq!(
        declared["display_name"], "pre-door-edited",
        "the true birth"
    );

    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    rebuild_classes(&fresh.pool).await.expect("rebuild");
    let rebuilt = PgClasses::new(fresh.pool.clone());
    assert_eq!(rebuilt.get(&key).await.unwrap(), Some(edited));
}

/// Backlog 6c2aa86c, reproduced in the shape the triage found it: a row
/// inserted with no fact, changed outside the doors, then retired through
/// the door. Before this car the retirement landed, and the bodiless
/// backfill then un-applied it and declared the OUTSIDE body as the
/// birth — the stamp guard ran only on an empty log. Now the retirement
/// and the edit are each refused 409, naming the named-birth door, and
/// write nothing; once that door has recorded the birth and the edit,
/// the door edits as usual.
#[tokio::test(flavor = "multi_thread")]
async fn a_door_write_on_an_undeclared_class_changed_outside_the_doors_is_refused_and_writes_nothing()
 {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use boss_classes::http::{ClassesApiState, router};
    use boss_classes::port::{Backfill, NamedBirth};
    use boss_core::primitives::ClassRef;
    use std::sync::Arc;
    use tower::ServiceExt;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    insert_undeclared(&live.pool, "pre-door-drifted").await;
    let key = ClassRef::new("employee", "pre-door-drifted");
    let born = repo.get(&key).await.unwrap().expect("the row");
    change_outside_the_doors(&live.pool, "pre-door-drifted").await;
    let held = repo.get(&key).await.unwrap().expect("the row");

    let err = repo.retire(&key, &stamp()).await.unwrap_err();
    assert!(
        matches!(err, ClassError::Drift(_)) && err.to_string().contains("backfill-declared"),
        "{err}"
    );
    let mut edited = held.clone();
    edited.sort_order = 61;
    let err = repo.update(&edited, &stamp()).await.unwrap_err();
    assert!(matches!(err, ClassError::Drift(_)), "{err}");
    assert_eq!(repo.get(&key).await.unwrap(), Some(held.clone()));
    assert!(staged(&live.pool).await.is_empty(), "nothing written");

    // Through HTTP, the refusal is a 409 that names the way.
    let app = router(ClassesApiState {
        classes: Arc::new(PgClasses::new(live.pool.clone())),
        policy: Arc::new(
            boss_policy_client::FakePolicyClient::builder()
                .with_default_rules()
                .build(),
        ),
    });
    let operator = serde_json::json!({
        "id": "automation:test-seed", "role": "platform-admin", "access_tier": "operator",
        "territory_account_ids": [], "direct_report_ids": [],
    })
    .to_string();
    let requests = [
        Request::builder()
            .method("PUT")
            .uri("/api/classes/employee/pre-door-drifted")
            .header("content-type", "application/json")
            .header("x-boss-user", &operator)
            .body(Body::from(serde_json::to_string(&edited).unwrap()))
            .unwrap(),
        Request::builder()
            .method("POST")
            .uri("/api/classes/employee/pre-door-drifted/retire")
            .header("x-boss-user", &operator)
            .body(Body::empty())
            .unwrap(),
    ];
    for req in requests {
        let uri = req.uri().to_string();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{uri}");
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            body.contains("/api/classes/employee/pre-door-drifted/backfill-declared"),
            "{uri}: {body}"
        );
    }
    assert!(staged(&live.pool).await.is_empty(), "nothing written");

    // The way it names: the birth and the outside edit, then the door.
    let named = NamedBirth {
        born,
        changed: vec!["display_name".into()],
        source: "a tenant commit".into(),
    };
    assert!(matches!(
        repo.backfill_edited(&key, &named, &stamp()).await.unwrap(),
        Backfill::RecordedWithEdit { .. }
    ));
    assert!(repo.update(&edited, &stamp()).await.unwrap());
    assert_eq!(
        staged(&live.pool).await,
        vec![
            pair(CLASS_DECLARED, "pre-door-drifted"),
            pair(CLASS_UPDATED, "pre-door-drifted"),
            pair(CLASS_UPDATED, "pre-door-drifted"),
        ]
    );
}

/// Backlog 6c2aa86c (b): a Class the door retired before it declared
/// (the four live ones) is backfilled by un-applying the retirement only
/// while the row's `updated_at` is still that retirement's — the stamp
/// the retire door wrote. A change outside the doors AFTER it moves the
/// stamp: the bodiless backfill refuses, and so does the door, whose
/// refusal says the named-birth door cannot take a log with a fact
/// either. A change BEFORE the retirement is covered by the retirement's
/// own stamp and cannot be seen — the documented limit. An untouched one
/// edited through the door is declared first, as the row before its
/// retirement, and migrations plus the log reproduce it.
#[tokio::test(flavor = "multi_thread")]
async fn a_class_retired_before_the_door_declared_is_backfilled_only_while_its_stamp_is_the_retirements()
 {
    use boss_classes::port::Backfill;
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    for code in ["retired-untouched", "retired-then-edited"] {
        insert_undeclared(&live.pool, code).await;
        retire_as_the_door_did_before_it_declared(&live.pool, code).await;
    }
    let untouched = ClassRef::new("employee", "retired-untouched");
    let born = boss_core::primitives::Class {
        member_attribute: Some("department".into()),
        sort_order: 60,
        ..class_row("employee", "retired-untouched")
    };
    assert_eq!(
        repo.backfill_declared(&untouched, &stamp()).await.unwrap(),
        Backfill::Recorded(born)
    );
    let key = ClassRef::new("employee", "retired-then-edited");
    let mut edited = repo.get(&key).await.unwrap().expect("the row");
    edited.display_name = "Edited after its retirement".into();
    assert!(repo.update(&edited, &stamp()).await.unwrap());
    assert_eq!(
        staged(&live.pool).await,
        vec![
            pair(CLASS_RETIRED, "retired-untouched"),
            pair(CLASS_RETIRED, "retired-then-edited"),
            pair(CLASS_DECLARED, "retired-untouched"),
            pair(CLASS_DECLARED, "retired-then-edited"),
            pair(CLASS_UPDATED, "retired-then-edited"),
        ]
    );
    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    rebuild_classes(&fresh.pool).await.expect("rebuild");
    let rebuilt = PgClasses::new(fresh.pool.clone());
    for k in [&untouched, &key] {
        assert_eq!(
            rebuilt.get(k).await.unwrap(),
            repo.get(k).await.unwrap(),
            "{k:?}"
        );
    }

    // Changed outside the doors after its retirement: refused by both.
    let other = TestDb::new().await;
    let repo = PgClasses::new(other.pool.clone());
    insert_undeclared(&other.pool, "retired-then-changed").await;
    retire_as_the_door_did_before_it_declared(&other.pool, "retired-then-changed").await;
    change_outside_the_doors(&other.pool, "retired-then-changed").await;
    let key = ClassRef::new("employee", "retired-then-changed");
    let err = repo.backfill_declared(&key, &stamp()).await.unwrap_err();
    assert!(
        matches!(err, ClassError::Drift(_)) && err.to_string().contains("newest fact"),
        "{err}"
    );
    let mut edited = repo.get(&key).await.unwrap().expect("the row");
    edited.sort_order = 1;
    let err = repo
        .update(&edited, &stamp())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("\"observed\": true") && !err.contains("{born, changed, source}"),
        "names the observed birth, which takes it, and not the named one, which would not: {err}"
    );
    assert_eq!(
        staged(&other.pool).await,
        vec![pair(CLASS_RETIRED, "retired-then-changed")],
        "only the retirement it already had"
    );
}

/// A pre-10dabe13 edit as the door made it: the body and `updated_at`
/// (the database's clock) change, and the `class.updated` is staged with
/// the service's clock — so the stamp and the fact never agree, and no
/// declare.
async fn edit_as_the_door_did_before_it_declared(pool: &sqlx::PgPool, code: &str) {
    use boss_classes::port::{class_change, updated_event};
    let repo = PgClasses::new(pool.clone());
    let key = boss_core::primitives::ClassRef::new("employee", code);
    let held = repo.get(&key).await.unwrap().expect("the row");
    let mut edited = held.clone();
    edited.sort_order = 99;
    let change = class_change(&held, &edited).unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "UPDATE classes SET sort_order = 99, updated_at = clock_timestamp() \
          WHERE subject_kind = 'employee' AND code = $1",
    )
    .bind(code)
    .execute(&mut *tx)
    .await
    .unwrap();
    let event = updated_event(&stamp(), &held, &change).unwrap().unwrap();
    boss_events::outbox::record_event_in_tx(&mut tx, &event)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Retired by a migration, as asset/triaging, refurbing, qa and ready
/// were on 2026-09-24 and message/archived on 2026-09-26: `retired_at`
/// and `updated_at` set to one NOW(), and no fact. `ago` sets how long
/// ago, so two databases can retire the same row at different times.
async fn retire_as_a_migration_did(pool: &sqlx::PgPool, code: &str, ago: &str) {
    sqlx::query(
        "UPDATE classes SET retired_at = now() - $2::interval, updated_at = now() - $2::interval \
          WHERE subject_kind = 'employee' AND code = $1",
    )
    .bind(code)
    .bind(ago)
    .execute(pool)
    .await
    .unwrap();
}

/// Review of this car (backlog 6c2aa86c): three shapes the declare-first
/// doors left with no door that would take them — (a) retired by a
/// migration, no fact (a migration-seeded role here, as the asset phases
/// are); (b) retired through the door, then changed outside it; (c) a
/// pre-10dabe13 edit fact and no declare. Each: the edit door refuses on
/// its stamps and names the observed birth; the dry run says `refuse`;
/// the observed birth is recorded (the stamps and the drift on the fact);
/// the dry run says `nothing`; the edit door then edits. Migrations plus
/// the log reproduce every row — on a fresh database whose migration
/// retired (a) at ANOTHER time, too — and the live table is already at
/// the log's head.
#[tokio::test(flavor = "multi_thread")]
async fn an_observed_birth_opens_every_door_the_stamps_closed_and_rebuilds_the_row() {
    use boss_classes::port::{BIRTH, Backfill, DRIFT, OBSERVED, ROW_UPDATED_AT, is_backfill};
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    // (a) a migration-seeded role, retired as a migration retires.
    retire_as_a_migration_did(&live.pool, "recruiter", "2 days").await;
    // (b) retired through the door before it declared, changed after.
    insert_undeclared(&live.pool, "observed-b").await;
    retire_as_the_door_did_before_it_declared(&live.pool, "observed-b").await;
    change_outside_the_doors(&live.pool, "observed-b").await;
    // (c) an early edit fact, no declare.
    insert_undeclared(&live.pool, "observed-c").await;
    edit_as_the_door_did_before_it_declared(&live.pool, "observed-c").await;
    let codes = ["recruiter", "observed-b", "observed-c"];
    let before = staged(&live.pool).await.len();

    let plans = repo.birth_plans(Some("employee")).await.unwrap();
    for code in codes {
        let key = ClassRef::new("employee", code);
        let plan = plans.iter().find(|p| p.code == code).expect("planned");
        assert_eq!(plan.door, "refuse", "{code}: {plan:?}");
        assert!(plan.created_at.is_some() && plan.updated_at.is_some());
        let mut edited = repo.get(&key).await.unwrap().expect("the row");
        edited.display_name = format!("{code}, edited");
        let err = repo
            .update(&edited, &stamp())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("\"observed\": true"), "{code}: {err}");
        assert!(
            matches!(
                repo.backfill_declared(&key, &stamp()).await,
                Err(ClassError::Drift(_))
            ),
            "{code}: the bodiless backfill refuses on the stamps"
        );
    }
    assert_eq!(staged(&live.pool).await.len(), before, "nothing written");

    for code in codes {
        let key = ClassRef::new("employee", code);
        let held = repo.get(&key).await.unwrap().expect("the row");
        let Backfill::Observed(born) = repo
            .backfill_observed(&key, "measured by the dry run", &stamp())
            .await
            .unwrap()
        else {
            panic!("{code}: the observed birth takes it");
        };
        assert_eq!(
            born.retired_at.is_some(),
            code == "recruiter",
            "{code}: a retirement no fact records stays on the birth"
        );
        assert_eq!(
            repo.backfill_observed(&key, "again", &stamp())
                .await
                .unwrap(),
            Backfill::AlreadyDeclared,
            "{code}"
        );
        assert_eq!(
            repo.get(&key).await.unwrap(),
            Some(held),
            "{code}: no row write"
        );
        let plan = repo.birth_plans(Some("employee")).await.unwrap();
        let plan = plan.iter().find(|p| p.code == code).unwrap();
        assert_eq!((plan.door, plan.declared), ("nothing", true), "{code}");
    }
    let observed: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT payload FROM event_outbox WHERE kind = $1 AND payload->>'birth' = $2 ORDER BY id",
    )
    .bind(CLASS_DECLARED)
    .bind(OBSERVED)
    .fetch_all(&live.pool)
    .await
    .unwrap();
    assert_eq!(observed.len(), 3, "{observed:?}");
    for p in &observed {
        assert!(is_backfill(p), "{p}");
        assert_eq!(p[BIRTH], OBSERVED);
        assert!(!p[ROW_UPDATED_AT].is_null(), "{p}");
        assert!(
            p[DRIFT].is_string(),
            "the stamps' drift rides on the fact: {p}"
        );
    }

    // The doors open.
    for code in codes {
        let key = ClassRef::new("employee", code);
        let mut edited = repo.get(&key).await.unwrap().expect("the row");
        edited.display_name = format!("{code}, edited");
        assert!(repo.update(&edited, &stamp()).await.unwrap(), "{code}");
    }

    // A fresh database: migrations (recruiter retired there by its own
    // migration run, at another time), then the log.
    let fresh = TestDb::new().await;
    retire_as_a_migration_did(&fresh.pool, "recruiter", "1 hour").await;
    import_log(&live.pool, &fresh.pool).await;
    rebuild_classes(&fresh.pool).await.expect("rebuild");
    let rebuilt = PgClasses::new(fresh.pool.clone());
    for code in codes {
        let key = ClassRef::new("employee", code);
        assert_eq!(
            rebuilt.get(&key).await.unwrap(),
            repo.get(&key).await.unwrap(),
            "{code}"
        );
    }
    import_log(&live.pool, &live.pool).await;
    let in_place = rebuild_classes(&live.pool).await.expect("in place");
    assert_eq!(in_place.classes_written, 0, "{in_place:?}");
}

/// Review of car ce5ce2de, LOW-3: a Class declared, deleted outside the
/// doors and declared again with another body is not "already
/// declared" — its log replays to drift on a fresh database — so the
/// backfill answers drift and stages nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_redeclared_class_whose_log_does_not_replay_is_drift() {
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    repo.batch_upsert(&[class_row("employee", "dept-sales")], &stamp())
        .await
        .unwrap();
    sqlx::query("DELETE FROM classes WHERE code = 'dept-sales'")
        .execute(&live.pool)
        .await
        .unwrap();
    let mut again = class_row("employee", "dept-sales");
    again.display_name = "Sales & Sponsorships".into();
    repo.batch_upsert(&[again], &stamp()).await.unwrap();

    let err = repo
        .backfill_declared(&ClassRef::new("employee", "dept-sales"), &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, ClassError::Drift(_)), "{err}");
    let staged: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
        .bind(CLASS_DECLARED)
        .fetch_one(&live.pool)
        .await
        .unwrap();
    assert_eq!(staged, 2, "the two door declares, and no backfill");
}

/// Review of car ce5ce2de, LOW-4: the row lock is what makes a race of
/// backfills record ONE birth — each waits for the one before it, then
/// reads its staged fact. Sixteen at once: one recorded, one staged.
///
/// The race is forced, not hoped for: a holder transaction takes the
/// outbox in EXCLUSIVE mode (reads pass, the staging INSERT waits), so
/// every racer that got past its fact read parks at the stage. With the
/// row lock only the first gets there — the rest wait on the row, and
/// read its fact once it commits. Without it all sixteen read "no fact"
/// and all sixteen stage (measured: the mutant records 16).
#[tokio::test(flavor = "multi_thread")]
async fn sixteen_concurrent_backfills_record_one_birth() {
    use boss_classes::port::Backfill;
    use boss_core::primitives::ClassRef;
    use std::sync::Arc;

    let live = TestDb::new().await;
    sqlx::query(
        "INSERT INTO classes (subject_kind, code, display_name, member_attribute, sort_order) \
         VALUES ('employee', 'pre-door-race', 'Race', 'department', 70)",
    )
    .execute(&live.pool)
    .await
    .unwrap();
    // One connection per racer plus the holder, so no racer queues for
    // a connection instead of for a lock.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(20)
        .connect(&live.url())
        .await
        .unwrap();
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE event_outbox IN EXCLUSIVE MODE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let repo = Arc::new(PgClasses::new(pool.clone()));
    let key = ClassRef::new("employee", "pre-door-race");
    let racers: Vec<_> = (0..16)
        .map(|_| {
            let (repo, key) = (repo.clone(), key.clone());
            tokio::spawn(async move { repo.backfill_declared(&key, &stamp()).await })
        })
        .collect();
    // Every racer reaches its wait (the row, or the stage) well inside
    // this; a late one only makes the mutant's count smaller than 16,
    // never the correct adapter's larger than 1.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    holder.rollback().await.unwrap();
    let mut recorded = 0;
    for r in racers {
        match r.await.unwrap().unwrap() {
            Backfill::Recorded(_) => recorded += 1,
            Backfill::AlreadyDeclared => {}
            Backfill::NotFound => panic!("the row exists"),
            Backfill::RecordedWithEdit { .. } => panic!("no birth was named"),
            Backfill::Observed(_) => panic!("no birth was observed"),
        }
    }
    let staged: i64 = sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = $1")
        .bind(CLASS_DECLARED)
        .fetch_one(&live.pool)
        .await
        .unwrap();
    assert_eq!((recorded, staged), (1, 1));
}

/// Backlog 3c6d0186 (closure): a FRESH database — migrations only —
/// plus the replayed log reproduces a declared Class, an edited seeded
/// Class, and a retired one (the operator retired nine employee Classes
/// live on 2026-09-28; the replay must bring the retirements back). A
/// second rebuild writes nothing; a row changed outside the doors is
/// drift, named and rolled back rather than overwritten.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_database_and_the_replayed_log_reproduce_every_class_change() {
    use boss_classes::rebuild::rebuild_classes;
    use boss_core::primitives::ClassRef;

    let live = TestDb::new().await;
    let repo = PgClasses::new(live.pool.clone());
    // A tenant's declare, then an edit and a retirement of it.
    repo.batch_upsert(&[class_row("employee", "dept-ops")], &stamp())
        .await
        .unwrap();
    let mut ops = class_row("employee", "dept-ops");
    ops.display_name = "Operations".into();
    ops.metadata = serde_json::json!({"department": "ops"});
    assert!(repo.update(&ops, &stamp()).await.unwrap());
    let dept_ops = ClassRef::new("employee", "dept-ops");
    assert!(repo.retire(&dept_ops, &stamp()).await.unwrap());
    // A migration-seeded role, edited; another, retired.
    let ceo = ClassRef::new("employee", "ceo");
    let mut edited = repo.get(&ceo).await.unwrap().expect("seeded");
    edited.display_name = "Chief Executive".into();
    edited.sort_order = 1;
    assert!(repo.update(&edited, &stamp()).await.unwrap());
    let recruiter = ClassRef::new("employee", "recruiter");
    assert!(repo.retire(&recruiter, &stamp()).await.unwrap());

    let fresh = TestDb::new().await;
    import_log(&live.pool, &fresh.pool).await;
    let rebuilt = PgClasses::new(fresh.pool.clone());
    assert!(
        rebuilt.get(&dept_ops).await.unwrap().is_none(),
        "not seeded"
    );

    let report = rebuild_classes(&fresh.pool).await.expect("rebuild");
    // Five door facts, and the births of the two seeded roles, which the
    // doors recorded before their own fact (backlog 6c2aa86c): each is
    // the seeded row, so it replays as current onto the migration's.
    assert_eq!(report.events_processed, 7, "{report:?}");
    assert_eq!(report.classes_written, 3, "{report:?}");
    for key in [&dept_ops, &ceo, &recruiter] {
        let want = repo.get(key).await.unwrap().expect("live row");
        assert_eq!(rebuilt.get(key).await.unwrap(), Some(want), "{key:?}");
    }
    assert!(
        rebuilt
            .get(&recruiter)
            .await
            .unwrap()
            .unwrap()
            .retired_at
            .is_some(),
        "the retirement replays"
    );

    // Idempotence, on the rebuilt table AND on the live one: every
    // change went through a door, so both are already at the log's head.
    let again = rebuild_classes(&fresh.pool).await.expect("re-run");
    assert_eq!(
        (again.classes_written, again.classes_already_current),
        (0, 3)
    );
    import_log(&live.pool, &live.pool).await;
    let in_place = rebuild_classes(&live.pool).await.expect("in place");
    assert_eq!(in_place.classes_written, 0, "{in_place:?}");

    // A change the log never heard of: drift, named, and nothing
    // written. `dept-ops` sorts before `recruiter`, so the rebuild has
    // already re-declared it when it meets the drift — and must roll
    // that back too.
    sqlx::query("DELETE FROM classes WHERE code = 'dept-ops'")
        .execute(&fresh.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE classes SET retired_at = retired_at - interval '1 day' WHERE code = 'recruiter'",
    )
    .execute(&fresh.pool)
    .await
    .unwrap();
    let err = rebuild_classes(&fresh.pool).await.unwrap_err();
    assert!(err.contains("drift") && err.contains("recruiter"), "{err}");
    assert!(
        rebuilt.get(&dept_ops).await.unwrap().is_none(),
        "the failed rebuild rolled back the row it had re-declared"
    );
}

/// `boss-rebuild-all` says every step holds its projection's
/// `pg_advisory_xact_lock` under `lock_key(<step>)`; `classes` took none
/// (backlog 8d5ac7c5). Holding `lock_key("classes")` on another session
/// must hold the rebuild back until it is released.
#[tokio::test(flavor = "multi_thread")]
async fn the_rebuild_waits_on_the_classes_rebuild_lock() {
    use boss_classes::rebuild::rebuild_classes;
    let db = TestDb::new().await;
    let mut holder = db.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(boss_core::rebuild::lock_key("classes"))
        .execute(&mut *holder)
        .await
        .unwrap();

    let pool = db.pool.clone();
    let rebuild = tokio::spawn(async move { rebuild_classes(&pool).await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        !rebuild.is_finished(),
        "the rebuild ran while another session held the classes rebuild lock"
    );

    holder.rollback().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), rebuild)
        .await
        .expect("the rebuild finishes once the lock is released")
        .unwrap()
        .expect("rebuild");
}
