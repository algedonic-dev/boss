//! Postgres half of `repin_workflow_version_at` (design 7cf202a9,
//! backlog 4347a1af).
//!
//! The in-memory adapter states the move as Rust under one lock; the Pg
//! adapter states it as one transaction of UPDATEs and INSERTs with the
//! terminal-row freeze in SQL `CASE`s. Two implementations of one rule,
//! so the rule is pinned against the real SQL: the pin moves, the
//! `repins` record is appended, a pending row reads the target's text,
//! an inserted step gets a row, a row written after the plan was read
//! refuses the move whole, a completed row keeps what it ran under — and
//! every event rides the outbox.

use boss_core::actor::ActorId;
use boss_core::job::{Job, JobId, JobStatus, Priority, StepId, StepStatus, Subject};
use boss_core::publisher::EventStamp;
use boss_jobs::JobsRepository;
use boss_jobs::registry::{WorkflowSpec, materialize_steps_at, seedable_platform_workflows};
use boss_testing::TestDb;
use chrono::NaiveDate;
use uuid::Uuid;

fn spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "ship-a-change")
        .expect("ship-a-change is a platform workflow")
}

fn set_procedure(spec: &mut WorkflowSpec, slug: &str, text: &str) {
    let step = spec
        .steps
        .iter_mut()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("step {slug}"));
    let mut defaults = step
        .metadata_defaults
        .as_object()
        .cloned()
        .unwrap_or_default();
    defaults.insert("procedure".into(), serde_json::json!(text));
    step.metadata_defaults = serde_json::Value::Object(defaults);
}

fn job(from: &WorkflowSpec) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-00000000a4e1").unwrap()),
        workflow_version: from.version,
        status: JobStatus::Open,
        metadata: serde_json::json!({ "note": "kept" }),
        ..Job::new(
            from.kind.clone(),
            Subject::new("custom", "feat/x"),
            "t",
            "emp-1",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        )
    }
}

async fn outbox(db: &TestDb, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT payload FROM event_outbox WHERE kind = $1 ORDER BY timestamp",
    )
    .bind(kind)
    .fetch_all(&db.pool)
    .await
    .unwrap()
    .into_iter()
    .map(|(p,)| p)
    .collect()
}

/// Backlog 4c6b4b74: a SKIPPED row the plan re-derived to live under
/// the target is written pending, with the target's text; a skipped
/// row the plan left skipped keeps what it has. Skipped is the one
/// terminal status the readiness engine sets alone — no actor, no
/// evidence — so it is the one a re-pin may re-judge, and only through
/// the plan's own `unskipped` flag, never by a live write.
#[tokio::test(flavor = "multi_thread")]
async fn a_repin_unskips_only_the_rows_the_plan_re_derived() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let from = spec();
    let j = job(&from);
    repo.create_job(&j).await.unwrap();
    let mut rows = materialize_steps_at(
        &from,
        &j.subject,
        j.id,
        &j.metadata,
        StepId::new,
        Some(j.opened_on),
        None,
    );
    for s in rows.iter_mut() {
        if matches!(s.spec_slug.as_deref(), Some("build") | Some("merged")) {
            s.status = StepStatus::Skipped;
        }
        repo.add_step(s).await.unwrap();
    }
    let versions = repo.list_steps_versioned(&j.id).await.unwrap();
    let read_of = |id| versions.iter().find(|(s, _)| s.id == id).map(|(_, v)| *v);
    let row = |slug: &str| {
        rows.iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("{slug} row"))
            .clone()
    };
    let rewritten = |slug: &str, unskipped: bool| {
        let mut step = row(slug);
        step.status = if unskipped {
            StepStatus::Pending
        } else {
            StepStatus::Skipped
        };
        step.metadata["procedure"] = serde_json::json!("the target's text");
        boss_jobs::repin::Reprojected {
            changed: vec!["`procedure`".into()],
            kept: vec![],
            unskipped,
            read: read_of(step.id),
            step,
        }
    };
    let plan = boss_jobs::repin::RepinPlan {
        reprojected: vec![rewritten("build", true), rewritten("merged", false)],
        inserted: vec![],
    };
    let stamp = EventStamp::new("jobs", ActorId::Human("emp-bootstrap-admin".into()));
    let record = boss_jobs::repin::record(&plan, 1, 2, "emp-bootstrap-admin", stamp.timestamp);
    repo.repin_workflow_version_at(&j.id, 2, &plan, &record, &stamp)
        .await
        .unwrap();

    let steps = repo.list_steps(&j.id).await.unwrap();
    let by = |slug: &str| {
        steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("{slug} row: {steps:?}"))
    };
    assert_eq!(by("build").status, StepStatus::Pending);
    assert_eq!(by("build").metadata["procedure"], "the target's text");
    assert_eq!(by("merged").status, StepStatus::Skipped);
    assert_eq!(
        by("merged").metadata["procedure"],
        row("merged").metadata["procedure"],
        "a skipped row the plan did not re-derive keeps its text"
    );
    assert!(
        outbox(&db, "jobs.step.updated")
            .await
            .iter()
            .any(|p| p["spec_slug"] == "build" && p["status"] == "pending"),
        "the un-skipping rides the outbox as the row was written, so a replay reproduces it"
    );
}

/// Review of 28f3f28a: a move whose plan un-skips (or inserts) a step
/// is written only onto a packet still walked. A close that lands
/// between the judgement and the write is answered by the statement
/// itself — refused whole, naming the stored status, nothing written.
#[tokio::test(flavor = "multi_thread")]
async fn a_repin_that_opens_rows_refuses_a_packet_closed_since_it_was_judged() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let from = spec();
    let j = job(&from);
    repo.create_job(&j).await.unwrap();
    let mut rows = materialize_steps_at(
        &from,
        &j.subject,
        j.id,
        &j.metadata,
        StepId::new,
        Some(j.opened_on),
        None,
    );
    for s in rows.iter_mut() {
        if s.spec_slug.as_deref() == Some("build") {
            s.status = StepStatus::Skipped;
        }
        repo.add_step(s).await.unwrap();
    }
    let mut build = rows
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("build"))
        .expect("build row")
        .clone();
    build.status = StepStatus::Pending;
    let (_, read) = repo
        .get_step_versioned(&build.id)
        .await
        .unwrap()
        .expect("build");
    let plan = boss_jobs::repin::RepinPlan {
        reprojected: vec![boss_jobs::repin::Reprojected {
            step: build,
            changed: vec!["status".into()],
            kept: vec![],
            unskipped: true,
            read: Some(read),
        }],
        inserted: vec![],
    };

    sqlx::query("UPDATE jobs SET status = 'closed' WHERE id = $1")
        .bind(*j.id.inner().as_uuid())
        .execute(&db.pool)
        .await
        .unwrap();

    let stamp = EventStamp::new("jobs", ActorId::Human("emp-bootstrap-admin".into()));
    let record = boss_jobs::repin::record(&plan, 1, 2, "emp-bootstrap-admin", stamp.timestamp);
    let refused = repo
        .repin_workflow_version_at(&j.id, 2, &plan, &record, &stamp)
        .await;
    assert!(
        matches!(&refused, Err(boss_jobs::JobsError::TerminalJob { status, .. }) if status == "closed"),
        "{refused:?}"
    );
    let stored = repo.get_job(&j.id).await.unwrap().expect("packet");
    assert_eq!(stored.workflow_version, from.version, "the pin stays");
    assert!(
        stored.metadata.get("repins").is_none(),
        "{}",
        stored.metadata
    );
    let steps = repo.list_steps(&j.id).await.unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s.spec_slug.as_deref() == Some("build") && s.status == StepStatus::Skipped),
        "nothing un-skipped: {steps:?}"
    );
    assert!(outbox(&db, "jobs.job.repinned").await.is_empty());
}

/// A RE-PIN WRITES ONLY THE ROW IT READ (backlog e6b05dd6, third review
/// 85c11887 B1). The convert door plans from a read and writes later; a
/// first claim landing in between marked the row, and the UPDATE wrote
/// the read's metadata over it with no compare — mark gone, the step
/// Active with no holder — so a release and a launch record after the
/// claim stood as the launch. The plan carries each row's version from
/// its read, and the statement refuses a row whose `xmin` moved since,
/// under the row's lock, refusing the whole move.
#[tokio::test(flavor = "multi_thread")]
async fn a_repin_planned_before_the_first_claim_cannot_erase_its_mark() {
    use boss_jobs::records_before_claim::{CLAIMED_KEY, KEY};
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let from = spec();
    let j = job(&from);
    repo.create_job(&j).await.unwrap();
    let mut rows = materialize_steps_at(
        &from,
        &j.subject,
        j.id,
        &j.metadata,
        StepId::new,
        Some(j.opened_on),
        None,
    );
    for s in rows.iter_mut() {
        if s.spec_slug.as_deref() == Some("build") {
            s.status = StepStatus::Ready;
            s.assignee_id = None;
            s.metadata[KEY] = serde_json::json!(["launch.model"]);
        }
        repo.add_step(s).await.unwrap();
    }
    let build_id = rows
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("build"))
        .expect("build row")
        .id;

    // The convert's read: Ready, unmarked; its plan moves the text.
    let (mut next, read) = repo
        .get_step_versioned(&build_id)
        .await
        .unwrap()
        .expect("build");
    next.title = "Build it (v2 text)".into();
    let plan = boss_jobs::repin::RepinPlan {
        reprojected: vec![boss_jobs::repin::Reprojected {
            step: next,
            changed: vec!["title".into()],
            kept: vec![],
            unskipped: false,
            read: Some(read),
        }],
        inserted: vec![],
    };

    // The FIRST claim lands between the read and the write.
    let claimed = repo
        .claim_step_at(
            &build_id,
            "agent-claude",
            &EventStamp::new("jobs", ActorId::automation("test")),
            &[],
        )
        .await
        .expect("an unassigned ready step is claimable");
    assert!(
        claimed.metadata.get(CLAIMED_KEY).is_some(),
        "{}",
        claimed.metadata
    );

    let stamp = EventStamp::new("jobs", ActorId::Human("emp-bootstrap-admin".into()));
    let record = boss_jobs::repin::record(&plan, 1, 2, "emp-bootstrap-admin", stamp.timestamp);
    let refused = repo
        .repin_workflow_version_at(&j.id, 2, &plan, &record, &stamp)
        .await;
    assert!(
        matches!(refused, Err(boss_jobs::JobsError::StepChanged { id }) if id == build_id),
        "{refused:?}"
    );
    let row = repo.get_step(&build_id).await.unwrap().expect("build");
    assert_eq!(row, claimed, "the claimed row stands as the claim left it");
    let stored = repo.get_job(&j.id).await.unwrap().expect("packet");
    assert_eq!(stored.workflow_version, from.version, "the pin stays");
    assert!(
        stored.metadata.get("repins").is_none(),
        "{}",
        stored.metadata
    );
    assert!(outbox(&db, "jobs.job.repinned").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repin_writes_the_whole_move_in_one_transaction() {
    let db = TestDb::new().await;
    let repo = boss_jobs::PgJobs::new(db.pool.clone());

    let from = spec();
    let mut to = spec();
    to.version = from.version + 1;
    set_procedure(&mut to, "build", "Build it, and say what you measured.");
    set_procedure(&mut to, "merged", "Merge it, and say where.");
    let mut extra = to
        .steps
        .iter()
        .find(|s| s.title == "settled")
        .expect("settled")
        .clone();
    extra.title = "archived".into();
    to.steps.push(extra);

    let j = job(&from);
    repo.create_job(&j).await.unwrap();
    let rows = materialize_steps_at(
        &from,
        &j.subject,
        j.id,
        &j.metadata,
        StepId::new,
        Some(j.opened_on),
        None,
    );
    for s in &rows {
        repo.add_step(s).await.unwrap();
    }
    let read = repo.list_steps_versioned(&j.id).await.unwrap();
    let stale = boss_jobs::repin::plan_read(&from, &to, &j, &read).expect("planned");

    // THE RACE: `merged` finishes after the plan was read. The plan no
    // longer describes that row, so the move is refused whole (third
    // review 85c11887 B1) — the pin, the record and every row stay.
    let mut merged = rows
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("merged"))
        .expect("merged row")
        .clone();
    let merged_text = merged.metadata["procedure"].clone();
    merged.status = StepStatus::Completed;
    repo.update_step(&merged).await.unwrap();

    let stamp = EventStamp::new("jobs", ActorId::Human("emp-bootstrap-admin".into()));
    let record_of = |plan: &boss_jobs::repin::RepinPlan| {
        boss_jobs::repin::record(
            plan,
            from.version,
            to.version,
            "emp-bootstrap-admin",
            stamp.timestamp,
        )
    };
    let refused = repo
        .repin_workflow_version_at(&j.id, to.version, &stale, &record_of(&stale), &stamp)
        .await;
    assert!(
        matches!(refused, Err(boss_jobs::JobsError::StepChanged { id }) if id == merged.id),
        "{refused:?}"
    );
    assert_eq!(
        repo.get_job(&j.id)
            .await
            .unwrap()
            .expect("packet")
            .workflow_version,
        from.version
    );
    assert!(outbox(&db, "jobs.job.repinned").await.is_empty());

    // Planned again from the rows as they stand, the move lands, and the
    // row that finished keeps the text it ran under.
    let read = repo.list_steps_versioned(&j.id).await.unwrap();
    let plan = boss_jobs::repin::plan_read(&from, &to, &j, &read).expect("planned");
    let moved = repo
        .repin_workflow_version_at(&j.id, to.version, &plan, &record_of(&plan), &stamp)
        .await
        .unwrap();

    assert_eq!(moved.workflow_version, to.version);
    assert_eq!(moved.metadata["note"], "kept", "other keys survive");
    assert_eq!(
        moved.metadata["repins"].as_array().map(Vec::len),
        Some(1),
        "{}",
        moved.metadata
    );

    let steps = repo.list_steps(&j.id).await.unwrap();
    let by = |slug: &str| {
        steps
            .iter()
            .find(|s| s.spec_slug.as_deref() == Some(slug))
            .unwrap_or_else(|| panic!("{slug} row: {steps:?}"))
    };
    assert_eq!(
        by("build").metadata["procedure"],
        "Build it, and say what you measured."
    );
    assert_eq!(
        by("merged").metadata["procedure"],
        merged_text,
        "a row that finished after the plan was read keeps its text"
    );
    assert_eq!(by("merged").status, StepStatus::Completed);
    assert_eq!(by("archived").status, StepStatus::Pending);
    assert_eq!(steps.len(), rows.len() + 1);

    assert_eq!(outbox(&db, "jobs.job.repinned").await.len(), 1);
    assert_eq!(
        outbox(&db, "jobs.job.repinned").await[0]["job_id"],
        j.id.to_string().as_str()
    );
    assert!(
        outbox(&db, "jobs.step.created")
            .await
            .iter()
            .any(|p| p["spec_slug"] == "archived"),
        "the inserted row's state event rides the outbox"
    );
    let updated = outbox(&db, "jobs.step.updated").await;
    assert!(
        updated.iter().any(|p| p["spec_slug"] == "build"
            && p["metadata"]["procedure"] == "Build it, and say what you measured."),
        "the re-projected row's state event carries the row as written"
    );
}
