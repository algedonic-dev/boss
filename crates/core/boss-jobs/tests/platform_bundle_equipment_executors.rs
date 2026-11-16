//! The steps EQUIPMENT executes, and the stations that carry them
//! (design ccfe87ff "Using the taxonomy: the rows that make the doors
//! bite", decided by David 2026-11-05; pass item 2, backlog 364b27a3).
//!
//! Two resolutions decide this file. `first-executors`: "record-verdict
//! and the probe by equipment". `converge-stations`: the converge is
//! "Three, each with its own bays, since they are operated by different
//! automata and hold different resources" — deploy, converge, probe
//! shed. What the record showed when this was built (2026-11-05, read
//! through the jobs API on origin/main 819ce856a):
//!
//! - `gate-run/record-verdict` — 30 of the 30 newest closed gate-runs
//!   completed it, none ever assigned: the gate runner judges the branch
//!   and the conductor records the receipt it leaves. It is the one step
//!   of a gate-run that stands open for the whole run, so it IS the bay
//!   occupancy the `gates` row's `bays` bound. It names `gates`; a new
//!   station would be a second home for the gate capacity (§9a).
//! - `pr-train/converged` — born assigned to the conductor (its
//!   audience), 15 of the 20 newest closed trains completed it by the
//!   conductor's own image-basis completion (the rule
//!   complete-train-converged-on-runner-rolled) and 5 skipped it. It
//!   names `converge`, whose automaton is the conductor.
//! - `ship-a-change/proven` — 60 of the 60 newest closed cars, and all
//!   11 open cars already proven, were completed by
//!   automation:ops-runner running `boss prove --from-car --unattended`
//!   on the forge (the run-car-probe verb). It names `probe-shed`.
//!
//! The deploy — cluster-deploy-runner.service building forge main and
//! rolling the cluster — is the third station; no decided step names it
//! (its packet's `run` step is boss-step's, and "everything else stays
//! undeclared until a constraint is real").
//!
//! NONE OF THE THREE STEPS IS CLAIMED TODAY. Each goes READY to
//! COMPLETED by a status PUT, and the claim door is the one door that
//! judges a kind executor (T2c, `http::steps`), so declaring these
//! strands no live step. What the declaration changes is who a CLAIM
//! admits, and the last test here drives the claim route over the
//! bundle's own rows: each station's automaton is admitted, and a
//! robot, a person and another automaton are refused by name.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{WorkflowSpec, WorkflowStatus, platform_bundle_path};
use boss_jobs::seed_loader::{load_stations, load_workflows};
use boss_jobs::station_seed::platform_stations_path;
use boss_jobs::stations::{StationKind, StationRegistry, StationSpec};
use boss_jobs::step_executor::ExecutorDemand;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestRequest};
use chrono::NaiveDate;
use serde_json::Value;
use uuid::Uuid;

fn bundle() -> Vec<WorkflowSpec> {
    load_workflows(platform_bundle_path()).expect("the platform bundle parses")
}

fn stations() -> Vec<StationSpec> {
    load_stations(platform_stations_path()).expect("the station bundle parses")
}

fn station(name: &str) -> StationSpec {
    stations()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no station `{name}` in infra/platform/stations"))
}

/// Every `equipment:<station>` declaration in the bundle, one line each:
/// `kind/step equipment:<station>`.
fn equipment_steps(bundle: &[WorkflowSpec]) -> BTreeSet<String> {
    bundle
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(move |s| {
                match ExecutorDemand::parse(s.executor.as_deref()?) {
                    Ok(Some(demand @ ExecutorDemand::Equipment(_))) => {
                        Some(format!("{}/{} {}", w.kind, s.title, demand.declared()))
                    }
                    _ => None,
                }
            })
        })
        .collect()
}

/// The steps naming a station that is not an ACTIVE equipment row with
/// an automaton and at least one bay — a step the claim door would
/// refuse every claimant on — named `kind/step equipment:<station>`.
fn uncarried(bundle: &[WorkflowSpec], stations: &[StationSpec]) -> Vec<String> {
    equipment_steps(bundle)
        .into_iter()
        .filter(|line| {
            line.rsplit_once("equipment:").is_none_or(|(_, name)| {
                !stations.iter().any(|s| {
                    s.name == name
                        && s.status == WorkflowStatus::Active
                        && s.kind == StationKind::Equipment
                        && s.automaton.is_some()
                        && s.bays.is_some_and(|b| b >= 1)
                })
            })
        })
        .collect()
}

/// The decided split, row by row: the station, the automaton that
/// operates it, its bays. Each bay count is what that automaton runs at
/// once today, read from the equipment's own definition (the station
/// files cite it).
#[test]
fn the_converge_is_three_equipment_stations_operated_by_three_automata() {
    let three = [
        ("deploy", "automation:cluster-deploy-runner", 1),
        ("converge", "automation:train-conductor", 1),
        ("probe-shed", "automation:ops-runner", 1),
    ];
    for (name, automaton, bays) in three {
        let s = station(name);
        assert_eq!(s.kind, StationKind::Equipment, "{name}");
        assert_eq!(s.status, WorkflowStatus::Active, "{name}");
        assert_eq!(s.automaton.as_deref(), Some(automaton), "{name}");
        assert_eq!(s.bays, Some(bays), "{name}");
    }
    let automata: BTreeSet<&str> = three.iter().map(|(_, a, _)| *a).collect();
    assert_eq!(
        automata.len(),
        3,
        "three stations, three different automata"
    );
}

#[test]
fn the_steps_executed_by_equipment_are_the_decided_ones() {
    let want: BTreeSet<String> = [
        "gate-run/record-verdict equipment:gates",
        "pr-train/converged equipment:converge",
        "ship-a-change/proven equipment:probe-shed",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(equipment_steps(&bundle()), want);
}

#[test]
fn every_equipment_executor_names_a_declared_equipment_station_with_bays() {
    let bundle = bundle();
    let stations = stations();
    assert_eq!(uncarried(&bundle, &stations), Vec::<String>::new());

    // The controls: the pin bites on a station that is missing, and on
    // one that is present but not equipment.
    let without_shed: Vec<StationSpec> = stations
        .iter()
        .filter(|s| s.name != "probe-shed")
        .cloned()
        .collect();
    assert_eq!(
        uncarried(&bundle, &without_shed),
        vec!["ship-a-change/proven equipment:probe-shed".to_string()]
    );
    let converge_generic: Vec<StationSpec> = stations
        .iter()
        .cloned()
        .map(|s| {
            if s.name == "converge" {
                StationSpec {
                    kind: StationKind::Group,
                    automaton: None,
                    bays: None,
                    ..s
                }
            } else {
                s
            }
        })
        .collect();
    assert_eq!(
        uncarried(&bundle, &converge_generic),
        vec!["pr-train/converged equipment:converge".to_string()]
    );
}

/// THE ORDER (the pass-item-1 hazard, said here rather than in a
/// comment). The three stations reach the live registry through the
/// platform seed on the converge after this car lands; the publish
/// endpoint judges only an executor's SHAPE, so a row published before
/// them would name a station the claim door reads as absent and admits
/// no one at. Nothing is held by default since backlog a4b57aca, so
/// each of these carries a file — a timing hold — saying why and what
/// lifts it, and the deliberate publish waits for the stations to read
/// back live.
#[test]
fn every_row_naming_equipment_carries_a_hold_naming_this_item() {
    let holds =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../infra/platform/workflow-holds");
    let kinds: BTreeSet<String> = equipment_steps(&bundle())
        .into_iter()
        .filter_map(|line| line.split_once('/').map(|(kind, _)| kind.to_string()))
        .collect();
    assert_eq!(kinds.len(), 3, "{kinds:?}");
    for kind in kinds {
        let file = holds.join(format!("{kind}.toml"));
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| {
            panic!("{kind} names equipment and has no {}: {e}", file.display())
        });
        let hold: toml::Table = text
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(
            hold.get("drift_publish").and_then(toml::Value::as_str),
            Some("held"),
            "{}",
            file.display()
        );
        let lifts = hold
            .get("lifts")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        assert!(
            lifts.contains("364b27a3"),
            "{} lifts on this item: {lifts}",
            file.display()
        );
    }
}

// ---------------------------------------------------------------------------
// The claim door, over the bundle's own rows
// ---------------------------------------------------------------------------

struct Fixture {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
    pinned: Vec<(String, i32)>,
}

async fn fixture() -> Fixture {
    let workflows = Arc::new(InMemoryWorkflows::for_fixture());
    let mut pinned = Vec::new();
    for spec in bundle()
        .into_iter()
        .filter(|w| ["gate-run", "pr-train", "ship-a-change"].contains(&w.kind.as_str()))
    {
        pinned.push((spec.kind.clone(), spec.version));
        workflows.seed(spec).unwrap();
    }
    let registry = Arc::new(boss_jobs::stations::InMemoryStations::new());
    for s in stations() {
        registry.seed(s).unwrap();
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for role in ["platform-admin", "system"] {
        policy = policy
            .allow(role, Action::Update, Resource::step(), Scope::All)
            .allow(role, Action::Read, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        kind_registry: Some(workflows as Arc<dyn WorkflowRegistry>),
        stations: Some(registry as Arc<dyn StationRegistry>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    });
    Fixture { app, jobs, pinned }
}

/// A packet of `kind`, pinned to the bundle's row, with one READY step
/// at `slug` held by `holder` (the step as it is born).
async fn packet(f: &Fixture, kind: &str, slug: &str, holder: Option<&str>) -> Step {
    let version = f
        .pinned
        .iter()
        .find(|(k, _)| k == kind)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("{kind} seeded"));
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        status: JobStatus::Open,
        ..Job::new(
            kind,
            Subject::new("custom", kind),
            kind,
            "emp-david",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        )
        .with_workflow_version(version)
    };
    f.jobs.create_job(&job).await.unwrap();
    let step = Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        spec_slug: Some(slug.into()),
        status: StepStatus::Ready,
        assignee_id: holder.map(str::to_string),
        ..Step::new(job.id, "task", slug, 0)
    };
    f.jobs.add_step(&step).await.unwrap();
    step
}

async fn claim(f: &Fixture, step: &Step, signer: &str) -> (StatusCode, Value) {
    let resp = TestRequest::post(format!("/api/jobs/{}/steps/{}/claim", step.job_id, step.id))
        .as_user(signer, "platform-admin")
        .send(&f.app)
        .await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(Value::Null);
    (resp.status, body)
}

#[tokio::test]
async fn the_claim_door_admits_each_stations_automaton_and_no_one_else() {
    let f = fixture().await;
    // (kind, step, station, the holder it is born with).
    let decided = [
        ("gate-run", "record-verdict", "gates", None),
        (
            "pr-train",
            "converged",
            "converge",
            Some("automation:train-conductor"),
        ),
        ("ship-a-change", "proven", "probe-shed", None),
    ];
    for (kind, slug, name, born) in decided {
        let operator = station(name)
            .automaton
            .unwrap_or_else(|| panic!("{name} names its automaton"));
        let step = packet(&f, kind, slug, born).await;

        for (signer, half, found) in [
            ("agent-claude", "kind", "robot"),
            ("emp-david", "kind", "person"),
        ] {
            let (status, body) = claim(&f, &step, signer).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{signer} on {kind}/{slug}: {body}"
            );
            assert_eq!(body["executor"], format!("equipment:{name}"), "{body}");
            assert_eq!(body["half"], half, "{body}");
            assert_eq!(body["demanded"], "automaton", "{body}");
            assert_eq!(body["found"], found, "{body}");
        }
        // An automaton, but not the one that operates this station.
        let (status, body) = claim(&f, &step, "automation:other").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{kind}/{slug}: {body}");
        assert_eq!(body["half"], "capability", "{body}");
        assert!(
            body["found"].as_str().unwrap_or("").contains(&operator),
            "names who operates {name}: {body}"
        );
        assert_eq!(
            f.jobs.get_step(&step.id).await.unwrap().unwrap().status,
            StepStatus::Ready,
            "no refused claim entered the race"
        );

        let (status, body) = claim(&f, &step, &operator).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{operator} on {kind}/{slug}: {body}"
        );
        let stored = f.jobs.get_step(&step.id).await.unwrap().unwrap();
        assert_eq!(stored.status, StepStatus::Active, "{kind}/{slug}");
        assert_eq!(stored.assignee_id.as_deref(), Some(operator.as_str()));
    }
}
