//! `GET /api/yard/regions` — the IT system map's KPI read, end-to-end
//! through the real router against the in-memory adapters (design
//! 0524fc95, car 1).
//!
//! What this pins:
//!
//! 1. **Eight regions, every one carrying `count`, `state` and `trend`**
//!    — the shape the map (car 2) and `boss orient` both read, so the
//!    yard, the CLI and the map answer with one voice.
//! 2. **The judgements are the yard's own.** A train the status read
//!    already calls blocked is the track's trouble here too — one
//!    predicate, not a second copy.
//! 3. **The window is a query parameter** (`window=48h`, default 24h),
//!    refused with a 400 when it cannot be read rather than silently
//!    answering the default.
//! 4. **A denied caller gets an empty, well-formed map**, not a 403 and
//!    not a false-empty — the same read gate every queue surface keeps.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::partition::Partition;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence};
use boss_jobs::delivery::{DeliveryPolicyRepository, InMemoryDeliveryPolicy};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::regions::REGIONS;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const NOW: &str = "2026-09-19T12:00:00Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn depth_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-board-on-dock-depth".into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        every_minutes: None,
        at_times: None,
        min_dock_depth: Some(4),
        cooldown_minutes: Some(120),
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

/// The loading-dock row in miniature — the same shape
/// `yard_status_http.rs` wires, so the dock region reads the row the
/// handler reads in production.
fn dock_station_row() -> boss_jobs::StationSpec {
    let mut s = boss_jobs::StationSpec::draft(
        "loading-dock",
        "Loading dock — parked ship-a-change cars",
        boss_jobs::StationKind::Batch,
        boss_jobs::station_queue::StationPredicate {
            kind: Some("ship-a-change".into()),
            status: Some(JobStatus::Open),
            metadata_present: vec!["branch".into()],
            metadata_absent: vec!["train".into()],
            step: Some(boss_jobs::station_queue::StepMatch {
                slug: Some("review".into()),
                status_in: vec![StepStatus::Ready, StepStatus::Active],
                metadata_unmarked: vec!["hold".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        t(NOW),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

/// A station every ready TASK lands on, whoever's packet it is — the
/// shape of the live `q.platform-admin.task`, which is where receiving
/// and marshalling counted the same packets (design 62de32ae decision 4).
fn task_station_row() -> boss_jobs::StationSpec {
    let mut s = boss_jobs::StationSpec::draft(
        "q.platform-admin.task",
        "Ready tasks",
        boss_jobs::StationKind::Constraint,
        boss_jobs::station_queue::StationPredicate {
            status: Some(JobStatus::Open),
            step: Some(boss_jobs::station_queue::StepMatch {
                kind: Some("task".into()),
                status_in: vec![StepStatus::Ready],
                ..Default::default()
            }),
            ..Default::default()
        },
        t(NOW),
    );
    s.status = boss_jobs::registry::WorkflowStatus::Active;
    s
}

/// The native history the in-memory adapter keeps none of (item ab09f1af,
/// car 2: the map counts flow on the packet-membership basis): the task
/// station published a month ago and nothing since, so the membership
/// reader counts a station that served NOTHING in either window — the
/// case the not-draining judgement decides.
fn quiet_history() -> boss_jobs::membership_replay::ReplayInput {
    use boss_clock_client::ClockMode;
    use boss_clock_client::mapping_history::{ClockMappingFact, ClockTimeline};
    use boss_events::original_group::{GroupReference, OriginalGroup};
    let now = Utc::now();
    let published = now - chrono::Duration::days(30);
    let until = now;
    let row = boss_jobs::move_diagnosis::HistoricalEvent {
        seq: 2,
        created_at: Some(published),
        event: boss_core::event::Event::new(
            "jobs",
            boss_jobs::events::STATION_PUBLISHED,
            json!(task_station_row()),
            published,
        ),
    };
    let ids = vec![row.event.id];
    let original = OriginalGroup {
        reference: GroupReference {
            id: Uuid::new_v4(),
            schema_version: 1,
            event_count: 1,
            member_hash: OriginalGroup::roster_hash(&ids),
        },
        event_ids: ids,
        audit_sequences: vec![Some(row.seq)],
        complete: true,
    };
    let clock = ClockMappingFact {
        schema_version: 1,
        revision: "clock".into(),
        previous_revision: None,
        epoch: Some("boot".into()),
        mode: Some(ClockMode::Wall),
        params: None,
        captured_at: published - chrono::Duration::days(1),
        audit_created_at: published - chrono::Duration::days(1),
        seq: 1,
        complete_group: true,
    };
    boss_jobs::membership_replay::ReplayInput {
        groups: vec![boss_jobs::membership_replay::ReplayGroup {
            original: Some(original),
            rows: vec![row],
        }],
        since: published,
        until,
        audit_cutoff: 2,
        actor: None,
        scope: boss_jobs::port::JobScope::All,
        currently_authorized: None,
        clock: ClockTimeline::build(
            &[clock],
            Some("clock"),
            published - chrono::Duration::days(1),
            until,
        ),
        registry_prefix_complete: true,
        packet_prefix_complete: true,
        source_unavailable: vec![],
        initial: Default::default(),
    }
}

/// The app with the platform protocols in its workflow registry — so the
/// inbound kinds can be named — and the task station beside the dock.
fn app_with_intake() -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_intake_and(None)
}

/// [`app_with_intake`] with the agents registry wired (backlog
/// 8e9d3723): the top board reads which holders are agents off it.
fn app_with_intake_and(
    agent_budget: Option<Arc<boss_jobs::agent_budget::BudgetDoor>>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    app_with_intake_seeding(agent_budget, Vec::new())
}

/// [`app_with_intake_and`] with further protocol rows in the registry —
/// a protocol a test's packets are pinned to.
fn app_with_intake_seeding(
    agent_budget: Option<Arc<boss_jobs::agent_budget::BudgetDoor>>,
    extra: Vec<boss_jobs::registry::WorkflowSpec>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let (app, jobs, _) = app_with_intake_projection(agent_budget, extra);
    (app, jobs)
}

fn app_with_intake_projection(
    agent_budget: Option<Arc<boss_jobs::agent_budget::BudgetDoor>>,
    extra: Vec<boss_jobs::registry::WorkflowSpec>,
) -> (
    axum::Router,
    Arc<InMemoryJobs>,
    Arc<boss_jobs::membership_projection_service::MembershipProjectionService>,
) {
    let jobs = Arc::new(InMemoryJobs::new());
    let projection = Arc::new(
        boss_jobs::membership_projection_service::MembershipProjectionService::new(
            jobs.clone(),
            Arc::new(boss_jobs::membership_projection::InMemoryMembershipProjection::new()),
            Arc::new(boss_clock_client::WallClockClient),
        ),
    );
    let kinds = Arc::new(boss_jobs::InMemoryWorkflows::for_fixture());
    for spec in boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .chain(extra)
    {
        kinds.seed(spec).expect("seed platform kind");
    }
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            // A NARROWED scope — its own packets only (backlog 57326714).
            .allow("sales", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations
        .seed(dock_station_row())
        .expect("seed the dock row");
    stations
        .seed(task_station_row())
        .expect("seed the task station");
    let state = JobsApiState {
        membership_projection: Some(projection.clone()),
        stations: Some(stations),
        kind_registry: Some(kinds as Arc<dyn boss_jobs::WorkflowRegistry>),
        agent_budget,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy_client,
            Arc::new(boss_clock_client::FixedClockClient::new(
                boss_clock_client::ClockNow {
                    now: t(NOW),
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                    warp_factor: None,
                },
            )),
        )
    };
    (router(state), jobs, projection)
}

/// An open backlog-item as the protocol admits one: `filed` (its
/// trigger) completed, then `triage` — completed or still ready — and,
/// once triaged, `build` ready.
async fn backlog_item(jobs: &InMemoryJobs, n: u32, triaged: bool) -> JobId {
    backlog_item_ranked(jobs, n, triaged, Priority::Standard, None, Partition::Real).await
}

/// [`backlog_item`] admitted at a priority, an instant and a partition —
/// all three fixed at admission, so they are set on the row it is
/// created with.
async fn backlog_item_ranked(
    jobs: &InMemoryJobs,
    n: u32,
    triaged: bool,
    priority: Priority,
    opened_at: Option<&str>,
    partition: Partition,
) -> JobId {
    let id = Uuid::from_u128(0xB0B0_0000_0000_0000_0000_0000_0000_0000 + u128::from(n)).to_string();
    let item = Job {
        priority,
        opened_at: opened_at.map(t),
        partition,
        ..job(
            "backlog-item",
            &id,
            &format!("item {n}"),
            JobStatus::Open,
            json!({}),
        )
    };
    jobs.create_job_at(&item, t(NOW), &[]).await.unwrap();
    let mut filed = step(&item.id, "filed", "filed", StepStatus::Completed, json!({}));
    filed.kind = boss_jobs::regions::TRIGGER_STEP_KIND.into();
    let triage_status = if triaged {
        StepStatus::Completed
    } else {
        StepStatus::Ready
    };
    let mut steps = vec![
        filed,
        step(&item.id, "triage", "triage", triage_status, json!({})),
    ];
    if triaged {
        steps.push(step(
            &item.id,
            "build",
            "build",
            StepStatus::Ready,
            json!({}),
        ));
    }
    for s in steps {
        jobs.add_step_at(&s, t(NOW), &[]).await.unwrap();
    }
    item.id
}

/// THE PARTITION, END TO END (design 62de32ae decision 4): two
/// backlog-items standing at the one task station — one waiting on its
/// triage, one triaged and waiting on its build — are one packet in
/// receiving and one in marshalling, and the two borders out of the pair
/// wait on one each. Before, both regions counted both, and the world's
/// "waiting at the borders" summed the overlap.
#[tokio::test]
async fn a_packet_is_in_receiving_or_marshalling_and_never_both() {
    let (app, jobs) = app_with_intake();
    backlog_item(&jobs, 1, false).await;
    backlog_item(&jobs, 2, true).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], 1, "{receiving}");
    let marshalling = region(&v, "marshalling");
    assert_eq!(marshalling["count"], 1, "{marshalling}");

    let (status, b) = get(&app, "operator", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let waiting = |from: &str, to: &str| {
        b["borders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["from"] == from && x["to"] == to)
            .map(|x| x["waiting"].clone())
            .unwrap_or_else(|| panic!("no border {from} -> {to} in {b}"))
    };
    assert_eq!(waiting("receiving", "marshalling"), 1);
    assert_eq!(waiting("marshalling", "shop-floor"), 1);
}

async fn complete_quiet_flow(app: &axum::Router) {
    // Readiness is the HTTP state, not five seconds of gate wall time.
    // Keep a bounded watchdog for a broken fixture while allowing scheduling.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (status, body) = get(app, "operator", "/api/yard/flow").await;
            if status == StatusCode::OK && body["state"] == "ready" {
                return;
            }
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiet membership fixture must publish a completed flow reading");
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "quiet membership fixture must publish a completed flow reading")]
async fn a_flow_response_that_never_finishes_still_fails_bounded() {
    let app = axum::Router::new().route(
        "/api/yard/flow",
        axum::routing::get(|| async { std::future::pending::<axum::Json<Value>>().await }),
    );
    complete_quiet_flow(&app).await;
}

#[tokio::test(start_paused = true)]
async fn a_delayed_ready_flow_response_is_not_a_membership_failure() {
    use axum::routing::get;
    let start = tokio::time::Instant::now();
    let app = axum::Router::new().route(
        "/api/yard/flow",
        get(move || async move {
            if start.elapsed() < std::time::Duration::from_secs(6) {
                // A real timer lets the paused runtime advance deterministically.
                tokio::time::sleep(std::time::Duration::from_secs(6)).await;
            }
            axum::Json(json!({"state":"ready"}))
        }),
    );
    complete_quiet_flow(&app).await;
}

/// A STATION THAT SERVED NOTHING, JUDGED ON WHO OWNS WHAT STANDS THERE,
/// END TO END (backlog 6254e602). Live on 2026-10-17 the region read
/// troubled (not draining) on a station whose five steps waited on
/// emp-david: red, for a wait on its named owner. Through the HTTP read —
/// the packet's pinned protocol from the registry, the step's recorded
/// ready flip from the queue-age read — a step an hour into the 24h its
/// protocol declares is attention naming its owner; the same step two
/// days old is past its bound and still troubled.
#[tokio::test]
async fn a_step_waiting_on_its_named_owner_reads_attention_inside_its_bound_and_troubled_past_it() {
    let mut bounded = boss_jobs::registry::WorkflowSpec::platform_seed(
        "bounded-question",
        "A question with a declared bound",
        // Not `platform`: a platform kind is inbound, and an inbound
        // packet nothing has taken in is receiving's, not marshalling's.
        "operations",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            title: "answer".into(),
            duration_hours: Some(24.0),
            ..Default::default()
        }],
    );
    // The version the fixture's packets are pinned to.
    bounded.version = 16;
    for (ready_at, state) in [
        ("2026-09-19T11:00:00Z", "attention"),
        ("2026-09-17T11:00:00Z", "troubled"),
    ] {
        let (app, jobs, projection) = app_with_intake_projection(None, vec![bounded.clone()]);
        jobs.answer_membership(quiet_history());
        projection
            .refresh_one(&boss_jobs::membership_projection::ProjectionIdentity::new(
                Some("emp-david".into()),
                boss_jobs::port::JobScope::All,
            ))
            .await
            .unwrap();
        complete_quiet_flow(&app).await;
        let id = Uuid::from_u128(0x6254_e602_0000_0000_0000_0000_0000_0001).to_string();
        let packet = job(
            "bounded-question",
            &id,
            "a question for David",
            JobStatus::Open,
            json!({}),
        );
        jobs.create_job_at(&packet, t(ready_at), &[]).await.unwrap();
        let mut answer = step(
            &packet.id,
            "answer",
            "answer",
            StepStatus::Ready,
            json!({"authority_role": "platform-admin"}),
        );
        answer.assignee_id = Some("emp-david".into());
        jobs.add_step_at(&answer, t(ready_at), &[]).await.unwrap();
        let (status, v) = get(&app, "operator", "/api/yard/regions").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let m = region(&v, "marshalling");
        assert_eq!(m["count"], 1, "{m}");
        assert_eq!(m["state"], state, "{m}");
        let why = m["why"].as_str().unwrap_or_default();
        assert!(
            why.contains("q.platform-admin.task") && why.contains("emp-david"),
            "the station and the owner are named: {m}"
        );
        if state == "troubled" {
            assert!(why.contains("past its 24h bound"), "{m}");
        }
    }
}

/// A FAILED ONSET READ NAMES THE READ, NOT THE RECORD (backlog 35d29890,
/// from landed-review 5e2e42aa of car ba03b95e). The handler read the
/// queue-age rows with `.ok()`, so when that read failed every standing
/// step said "its ready instant is not on the record" — the troubled
/// verdict right, its stated cause wrong (CLAUDE.md, "a verdict must name
/// what failed"). The same owned step an hour into its 24h bound, with
/// the queue-age read failing, is troubled for THAT reason, the error
/// carried through.
#[tokio::test]
async fn a_failed_queue_age_read_is_named_as_the_cause_not_the_record() {
    assert_queue_age_failure(false).await;
}

#[tokio::test]
async fn cold_flow_does_not_hide_an_independent_queue_age_failure() {
    assert_queue_age_failure(true).await;
}

async fn assert_queue_age_failure(cold: bool) {
    let mut bounded = boss_jobs::registry::WorkflowSpec::platform_seed(
        "bounded-question",
        "A question with a declared bound",
        "operations",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            title: "answer".into(),
            duration_hours: Some(24.0),
            ..Default::default()
        }],
    );
    bounded.version = 16;
    let ready_at = "2026-09-19T11:00:00Z";
    let (app, jobs, projection) = app_with_intake_projection(None, vec![bounded]);
    jobs.answer_membership(quiet_history());
    if !cold {
        projection
            .refresh_one(&boss_jobs::membership_projection::ProjectionIdentity::new(
                Some("emp-david".into()),
                boss_jobs::port::JobScope::All,
            ))
            .await
            .unwrap();
        complete_quiet_flow(&app).await;
    }
    let id = Uuid::from_u128(0x35d2_9890_0000_0000_0000_0000_0000_0001).to_string();
    let packet = job(
        "bounded-question",
        &id,
        "a question for David",
        JobStatus::Open,
        json!({}),
    );
    jobs.create_job_at(&packet, t(ready_at), &[]).await.unwrap();
    let mut answer = step(
        &packet.id,
        "answer",
        "answer",
        StepStatus::Ready,
        json!({"authority_role": "platform-admin"}),
    );
    answer.assignee_id = Some("emp-david".into());
    jobs.add_step_at(&answer, t(ready_at), &[]).await.unwrap();
    jobs.fail_queue_age();

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let m = region(&v, "marshalling");
    assert_eq!(m["state"], "troubled", "{m}");
    let why = m["why"].as_str().unwrap_or_default();
    if cold {
        assert!(
            v["flow_as_of"].is_null(),
            "cold history remains unread: {v}"
        );
        assert!(why.contains("pending"), "pending remains visible: {m}");
        assert!(
            m["trend"]["current"].is_null(),
            "unknown flow is never zero: {m}"
        );
    }
    assert!(
        why.contains("the queue-age read failed") && why.contains("injected by fail_queue_age"),
        "the failed read and its error are named: {m}"
    );
    assert!(
        !why.contains("not on the record"),
        "a failed read does not blame the record: {m}"
    );
}

/// RECEIVING READS PAST ITS PAGE (design 62de32ae decision 4): the
/// inbound read took ONE page of the handler's `MAX_LIMIT` (1000) and
/// counted what came back, so past it the count was a floor that did not
/// say it was one. One more untriaged item than a page holds is counted
/// in full — and the open-packet page the steps ride is a page too, so
/// the one past it is judged on steps read for it alone.
#[tokio::test]
async fn receiving_counts_every_inbound_packet_past_one_page() {
    const PAST_ONE_PAGE: u32 = 1001;
    let (app, jobs) = app_with_intake();
    for n in 0..PAST_ONE_PAGE {
        backlog_item(&jobs, n, false).await;
    }
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], PAST_ONE_PAGE, "{receiving}");
}

/// THE CROSSING INTO MARSHALLING IS THE INTAKE, END TO END (design
/// 62de32ae, car C). A backlog-item triaged this morning and CLOSED on
/// that act is one crossing of receiving -> marshalling, stamped at its
/// triage — which the handler can only see if it reads the steps of a
/// CLOSED inbound row, not just the open ones (it read none of them,
/// and the rail counted closures instead).
#[tokio::test]
async fn a_closed_packets_intake_is_a_crossing_into_marshalling() {
    let (app, jobs) = app_with_intake();
    let id = Uuid::from_u128(0xC105_0000_0000_0000_0000_0000_0000_0001).to_string();
    let item = job(
        "backlog-item",
        &id,
        "triaged and closed",
        JobStatus::Closed,
        json!({ "closed_at": "2026-09-19T10:00:00Z" }),
    );
    jobs.create_job_at(&item, t(NOW), &[]).await.unwrap();
    let mut filed = step(&item.id, "filed", "filed", StepStatus::Completed, json!({}));
    filed.kind = boss_jobs::regions::TRIGGER_STEP_KIND.into();
    filed.completed_at = Some(t("2026-09-18T20:00:00Z"));
    let mut triage = step(
        &item.id,
        "triage",
        "triage",
        StepStatus::Completed,
        json!({}),
    );
    triage.completed_at = Some(t("2026-09-19T08:00:00Z"));
    for s in [filed, triage] {
        jobs.add_step_at(&s, t(NOW), &[]).await.unwrap();
    }

    let (status, b) = get(&app, "operator", "/api/yard/borders").await;
    assert_eq!(status, StatusCode::OK, "{b}");
    let rail = b["borders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["from"] == "receiving" && x["to"] == "marshalling")
        .unwrap_or_else(|| panic!("no receiving -> marshalling rail in {b}"));
    assert_eq!(rail["rate"]["samples"], 1, "{rail}");
    assert_eq!(
        rail["last_crossed"], "2026-09-19T08:00:00+00:00",
        "stamped at the intake, not the close: {rail}"
    );
    assert_eq!(rail["waiting"], 0, "a closed packet stands nowhere: {rail}");
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            // The estate registry's own question (`/api/estate/nodes`),
            // which the operator's shipped grant answers (backlog 0964ba80).
            .allow("operator", Action::Read, Resource::estate(), Scope::All)
            // A NARROWED scope — its own packets only (backlog 070de88c).
            .allow("sales", Action::Read, Resource::job(), Scope::Self_)
            // EVERY packet, and no Read on the estate (backlog 0964ba80).
            .allow("packets-only", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let cadence: Arc<dyn CadenceRepository> = Arc::new(InMemoryCadence::new(vec![depth_rule()]));
    let delivery: Arc<dyn DeliveryPolicyRepository> = Arc::new(InMemoryDeliveryPolicy::new(vec![]));
    let stations = Arc::new(boss_jobs::InMemoryStations::new());
    stations
        .seed(dock_station_row())
        .expect("seed the dock row");
    let state = JobsApiState {
        stations: Some(stations),
        cadence: Some(cadence),
        delivery: Some(delivery),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy_client,
            Arc::new(boss_clock_client::FixedClockClient::new(
                boss_clock_client::ClockNow {
                    now: t(NOW),
                    simulated: false,
                    epoch_start: None,
                    epoch_end: None,
                    paused: false,
                    restart_in_progress: false,
                    warp_factor: None,
                },
            )),
        )
    };
    (router(state), jobs)
}

fn job(kind: &str, id: &str, title: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        workflow_version: 16,
        status,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()),
        metadata,
        ..Job::new(
            kind,
            Subject::new("custom", "s"),
            title,
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
        )
    }
}

fn step(job_id: &JobId, slug: &str, title: &str, status: StepStatus, metadata: Value) -> Step {
    let mut s = Step::new(*job_id, "task", title, 0);
    s.id = StepId::new();
    s.spec_slug = Some(slug.into());
    s.status = status;
    s.metadata = metadata;
    s
}

/// A red PR train in transit, a parked car on the dock, and an arrival
/// this morning.
async fn seed(jobs: &InMemoryJobs) {
    let now = t(NOW);
    let train = job(
        "pr-train",
        "11111111-1111-1111-1111-111111111111",
        "train #470",
        JobStatus::Open,
        json!({ "boarded_jobs": ["33333333-3333-3333-3333-333333333333"] }),
    );
    jobs.create_job_at(&train, now, &[]).await.unwrap();
    for s in [
        step(
            &train.id,
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:00:00Z" }),
        ),
        step(
            &train.id,
            "pr",
            "Open the batched PR",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:00:00Z" }),
        ),
        step(
            &train.id,
            "ci",
            "CI verdict",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T11:20:00Z", "result": "failing" }),
        ),
        step(
            &train.id,
            "merged",
            "Merged into main",
            StepStatus::Ready,
            json!({}),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    let car = job(
        "ship-a-change",
        "22222222-2222-2222-2222-222222222222",
        "A fix",
        JobStatus::Open,
        json!({ "branch": "fix/a" }),
    );
    jobs.create_job_at(&car, now, &[]).await.unwrap();
    for s in [
        step(
            &car.id,
            "gate",
            "Green, and observed working",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-19T10:00:00Z" }),
        ),
        step(
            &car.id,
            "review",
            "Open for review",
            StepStatus::Ready,
            json!({}),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    let arrived = job(
        "pr-train",
        "44444444-4444-4444-4444-444444444444",
        "train #469",
        JobStatus::Closed,
        json!({ "outcome": "arrived", "closed_at": "2026-09-19T09:00:00Z", "boarded_jobs": ["x"] }),
    );
    jobs.create_job_at(&arrived, now, &[]).await.unwrap();
}

async fn get(app: &axum::Router, role: &str, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("x-boss-user", user_header(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, v)
}

fn region<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no region {name} in {v}"))
}

#[tokio::test]
async fn the_read_answers_every_region_each_with_count_state_and_trend() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["regions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, REGIONS);
    for r in v["regions"].as_array().unwrap() {
        for key in ["count", "state", "trend", "why"] {
            assert!(r.get(key).is_some(), "{} lacks {key}: {r}", r["name"]);
        }
        for key in ["metric", "unit", "current", "previous"] {
            assert!(
                r["trend"].get(key).is_some(),
                "{} trend lacks {key}: {r}",
                r["name"]
            );
        }
    }
    assert_eq!(v["window_hours"], 24);
    assert_eq!(v["now"], NOW);

    // The red-PR train is under its gate, not yet merged — so it stands
    // at the GATES (design e765b3fc §2a, car R1), and the yard's own
    // block reading troubles the gates, where it is drawn. The track,
    // which holds merged trains, is empty.
    // Its gate has already judged it, so it needs no bay and waits in no
    // line: it stands as the third figure, `judged` (backlog bce67f09) —
    // not in the bays, and not in the line (357fe1da).
    let gates = region(&v, "gates");
    assert_eq!(gates["count"], 0, "{gates}");
    assert!(gates.get("waiting").is_none(), "{gates}");
    assert_eq!(gates["judged"], 1, "{gates}");
    assert_eq!(gates["state"], "troubled");
    assert!(
        gates["why"].as_str().unwrap().contains("train #470"),
        "{gates}"
    );
    let track = region(&v, "track");
    assert_eq!(track["count"], 0, "{track}");
    assert_eq!(track["state"], "clear");
    // The dock: the station row read, one car parked, below the depth.
    let dock = region(&v, "dock");
    assert_eq!(dock["count"], 1);
    assert_eq!(dock["state"], "clear");
    assert_eq!(dock["bound"], 4);
    // Arrivals: one this morning; per day at a 24h window.
    let arrivals = region(&v, "arrivals");
    assert_eq!(arrivals["count"], 1);
    assert_eq!(arrivals["trend"]["current"], 1.0);
    // Receiving: no workflow registry is wired, so the inbound kinds
    // cannot be named — troubled and null, never an empty region.
    let receiving = region(&v, "receiving");
    assert_eq!(receiving["count"], Value::Null);
    assert_eq!(receiving["state"], "troubled");
    // WHO MOVED THE WORK (backlog 9ca41c08) rides every region downstream
    // of the shop floor as its last measure; with no workflow registry
    // wired the routes cannot be derived, and it says so rather than
    // reading as moved by nobody. Upstream regions do not carry it.
    for name in [
        "gates", "dock", "track", "shed", "arrivals", "garage", "publish",
    ] {
        let last = region(&v, name)["kpi"].as_array().unwrap().last().cloned();
        let last = last.unwrap_or_else(|| panic!("{name} carries no KPI"));
        assert_eq!(last["name"], "machine-moved", "{name}: {last}");
        assert_eq!(last["value"], Value::Null, "{name}: {last}");
        assert!(
            last["text"]
                .as_str()
                .unwrap()
                .contains("could not be derived"),
            "{name}: {last}"
        );
    }
    assert!(
        !receiving["kpi"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "machine-moved"),
        "{receiving}"
    );
}

/// THE STUCK BLOCK RIDES THE SAME READ (backlog 4142d821, design
/// cf820810 car 2): one entry per third, in the operator surface's order,
/// computed here so the HUD and `boss orient` read it rather than
/// recompute it. With no workflow registry wired the intake cannot be
/// read, and the queue-management third says so in `unknown` — a floor,
/// never a confident zero.
#[tokio::test]
async fn the_read_carries_a_stuck_block_per_third_and_an_unread_intake_is_unknown() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let thirds: Vec<&str> = v["stuck"]
        .as_array()
        .unwrap_or_else(|| panic!("no stuck block: {v}"))
        .iter()
        .map(|t| t["third"].as_str().unwrap())
        .collect();
    assert_eq!(thirds, ["queue-management", "actors-building", "delivery"]);
    let queue = &v["stuck"][0];
    assert!(
        queue["unknown"].as_array().unwrap().iter().any(|u| u
            .as_str()
            .unwrap()
            .contains("inbound kinds could not be read")),
        "{queue}"
    );
    assert!(
        queue["regions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "receiving"),
        "{queue}"
    );
    // The parked car declares no edge and nothing has landed: delivery
    // has nothing stuck and nothing waiting.
    let delivery = &v["stuck"][2];
    assert_eq!(delivery["stuck"], 0, "{delivery}");
    assert_eq!(delivery["waiting"], 0, "{delivery}");
    assert_eq!(delivery["oldest_hours"], Value::Null, "{delivery}");
}

#[tokio::test]
async fn the_window_is_a_query_parameter_and_a_bad_one_is_refused() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "operator", "/api/yard/regions?window=48h").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["window_hours"], 48);
    assert_eq!(region(&v, "arrivals")["trend"]["current"], 0.5);

    let (status, v) = get(&app, "operator", "/api/yard/regions?window=soon").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn a_denied_caller_gets_an_empty_well_formed_map() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let (status, v) = get(&app, "guest", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["regions"].as_array().unwrap().len(), REGIONS.len());
    assert_eq!(region(&v, "track")["count"], 0);
    assert_eq!(v["outranks"], json!([]), "read, and empty: {v}");
}

/// WHAT OUTRANKS REGULAR ORDER, END TO END (backlog 74569e94, design
/// ea906603 Q1): the top board's first row rides the one regions read.
/// Of four open packets — urgent real, emergency real but newer, urgent
/// SIMULATED, standard real — the board lists the two real ones that
/// drain before standard, oldest first, each with the step it is at and
/// the station it stands at.
#[tokio::test]
async fn the_read_carries_what_outranks_regular_order_oldest_first() {
    let (app, jobs) = app_with_intake();
    let at = Some;
    backlog_item_ranked(
        &jobs,
        2,
        false,
        Priority::Emergency,
        at("2026-09-19T11:00:00Z"),
        Partition::Real,
    )
    .await;
    backlog_item_ranked(
        &jobs,
        1,
        true,
        Priority::Urgent,
        at("2026-09-18T09:00:00Z"),
        Partition::Real,
    )
    .await;
    backlog_item_ranked(
        &jobs,
        3,
        true,
        Priority::Urgent,
        at("2026-09-17T09:00:00Z"),
        Partition::Simulated,
    )
    .await;
    backlog_item(&jobs, 4, true).await;

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let board = v["outranks"].as_array().expect("an answered board");
    let titles: Vec<&str> = board.iter().filter_map(|o| o["title"].as_str()).collect();
    assert_eq!(titles, ["item 1", "item 2"], "{v}");
    assert_eq!(board[0]["priority"], "urgent");
    assert_eq!(board[0]["kind"], "backlog-item");
    assert_eq!(board[0]["age_minutes"], 27 * 60);
    assert_eq!(board[0]["at"]["slug"], "build");
    assert_eq!(board[0]["stations"], json!(["q.platform-admin.task"]));
    assert_eq!(board[1]["priority"], "emergency");
    assert_eq!(board[1]["at"]["slug"], "triage");
}

/// A CLAIM SAYS WHETHER ANYONE IS ON IT, END TO END (backlog 8e9d3723).
/// Two urgent items at `build` ACTIVE, both held by agent-claude: one
/// with an open agent-run building it, one whose run is gone — the shape
/// five urgent items sat in for days, printed identically. Through the
/// real router, with the agents registry wired, the first names its run
/// and the second says it has none; without the registry nobody can say
/// the holder is an agent, and the claim reads unread, never "no run".
#[tokio::test]
async fn an_agents_active_claim_names_its_open_run_or_says_it_has_none() {
    let agents: Arc<dyn boss_jobs::agents::AgentsRegistry> = Arc::new(
        boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-claude", ["claude@algedonic.dev"]),
    );
    let door = Arc::new(boss_jobs::agent_budget::BudgetDoor {
        agents,
        runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
    });
    let (app, jobs) = app_with_intake_and(Some(door));
    let (bare, bare_jobs) = app_with_intake();
    let claimed = |n: u128, title: &str| {
        let id = Uuid::from_u128(0xC1A1_0000_0000_0000_0000_0000_0000_0000 + n).to_string();
        Job {
            priority: Priority::Urgent,
            opened_at: Some(t("2026-09-18T09:00:00Z")),
            ..job("backlog-item", &id, title, JobStatus::Open, json!({}))
        }
    };
    let worked = claimed(1, "worked");
    let stalled = claimed(2, "stalled");
    let run = job(
        "agent-run",
        "c1a10000-0000-0000-0000-0000000000a1",
        "builder run: worked",
        JobStatus::Open,
        json!({ "packet": worked.id.to_string(), "step": "build" }),
    );
    for store in [&jobs, &bare_jobs] {
        for item in [&worked, &stalled] {
            store.create_job_at(item, t(NOW), &[]).await.unwrap();
            let mut build = step(&item.id, "build", "Build", StepStatus::Active, json!({}));
            build.assignee_id = Some("agent-claude".into());
            store.add_step_at(&build, t(NOW), &[]).await.unwrap();
        }
        store.create_job_at(&run, t(NOW), &[]).await.unwrap();
        let building = step(
            &run.id,
            "building",
            "Building",
            StepStatus::Active,
            json!({}),
        );
        store.add_step_at(&building, t(NOW), &[]).await.unwrap();
    }

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let board = v["outranks"].as_array().expect("an answered board");
    let at = |title: &str| {
        board
            .iter()
            .find(|o| o["title"] == title)
            .unwrap_or_else(|| panic!("{title} on the board: {v}"))["at"]
            .clone()
    };
    assert_eq!(
        at("worked")["run"],
        json!({ "state": "building", "run": run.id.to_string() }),
        "{v}"
    );
    assert_eq!(at("stalled")["run"], json!({ "state": "no-run" }), "{v}");

    let (status, v) = get(&bare, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let board = v["outranks"].as_array().expect("an answered board");
    for o in board {
        assert_eq!(o["at"]["run"], json!({ "state": "unread" }), "{v}");
    }
}

/// The agents-registry-wired app, with one urgent item at `build`
/// ACTIVE held by agent-claude, and an open run building it owned by
/// `run_owner`. Answers the app, its store, and the run's id.
async fn a_claim_with_its_run(run_owner: &str) -> (axum::Router, Arc<InMemoryJobs>, String) {
    let agents: Arc<dyn boss_jobs::agents::AgentsRegistry> = Arc::new(
        boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-claude", ["claude@algedonic.dev"]),
    );
    let door = Arc::new(boss_jobs::agent_budget::BudgetDoor {
        agents,
        runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
    });
    let (app, jobs) = app_with_intake_and(Some(door));
    let item = Job {
        priority: Priority::Urgent,
        opened_at: Some(t("2026-09-18T09:00:00Z")),
        ..job(
            "backlog-item",
            "c1a20000-0000-0000-0000-000000000001",
            "worked",
            JobStatus::Open,
            json!({}),
        )
    };
    jobs.create_job_at(&item, t(NOW), &[]).await.unwrap();
    let mut build = step(&item.id, "build", "Build", StepStatus::Active, json!({}));
    build.assignee_id = Some("agent-claude".into());
    jobs.add_step_at(&build, t(NOW), &[]).await.unwrap();
    let run = Job {
        owner_id: run_owner.to_string(),
        ..job(
            "agent-run",
            "c1a20000-0000-0000-0000-0000000000a1",
            "builder run: worked",
            JobStatus::Open,
            json!({ "packet": item.id.to_string(), "step": "build" }),
        )
    };
    jobs.create_job_at(&run, t(NOW), &[]).await.unwrap();
    let building = step(
        &run.id,
        "building",
        "Building",
        StepStatus::Active,
        json!({}),
    );
    jobs.add_step_at(&building, t(NOW), &[]).await.unwrap();
    (app, jobs, run.id.to_string())
}

/// The run the board read for the one claimed item, as `role` reads it.
async fn claimed_run(app: &axum::Router, role: &str) -> Value {
    let (status, v) = get(app, role, "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let board = v["outranks"].as_array().expect("an answered board");
    assert_eq!(board.len(), 1, "the claimed item is on {role}'s board: {v}");
    board[0]["at"]["run"].clone()
}

/// A NARROWED CALLER IS NEVER TOLD A CLAIM HAS NO RUN (backlog 57326714,
/// review c58e1892 F2). A caller who reads only its own packets sees the
/// item it owns and not the run another actor owns, so judging the runs
/// it was handed would say `no-run` — a confident wrong "stalled". The
/// board judges runs only for a caller who reads every packet
/// (`reads_every_packet` in `MapRows::outranks`), and this holds it:
/// with that guard gone, the narrowed answer is `no-run`. The control is
/// the operator on the same store, who reads the run building it.
#[tokio::test]
async fn a_narrowed_caller_reads_a_claims_run_as_unread_never_as_no_run() {
    let (app, _jobs, run) = a_claim_with_its_run("agent-claude").await;
    assert_eq!(
        claimed_run(&app, "operator").await,
        json!({ "state": "building", "run": run })
    );
    assert_eq!(
        claimed_run(&app, "sales").await,
        json!({ "state": "unread" })
    );
}

/// THE CLOSED RUNS CANNOT CUT THE OPEN ONES (backlog e2519eca item 4).
/// The board judges only the OPEN runs, but they were read on one page
/// of `TAIL_WINDOW` (400) beside every run that closed within reach, so
/// a busy two days of finished runs cut the page and every claim on the
/// board read `unread` — measured 2026-10-10 05:10Z at 9 open and 288
/// closed within reach, 297 of the 400. The open runs are their own
/// read now: 400 closed runs beside the one building the item leave the
/// claim judged. Before, the page held 400 of 401 and the claim read
/// unread.
#[tokio::test]
async fn closed_runs_within_reach_do_not_cut_a_claims_open_run() {
    let (app, jobs, run) = a_claim_with_its_run("emp-david").await;
    for n in 0..400u128 {
        let closed = job(
            "agent-run",
            &Uuid::from_u128(0xC1A4_0000_0000_0000_0000_0000_0000_0000 + n).to_string(),
            "a finished run",
            JobStatus::Closed,
            json!({}),
        );
        jobs.create_job_at(&closed, t(NOW), &[]).await.unwrap();
    }
    assert_eq!(
        claimed_run(&app, "operator").await,
        json!({ "state": "building", "run": run })
    );
}

/// A CUT PAGE OF OPEN RUNS IS NEVER JUDGED (backlog 57326714, review
/// c58e1892 F2). The open runs are read one page of the API's
/// `MAX_LIMIT` (1000) deep; past it, the run working a claim may be the
/// one the page left out, and judging the page would say `no-run`. The
/// board judges runs only when the page held every open run
/// (`agent_runs_whole`), and this holds it: 1000 other open runs beside
/// the one building the item make the page cut, and the claim reads
/// unread. With that guard gone the answer is a judged run — building
/// or no-run, whichever side of the cut the run landed — never unread.
#[tokio::test]
async fn a_cut_page_of_runs_reads_a_claims_run_as_unread() {
    let (app, jobs, _run) = a_claim_with_its_run("emp-david").await;
    for n in 0..1000u128 {
        let other = job(
            "agent-run",
            &Uuid::from_u128(0xC1A3_0000_0000_0000_0000_0000_0000_0000 + n).to_string(),
            "another run",
            JobStatus::Open,
            json!({}),
        );
        jobs.create_job_at(&other, t(NOW), &[]).await.unwrap();
    }
    assert_eq!(
        claimed_run(&app, "operator").await,
        json!({ "state": "unread" })
    );
}

/// A READY STEP NOTHING CAN DISPATCH, END TO END (backlog e2519eca).
/// ff309ce6's shape: backlog-item's `prove-delivery` READY, held by
/// agent-claude, declaring no agent block — `boss dispatch` refuses it
/// and no person holds it. Through the real router: that step names its
/// row; the same step held by a person does not; and a ready `build`
/// whose packet carries no block of its own resolves the ACTIVE row's,
/// as the station queue and the claim door do, so it is dispatchable.
#[tokio::test]
async fn a_ready_step_nothing_can_dispatch_names_its_row() {
    let agents: Arc<dyn boss_jobs::agents::AgentsRegistry> = Arc::new(
        boss_jobs::agents::InMemoryAgents::new()
            .with_agent("agent-claude", ["claude@algedonic.dev"]),
    );
    let door = Arc::new(boss_jobs::agent_budget::BudgetDoor {
        agents,
        runs: Arc::new(boss_jobs::agent_runs::InMemoryAgentRuns::new(vec![])),
    });
    let (app, jobs) = app_with_intake_and(Some(door));
    let build_fields = boss_jobs::registry::seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "backlog-item")
        .and_then(|s| s.steps.into_iter().find(|st| st.title == "build"))
        .expect("backlog-item declares build")
        .fields;
    let item = |n: u128, title: &str| {
        let id = Uuid::from_u128(0xD15C_0000_0000_0000_0000_0000_0000_0000 + n).to_string();
        Job {
            priority: Priority::Urgent,
            opened_at: Some(t("2026-09-18T09:00:00Z")),
            ..job("backlog-item", &id, title, JobStatus::Open, json!({}))
        }
    };
    for (n, title, slug, who) in [
        (1, "unfunded", "prove-delivery", "agent-claude"),
        (2, "a person's", "prove-delivery", "emp-david"),
        (3, "funded by the row", "build", "agent-claude"),
    ] {
        let packet = item(n, title);
        jobs.create_job_at(&packet, t(NOW), &[]).await.unwrap();
        let mut s = step(&packet.id, slug, slug, StepStatus::Ready, json!({}));
        s.assignee_id = Some(who.into());
        if slug == "build" {
            s.fields = build_fields.clone();
        }
        jobs.add_step_at(&s, t(NOW), &[]).await.unwrap();
    }

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let board = v["outranks"].as_array().expect("an answered board");
    let dispatch = |title: &str| {
        board
            .iter()
            .find(|o| o["title"] == title)
            .unwrap_or_else(|| panic!("{title} on the board: {v}"))["at"]["dispatch"]
            .clone()
    };
    assert_eq!(
        dispatch("unfunded"),
        json!({ "state": "undispatchable", "kind": "backlog-item", "version": 16 }),
        "{v}"
    );
    assert_eq!(dispatch("a person's"), Value::Null, "{v}");
    assert_eq!(dispatch("funded by the row"), Value::Null, "{v}");
}

/// THE HOST THAT ANSWERED NOTHING IS STILL DRAWN (backlog 49ed87b4).
/// The runners used to be named only by the ops-requests they happened
/// to have answered, so a host that died was drawn nowhere — and an
/// absent glyph is indistinguishable from a runner that does not exist.
/// This pins the read that closes it: the handler asks the ESTATE
/// REGISTRY which hosts should have a runner, and each declared host
/// stands in the PLANT — the strip of machinery that serves every region
/// (design 62de32ae, decision 11) — whether or not it has said anything.
#[tokio::test]
async fn a_declared_runner_host_stands_on_the_map_with_no_request_of_its_own() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let node = |id: &str, roles: &[&str]| boss_jobs::port::EstateNodeInput {
        id: id.to_string(),
        label: id.to_string(),
        address: "10.20.0.15".to_string(),
        role: "forge".to_string(),
        roles: roles.iter().map(|r| r.to_string()).collect(),
        cpu: None,
        memory_gb: None,
        disk_gb: None,
        notes: None,
    };
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    jobs.declare_estate_nodes(
        &[
            node("forge", &[boss_jobs::regions::OPS_RUNNER_ROLE]),
            node("w-1", &[]),
        ],
        &stamp,
    )
    .await
    .expect("the declaration lands");

    let (status, v) = get(&app, "operator", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let machines = v["plant"].as_array().unwrap();
    let ids: Vec<&str> = machines.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec!["runner:host:forge"],
        "the declared host is drawn; a node that declares no runner is not invented"
    );
    assert_eq!(machines[0]["state"], "unknown");
    assert!(
        machines[0]["why"]
            .as_str()
            .unwrap()
            .contains("declares an ops-runner"),
        "{}",
        machines[0]["why"]
    );
}

/// A NARROWED SCOPE IS NOT HANDED THE ESTATE (backlog 0f462796 item 1,
/// built on the car of 070de88c). The estate registry is not scoped by
/// packet: it names every host and what it runs. The map asked policy
/// for its packet rows and then read the registry for any caller whose
/// scope was not empty, so a caller who may read only its OWN packets
/// was drawn each ops-runner host by id and label. The rule the moves
/// reading keeps is the rule here: below a full packet scope, a read
/// that is not scoped by packet is not made. The control is the
/// operator, whose full scope still draws the host from the same
/// registry — so the absence below is the gate, not an empty estate.
#[tokio::test]
async fn a_narrowed_scope_is_not_drawn_the_estates_runner_hosts() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    jobs.declare_estate_nodes(
        &[boss_jobs::port::EstateNodeInput {
            id: "forge".to_string(),
            label: "the forge".to_string(),
            address: "10.20.0.15".to_string(),
            role: "forge".to_string(),
            roles: vec![boss_jobs::regions::OPS_RUNNER_ROLE.to_string()],
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            notes: None,
        }],
        &stamp,
    )
    .await
    .expect("the declaration lands");

    let (status, narrowed) = get(&app, "sales", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{narrowed}");
    let text = narrowed.to_string();
    assert!(
        !text.contains("runner:host:forge") && !text.contains("the forge"),
        "a caller who reads only its own packets was drawn the estate's runner host: {}",
        narrowed["plant"]
    );

    let (_, operator) = get(&app, "operator", "/api/yard/regions").await;
    let ids: Vec<&str> = operator["plant"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"runner:host:forge"),
        "the control: a full scope still draws the declared host: {ids:?}"
    );
}

/// A FULL PACKET SCOPE IS NOT A READ ON THE ESTATE (backlog 0964ba80).
/// `/api/estate/nodes` answers only a caller policy grants Read on
/// `estate` at scope all (`estate_read_refusal`); the map drew the same
/// registry's hosts for any caller who read every packet, without
/// asking. Now it asks the registry door's own question first, and a
/// caller it refuses is drawn no host — the empty list a narrowed scope
/// gets, never "the estate could not be read", which would be a
/// failure's words for a refusal. The control is the operator, granted
/// both, who is still drawn the host.
#[tokio::test]
async fn a_full_packet_scope_without_read_on_the_estate_is_not_drawn_its_hosts() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::Automation("estate-seed".into()),
    );
    jobs.declare_estate_nodes(
        &[boss_jobs::port::EstateNodeInput {
            id: "forge".to_string(),
            label: "the forge".to_string(),
            address: "10.20.0.15".to_string(),
            role: "forge".to_string(),
            roles: vec![boss_jobs::regions::OPS_RUNNER_ROLE.to_string()],
            cpu: None,
            memory_gb: None,
            disk_gb: None,
            notes: None,
        }],
        &stamp,
    )
    .await
    .expect("the declaration lands");

    // The estate door itself refuses this caller — the question the map
    // must ask is the one this answers.
    let (status, _) = get(&app, "packets-only", "/api/estate/nodes").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, v) = get(&app, "packets-only", "/api/yard/regions").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let text = v.to_string();
    assert!(
        !text.contains("runner:host:forge") && !text.contains("the forge"),
        "a caller the estate door refuses was drawn its runner host: {}",
        v["plant"]
    );
    assert!(
        !text.contains("estate registry could not be read"),
        "a refusal in a failure's words: {}",
        v["plant"]
    );

    let (_, operator) = get(&app, "operator", "/api/yard/regions").await;
    let ids: Vec<&str> = operator["plant"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"runner:host:forge"),
        "the control: Read on the estate still draws the declared host: {ids:?}"
    );
}
