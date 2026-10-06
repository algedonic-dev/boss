//! The file-references store answers the same on both `FileRepository`
//! adapters — reliability mechanism C of design 3036296f, "adapters
//! agree" (`boss_testing::adapters_agree!`), on the next core port in
//! the census (backlog be459ab9): `FileRepository`, which
//! `InMemoryFileRepository` and `PgFileRepository` implement.
//!
//! WHY THIS PORT. It holds every attachment on a Subject, Job, Step or
//! Event — the proof of work a step carries. The files door's own unit
//! tests (`boss-content/src/files/http.rs`) and `boss attach`'s ask
//! `InMemoryFileRepository`; production is answered by
//! `PgFileRepository`. `files_e2e.rs` already ran six hand-wired bodies
//! against both, but only by `matches!` on a variant, so it held the
//! two to agree in kind and never in answer; those six move here.
//!
//! The first run found five disagreements, fixed in this car in
//! production code:
//!
//! - LIST ORDER. `list_for` and `list_for_sha256` promise newest first
//!   by `uploaded_at` and stopped there on both, so two files uploaded
//!   in the same instant answered in HashMap order in the double and
//!   plan order in Postgres. Both now break the tie on id, ascending.
//!   Cases `the_targets_list_is_its_live_rows_newest_first_then_id` and
//!   `the_sha256_list_is_every_row_live_or_detached_newest_first_then_id`.
//! - A DETACHED ROW STILL HOLDS ITS OBJECT KEY. The table's
//!   `UNIQUE (bucket, object_key)` covers every row, detached ones too,
//!   so Postgres refuses a second row on a detached row's key; the
//!   double exempted detached rows and wrote it. The double now refuses
//!   as production does. Case
//!   `a_taken_object_key_is_refused_naming_the_sha256_even_after_a_detach`.
//! - THE REFUSAL'S WORDS. That refusal is `DuplicateObject(sha256)` —
//!   its Display reads "duplicate object_key for sha256 {0}" — and the
//!   double carried the sha256 while Postgres carried the raw
//!   constraint message. Postgres now names the sha256. Same case.
//! - A TAKEN ID. The double REPLACED the row when a draft reused an id
//!   (its dedup skipped the row with the draft's own id), so a retried
//!   insert on a new key silently rewrote the first row and revived it
//!   if detached; Postgres refused on the primary key as a
//!   `Repository` error carrying the constraint text. Both now refuse
//!   with `Validation("file <id> already exists")` and write nothing.
//!   Case `a_taken_id_is_refused_and_changes_nothing`.
//! - A NEGATIVE SIZE. The schema CHECKs `size_bytes >= 0`; Postgres
//!   answered a negative size with that CHECK's raw text as a
//!   `Repository` error and the double stored it. Both now refuse with
//!   `Validation` before writing. Case
//!   `a_negative_size_is_refused_and_writes_nothing`.
//!
//! THE SHAPE, the other suites': each case states its answer and each
//! adapter is held to that stated answer, not merely to the other one.
//! Instants are whole seconds, so Postgres's microseconds and the
//! double's nanoseconds compare equal. The migrations seed no file_refs
//! row, and both adapters start empty. The outbox events
//! `PgFileRepository` records are not the port's to answer — the double
//! records none — and are not judged here.

use boss_content::files::{
    FileError, FileRef, FileRefDraft, FileRepository, InMemoryFileRepository, PgFileRepository,
    ResourceKind, ResourceRef,
};
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

boss_testing::adapters_agree! {
    adapters {
        in_memory => (InMemoryFileRepository::new(), ()),
        postgres => {
            let db = boss_testing::TestDb::new().await;
            (PgFileRepository::new(db.pool.clone()), db)
        },
    }
    cases {
        an_empty_store_answers_nothing,
        a_file_reads_back_as_written_on_every_target_kind,
        the_targets_list_is_its_live_rows_newest_first_then_id,
        the_sha256_list_is_every_row_live_or_detached_newest_first_then_id,
        a_detach_hides_the_row_from_its_target_and_keeps_the_first_instant,
        a_taken_object_key_is_refused_naming_the_sha256_even_after_a_detach,
        a_taken_id_is_refused_and_changes_nothing,
        a_negative_size_is_refused_and_writes_nothing,
    }
}

// ----- fixtures ------------------------------------------------------------

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 30, h, m, 0).unwrap()
}

fn stamp() -> EventStamp {
    EventStamp::new("content", ActorId::Human("emp-suite".into())).with_timestamp(at(0, 0))
}

fn target(kind: ResourceKind, id: &str) -> ResourceRef {
    ResourceRef {
        kind,
        id: id.into(),
    }
}

fn job(id: &str) -> ResourceRef {
    target(ResourceKind::Job, id)
}

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A draft on its own object key (`sha256/<sha>` in bucket `bk`).
fn draft(n: u128, on: ResourceRef, sha: &str, uploaded_at: DateTime<Utc>) -> FileRefDraft {
    FileRefDraft {
        id: id(n),
        target: on,
        bucket: "bk".into(),
        object_key: format!("sha256/{sha}"),
        sha256: sha.into(),
        size_bytes: 4096,
        mime: "image/png".into(),
        filename: format!("{sha}.png"),
        uploaded_by: "emp-uploader".into(),
        uploaded_at,
    }
}

async fn put<R: FileRepository>(r: &R, d: FileRefDraft) -> FileRef {
    r.insert(d, &stamp()).await.expect("insert answers")
}

async fn detach<R: FileRepository>(r: &R, n: u128, when: DateTime<Utc>) {
    r.soft_delete(id(n), when, "emp-detacher", &stamp())
        .await
        .expect("soft_delete answers");
}

fn ids(rows: &[FileRef]) -> Vec<Uuid> {
    rows.iter().map(|r| r.id).collect()
}

// ----- cases ---------------------------------------------------------------

/// Nothing written: every read answers empty or absent, and a detach
/// names the id it could not find.
async fn an_empty_store_answers_nothing<R: FileRepository>(r: &R, adapter: &str) {
    assert_eq!(r.get(id(1)).await.expect("get"), None, "{adapter}");
    let listed = r.list_for(&job("job-1")).await.expect("list_for");
    assert!(listed.is_empty(), "{adapter}: {listed:?}");
    let by_sha = r.list_for_sha256("aaa").await.expect("list_for_sha256");
    assert!(by_sha.is_empty(), "{adapter}: {by_sha:?}");
    match r.soft_delete(id(1), at(1, 0), "emp-x", &stamp()).await {
        Err(FileError::NotFound(msg)) => assert_eq!(msg, id(1).to_string(), "{adapter}"),
        other => panic!("{adapter}: detach of a missing id answered {other:?}"),
    }
}

/// An insert answers every field it was drafted with and no
/// `deleted_at`, on each of the four target kinds; `get`, the target's
/// list and the sha256's list answer that same row.
async fn a_file_reads_back_as_written_on_every_target_kind<R: FileRepository>(
    r: &R,
    adapter: &str,
) {
    let kinds = [
        ResourceKind::Subject,
        ResourceKind::Job,
        ResourceKind::Step,
        ResourceKind::Event,
    ];
    for (n, kind) in (1u128..).zip(kinds) {
        let sha = format!("sha-{}", kind.as_str());
        let d = FileRefDraft {
            size_bytes: 0,
            mime: "application/pdf".into(),
            filename: "Proof of Work.pdf".into(),
            ..draft(n, target(kind, "t-1"), &sha, at(9, n as u32))
        };
        let want = FileRef {
            id: id(n),
            target: target(kind, "t-1"),
            bucket: "bk".into(),
            object_key: format!("sha256/{sha}"),
            sha256: sha.clone(),
            size_bytes: 0,
            mime: "application/pdf".into(),
            filename: "Proof of Work.pdf".into(),
            uploaded_by: "emp-uploader".into(),
            uploaded_at: at(9, n as u32),
            deleted_at: None,
        };
        assert_eq!(put(r, d).await, want, "{adapter}: insert on {kind:?}");
        assert_eq!(
            r.get(id(n)).await.expect("get"),
            Some(want.clone()),
            "{adapter}: get on {kind:?}"
        );
        assert_eq!(
            r.list_for(&target(kind, "t-1")).await.expect("list_for"),
            vec![want.clone()],
            "{adapter}: list_for on {kind:?}"
        );
        assert_eq!(
            r.list_for_sha256(&sha).await.expect("list_for_sha256"),
            vec![want],
            "{adapter}: list_for_sha256 on {kind:?}"
        );
    }
}

/// `list_for` answers the live rows whose kind AND id both match —
/// never a detached row, never another kind's row under the same id —
/// newest first by `uploaded_at`, and a tie in id order. The ids are
/// planted out of insertion order so neither a map's order nor a
/// table's can pass for the tie-break.
async fn the_targets_list_is_its_live_rows_newest_first_then_id<R: FileRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, draft(30, job("job-1"), "c", at(10, 0))).await;
    put(r, draft(10, job("job-1"), "a", at(10, 0))).await;
    put(r, draft(20, job("job-1"), "b", at(10, 0))).await;
    put(r, draft(5, job("job-1"), "e", at(9, 0))).await;
    put(r, draft(40, job("job-1"), "d", at(11, 0))).await;
    // Same id, other kind; other id, same kind; a detached row.
    put(
        r,
        draft(50, target(ResourceKind::Step, "job-1"), "f", at(12, 0)),
    )
    .await;
    put(r, draft(60, job("job-2"), "g", at(12, 0))).await;
    put(r, draft(70, job("job-1"), "h", at(12, 0))).await;
    detach(r, 70, at(13, 0)).await;

    let listed = r.list_for(&job("job-1")).await.expect("list_for");
    assert_eq!(
        ids(&listed),
        vec![id(40), id(10), id(20), id(30), id(5)],
        "{adapter}"
    );
}

/// `list_for_sha256` answers every row carrying the sha256, detached
/// ones included (the GC sweep needs them), across buckets and
/// targets, newest first and a tie in id order.
async fn the_sha256_list_is_every_row_live_or_detached_newest_first_then_id<R: FileRepository>(
    r: &R,
    adapter: &str,
) {
    let in_bucket = |n: u128, bucket: &str, on: ResourceRef, when| FileRefDraft {
        bucket: bucket.into(),
        ..draft(n, on, "shared", when)
    };
    put(r, in_bucket(3, "bk-c", job("job-1"), at(10, 0))).await;
    put(r, in_bucket(1, "bk-a", job("job-2"), at(10, 0))).await;
    put(r, in_bucket(2, "bk-b", job("job-3"), at(11, 0))).await;
    put(r, in_bucket(4, "bk-d", job("job-4"), at(10, 0))).await;
    put(r, draft(9, job("job-1"), "other", at(12, 0))).await;
    detach(r, 4, at(12, 0)).await;

    let rows = r.list_for_sha256("shared").await.expect("list_for_sha256");
    assert_eq!(ids(&rows), vec![id(2), id(1), id(3), id(4)], "{adapter}");
    assert_eq!(rows[3].deleted_at, Some(at(12, 0)), "{adapter}");
}

/// A detach hides the row from its target's list and nothing else:
/// `get` and the sha256 list still answer it, now carrying the instant
/// it was detached at. A second detach is a no-op that keeps the FIRST
/// instant.
async fn a_detach_hides_the_row_from_its_target_and_keeps_the_first_instant<R: FileRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, draft(1, job("job-1"), "a", at(9, 0))).await;
    put(r, draft(2, job("job-1"), "b", at(9, 0))).await;
    detach(r, 1, at(10, 0)).await;
    detach(r, 1, at(11, 0)).await;

    assert_eq!(
        ids(&r.list_for(&job("job-1")).await.expect("list_for")),
        vec![id(2)],
        "{adapter}"
    );
    let got = r.get(id(1)).await.expect("get").expect("row still read");
    assert_eq!(got.deleted_at, Some(at(10, 0)), "{adapter}");
    assert_eq!(
        r.list_for_sha256("a").await.expect("list_for_sha256"),
        vec![got],
        "{adapter}"
    );
}

/// A second row on a (bucket, object_key) already held is refused as
/// `DuplicateObject` naming the draft's sha256 — whether the holder is
/// live or detached, because the table's UNIQUE covers every row — and
/// writes nothing. The same key in another bucket is a different
/// object and is written.
async fn a_taken_object_key_is_refused_naming_the_sha256_even_after_a_detach<R: FileRepository>(
    r: &R,
    adapter: &str,
) {
    put(r, draft(1, job("job-1"), "abc", at(9, 0))).await;
    for (n, what) in [(2u128, "on a live holder"), (3, "on a detached holder")] {
        if n == 3 {
            detach(r, 1, at(10, 0)).await;
        }
        match r
            .insert(draft(n, job("job-2"), "abc", at(11, 0)), &stamp())
            .await
        {
            Err(FileError::DuplicateObject(msg)) => {
                assert_eq!(msg, "abc", "{adapter}: {what}")
            }
            other => panic!("{adapter}: {what} expected DuplicateObject, got {other:?}"),
        }
        assert_eq!(r.get(id(n)).await.expect("get"), None, "{adapter}: {what}");
    }
    assert!(
        r.list_for(&job("job-2"))
            .await
            .expect("list_for")
            .is_empty(),
        "{adapter}"
    );
    assert_eq!(
        ids(&r.list_for_sha256("abc").await.expect("list_for_sha256")),
        vec![id(1)],
        "{adapter}"
    );

    let elsewhere = FileRefDraft {
        bucket: "bk-2".into(),
        ..draft(4, job("job-2"), "abc", at(12, 0))
    };
    put(r, elsewhere).await;
    assert_eq!(
        ids(&r.list_for(&job("job-2")).await.expect("list_for")),
        vec![id(4)],
        "{adapter}"
    );
}

/// A draft reusing an id already written — on a fresh key, so no other
/// refusal applies — is refused as `Validation` naming the id, and the
/// first row is left exactly as it was, detached instant and all.
async fn a_taken_id_is_refused_and_changes_nothing<R: FileRepository>(r: &R, adapter: &str) {
    let first = put(r, draft(1, job("job-1"), "a", at(9, 0))).await;
    detach(r, 1, at(10, 0)).await;
    let before = r.get(id(1)).await.expect("get").expect("row");
    assert_eq!(before.deleted_at, Some(at(10, 0)), "{adapter}");

    match r
        .insert(draft(1, job("job-2"), "b", at(11, 0)), &stamp())
        .await
    {
        Err(FileError::Validation(msg)) => {
            assert_eq!(msg, format!("file {} already exists", id(1)), "{adapter}")
        }
        other => panic!("{adapter}: a taken id expected Validation, got {other:?}"),
    }
    assert_eq!(r.get(id(1)).await.expect("get"), Some(before), "{adapter}");
    assert_eq!(first.target, job("job-1"), "{adapter}");
    assert!(
        r.list_for(&job("job-2"))
            .await
            .expect("list_for")
            .is_empty(),
        "{adapter}"
    );
    assert!(
        r.list_for_sha256("b")
            .await
            .expect("list_for_sha256")
            .is_empty(),
        "{adapter}"
    );
}

/// A draft with a negative size is refused as `Validation` naming the
/// size, and nothing is written.
async fn a_negative_size_is_refused_and_writes_nothing<R: FileRepository>(r: &R, adapter: &str) {
    let d = FileRefDraft {
        size_bytes: -1,
        ..draft(1, job("job-1"), "a", at(9, 0))
    };
    match r.insert(d, &stamp()).await {
        Err(FileError::Validation(msg)) => {
            assert_eq!(msg, "size_bytes must not be negative, got -1", "{adapter}")
        }
        other => panic!("{adapter}: a negative size expected Validation, got {other:?}"),
    }
    assert_eq!(r.get(id(1)).await.expect("get"), None, "{adapter}");
    assert!(
        r.list_for(&job("job-1"))
            .await
            .expect("list_for")
            .is_empty(),
        "{adapter}"
    );
}
