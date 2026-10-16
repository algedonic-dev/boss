use super::*;
use crate::moves::{
    Cause, DispatchContext, InMemoryMoves, Move, MovesStore, Snapshot, judged, moves,
};
use serde_json::{Value, json};

fn original() -> (
    Job,
    Vec<Step>,
    Job,
    Vec<Step>,
    Vec<crate::registry::WorkflowSpec>,
) {
    let data: Value =
        serde_json::from_str(include_str!("dispatch/native-registered-review.json")).unwrap();
    let run = serde_json::from_value(data["run"].clone()).unwrap();
    let run_steps = serde_json::from_value(data["run"]["steps"].clone()).unwrap();
    let parent = serde_json::from_value(data["parent"].clone()).unwrap();
    let parent_steps = serde_json::from_value(data["parent"]["steps"].clone()).unwrap();
    let mut specs =
        crate::seed_loader::load_workflows(crate::registry::platform_bundle_path()).unwrap();
    let native: crate::registry::WorkflowSpec =
        serde_json::from_str(include_str!("dispatch/native-workflow-routing.json")).unwrap();
    specs.retain(|spec| spec.kind != native.kind);
    specs.push(native);
    (run, run_steps, parent, parent_steps, specs)
}

fn time() -> crate::moves::Instant {
    chrono::DateTime::parse_from_rfc3339("2026-10-14T17:05:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn snapshot(
    run: Job,
    run_steps: Vec<Step>,
    parent: Job,
    parent_steps: Vec<Step>,
    specs: &[crate::registry::WorkflowSpec],
) -> Snapshot {
    let status = crate::yard::build_status_for(
        crate::yard::YardInputs::default(),
        crate::yard::Reading::Read,
        crate::yard::BoardingReadings::default(),
    );
    let cars = [(parent, parent_steps)];
    let runs = [(run, run_steps)];
    Snapshot::of_with_registry(
        &crate::regions::RegionInputs {
            internal_protocols: None,
            status: &status,
            dock_reading: crate::yard::Reading::Read,
            open_trains: &[],
            closed_trains: &[],
            cars: &cars,
            gate_runs: &[],
            inbound: Some(&[]),
            stations: Some(&[]),
            conductor: None,
            ops_requests: Some(&[]),
            agent_runs: Some(&runs),
            sessions: Some(&[]),
            publish_packets: Some(&[]),
            run_capacity: None,
            runner_hosts: Some(&[]),
            predecessors: &[],
            now: time(),
            window_hours: 24,
        },
        specs,
    )
}

fn native_move() -> (Move, crate::routes::RouteMap) {
    let (run, run_steps, parent, parent_steps, specs) = original();
    let run_id = run.id.to_string();
    let parent_id = parent.id.to_string();
    let next = snapshot(run, run_steps, parent, parent_steps, &specs);
    assert_eq!(next.places.get(&run_id), Some(&"shop-floor"));
    assert_eq!(next.places.get(&parent_id), Some(&"shed"));
    let mut previous = next.clone();
    previous.places.remove(&run_id);
    previous.facts.remove(&run_id);
    previous.dispatch.remove(&run_id);
    let found = moves(
        &previous,
        &next,
        &[Cause {
            seq: 1929000,
            event_id: "9dc67546-131f-499f-bc44-e346571f1e6f".parse().unwrap(),
            at: chrono::DateTime::parse_from_rfc3339("2026-10-14T17:02:53.657445Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind: "step.assigned.task".into(),
            packet: parent_id.clone(),
        }],
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].packet, run_id);
    assert_eq!(found[0].handoff_from.as_deref(), Some(parent_id.as_str()));
    let stations =
        crate::seed_loader::load_stations(crate::station_seed::platform_stations_path()).unwrap();
    let map = crate::routes::derive_from(&specs, &stations, None, time());
    (found.into_iter().next().unwrap(), map)
}

#[test]
fn original_native_registered_landed_review_is_judged_from_its_own_context() {
    let (m, map) = native_move();
    assert!(matches!(
        m.dispatch,
        Some(DispatchContext::Registered { .. })
    ));
    assert_eq!(judged(vec![m], &map)[0].declared, Some(true));
}

#[test]
fn forged_unregistered_human_only_and_refused_originals_do_not_pass() {
    for case in 0..6 {
        let (mut run, mut run_steps, parent, mut parent_steps, specs) = original();
        let active = specs.iter().find(|s| s.kind == parent.kind).unwrap();
        let step = parent_steps
            .iter_mut()
            .find(|s| s.spec_slug.as_deref() == Some("landed-review"))
            .unwrap();
        match case {
            0 => step.metadata["agent_run"] = json!("00000000-0000-0000-0000-000000000001"),
            1 => run.metadata["step"] = json!("unregistered-step"),
            2 => step.metadata["human_only"] = json!(true),
            3 => step.metadata["agent_model"] = json!("unpriced-control"),
            4 => {
                run_steps
                    .iter_mut()
                    .find(|s| s.spec_slug.as_deref() == Some("claimed"))
                    .unwrap()
                    .metadata["trigger_name"] = json!("forged-trigger")
            }
            _ => {
                run_steps
                    .iter_mut()
                    .find(|s| s.spec_slug.as_deref() == Some("refused"))
                    .unwrap()
                    .status = StepStatus::Completed
            }
        }
        assert!(
            matches!(
                dispatch_context(&run, &run_steps, &parent, &parent_steps, Some(active)),
                Some(DispatchContext::Refused { .. })
            ),
            "case{case}"
        );
    }
}

#[test]
fn missing_historical_context_and_unwalked_versions_are_unknown() {
    let (m, map) = native_move();
    let mut absent = m.clone();
    absent.dispatch = None;
    assert_eq!(judged(vec![absent], &map)[0].declared, None);
    let mut old = m;
    if let Some(DispatchContext::Registered { source_version, .. }) = old.dispatch.as_mut() {
        *source_version += 100;
    }
    assert_eq!(judged(vec![old], &map)[0].declared, None);
}

#[test]
fn a_later_claim_cannot_rewrite_the_original_snapshot_registration() {
    let (run, run_steps, parent, mut parent_steps, specs) = original();
    let run_id = run.id.to_string();
    let first = snapshot(
        run.clone(),
        run_steps.clone(),
        parent.clone(),
        parent_steps.clone(),
        &specs,
    );
    let original_context = first.dispatch.get(&run_id).cloned();
    assert!(matches!(
        original_context,
        Some(DispatchContext::Registered { .. })
    ));
    parent_steps
        .iter_mut()
        .find(|step| step.spec_slug.as_deref() == Some("landed-review"))
        .unwrap()
        .metadata["agent_run"] = json!("a-different-run");
    let later = snapshot(run, run_steps, parent, parent_steps, &specs);
    assert!(matches!(
        later.dispatch.get(&run_id),
        Some(DispatchContext::Refused { .. })
    ));
    assert_eq!(first.dispatch.get(&run_id), original_context.as_ref());
}

#[tokio::test]
async fn an_existing_run_moves_without_borrowing_its_initial_handoff_context() {
    let (run, run_steps, parent, parent_steps, specs) = original();
    let id = run.id.to_string();
    let next = snapshot(run, run_steps, parent, parent_steps, &specs);
    let mut previous = next.clone();
    previous.places.insert(id.clone(), "marshalling");
    let found = moves(
        &previous,
        &next,
        &[Cause {
            seq: 1929001,
            event_id: uuid::Uuid::from_u128(15),
            at: time(),
            kind: "jobs.job.updated".into(),
            packet: id,
        }],
    );
    assert_eq!(found.len(), 1);
    assert!(found[0].handoff_from.is_none());
    assert!(found[0].dispatch.is_none());
    assert_eq!(InMemoryMoves::new().record(&found).await.unwrap(), 1);
}

#[tokio::test]
async fn original_context_survives_storage_grouping_and_replay_without_blessing_unknowns() {
    let (m, map) = native_move();
    let mut unknown = m.clone();
    unknown.cause_event_id = uuid::Uuid::from_u128(11);
    unknown.dispatch = None;
    let mut refused = m.clone();
    refused.cause_event_id = uuid::Uuid::from_u128(12);
    refused.dispatch = Some(DispatchContext::Refused {
        why: "original backlink disagrees".into(),
    });
    let original = vec![m, unknown, refused];
    let store = InMemoryMoves::new();
    assert_eq!(store.record(&original).await.unwrap(), 3);
    assert_eq!(store.record(&original).await.unwrap(), 0);
    let read = store.since(0, 100).await.unwrap();
    assert_eq!(
        read.iter().map(|r| r.r#move.clone()).collect::<Vec<_>>(),
        original
    );
    let counts = store
        .crossings(time() - chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(counts.len(), 3);
    assert_eq!(counts.iter().map(|c| c.moves).sum::<i64>(), 3);
    let mut judgments: Vec<_> = counts.iter().map(|c| map.judges_observation(c)).collect();
    judgments.sort();
    assert_eq!(judgments, [None, Some(false), Some(true)]);
    let mut misbound = original[0].clone();
    misbound.packet = uuid::Uuid::from_u128(13).to_string();
    assert_eq!(
        judged(vec![misbound.clone()], &map)[0].declared,
        Some(false)
    );
    assert!(store.record(&[misbound]).await.is_err());
    assert_eq!(store.latest_seq().await.unwrap(), 3);
}
