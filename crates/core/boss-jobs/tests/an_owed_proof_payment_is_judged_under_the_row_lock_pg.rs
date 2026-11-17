//! Postgres half of the payment precondition (backlog 87fe090d).
//!
//! The job doors judge an owed proof's payment on the row they READ and
//! carry it into the store as `expect` (`boss_jobs::car::payment_as_read`);
//! the store judges it under the row lock in the write's own transaction.
//! The in-memory half is the HTTP sequences in
//! `an_owed_proof_payment_is_judged_on_the_row_as_it_stands.rs`; this pins
//! that the Pg adapter's FOR UPDATE read refuses the same writes — the
//! whole-row save and the merge — when the payment moved after the read,
//! writes and records nothing, and lands both when only another key moved.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::car::{OWED_PROOF, PROOF_OWED, payment_as_read};
use boss_jobs::port::JobsError;
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

fn car(id: &str, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        status: JobStatus::Open,
        metadata,
        ..Job::new(
            "ship-a-change",
            Subject::new("custom", "change-1"),
            "Ship a change",
            "automation:filer",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
        )
    }
}

fn stamp() -> EventStamp {
    EventStamp::new("jobs", ActorId::Automation("test".into()))
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// The runner's payment, committed by another writer after the read.
async fn pay(repo: &boss_jobs::PgJobs, id: &JobId) {
    repo.merge_job_metadata_at(
        id,
        &obj(json!({ PROOF_OWED: "paid", OWED_PROOF: {"verified": "v"} })),
        None,
        &stamp(),
    )
    .await
    .unwrap();
}

async fn updated_events(db: &TestDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM event_outbox WHERE kind = 'jobs.job.updated'")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

/// RACE (3) in the adapter: a whole-row save built while the car owed,
/// after the runner paid. Refused under the lock; the payment stands and
/// nothing is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn a_whole_row_save_is_refused_when_the_payment_moved_under_it() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let read = car(
        "00000000-0000-0000-0000-0000000087f1",
        json!({ "merged": "true", PROOF_OWED: "true" }),
    );
    repo.create_job(&read).await.unwrap();
    let expect = payment_as_read(&read.metadata);
    pay(&repo, &read.id).await;
    let before = updated_events(&db).await;

    let stale = Job {
        metadata: json!({ "merged": "true", PROOF_OWED: "true", "note": "seen" }),
        ..read.clone()
    };
    let err = repo
        .update_job_expecting_at(&stale, JobStatus::Open, Some(&expect), Utc::now(), &[])
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::JobChanged { .. }), "got: {err}");
    let md = repo.get_job(&read.id).await.unwrap().unwrap().metadata;
    assert_eq!(md[PROOF_OWED], "paid", "{md}");
    assert_eq!(md[OWED_PROOF]["verified"], "v", "{md}");
    assert!(md.get("note").is_none(), "nothing written: {md}");
    assert_eq!(updated_events(&db).await, before, "nothing recorded");

    // CONTROL: read afresh, the same save lands.
    let fresh = repo.get_job(&read.id).await.unwrap().unwrap();
    let mut body = fresh.clone();
    body.metadata["note"] = json!("seen");
    repo.update_job_expecting_at(
        &body,
        JobStatus::Open,
        Some(&payment_as_read(&fresh.metadata)),
        Utc::now(),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        repo.get_job(&read.id).await.unwrap().unwrap().metadata["note"],
        "seen"
    );
}

/// RACE (4) in the adapter: a debt marked on a row read with none, after
/// the runner paid. Refused; and a merge raced only by ANOTHER key lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_mark_is_refused_when_the_payment_moved_and_lands_when_another_key_did() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());
    let read = car(
        "00000000-0000-0000-0000-0000000087f2",
        json!({ "merged": "true" }),
    );
    repo.create_job(&read).await.unwrap();
    let expect = payment_as_read(&read.metadata);
    let mark = obj(json!({ PROOF_OWED: "true" }));

    // Another key moves under the mark: it still lands.
    repo.merge_job_metadata_at(
        &read.id,
        &obj(json!({ "merge_ref": "abc" })),
        None,
        &stamp(),
    )
    .await
    .unwrap();
    let merged = repo
        .merge_job_metadata_expecting_at(&read.id, &mark, None, Some(&expect), &stamp())
        .await
        .unwrap();
    assert_eq!(merged.metadata[PROOF_OWED], "true");
    assert_eq!(merged.metadata["merge_ref"], "abc");

    // The payment moves under it: refused, the payment stands.
    let second = car(
        "00000000-0000-0000-0000-0000000087f3",
        json!({ "merged": "true" }),
    );
    repo.create_job(&second).await.unwrap();
    let expect = payment_as_read(&second.metadata);
    pay(&repo, &second.id).await;
    let before = updated_events(&db).await;
    let err = repo
        .merge_job_metadata_expecting_at(&second.id, &mark, None, Some(&expect), &stamp())
        .await
        .unwrap_err();
    assert!(matches!(err, JobsError::JobChanged { .. }), "got: {err}");
    let md = repo.get_job(&second.id).await.unwrap().unwrap().metadata;
    assert_eq!(md[PROOF_OWED], "paid", "{md}");
    assert_eq!(updated_events(&db).await, before, "nothing recorded");
}
