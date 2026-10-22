//! The bounded read against a real schema (design c195e83e). Each test
//! writes originals through the ordinary producers (the outbox, its
//! group triggers, the relay), opens the window by the DATABASE clock so
//! the cut falls between relayed rows, and reads the snapshot back.
use super::*;
use crate::membership_replay::replay;
use crate::{JobsRepository, PgJobs, StationSpec};
use boss_core::event::Event;
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_events::outbox::{drain_outbox_once, record_event_in_tx};
use boss_testing::{RecordingEventBus, TestDb};
use serde_json::json;
use std::sync::Arc;

fn station(name: &str) -> StationSpec {
    let source: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/station-predicate-population-20261016.json"
    ))
    .unwrap();
    serde_json::from_value(
        source["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone(),
    )
    .unwrap()
}

struct World {
    db: TestDb,
    jobs: PgJobs,
    bus: Arc<dyn EventBus>,
}

impl World {
    /// The clock fact first, so every instant of a later window maps to
    /// a known business date; then the original registry.
    async fn new(spec: &StationSpec) -> Self {
        let db = TestDb::new().await;
        let jobs = PgJobs::new(db.pool.clone());
        let bus: Arc<dyn EventBus> = RecordingEventBus::new();
        sqlx::query(
            "INSERT INTO clock_mapping_mode(id,mode,epoch) VALUES(1,'wall',gen_random_uuid())",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let workflow = crate::registry::WorkflowSpec::platform_seed(
            "fixture",
            "Original",
            "fixture",
            vec!["custom".into()],
            vec![],
        );
        let now = chrono::Utc::now();
        let mut tx = db.pool.begin().await.unwrap();
        for event in [
            Event::new("jobs", e::STATION_PUBLISHED, json!(spec), now),
            Event::new("jobs", e::WORKFLOW_PUBLISHED, json!(workflow), now),
        ] {
            record_event_in_tx(&mut tx, &event).await.unwrap();
        }
        tx.commit().await.unwrap();
        let world = Self { db, jobs, bus };
        world.relay().await;
        world
    }

    async fn relay(&self) {
        drain_outbox_once(&self.db.pool, &self.bus, 10_000)
            .await
            .unwrap();
    }

    async fn admit(&self, job: &Job, steps: &[Step]) {
        let now = chrono::Utc::now();
        self.jobs
            .create_job_at(
                job,
                now,
                &[Event::new("jobs", e::JOB_CREATED, json!(job), now)],
            )
            .await
            .unwrap();
        for step in steps {
            self.jobs
                .add_step_at(
                    step,
                    now,
                    &[Event::new("jobs", e::STEP_CREATED, json!(step), now)],
                )
                .await
                .unwrap();
        }
    }

    async fn update(&self, job: &Job) {
        let now = chrono::Utc::now();
        self.jobs
            .update_job_at(
                job,
                job.status,
                now,
                &[Event::new("jobs", e::JOB_UPDATED, json!(job), now)],
            )
            .await
            .unwrap();
    }

    /// The window opens between two database-clock instants, so every
    /// row relayed before it sits below the cut and every later one above.
    async fn open_window(&self) -> DateTime<Utc> {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let since: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&self.db.pool)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        since
    }
}

/// An open packet of the `q.platform-admin.task` shape: a member while
/// `proof_probe` is absent.
fn task_packet(today: NaiveDate) -> (Job, Step) {
    let mut job = Job::new(
        "fixture",
        Subject::new("custom", "bounded"),
        "Bounded",
        "alice",
        Priority::Standard,
        today,
    );
    job.status = JobStatus::Open;
    let mut step = Step::new(job.id, "task", "Task", 0);
    step.status = StepStatus::Ready;
    step.metadata["authority_role"] = json!("platform-admin");
    (job, step)
}

fn flow<'a>(
    reading: &'a crate::membership_replay::MembershipReading,
    name: &str,
) -> &'a crate::membership_replay::MembershipFlow {
    reading.stations.iter().find(|f| f.station == name).unwrap()
}

/// THE FINISH LINE'S BOUND TEST (item ab09f1af, check 1). The packet's
/// whole history is larger than the row bound the read is given, and a
/// genesis fold refused exactly this ("complete native membership
/// history exceeds row bound", every station UNKNOWN). Folding from the
/// cut, the same read answers counted flows.
#[tokio::test(flavor = "multi_thread")]
async fn a_window_whose_full_history_exceeds_the_row_bound_still_answers() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, step) = task_packet(chrono::Utc::now().date_naive());
    world.admit(&job, std::slice::from_ref(&step)).await;
    for revision in 0..40 {
        job.metadata["revision"] = json!(revision);
        world.update(&job).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    world.update(&job).await;
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    world.update(&job).await;
    world.relay().await;

    let bound = 20;
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE kind=ANY($1)")
        .bind(PACKET_KINDS)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert!(
        history > bound,
        "the premise: the packet's full history ({history} rows) exceeds the bound ({bound})"
    );
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        bound,
    )
    .await
    .unwrap();
    assert!(input.initial.cut > 0);
    let seeded: Vec<_> = input
        .initial
        .rows
        .iter()
        .map(|r| r.event.kind.as_str())
        .collect();
    assert_eq!(
        seeded,
        vec![e::STEP_CREATED, e::JOB_UPDATED],
        "the starting state is the packet's latest snapshot per entity, nothing older"
    );
    // By sequence, not by a metadata key: no station reads `revision`,
    // so the read no longer carries it (backlog 45b25020).
    let latest: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log WHERE kind=$1 AND id<=$2")
        .bind(e::JOB_UPDATED)
        .bind(input.initial.cut)
        .fetch_one(&world.db.pool)
        .await
        .unwrap();
    assert_eq!(
        input.initial.rows[1].seq, latest,
        "the Job snapshot is the LATEST one before the cut"
    );
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, Some(1), "{:?}", task.unknown);
    assert_eq!(task.departed, Some(1), "{:?}", task.unknown);
    assert_eq!(task.served, Some(0));
}

/// c195e83e Q5: groups are read 500 ids at a time, and where a batch
/// boundary falls can never change what the read answers.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_batch_boundary_never_changes_a_verdict() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let today = chrono::Utc::now().date_naive();
    let mut packets = Vec::new();
    for _ in 0..3 {
        let (job, step) = task_packet(today);
        world.admit(&job, std::slice::from_ref(&step)).await;
        packets.push(job);
    }
    world.relay().await;
    let since = world.open_window().await;
    for job in &mut packets {
        job.metadata["proof_probe"] = json!("left");
        world.update(job).await;
        job.metadata.as_object_mut().unwrap().remove("proof_probe");
        world.update(job).await;
    }
    world.relay().await;
    let mut answers = Vec::new();
    let mut until = None;
    for batch in [1, 2, GROUP_READ_BATCH] {
        let mut input = snapshot_batched(
            &world.db.pool,
            since,
            Some("alice".into()),
            &JobScope::All,
            4096,
            batch,
        )
        .await
        .unwrap();
        assert!(
            input.groups.iter().filter(|g| g.original.is_some()).count() > 2,
            "enough groups that batches of one and two really split them"
        );
        // Each read takes its own transaction instant as the window's
        // end; pin it so only the batch size differs between readings.
        input.until = *until.get_or_insert(input.until);
        answers.push((
            serde_json::to_value(&input.groups).unwrap(),
            serde_json::to_value(&input.initial).unwrap(),
            serde_json::to_value(replay(&input)).unwrap(),
        ));
    }
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[1], answers[2]);
    let reading: crate::membership_replay::MembershipReading =
        serde_json::from_value(answers[2].2.clone()).unwrap();
    assert_eq!(flow(&reading, "q.platform-admin.task").arrived, Some(3));
}

/// c195e83e Q1: a terminal packet is left out only when no registered
/// station's terminal window can still hold it, each window read live.
/// Under a zero-day window, a packet closed two days ago is past it and
/// one closed today is not; once the live registry declares seven
/// days, neither is left out.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_packet_is_left_out_only_past_the_widest_live_terminal_window() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    // The schema's own historical station rows declare 7- and 14-day
    // windows; the first half of this test needs a registry declaring
    // only the zero-day one, which is itself the proof that the live
    // rows are read.
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    // A station that retains the packets' kind for zero days: a packet
    // closed today is inside its window, one closed two days ago is
    // past it. With no retaining station at all, no terminal packet can
    // be counted, and both would be left out (backlog bbeff5cb).
    let mut same_day = station("design-decided");
    same_day.name = "same-day.fixture".into();
    same_day.predicate.kind = Some("fixture".into());
    same_day.terminal_window_days = Some(0);
    crate::stations::StationRegistry::publish_declared(
        &crate::PgStations::new(world.db.pool.clone()),
        same_day,
        &boss_core::actor::ActorId::Automation("membership-test".into()),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let today = chrono::Utc::now().date_naive();
    let (open, _) = task_packet(today);
    let (mut long_closed, _) = task_packet(today);
    long_closed.status = JobStatus::Closed;
    long_closed.closed_on = today.checked_sub_days(chrono::Days::new(2));
    let (mut closed_today, _) = task_packet(today);
    closed_today.status = JobStatus::Closed;
    closed_today.closed_on = Some(today);
    for job in [&open, &long_closed, &closed_today] {
        world.admit(job, &[]).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    let read = || async {
        snapshot(
            &world.db.pool,
            since,
            Some("alice".into()),
            &JobScope::All,
            4096,
        )
        .await
        .unwrap()
    };
    let seeded = |input: &ReplayInput| -> BTreeSet<String> {
        input
            .initial
            .rows
            .iter()
            .filter_map(crate::membership_replay::packet_identity)
            .collect()
    };
    let narrow = read().await;
    assert_eq!(narrow.initial.pruned_terminal, 1);
    assert!(seeded(&narrow).contains(&open.id.to_string()));
    assert!(seeded(&narrow).contains(&closed_today.id.to_string()));
    assert!(!seeded(&narrow).contains(&long_closed.id.to_string()));

    // The live registry declares a terminal window. Its publication is
    // NOT relayed, so only the live row can widen the census. Its kind
    // clause names the packets' own kind: a station that could never
    // count them would not hold them (backlog bbeff5cb, the test below).
    let mut retained = station("design-decided");
    retained.name = "retention.fixture".into();
    retained.predicate.kind = Some("fixture".into());
    retained.terminal_window_days = Some(7);
    crate::stations::StationRegistry::publish_declared(
        &crate::PgStations::new(world.db.pool.clone()),
        retained,
        &boss_core::actor::ActorId::Automation("membership-test".into()),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let wide = read().await;
    assert_eq!(wide.initial.pruned_terminal, 0);
    assert!(seeded(&wide).contains(&long_closed.id.to_string()));
}

/// Backlog bbeff5cb: inside a terminal window, a frozen terminal packet
/// is carried only for a station whose Job-level clauses it meets. One
/// global window carried every packet closed within the widest window
/// whatever the station asked of it: measured 2026-10-22, 38,811
/// packets closed within my-watchlist's 14 days were carried, of which
/// one carried `submitted_by` and 25 closed design docs fell inside
/// design-decided's 7, and the starting state passed the 250k row bound.
/// Step clauses are not judged here — a packet that meets every Job
/// clause is carried whatever its steps — and a self clause binds to
/// the reader, as the replay binds it.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_packet_is_carried_only_for_a_station_that_could_count_it() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    sqlx::query("UPDATE stations SET status='retired' WHERE status='active'")
        .execute(&world.db.pool)
        .await
        .unwrap();
    let stations = crate::PgStations::new(world.db.pool.clone());
    let actor = boss_core::actor::ActorId::Automation("membership-test".into());
    for (name, from) in [
        ("decided.fixture", "design-decided"),
        ("watch.fixture", "my-watchlist"),
    ] {
        let mut live = station(from);
        live.name = name.into();
        crate::stations::StationRegistry::publish_declared(
            &stations,
            live,
            &actor,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }
    let today = chrono::Utc::now().date_naive();
    let closed = |kind: &str, submitted_by: Option<&str>| {
        let (mut job, _) = task_packet(today);
        job.kind = kind.into();
        job.status = JobStatus::Closed;
        job.closed_on = Some(today);
        if let Some(who) = submitted_by {
            job.metadata = json!({"submitted_by": who});
        }
        job
    };
    let nobodys = closed("fixture", None);
    let design = closed("design-doc", None);
    let mine = closed("fixture", Some("alice"));
    let theirs = closed("fixture", Some("bob"));
    for job in [&nobodys, &design, &mine, &theirs] {
        world.admit(job, &[]).await;
    }
    world.relay().await;
    let since = world.open_window().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    let seeded: BTreeSet<String> = input
        .initial
        .rows
        .iter()
        .filter_map(crate::membership_replay::packet_identity)
        .collect();
    assert!(
        seeded.contains(&design.id.to_string()),
        "design-decided can count it"
    );
    assert!(
        seeded.contains(&mine.id.to_string()),
        "alice's watchlist can count it"
    );
    assert!(
        !seeded.contains(&nobodys.id.to_string()),
        "no station can count it"
    );
    assert!(
        !seeded.contains(&theirs.id.to_string()),
        "bob's, not alice's"
    );
    assert_eq!(input.initial.pruned_terminal, 2);
}

/// c195e83e Q2: a transaction relayed partly before the window and partly
/// after it is replayed whole, never split into the starting state, and
/// the existing rule for a group crossing the window start still holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_straddling_the_cut_is_replayed_whole_and_never_seeded() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (job, step) = task_packet(chrono::Utc::now().date_naive());
    let now = chrono::Utc::now();
    let admitted = Event::new("jobs", e::JOB_CREATED, json!(job), now);
    let stepped = Event::new("jobs", e::STEP_CREATED, json!(step), now);
    let mut tx = world.db.pool.begin().await.unwrap();
    record_event_in_tx(&mut tx, &admitted).await.unwrap();
    record_event_in_tx(&mut tx, &stepped).await.unwrap();
    tx.commit().await.unwrap();
    drain_outbox_once(&world.db.pool, &world.bus, 1)
        .await
        .unwrap();
    let since = world.open_window().await;
    world.relay().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert!(
        input
            .initial
            .rows
            .iter()
            .all(|r| r.event.id != admitted.id && r.event.id != stepped.id),
        "no row of a group that reaches past the cut is in the starting state"
    );
    let whole = input
        .groups
        .iter()
        .find(|g| g.rows.iter().any(|r| r.event.id == admitted.id))
        .unwrap();
    assert_eq!(whole.rows.len(), 2);
    assert!(whole.rows.iter().any(|r| r.seq <= input.initial.cut));
    assert!(whole.rows.iter().any(|r| r.seq > input.initial.cut));
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, None);
    assert!(
        task.unknown.iter().any(|u| u.reason.contains("crosses")),
        "{:?}",
        task.unknown
    );
}

/// c195e83e Q3 for Steps: a current Step with no original at the cut and
/// no creation in the window makes its own packet UNKNOWN.
#[tokio::test(flavor = "multi_thread")]
async fn a_current_step_with_no_original_makes_its_packet_unknown() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (job, step) = task_packet(chrono::Utc::now().date_naive());
    world.admit(&job, &[]).await;
    world.jobs.add_step(&step).await.unwrap();
    world.relay().await;
    let since = world.open_window().await;
    let input = snapshot(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        4096,
    )
    .await
    .unwrap();
    assert!(
        input
            .initial
            .unknown
            .get(&job.id.to_string())
            .is_some_and(|reason| reason.contains("current Step")),
        "{:?}",
        input.initial.unknown
    );
    assert_eq!(flow(&replay(&input), "q.platform-admin.task").arrived, None);
}

/// Backlog 45b25020: the map's 24 h and 48 h reads both refused on the
/// 128 MiB byte bound, because every Job and Step row moved whole.
/// Measured live 2026-10-22: 500 `jobs.job.updated` rows were 12.9 MB,
/// 98% of it metadata no station predicate reads (`brief` alone 3.9 MB,
/// `regate_receipt` 1.9 MB) — about 430 MB a day of job updates. The
/// bound stays; what moves is only the metadata a predicate evaluates,
/// so a packet whose rows each outweigh the bound still answers, and
/// the keys the station does read (`proof_probe` on the Job,
/// `authority_role` on the Step) still decide its membership.
#[tokio::test(flavor = "multi_thread")]
async fn metadata_no_predicate_reads_never_spends_the_byte_bound() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, mut step) = task_packet(chrono::Utc::now().date_naive());
    let bound: i64 = 64 * 1024;
    let brief = "x".repeat(bound as usize);
    job.metadata["brief"] = json!(brief);
    step.metadata["receipt"] = json!(brief);
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    world.update(&job).await;
    job.metadata.as_object_mut().unwrap().remove("proof_probe");
    world.update(&job).await;
    world.relay().await;

    let whole: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(octet_length(payload::text)),0)::bigint FROM audit_log",
    )
    .fetch_one(&world.db.pool)
    .await
    .unwrap();
    assert!(
        whole > 4 * bound,
        "the premise: the rows moved whole ({whole} bytes) outweigh the bound ({bound})"
    );
    let input = snapshot_bounded(
        &world.db.pool,
        since,
        Some("alice".into()),
        &JobScope::All,
        1000,
        GROUP_READ_BATCH,
        bound,
    )
    .await
    .unwrap();
    let rows: Vec<_> = input
        .groups
        .iter()
        .flat_map(|g| &g.rows)
        .chain(&input.initial.rows)
        .collect();
    assert!(
        rows.iter().any(|r| r.event.kind == e::JOB_UPDATED) && !input.initial.rows.is_empty(),
        "both the window and the starting state were read"
    );
    for row in rows {
        let metadata = &row.event.payload["metadata"];
        assert!(
            metadata.get("brief").is_none() && metadata.get("receipt").is_none(),
            "{} carried metadata no predicate reads",
            row.event.kind
        );
    }
    let reading = replay(&input);
    let task = flow(&reading, "q.platform-admin.task");
    assert_eq!(task.arrived, Some(1), "{:?}", task.unknown);
    assert_eq!(task.departed, Some(1), "{:?}", task.unknown);
}

/// Backlog 4744f5bd: after 45b25020 the 48 h read still refused on the
/// byte bound, because a Job or Step row's transaction siblings moved
/// whole. Measured live 2026-10-22 over 48 h (189,054 rows): the kinds
/// the replay applies were 83.3 MB projected, every other kind 104.5 MB,
/// the ceiling on their siblings — `step.done.task` 21.8,
/// `jobs.metadata.conditionally_merged` 19.6,
/// `step.done.gate-verdict` 18.5, each mostly a copy of the metadata
/// the Job row had just had cut — against 134 MB. The replay reads such
/// a sibling only for its packet identity, so it carries only the keys
/// that identity is read from: 3.5 MB for all of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_sibling_the_replay_never_applies_carries_only_its_packet_identity() {
    let spec = station("q.platform-admin.task");
    let world = World::new(&spec).await;
    let (mut job, step) = task_packet(chrono::Utc::now().date_naive());
    let bound: i64 = 64 * 1024;
    let brief = "x".repeat(bound as usize);
    world.admit(&job, std::slice::from_ref(&step)).await;
    world.relay().await;
    let since = world.open_window().await;
    job.metadata["proof_probe"] = json!("left");
    let now = chrono::Utc::now();
    world
        .jobs
        .update_job_at(
            &job,
            job.status,
            now,
            &[
                Event::new("jobs", e::JOB_UPDATED, json!(job), now),
                Event::new(
                    "jobs",
                    "step.done.task",
                    json!({"job_id": job.id.to_string(), "step_id": step.id.to_string(),
                           "kind": "task", "metadata": {"brief": brief}}),
                    now,
                ),
            ],
        )
        .await
        .unwrap();
    world.relay().await;

    let sibling: i64 = sqlx::query_scalar(
        "SELECT octet_length(payload::text)::bigint FROM audit_log WHERE kind='step.done.task'",
    )
    .fetch_one(&world.db.pool)
    .await
    .unwrap();
    assert!(
        sibling > bound,
        "the premise: the sibling alone ({sibling} bytes) outweighs the bound ({bound})"
    );
    for scope in [JobScope::All, JobScope::OwnerIs("alice".into())] {
        let input = snapshot_bounded(
            &world.db.pool,
            since,
            Some("alice".into()),
            &scope,
            1000,
            GROUP_READ_BATCH,
            bound,
        )
        .await
        .unwrap();
        let done = input
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .find(|r| r.event.kind == "step.done.task")
            .expect("the sibling is read with its group");
        assert_eq!(done.event.payload, json!({"job_id": job.id.to_string()}));
        assert_eq!(
            crate::membership_replay::packet_identity(done),
            Some(job.id.to_string())
        );
        let reading = replay(&input);
        let task = flow(&reading, "q.platform-admin.task");
        assert_eq!(task.departed, Some(1), "{scope:?} {:?}", task.unknown);
    }
}

/// The keys a sibling carries are the keys `packet_identity` reads: a
/// sibling's identity is the same whole or cut to `IDENTITY_KEYS`, for
/// every shape a producer writes (CLAUDE.md §9a — the projection and the
/// reader are two spellings of one fact, so this pins them together).
#[test]
fn a_sibling_cut_to_its_identity_keys_has_the_same_packet_identity() {
    let cut = |payload: &serde_json::Value| -> serde_json::Value {
        payload
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(k, _)| IDENTITY_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<serde_json::Map<_, _>>()
                    .into()
            })
            .unwrap_or_else(|| payload.clone())
    };
    let row = |payload: serde_json::Value| HistoricalEvent {
        seq: 1,
        created_at: None,
        event: Event::new("jobs", "step.done.task", payload, chrono::Utc::now()),
    };
    for payload in [
        json!({"job_id": "j", "id": "i", "step": {"job_id": "s"}, "metadata": {"x": 1}}),
        json!({"job_id": null, "id": "i", "metadata": {"job_id": "m"}}),
        json!({"step": {"job_id": "s", "title": "t"}, "id": "i", "brief": "b"}),
        json!({"step": ["job_id"], "id": "i"}),
        json!({"id": 7, "payload": {"job_id": "p"}}),
        json!({"title": "no identity"}),
        json!(["job_id"]),
    ] {
        assert_eq!(
            crate::membership_replay::packet_identity(&row(cut(&payload))),
            crate::membership_replay::packet_identity(&row(payload.clone())),
            "{payload}"
        );
    }
}

/// c195e83e Q4: the sibling match of the history read has its index,
/// built by the ordinary schema path, and valid.
#[tokio::test(flavor = "multi_thread")]
async fn the_original_group_sibling_match_is_indexed() {
    let db = TestDb::new().await;
    let (definition, valid): (String, bool) = sqlx::query_as(
        "SELECT pg_get_indexdef(i.indexrelid),i.indisvalid FROM pg_index i \
         JOIN pg_class c ON c.oid=i.indexrelid WHERE c.relname='audit_log_original_group_id'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(valid);
    assert!(
        definition.contains("(original_group ->> 'id'::text)"),
        "{definition}"
    );
}
