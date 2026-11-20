//! Guarded admission on the Postgres adapter (backlog 84a010cb, car 3;
//! design f1d66249): the shared port scenarios, and the writer races the
//! design names — "Concurrency tests must prove the actual answer/close
//! writers share the fences."
//!
//! Both directions, against the REAL writers (the step write door and
//! the close door), never a hand-written UPDATE standing in for them:
//!
//! - a writer already in flight when the attempt arrives is waited for
//!   and its commit SEEN: an answer committed while the attempt waited
//!   suppresses it;
//! - an attempt in flight makes the writers that arrive wait for its
//!   commit: the answer and the close land after the decision, never
//!   between the guard read and the child insert.
//!
//! The attempt is paused mid-transaction the way
//! `a_packet_is_created_whole_or_not_at_all_pg.rs` pauses an admission:
//! another connection holds an uncommitted row under the id of the
//! child's first step, so the attempt's INSERT waits on the unique index
//! with every guard lock already taken.

#[path = "guarded_admission/mod.rs"]
mod scenarios;

use std::sync::Arc;
use std::time::Duration;

use boss_core::job::{JobStatus, StepStatus};
use boss_jobs::guarded_admission::Outcome;
use boss_jobs::{JobsRepository, PgJobs};
use boss_testing::TestDb;
use chrono::TimeDelta;
use scenarios::*;

async fn repo() -> (TestDb, Arc<PgJobs>) {
    let db = TestDb::new().await;
    let repo = Arc::new(PgJobs::new(db.pool.clone()));
    (db, repo)
}

macro_rules! shared {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let (_db, repo) = repo().await;
            scenarios::$name(repo.as_ref()).await;
        }
    )*};
}

shared!(
    a_due_unanswered_intent_admits_its_child_and_decides,
    nothing_is_written_before_the_due_instant,
    an_answered_inquiry_suppresses_and_admits_no_child,
    a_closed_source_suppresses,
    unrelated_work_does_not_move_the_anchor,
    a_retry_after_the_decision_writes_nothing_more,
    an_existing_closed_child_is_reconciled_not_admitted_twice,
    a_stranger_under_the_child_id_is_refused,
    a_same_kind_packet_about_another_subject_under_the_child_id_is_refused,
    a_child_step_of_another_job_is_refused,
    a_child_without_its_step_graph_is_refused_not_reconciled,
    a_conflicting_child_is_refused_even_when_the_guard_suppresses,
    a_missing_guard_step_is_held,
    a_child_the_envelope_does_not_name_is_refused,
);

/// How many backends of this database wait on a lock now.
async fn lock_waiters(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pg_stat_activity \
          WHERE datname = current_database() AND wait_event_type = 'Lock'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn until_waiting(pool: &sqlx::PgPool, n: i64) {
    for _ in 0..200 {
        if lock_waiters(pool).await >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("fewer than {n} backend(s) ever waited on a lock");
}

/// The answer's transaction is open when the attempt arrives: the
/// attempt waits on the `sent` row, then reads the committed answer and
/// suppresses. It never admits off the row as it stood before.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_in_flight_is_waited_for_and_seen() {
    let (db, repo) = repo().await;
    let w = plant(repo.as_ref()).await;
    // The answer, paused before its commit: the step write door's own
    // shape (the reservation, the row lock, the UPDATE), held open.
    let mut answer = db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE steps IN ROW EXCLUSIVE MODE")
        .execute(&mut *answer)
        .await
        .unwrap();
    sqlx::query("UPDATE steps SET status = 'completed' WHERE id = $1")
        .bind(*w.sent.id.inner().as_uuid())
        .execute(&mut *answer)
        .await
        .unwrap();
    let attempt = {
        let repo = repo.clone();
        tokio::spawn(async move { scenarios::attempt(repo.as_ref(), due_at()).await })
    };
    until_waiting(&db.pool, 1).await;
    assert!(!attempt.is_finished(), "the attempt waits on the answer");
    answer.commit().await.unwrap();
    let outcome = attempt.await.unwrap();
    assert!(
        matches!(&outcome, Outcome::Suppressed { reason, .. } if reason.contains("completed")),
        "the committed answer is seen: {outcome:?}"
    );
    assert!(repo.get_job(&id(CHILD)).await.unwrap().is_none());
}

/// An attempt in flight — every guard lock taken, its child insert
/// paused — makes the REAL answer and close writers wait. They land
/// after the decision; the decision admitted, because at its evaluation
/// the inquiry was still unanswered and open.
#[tokio::test(flavor = "multi_thread")]
async fn the_real_answer_and_close_writers_wait_for_an_attempt_in_flight() {
    let (db, repo) = repo().await;
    let w = plant(repo.as_ref()).await;
    let request = scenarios::request(due_at());
    // The pause: an uncommitted step row under the id of the child's
    // first step, hung on an unrelated committed job (for the FK) — not
    // on the source, whose row lock would then be the blocker's too.
    let mut other = w.source.clone();
    other.id = boss_core::job::JobId::from_uuid(uuid::Uuid::from_u128(0xdead));
    other.kind = "unrelated".into();
    repo.create_job(&other).await.unwrap();
    let mut blocker = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO steps (id, job_id, title) VALUES ($1, $2, 'blocker')")
        .bind(*request.child_steps[0].id.inner().as_uuid())
        .bind(*other.id.inner().as_uuid())
        .execute(&mut *blocker)
        .await
        .unwrap();
    let attempt = {
        let repo = repo.clone();
        tokio::spawn(async move {
            repo.guarded_admission_at(&request, &due_events(due_at()))
                .await
        })
    };
    until_waiting(&db.pool, 1).await;
    let later = due_at() + TimeDelta::minutes(1);
    let answering = {
        let (repo, sent) = (repo.clone(), w.sent.clone());
        tokio::spawn(async move { scenarios::answer(repo.as_ref(), &sent, later).await })
    };
    let closing = {
        let repo = repo.clone();
        tokio::spawn(async move { scenarios::close_source(repo.as_ref(), later).await })
    };
    until_waiting(&db.pool, 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!attempt.is_finished(), "the attempt is paused");
    assert!(!answering.is_finished(), "the answer waits for the attempt");
    assert!(!closing.is_finished(), "the close waits for the attempt");
    // Release the pause: the blocker never commits.
    blocker.rollback().await.unwrap();
    let outcome = attempt.await.unwrap().expect("the attempt runs");
    assert!(
        matches!(
            outcome,
            Outcome::Admitted {
                reconciled: false,
                ..
            }
        ),
        "{outcome:?}"
    );
    answering.await.unwrap();
    closing.await.unwrap();
    // Every write landed, in order, and none was lost.
    assert_eq!(repo.list_steps(&id(CHILD)).await.unwrap().len(), 2);
    let decided = due_now(repo.as_ref(), &w).await;
    assert_eq!(decided.metadata["result"], "admitted");
    let sent = repo.get_step(&w.sent.id).await.unwrap().unwrap();
    assert_eq!(sent.status, StepStatus::Completed);
    let source = repo.get_job(&id(SOURCE)).await.unwrap().unwrap();
    assert_eq!(source.status, JobStatus::Closed);
}

/// A failure inside the transaction — here the child's workflow version
/// is spent by a discard — takes the decision with it: no child, and the
/// intent still undecided for the next attempt.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_child_write_leaves_the_intent_undecided() {
    let (db, repo) = repo().await;
    let w = plant(repo.as_ref()).await;
    sqlx::query(
        "INSERT INTO workflow_discarded_versions (kind, version, discarded_at, discarded_by) \
         VALUES ('follow-up-a-prospect', 1, now(), 'test')",
    )
    .execute(&db.pool)
    .await
    .expect("spend the child's version");
    let err = repo
        .guarded_admission_at(&scenarios::request(due_at()), &due_events(due_at()))
        .await
        .expect_err("a spent version admits nothing");
    assert!(err.to_string().contains("follow-up-a-prospect"), "{err}");
    nothing_written(repo.as_ref(), &w).await;
    let outbox: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM event_outbox WHERE payload->>'id' = $1 OR payload->>'job_id' = $1",
    )
    .bind(uuid::Uuid::from_u128(CHILD).to_string())
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(outbox, 0, "no child fact recorded");
}
