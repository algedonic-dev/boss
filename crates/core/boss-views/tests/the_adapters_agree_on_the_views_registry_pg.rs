//! The View registry answers the same on both `ViewsRepo` adapters —
//! reliability mechanism C of design 3036296f, "adapters agree"
//! (`boss_testing::adapters_agree!`), on the next core port in the
//! census (backlog be459ab9): `ViewsRepo`, which `InMemoryViewsRepo`
//! and `PgViewsRepo` implement.
//!
//! WHY THIS PORT. It holds every saved View and enforces who may read,
//! edit and delete one — ownership lives in the port, not the door, so
//! a second caller cannot forget it. The HTTP handlers' tests and the
//! resolver's tests ask `InMemoryViewsRepo`; production is answered by
//! `PgViewsRepo`. Until this suite each adapter carried its own tests,
//! which held each to itself and never to the other.
//!
//! The first run found four disagreements, fixed in this car in
//! production code:
//!
//! - THE LIST'S ORDER. Postgres answered most recently updated first
//!   and stopped there, so two Views stamped in one microsecond came
//!   back in plan order; the double sorted by id, so its order had
//!   nothing to do with Postgres's at all. Both now answer newest
//!   `updated_at` first, then id byte-wise (`COLLATE "C"`). Case
//!   `the_list_is_most_recently_updated_first`.
//! - A REPLACE'S `updated_at`. Postgres moved it to the moment of the
//!   edit; the double stamped every write with the one instant it was
//!   built with, so an edit never moved it and an edited View never
//!   rose to the head of the list. The double's clock now ticks one
//!   microsecond per write from that instant — still deterministic.
//!   Case `a_replace_keeps_id_owner_and_created_at_and_moves_updated_at`.
//! - AN INSTANT'S PRECISION. Postgres keeps microseconds; the double
//!   stamped whatever instant it was handed, nanoseconds and all, so
//!   the same write read back unequal across adapters. The double now
//!   truncates to microseconds, as the column does. Case
//!   `an_instant_reads_back_at_microsecond_precision`.
//! - A NUL BYTE. Postgres cannot store one in TEXT and refused the
//!   write with its encoding error as `Storage` text (a 500) — and a
//!   READ keyed by one the same way; the double stored it and answered.
//!   Both now refuse a write carrying one with `Invalid` naming the
//!   field and write nothing, and answer a read keyed by one as the
//!   miss it is (no stored id or owner can hold one). Case
//!   `a_nul_byte_is_refused_on_write_and_misses_on_read`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Ids are the adapter's own (`view-N` against a uuid) and instants its
//! own clock, so both are judged by what they must satisfy — equality
//! on read-back, order, precision — never by value. The migrations seed
//! no `views` row, and both adapters start empty.

use std::time::Duration;

use boss_views::{
    InMemoryViewsRepo, PgViewsRepo, View, ViewInput, ViewLayout, ViewSource, ViewsError, ViewsRepo,
    Visibility,
};
use chrono::{Timelike, Utc};

boss_testing::adapters_agree! {
    adapters {
        // Built on the wall clock, nanoseconds and all, so the
        // precision case has a sub-microsecond instant to catch.
        in_memory => (InMemoryViewsRepo::new(Utc::now()), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgViewsRepo::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_store_answers_nothing,
        a_view_reads_back_as_written,
        a_private_view_is_its_owners_and_a_shared_one_everyones,
        the_list_is_most_recently_updated_first,
        a_replace_keeps_id_owner_and_created_at_and_moves_updated_at,
        a_stranger_can_neither_replace_nor_delete_and_changes_nothing,
        a_bad_filter_is_refused_and_writes_nothing,
        a_deleted_view_is_gone_for_everyone,
        an_instant_reads_back_at_microsecond_precision,
        a_nul_byte_is_refused_on_write_and_misses_on_read,
    }
}

// ----- fixtures ------------------------------------------------------------

fn input(title: &str, visibility: Visibility) -> ViewInput {
    ViewInput {
        title: title.into(),
        source: ViewSource::Jobs,
        filter: String::new(),
        columns: vec![],
        layout: ViewLayout::Table,
        visibility,
    }
}

/// A create, then a pause long enough that the next write's instant is
/// a later microsecond on the Postgres clock (the double's ticks).
async fn make<R: ViewsRepo>(r: &R, owner: &str, i: &ViewInput) -> View {
    let v = r.create(owner, i).await.expect("create answers");
    tokio::time::sleep(Duration::from_millis(5)).await;
    v
}

fn ids(rows: &[View]) -> Vec<String> {
    rows.iter().map(|v| v.id.clone()).collect()
}

fn is_not_found(e: &ViewsError) -> bool {
    matches!(e, ViewsError::NotFound(_))
}

fn micro_precise(t: chrono::DateTime<Utc>) -> bool {
    t.nanosecond().is_multiple_of(1_000)
}

// ----- cases ---------------------------------------------------------------

/// Nothing written: the list is empty and every id-keyed call misses.
async fn an_empty_store_answers_nothing<R: ViewsRepo>(r: &R, adapter: &str) {
    assert!(
        r.list_for_viewer("alice").await.expect("list").is_empty(),
        "{adapter}"
    );
    let e = r.get_for_viewer("view-1", "alice").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r
        .replace("view-1", "alice", &input("T", Visibility::Private))
        .await
        .unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r.delete("view-1", "alice").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
}

/// A create answers every field it was given — the owner from the
/// caller, not the body — with `created_at == updated_at`, and the
/// get and the list answer exactly that row.
async fn a_view_reads_back_as_written<R: ViewsRepo>(r: &R, adapter: &str) {
    let i = ViewInput {
        title: "Open kegs".into(),
        source: ViewSource::Steps,
        filter: "status = \"open\" AND priority = \"high\"".into(),
        columns: vec!["title".into(), "status".into(), "Priority".into()],
        layout: ViewLayout::List,
        visibility: Visibility::Shared,
    };
    let made = r.create("alice", &i).await.expect("create");
    assert_eq!(made.owner_id, "alice", "{adapter}");
    assert_eq!(made.title, i.title, "{adapter}");
    assert_eq!(made.source, i.source, "{adapter}");
    assert_eq!(made.filter, i.filter, "{adapter}");
    assert_eq!(made.columns, i.columns, "{adapter}");
    assert_eq!(made.layout, i.layout, "{adapter}");
    assert_eq!(made.visibility, i.visibility, "{adapter}");
    assert_eq!(made.created_at, made.updated_at, "{adapter}");
    assert!(!made.id.is_empty(), "{adapter}");

    assert_eq!(
        r.get_for_viewer(&made.id, "alice").await.expect("get"),
        made,
        "{adapter}"
    );
    assert_eq!(
        r.list_for_viewer("alice").await.expect("list"),
        vec![made],
        "{adapter}"
    );
}

/// Private is the owner's alone — absent from a stranger's list, and
/// NotFound (never Forbidden) at its id. Shared is everyone's to read.
async fn a_private_view_is_its_owners_and_a_shared_one_everyones<R: ViewsRepo>(
    r: &R,
    adapter: &str,
) {
    let mine = make(r, "alice", &input("Mine", Visibility::Private)).await;
    let ours = make(r, "alice", &input("Ours", Visibility::Shared)).await;

    assert_eq!(
        ids(&r.list_for_viewer("bob").await.expect("bob's list")),
        vec![ours.id.clone()],
        "{adapter}"
    );
    let e = r.get_for_viewer(&mine.id, "bob").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    assert_eq!(
        r.get_for_viewer(&ours.id, "bob").await.expect("shared get"),
        ours,
        "{adapter}"
    );
    assert_eq!(
        r.get_for_viewer(&mine.id, "alice").await.expect("own get"),
        mine,
        "{adapter}"
    );
    assert_eq!(
        r.list_for_viewer("alice")
            .await
            .expect("alice's list")
            .len(),
        2,
        "{adapter}"
    );
}

/// The list answers the most recently updated View first, so an edit
/// brings a View to the head. Two Views stamped in one microsecond
/// break the tie on id byte-wise (`COLLATE "C"` in Postgres, `String`
/// order in the double); the port stamps its own instants, so no case
/// can force that tie through it, and the tie-break is read in both
/// adapters' source.
async fn the_list_is_most_recently_updated_first<R: ViewsRepo>(r: &R, adapter: &str) {
    let a = make(r, "alice", &input("A", Visibility::Private)).await;
    let b = make(r, "alice", &input("B", Visibility::Private)).await;
    let c = make(r, "alice", &input("C", Visibility::Shared)).await;
    assert_eq!(
        ids(&r.list_for_viewer("alice").await.expect("list")),
        vec![c.id.clone(), b.id.clone(), a.id.clone()],
        "{adapter}: newest first"
    );

    r.replace(&a.id, "alice", &input("A2", Visibility::Private))
        .await
        .expect("replace");
    assert_eq!(
        ids(&r.list_for_viewer("alice").await.expect("list")),
        vec![a.id, c.id, b.id],
        "{adapter}: an edit moves a View to the head"
    );
}

/// A replace rewrites every input field and moves `updated_at` later;
/// the id, the owner and `created_at` — the View's birth — survive it,
/// and the stored row reads back as the replace answered.
async fn a_replace_keeps_id_owner_and_created_at_and_moves_updated_at<R: ViewsRepo>(
    r: &R,
    adapter: &str,
) {
    let made = make(r, "alice", &input("First", Visibility::Private)).await;
    let next = ViewInput {
        title: "Second".into(),
        source: ViewSource::Events,
        filter: "kind = \"x\"".into(),
        columns: vec!["kind".into()],
        layout: ViewLayout::Count,
        visibility: Visibility::Shared,
    };
    let updated = r.replace(&made.id, "alice", &next).await.expect("replace");

    assert_eq!(updated.id, made.id, "{adapter}");
    assert_eq!(updated.owner_id, "alice", "{adapter}");
    assert_eq!(updated.created_at, made.created_at, "{adapter}");
    assert!(
        updated.updated_at > made.updated_at,
        "{adapter}: {} not after {}",
        updated.updated_at,
        made.updated_at
    );
    assert_eq!(updated.title, next.title, "{adapter}");
    assert_eq!(updated.source, next.source, "{adapter}");
    assert_eq!(updated.filter, next.filter, "{adapter}");
    assert_eq!(updated.columns, next.columns, "{adapter}");
    assert_eq!(updated.layout, next.layout, "{adapter}");
    assert_eq!(updated.visibility, next.visibility, "{adapter}");
    assert_eq!(
        r.get_for_viewer(&made.id, "bob").await.expect("get"),
        updated,
        "{adapter}"
    );
}

/// Shared means readable, not writable: a stranger's replace and delete
/// of a shared View are NotFound, and the View is untouched.
async fn a_stranger_can_neither_replace_nor_delete_and_changes_nothing<R: ViewsRepo>(
    r: &R,
    adapter: &str,
) {
    let ours = make(r, "alice", &input("Ours", Visibility::Shared)).await;

    let e = r
        .replace(&ours.id, "bob", &input("Hijacked", Visibility::Private))
        .await
        .unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r.delete(&ours.id, "bob").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");

    assert_eq!(
        r.get_for_viewer(&ours.id, "alice").await.expect("get"),
        ours,
        "{adapter}"
    );
}

/// A filter that does not parse is refused at save, on create and on
/// replace, and writes nothing — the create stores no row, the replace
/// leaves the View as it was.
async fn a_bad_filter_is_refused_and_writes_nothing<R: ViewsRepo>(r: &R, adapter: &str) {
    let mut bad = input("Broken", Visibility::Private);
    bad.filter = "status =".into();

    let e = r.create("alice", &bad).await.unwrap_err();
    assert!(
        matches!(e, ViewsError::InvalidFilter(_)),
        "{adapter}: {e:?}"
    );
    assert!(
        r.list_for_viewer("alice").await.expect("list").is_empty(),
        "{adapter}"
    );

    let made = make(r, "alice", &input("Good", Visibility::Private)).await;
    let e = r.replace(&made.id, "alice", &bad).await.unwrap_err();
    assert!(
        matches!(e, ViewsError::InvalidFilter(_)),
        "{adapter}: {e:?}"
    );
    assert_eq!(
        r.get_for_viewer(&made.id, "alice").await.expect("get"),
        made,
        "{adapter}"
    );
}

/// The owner's delete removes the View from every reader, and a second
/// delete is NotFound.
async fn a_deleted_view_is_gone_for_everyone<R: ViewsRepo>(r: &R, adapter: &str) {
    let ours = make(r, "alice", &input("Ours", Visibility::Shared)).await;
    let kept = make(r, "alice", &input("Kept", Visibility::Shared)).await;

    r.delete(&ours.id, "alice").await.expect("delete");
    for viewer in ["alice", "bob"] {
        let e = r.get_for_viewer(&ours.id, viewer).await.unwrap_err();
        assert!(is_not_found(&e), "{adapter} {viewer}: {e:?}");
        assert_eq!(
            ids(&r.list_for_viewer(viewer).await.expect("list")),
            vec![kept.id.clone()],
            "{adapter} {viewer}"
        );
    }
    let e = r.delete(&ours.id, "alice").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
}

/// Every instant a View carries is whole microseconds — what the
/// column keeps — on the create, the replace and every read of them.
async fn an_instant_reads_back_at_microsecond_precision<R: ViewsRepo>(r: &R, adapter: &str) {
    let made = make(r, "alice", &input("T", Visibility::Private)).await;
    assert!(
        micro_precise(made.created_at),
        "{adapter}: {}",
        made.created_at
    );
    assert!(
        micro_precise(made.updated_at),
        "{adapter}: {}",
        made.updated_at
    );
    let updated = r
        .replace(&made.id, "alice", &input("T2", Visibility::Private))
        .await
        .expect("replace");
    assert!(
        micro_precise(updated.updated_at),
        "{adapter}: {}",
        updated.updated_at
    );
    assert_eq!(
        r.get_for_viewer(&made.id, "alice").await.expect("get"),
        updated,
        "{adapter}"
    );
}

/// A NUL byte cannot be stored in TEXT. A write carrying one — in any
/// input field or the owner — is refused `Invalid` naming the field,
/// never a storage error's text, and writes nothing. A read keyed by
/// one is the miss it is: no stored id or owner can hold one.
async fn a_nul_byte_is_refused_on_write_and_misses_on_read<R: ViewsRepo>(r: &R, adapter: &str) {
    let shared = make(r, "alice", &input("Ours", Visibility::Shared)).await;

    let with_title = input("a\0b", Visibility::Private);
    let mut with_filter = input("F", Visibility::Private);
    with_filter.filter = "status = \"a\0b\"".into();
    let mut with_column = input("C", Visibility::Private);
    with_column.columns = vec!["title".into(), "st\0atus".into()];
    for (field, bad) in [
        ("title", &with_title),
        ("filter", &with_filter),
        ("columns", &with_column),
    ] {
        match r.create("alice", bad).await.unwrap_err() {
            ViewsError::Invalid(m) => assert!(m.contains(field), "{adapter}: {m}"),
            e => panic!("{adapter}: create with a NUL {field}: {e:?}"),
        }
        match r.replace(&shared.id, "alice", bad).await.unwrap_err() {
            ViewsError::Invalid(m) => assert!(m.contains(field), "{adapter}: {m}"),
            e => panic!("{adapter}: replace with a NUL {field}: {e:?}"),
        }
    }
    match r
        .create("al\0ice", &input("O", Visibility::Private))
        .await
        .unwrap_err()
    {
        ViewsError::Invalid(m) => assert!(m.contains("owner"), "{adapter}: {m}"),
        e => panic!("{adapter}: create with a NUL owner: {e:?}"),
    }
    assert_eq!(
        r.list_for_viewer("alice").await.expect("list"),
        vec![shared.clone()],
        "{adapter}: nothing written"
    );

    // Reads and owner-scoped writes keyed by a NUL miss.
    let e = r.get_for_viewer("view\0-1", "alice").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r
        .get_for_viewer(&shared.id, "bo\0b")
        .await
        .expect("a shared View is anyone's");
    assert_eq!(e, shared, "{adapter}");
    let e = r
        .replace(&shared.id, "ali\0ce", &input("X", Visibility::Private))
        .await
        .unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r.delete("view\0-1", "alice").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    let e = r.delete(&shared.id, "ali\0ce").await.unwrap_err();
    assert!(is_not_found(&e), "{adapter}: {e:?}");
    assert_eq!(
        r.list_for_viewer("b\0ob").await.expect("list"),
        vec![shared],
        "{adapter}: a NUL viewer owns nothing and sees the shared"
    );
}
