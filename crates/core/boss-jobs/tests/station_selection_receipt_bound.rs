//! Selection records evidence, not copies of every unrelated packet's prose.
use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
use boss_jobs::port::JobScope;
use boss_jobs::registry::WorkflowSpec;
use boss_jobs::station_claim::{SelectionContext, SelectionReceipt};
use boss_jobs::station_queue::StationPredicate;
use boss_jobs::{StationKind, StationSpec};
use boss_jobs::{StationRegistry, WorkflowRegistry};

fn context() -> (SelectionContext, boss_core::publisher::EventStamp, Step) {
    let stamp = boss_core::publisher::EventStamp::new(
        "jobs",
        boss_core::actor::ActorId::human("emp-worker"),
    );
    let job = Job::new(
        "fixture",
        Subject::new("custom", "receipt"),
        "Receipt",
        "emp-worker",
        Priority::Standard,
        stamp.timestamp.date_naive(),
    );
    let mut step = Step::new(job.id, "task", "Work", 0);
    step.status = StepStatus::Ready;
    let station = StationSpec::draft(
        "fixture",
        "Fixture",
        StationKind::Batch,
        StationPredicate::default(),
        stamp.timestamp,
    );
    let workflows = (0..77)
        .map(|i| {
            let kind = format!("fixture-{i}");
            let mut workflow = WorkflowSpec::platform_seed(
                &kind,
                "Fixture",
                "platform",
                vec!["custom".into()],
                vec![],
            );
            workflow.description = Some("observed registry prose ".repeat(240));
            workflow.created_at = stamp.timestamp;
            (kind, workflow)
        })
        .collect();
    let context = SelectionContext {
        native_authority: None,
        actor_observation: None,
        prepared_policy: None,
        definition_fence: None,
        request_id: uuid::Uuid::new_v4(),
        station,
        scope: JobScope::All,
        active_workflows: workflows,
        validated_job: job,
        validated_step: step.clone(),
    };
    step.status = StepStatus::Active;
    step.assignee_id = Some("emp-worker".into());
    (context, stamp, step)
}

#[test]
fn a_selection_event_fits_the_bus_with_estate_sized_unrelated_inputs() {
    let (context, stamp, step) = context();
    let definitions = serde_json::json!({"workflows": context.active_workflows.values().collect::<Vec<_>>(), "stations": [context.station]});
    let population: Vec<_> = (0..794)
        .map(
            |i| serde_json::json!({"id": i, "metadata": "unrelated packet evidence ".repeat(4000)}),
        )
        .collect();
    let (receipt, events) = context.record_consumed("emp-worker", &stamp, &step,
        serde_json::json!({"version": 1, "definitions": definitions, "open_population": population})).unwrap();
    let size = events
        .iter()
        .map(|event| serde_json::to_vec(event).unwrap().len())
        .max()
        .unwrap();
    println!(
        "estate-size inputs: event_bytes={size} receipt_bytes={}",
        receipt.json().len()
    );
    assert!(
        size < boss_events::outbox::MAX_STAGED_EVENT_BYTES,
        "jobs.station.claimed encodes to {size} bytes, beyond its staging bound"
    );
    assert!(
        receipt.json().len() > 32_768,
        "complete observations must survive rather than being reduced to a digest"
    );
    assert!(
        events.last().unwrap().payload.get("receipt").is_none(),
        "the exact receipt must ride only once"
    );
    assert_eq!(
        SelectionReceipt::from_json(
            boss_jobs::selection_evidence::reconstruct(&replay(&events))
                .unwrap()
                .last()
                .unwrap()
                .payload["receipt_json"]
                .as_str()
                .unwrap()
                .to_owned()
        )
        .unwrap(),
        receipt
    );
    assert_eq!(
        receipt["owner_consumption"]["open_population"]
            .as_array()
            .unwrap()
            .len(),
        794
    );
    assert_eq!(
        receipt["validated"]["workflows"].as_object().unwrap().len(),
        77
    );
}

#[test]
fn complete_owner_inputs_conserve_the_observed_order_and_changed_rows() {
    let (context, stamp, step) = context();
    let record = |rows| {
        context.record_consumed("emp-worker", &stamp, &step,
        serde_json::json!({"version": 1, "definitions": {"workflows": rows, "stations": []}, "open_population": []})).unwrap().0
    };
    let first = record(serde_json::json!([{"id":"b","version":1},{"id":"a","version":1}]));
    let reversed = record(serde_json::json!([{"id":"a","version":1},{"id":"b","version":1}]));
    assert!(
        first["owner_consumption_hash"] != reversed["owner_consumption_hash"],
        "the actual copied observation includes its original row order"
    );
    let changed = record(serde_json::json!([{"id":"a","version":2},{"id":"b","version":1}]));
    assert_ne!(
        first.pointer("/owner_consumption/definitions/workflows"),
        changed.pointer("/owner_consumption/definitions/workflows")
    );
    assert_eq!(
        SelectionReceipt::from_json(first.json().to_owned()).unwrap(),
        first
    );
    let mut corrupt = first.value().clone();
    corrupt["owner_consumption"]["definitions"]["workflows"][0]["id"] =
        serde_json::json!("corrupt");
    assert!(
        SelectionReceipt::from_value(corrupt).is_err(),
        "copied owner evidence must still match its digest"
    );
}

fn replay(events: &[boss_core::event::Event]) -> Vec<boss_events::replay::ReplayEvent> {
    events
        .iter()
        .enumerate()
        .map(|(index, event)| boss_events::replay::ReplayEvent {
            audit_id: index as i64,
            event_id: event.id,
            kind: event.kind.clone(),
            ts: event.timestamp,
            payload: event.payload.clone(),
        })
        .collect()
}

#[test]
fn malformed_chunk_batches_never_reconstruct_as_a_complete_receipt() {
    let (context, stamp, step) = context();
    let (receipt, events) = context
        .record_consumed(
            "emp-worker",
            &stamp,
            &step,
            serde_json::json!({"version":1,"opaque_original":"\"雪\\\n".repeat(60000)}),
        )
        .unwrap();
    let original = replay(&events);
    assert!(original.len() > 2);
    let exact = boss_jobs::selection_evidence::reconstruct(&original).unwrap();
    assert_eq!(
        exact.last().unwrap().payload["receipt_json"]
            .as_str()
            .unwrap()
            .as_bytes(),
        receipt.json().as_bytes()
    );
    for kind in [
        "missing",
        "duplicate",
        "reordered",
        "request",
        "tampered",
        "orphan",
        "manifest-version",
    ] {
        let mut broken = original.clone();
        match kind {
            "missing" => {
                broken.remove(0);
            }
            "duplicate" => {
                broken.insert(1, broken[0].clone());
            }
            "reordered" => broken.swap(0, 1),
            "request" => broken[0].payload["request_id"] = serde_json::json!(uuid::Uuid::new_v4()),
            "tampered" => broken[0].payload["text"] = serde_json::json!("changed"),
            "orphan" => {
                broken.pop();
            }
            "manifest-version" => {
                broken.last_mut().unwrap().payload["receipt_manifest"]["version"] =
                    serde_json::json!(9)
            }
            _ => unreachable!(),
        }
        assert!(
            boss_jobs::selection_evidence::reconstruct(&broken).is_err(),
            "{kind} must fail loudly"
        );
    }
}

#[test]
fn actual_memory_definition_guards_copy_rows_in_native_byte_order() {
    let workflows = boss_jobs::InMemoryWorkflows::new();
    let stations = boss_jobs::InMemoryStations::new();
    let now = chrono::Utc::now();
    for index in (0..64).rev() {
        let name = format!("fixture-{index:03}");
        workflows
            .seed(WorkflowSpec::platform_seed(
                &name,
                "Fixture",
                "platform",
                vec!["custom".into()],
                vec![],
            ))
            .unwrap();
        let mut row = StationSpec::draft(
            &name,
            "Fixture",
            StationKind::Batch,
            StationPredicate::default(),
            now,
        );
        row.version = 1;
        row.status = boss_jobs::registry::WorkflowStatus::Active;
        stations.seed(row).unwrap();
    }
    let workflow = workflows.lock_active_selection_rows().unwrap();
    let station = stations.lock_active_selection_rows().unwrap();
    assert_eq!(workflow.rows().len(), 64);
    assert_eq!(station.rows().len(), 64);
    for names in [
        workflow
            .rows()
            .iter()
            .map(|row| row.kind.clone())
            .collect::<Vec<_>>(),
        station.rows().iter().map(|row| row.name.clone()).collect(),
    ] {
        assert!(
            names.windows(2).all(|pair| pair[0] < pair[1]),
            "actual owner guard returns unordered rows: {names:?}"
        );
    }
}

#[test]
fn a_fresh_budget_helper_is_not_a_production_selection_door() {
    let source = include_str!("../src/native_selection_store.rs");
    let before = source
        .split("pub async fn acquire_selection_for(")
        .next()
        .unwrap();
    assert_eq!(
        before
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap()
            .trim(),
        "#[cfg(any(test, feature = \"test-support\"))]",
        "production selection must carry its original ingress deadline"
    );
}
