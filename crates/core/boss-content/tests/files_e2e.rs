//! File-references — port-conformance tests against both the
//! in-memory and Postgres adapters. The same tests run against both
//! to catch any divergence between the test-double and production
//! adapter (e.g. dedup constraint behavior, soft-delete idempotence).

#![cfg(feature = "postgres")]

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;

use std::sync::Arc;

use boss_content::files::{
    AuditMismatchKind, FileError, FileRef, FileRefDraft, FileRepository, FileStorage,
    InMemoryFileRepository, InMemoryFileStorage, PgFileRepository, ResourceKind, ResourceRef,
    audit_sample, gc_orphan_objects, rebuild_file_refs,
};
use boss_testing::TestDb;
use bytes::Bytes;
use sha2::{Digest, Sha256};

fn job_target(id: &str) -> ResourceRef {
    ResourceRef {
        kind: ResourceKind::Job,
        id: id.into(),
    }
}

fn at(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap() + chrono::Duration::seconds(secs)
}

fn draft(id: Uuid, target: ResourceRef, sha: &str, bucket: &str) -> FileRefDraft {
    FileRefDraft {
        id,
        target,
        bucket: bucket.into(),
        object_key: format!("sha256/{sha}"),
        sha256: sha.into(),
        size_bytes: 4096,
        mime: "image/png".into(),
        filename: format!("{sha}.png"),
        uploaded_by: "emp-001".into(),
        uploaded_at: at(0),
    }
}

// ---- Generic port-conformance suite ---------------------------------------
//
// `run_*` helpers take any `&dyn FileRepository` so the same body can
// drive in-memory + Pg. Each #[tokio::test] just constructs the adapter
// and calls into the suite.

async fn run_insert_get_round_trips(repo: &dyn FileRepository) {
    let id = Uuid::new_v4();
    let row = repo
        .insert(draft(id, job_target("job-001"), "abc", "bk"))
        .await
        .unwrap();
    assert_eq!(row.id, id);
    assert_eq!(row.target.id, "job-001");
    assert_eq!(row.sha256, "abc");
    assert_eq!(row.size_bytes, 4096);
    assert!(row.deleted_at.is_none());

    let fetched = repo.get(id).await.unwrap().expect("present");
    assert_eq!(fetched, row);
}

async fn run_list_for_filters_target_and_hides_soft_deleted(repo: &dyn FileRepository) {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let other = Uuid::new_v4();
    repo.insert(draft(a, job_target("job-001"), "aaa", "bk"))
        .await
        .unwrap();
    repo.insert(draft(b, job_target("job-001"), "bbb", "bk"))
        .await
        .unwrap();
    repo.insert(draft(other, job_target("job-002"), "ccc", "bk"))
        .await
        .unwrap();

    let target = job_target("job-001");
    let live = repo.list_for(&target).await.unwrap();
    assert_eq!(live.len(), 2);

    repo.soft_delete(a, at(60)).await.unwrap();
    let live2 = repo.list_for(&target).await.unwrap();
    assert_eq!(live2.len(), 1, "soft-deleted row hidden from list_for");
    assert_eq!(live2[0].id, b);

    let still = repo.get(a).await.unwrap().expect("present");
    assert_eq!(still.deleted_at, Some(at(60)));
}

async fn run_list_for_sha256_includes_soft_deleted(repo: &dyn FileRepository) {
    let live_id = Uuid::new_v4();
    let dead_id = Uuid::new_v4();
    repo.insert(draft(live_id, job_target("job-001"), "shr", "bk-a"))
        .await
        .unwrap();
    repo.insert(draft(dead_id, job_target("job-002"), "shr", "bk-b"))
        .await
        .unwrap();
    repo.soft_delete(dead_id, at(0)).await.unwrap();

    let all = repo.list_for_sha256("shr").await.unwrap();
    assert_eq!(all.len(), 2, "GC sweep needs to see soft-deleted refs");
    let live_only: Vec<_> = all.iter().filter(|r| r.deleted_at.is_none()).collect();
    assert_eq!(live_only.len(), 1);
}

async fn run_duplicate_object_key_in_same_bucket_is_rejected(repo: &dyn FileRepository) {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    repo.insert(draft(a, job_target("job-001"), "shared", "bk"))
        .await
        .unwrap();
    let err = repo
        .insert(draft(b, job_target("job-002"), "shared", "bk"))
        .await
        .unwrap_err();
    assert!(matches!(err, FileError::DuplicateObject(_)));
}

async fn run_soft_delete_is_idempotent_first_timestamp_wins(repo: &dyn FileRepository) {
    let id = Uuid::new_v4();
    repo.insert(draft(id, job_target("job-001"), "abc", "bk"))
        .await
        .unwrap();
    repo.soft_delete(id, at(0)).await.unwrap();
    repo.soft_delete(id, at(60)).await.unwrap();
    let row = repo.get(id).await.unwrap().expect("present");
    assert_eq!(
        row.deleted_at,
        Some(at(0)),
        "first delete timestamp preserved"
    );
}

async fn run_soft_delete_unknown_id_is_not_found(repo: &dyn FileRepository) {
    let err = repo.soft_delete(Uuid::new_v4(), at(0)).await.unwrap_err();
    assert!(matches!(err, FileError::NotFound(_)));
}

// ---- In-memory drivers ----------------------------------------------------

#[tokio::test]
async fn in_memory_insert_get_round_trips() {
    run_insert_get_round_trips(&InMemoryFileRepository::new()).await;
}

#[tokio::test]
async fn in_memory_list_for_filters_target_and_hides_soft_deleted() {
    run_list_for_filters_target_and_hides_soft_deleted(&InMemoryFileRepository::new()).await;
}

#[tokio::test]
async fn in_memory_list_for_sha256_includes_soft_deleted() {
    run_list_for_sha256_includes_soft_deleted(&InMemoryFileRepository::new()).await;
}

#[tokio::test]
async fn in_memory_duplicate_object_key_is_rejected() {
    run_duplicate_object_key_in_same_bucket_is_rejected(&InMemoryFileRepository::new()).await;
}

#[tokio::test]
async fn in_memory_soft_delete_is_idempotent() {
    run_soft_delete_is_idempotent_first_timestamp_wins(&InMemoryFileRepository::new()).await;
}

#[tokio::test]
async fn in_memory_soft_delete_unknown_id_is_not_found() {
    run_soft_delete_unknown_id_is_not_found(&InMemoryFileRepository::new()).await;
}

// ---- Pg drivers -----------------------------------------------------------

#[tokio::test]
async fn pg_insert_get_round_trips() {
    let db = TestDb::new().await;
    run_insert_get_round_trips(&PgFileRepository::new(db.pool.clone())).await;
}

#[tokio::test]
async fn pg_list_for_filters_target_and_hides_soft_deleted() {
    let db = TestDb::new().await;
    run_list_for_filters_target_and_hides_soft_deleted(&PgFileRepository::new(db.pool.clone()))
        .await;
}

#[tokio::test]
async fn pg_list_for_sha256_includes_soft_deleted() {
    let db = TestDb::new().await;
    run_list_for_sha256_includes_soft_deleted(&PgFileRepository::new(db.pool.clone())).await;
}

#[tokio::test]
async fn pg_duplicate_object_key_is_rejected() {
    let db = TestDb::new().await;
    run_duplicate_object_key_in_same_bucket_is_rejected(&PgFileRepository::new(db.pool.clone()))
        .await;
}

#[tokio::test]
async fn pg_soft_delete_is_idempotent() {
    let db = TestDb::new().await;
    run_soft_delete_is_idempotent_first_timestamp_wins(&PgFileRepository::new(db.pool.clone()))
        .await;
}

#[tokio::test]
async fn pg_soft_delete_unknown_id_is_not_found() {
    let db = TestDb::new().await;
    run_soft_delete_unknown_id_is_not_found(&PgFileRepository::new(db.pool.clone())).await;
}

// ---- Rebuilder ------------------------------------------------------------

/// Hand-write the audit_log rows the upload+detach flow would emit,
/// then verify rebuild_file_refs reconstructs the table from them.
/// Tests the contract documented at design line 96 (id ≡ event id).
#[tokio::test]
async fn pg_rebuild_replays_attached_then_detached_events() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();

    let id_a = uuid::Uuid::new_v4();
    let id_b = uuid::Uuid::new_v4();

    let row_a = FileRef {
        id: id_a,
        target: job_target("job-001"),
        bucket: "test".into(),
        object_key: format!("sha256/{}", "aaa"),
        sha256: "aaa".into(),
        size_bytes: 1024,
        mime: "image/png".into(),
        filename: "a.png".into(),
        uploaded_by: "emp-001".into(),
        uploaded_at: at(0),
        deleted_at: None,
    };
    let row_b = FileRef {
        id: id_b,
        target: job_target("job-002"),
        bucket: "test".into(),
        object_key: format!("sha256/{}", "bbb"),
        sha256: "bbb".into(),
        size_bytes: 2048,
        mime: "application/pdf".into(),
        filename: "b.pdf".into(),
        uploaded_by: "emp-002".into(),
        uploaded_at: at(60),
        deleted_at: None,
    };
    let detach_at = at(3600);

    insert_audit_event(
        &pool,
        "content.file.attached",
        at(0),
        serde_json::to_value(&row_a).unwrap(),
    )
    .await;
    insert_audit_event(
        &pool,
        "content.file.attached",
        at(60),
        serde_json::to_value(&row_b).unwrap(),
    )
    .await;
    insert_audit_event(
        &pool,
        "content.file.detached",
        detach_at,
        serde_json::json!({
            "file_id": id_a,
            "target_kind": "job",
            "target_id": "job-001",
            "deleted_by": "emp-001",
            "deleted_at": detach_at,
        }),
    )
    .await;

    let report = rebuild_file_refs(&pool).await.expect("rebuild");
    assert_eq!(report.events_processed, 3);
    assert_eq!(report.events_skipped, 0);
    assert_eq!(report.refs_inserted, 2);
    assert_eq!(report.refs_soft_deleted, 1);

    let repo = PgFileRepository::new(pool.clone());
    let a = repo.get(id_a).await.unwrap().expect("present");
    assert_eq!(a.deleted_at, Some(detach_at));
    let b = repo.get(id_b).await.unwrap().expect("present");
    assert!(b.deleted_at.is_none());
}

#[tokio::test]
async fn pg_rebuild_is_idempotent_under_re_application() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    let id = uuid::Uuid::new_v4();
    let row = FileRef {
        id,
        target: job_target("job-001"),
        bucket: "test".into(),
        object_key: format!("sha256/{}", "abc"),
        sha256: "abc".into(),
        size_bytes: 1,
        mime: "text/plain".into(),
        filename: "x.txt".into(),
        uploaded_by: "emp-001".into(),
        uploaded_at: at(0),
        deleted_at: None,
    };
    insert_audit_event(
        &pool,
        "content.file.attached",
        at(0),
        serde_json::to_value(&row).unwrap(),
    )
    .await;

    rebuild_file_refs(&pool).await.unwrap();
    let r1 = rebuild_file_refs(&pool).await.unwrap();
    // Second run still processes the same events but ends with the
    // identical projection state — no duplicate rows because the table
    // is TRUNCATEd at the top of each rebuild.
    assert_eq!(r1.refs_inserted, 1);
}

#[tokio::test]
async fn pg_rebuild_skips_detach_for_unknown_attach() {
    let db = TestDb::new().await;
    let pool = db.pool.clone();
    let id = uuid::Uuid::new_v4();
    insert_audit_event(
        &pool,
        "content.file.detached",
        at(0),
        serde_json::json!({
            "file_id": id,
            "target_kind": "job",
            "target_id": "job-x",
            "deleted_by": "emp-001",
            "deleted_at": at(0),
        }),
    )
    .await;
    let report = rebuild_file_refs(&pool).await.unwrap();
    assert_eq!(report.events_processed, 1);
    assert_eq!(report.events_skipped, 1);
    assert_eq!(report.refs_soft_deleted, 0);
}

async fn insert_audit_event(
    pool: &sqlx::PgPool,
    kind: &str,
    ts: chrono::DateTime<chrono::Utc>,
    payload: serde_json::Value,
) {
    // event_id is required by the audit_log schema; the rebuilder
    // doesn't read it (it pulls the FileRef id out of the payload),
    // but the table NOT-NULL still applies. Use a fresh UUID per row
    // — what the production DomainPublisher would also do.
    sqlx::query(
        "INSERT INTO audit_log (timestamp, source, kind, payload, event_id) \
         VALUES ($1, 'test', $2, $3, $4)",
    )
    .bind(ts)
    .bind(kind)
    .bind(payload)
    .bind(uuid::Uuid::new_v4())
    .execute(pool)
    .await
    .expect("insert audit_log row");
}

// ---- GC sweep -------------------------------------------------------------

#[tokio::test]
async fn pg_gc_deletes_orphan_bytes_past_grace() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());
    storage
        .put("sha256/old", Bytes::from_static(b"x"), "text/plain")
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4();
    let mut d = draft(id, job_target("job-1"), "old", "test");
    d.uploaded_at = at(-7 * 86_400); // 7 days back so the grace math is unambiguous
    repo.insert(d).await.unwrap();
    repo.soft_delete(id, at(-31 * 86_400)).await.unwrap();

    // Reference the GC `now` to the fixed test clock (`at(0)`), so the
    // 31-days-ago soft-delete is deterministically past the 30-day
    // grace regardless of the real calendar date.
    let report = gc_orphan_objects(&db.pool, storage.clone(), at(0), chrono::Duration::days(30))
        .await
        .expect("gc");
    assert_eq!(report.examined, 1);
    assert_eq!(report.bytes_deleted, 1);
    assert_eq!(report.kept_dedup, 0);
    assert!(storage.is_empty(), "object should have been deleted");
}

#[tokio::test]
async fn pg_gc_keeps_bytes_when_live_ref_shares_sha() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());
    storage
        .put("sha256/shared", Bytes::from_static(b"y"), "text/plain")
        .await
        .unwrap();

    let dead = uuid::Uuid::new_v4();
    let live = uuid::Uuid::new_v4();
    repo.insert(FileRefDraft {
        bucket: "a".into(),
        ..draft(dead, job_target("job-1"), "shared", "test")
    })
    .await
    .unwrap();
    repo.insert(FileRefDraft {
        bucket: "b".into(),
        ..draft(live, job_target("job-2"), "shared", "test")
    })
    .await
    .unwrap();
    // Soft-delete the dead ref well past the grace window.
    repo.soft_delete(dead, at(-31 * 86_400)).await.unwrap();

    // Reference the GC `now` to the fixed test clock (`at(0)`), so the
    // 31-days-ago soft-delete is deterministically past the 30-day
    // grace regardless of the real calendar date.
    let report = gc_orphan_objects(&db.pool, storage.clone(), at(0), chrono::Duration::days(30))
        .await
        .expect("gc");
    assert_eq!(report.examined, 1);
    assert_eq!(report.kept_dedup, 1);
    assert_eq!(report.bytes_deleted, 0);
    assert_eq!(storage.len(), 1, "live ref keeps shared bytes alive");
}

#[tokio::test]
async fn pg_gc_within_grace_does_not_delete() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());
    storage
        .put("sha256/recent", Bytes::from_static(b"z"), "text/plain")
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4();
    repo.insert(draft(id, job_target("job-1"), "recent", "test"))
        .await
        .unwrap();
    // Soft-deleted only 5 days ago — well inside the 30-day grace.
    repo.soft_delete(id, at(-5 * 86_400)).await.unwrap();

    // Reference the GC `now` to the same fixed test clock the
    // soft-delete uses (`at(0)`), not wall-clock `Utc::now()`. The
    // `at()` anchor is a fixed calendar date; once the real date
    // drifts >35 days past it, a wall-clock `now` would push this
    // 5-days-ago row outside the 30-day grace and the test would
    // spuriously examine (and delete) it.
    let report = gc_orphan_objects(&db.pool, storage.clone(), at(0), chrono::Duration::days(30))
        .await
        .expect("gc");
    assert_eq!(report.examined, 0);
    assert!(!storage.is_empty(), "bytes survive the grace window");
}

// ---- Audit-sample --------------------------------------------------------

#[tokio::test]
async fn pg_audit_sample_matches_when_bytes_are_intact() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());
    let bytes = Bytes::from_static(b"hello-audit");
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha = hex::encode(h.finalize());
    storage
        .put(&format!("sha256/{sha}"), bytes.clone(), "text/plain")
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4();
    let mut d = draft(id, job_target("job-1"), &sha, "test");
    d.size_bytes = bytes.len() as i64;
    d.object_key = format!("sha256/{sha}");
    repo.insert(d).await.unwrap();

    let report = audit_sample(&db.pool, storage.clone(), 50)
        .await
        .expect("audit");
    assert_eq!(report.sampled, 1);
    assert_eq!(report.matched, 1);
    assert_eq!(report.mismatched, 0);
    assert_eq!(report.missing, 0);
}

#[tokio::test]
async fn pg_audit_sample_flags_bytes_missing_for_live_ref() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());

    let id = uuid::Uuid::new_v4();
    repo.insert(draft(id, job_target("job-1"), "missing", "test"))
        .await
        .unwrap();

    let report = audit_sample(&db.pool, storage.clone(), 50)
        .await
        .expect("audit");
    assert_eq!(report.sampled, 1);
    assert_eq!(report.missing, 1);
    assert_eq!(report.mismatches.len(), 1);
    assert!(matches!(
        report.mismatches[0].kind,
        AuditMismatchKind::BytesMissing
    ));
}

#[tokio::test]
async fn pg_audit_sample_flags_hash_mismatch() {
    let db = TestDb::new().await;
    let repo = PgFileRepository::new(db.pool.clone());
    let storage = Arc::new(InMemoryFileStorage::new());
    // Object exists but the bytes don't hash to the recorded sha256
    // (corruption / out-of-band overwrite).
    storage
        .put(
            "sha256/expected",
            Bytes::from_static(b"different-bytes"),
            "text/plain",
        )
        .await
        .unwrap();

    let id = uuid::Uuid::new_v4();
    let mut d = draft(id, job_target("job-1"), "expected", "test");
    d.object_key = "sha256/expected".into();
    repo.insert(d).await.unwrap();

    let report = audit_sample(&db.pool, storage.clone(), 50)
        .await
        .expect("audit");
    assert_eq!(report.mismatched, 1);
    assert!(matches!(
        report.mismatches[0].kind,
        AuditMismatchKind::HashMismatch
    ));
}

#[tokio::test]
async fn pg_check_constraint_rejects_unknown_target_kind() {
    // Defense-in-depth: even if the Rust enum gains a new variant
    // before the schema does, the CHECK constraint will reject it.
    let db = TestDb::new().await;
    let err = sqlx::query(
        "INSERT INTO file_refs
            (id, target_kind, target_id, bucket, object_key,
             sha256, size_bytes, mime, filename, uploaded_by, uploaded_at)
         VALUES ($1, 'galaxy', 'g-001', 'bk', 'sha256/x', 'x', 1, 'x', 'x', 'x', NOW())",
    )
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("file_refs_target_kind_check") || msg.contains("violates check"),
        "expected CHECK violation, got: {msg}"
    );
}
