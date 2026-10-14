use super::diagnostics::*;
use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
struct Memory {
    intent: Option<Intent>,
    launch: Option<Launch>,
    receipt: Option<Receipt>,
    effects: usize,
    finishes: usize,
    lost_ack: bool,
    fail_retention: bool,
    fail_finish: bool,
}

struct Port {
    inputs: Inputs,
    state: Mutex<Memory>,
    observation: Observation,
}

impl Port {
    fn new() -> Self {
        let nonce = Uuid::from_u128(1);
        let inputs = Inputs {
            request: Request {
                car: Uuid::from_u128(2),
                candidate: "a".repeat(40),
                baseline: "b".repeat(40),
                receipt_sha256: "c".repeat(64),
            },
            admitted_original: json!({"full":"original admitted request"}),
            pinned_protocol: json!({"version":1,"operation":"isolated-consist"}),
            current_car: json!({"hold":"retain original hold"}),
            car_binding: json!({"hold":"retain original hold"}),
            policy: json!({"version":1}),
            edit_level: json!({"level":"source"}),
            required_checks: vec!["ordinary-check".into()],
            expected_executor_identity: super::consist_job::CONTROLLER_IDENTITY.into(),
            observed_cluster_identity: super::consist_job::CONTROLLER_IDENTITY.into(),
        };
        let observation = Observation {
            job_uid: "native-job-original".into(), pod_uid: "native-pod-original".into(),
            original_output: json!({"stdout":"complete original output", "stderr":"full diagnostic"}),
            original_receipt: json!({"version":1,"identity":format!("consist-{}",nonce.simple()),
                "head":"d".repeat(40),"baseline":"b".repeat(40),"exclusions":{"exit":0},
                "runs":[{"name":"ordinary-check","exit":0,"stdout":"entire successful output","stderr":"entire stderr"}],
                "warnings":[]}).to_string(),
            init_exit: Some(0), worker_exit: Some(0), observer_refusal: None,
        };
        Self {
            inputs,
            state: Mutex::default(),
            observation,
        }
    }
}

#[async_trait]
impl Door for Port {
    async fn inputs(&self) -> Result<Inputs> {
        Ok(self.inputs.clone())
    }
    async fn claim(&self) -> Result<()> {
        Ok(())
    }
    async fn first_intent(&self, proposed: &Intent) -> Result<FirstIntent> {
        let mut state = self.state.lock().unwrap();
        if let Some(old) = &state.intent {
            return Ok(FirstIntent::Replayed(old.clone()));
        }
        state.intent = Some(proposed.clone());
        Ok(FirstIntent::Recorded(proposed.clone()))
    }
    async fn launch_record(&self) -> Result<Option<Launch>> {
        Ok(self.state.lock().unwrap().launch.clone())
    }
    async fn receipt_record(&self) -> Result<Option<Receipt>> {
        Ok(self.state.lock().unwrap().receipt.clone())
    }
    async fn launch(&self, intent: &Intent) -> Result<Launch> {
        let mut state = self.state.lock().unwrap();
        state.effects += 1;
        assert_eq!(
            state.intent.as_ref(),
            Some(intent),
            "intent precedes the compute effect"
        );
        if state.lost_ack {
            bail!("native create acknowledgement lost; original observation retained");
        }
        Ok(Launch {
            job_uid: "native-job-original".into(),
            assembled: "d".repeat(40),
            manifest: json!({"trusted":"complete fixed manifest"}),
            original_output: json!({"native":"whole create response"}),
        })
    }
    async fn keep_launch(&self, launch: &Launch) -> Result<()> {
        self.state.lock().unwrap().launch = Some(launch.clone());
        Ok(())
    }
    async fn observe(&self, _intent: &Intent, _launch: &Launch) -> Result<Option<Observation>> {
        Ok(Some(self.observation.clone()))
    }
    async fn keep_receipt(&self, receipt: &Receipt) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.fail_retention {
            bail!("durable artifact store unavailable");
        }
        state.receipt = Some(receipt.clone());
        Ok(())
    }
    async fn finish(&self, receipt: &Receipt) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        assert_eq!(
            state.receipt.as_ref(),
            Some(receipt),
            "original receipt precedes completion"
        );
        if state.fail_finish {
            bail!("completion acknowledgement lost");
        }
        state.finishes += 1;
        Ok(())
    }
}

#[tokio::test]
async fn unknown_first_request_reconciles_known_launch_without_admitting_fresh_compute() {
    let unknown = Port::new();
    unknown.state.lock().unwrap().lost_ack = true;
    assert!(advance(&unknown, Uuid::from_u128(1)).await.is_err());
    let known = Port::new();
    known.state.lock().unwrap().fail_retention = true;
    assert!(advance(&known, Uuid::from_u128(1)).await.is_err());
    known.state.lock().unwrap().fail_retention = false;
    let ready = Port::new();
    let requests: Vec<_> = (1..=3)
        .map(|id| json!({"id":Uuid::from_u128(id)}))
        .collect();
    let error = super::diagnostic_adapter::advance_requests(requests, |selected, may_launch| {
        let port = match selected["id"].as_str().unwrap() {
            id if id == Uuid::from_u128(1).to_string() => &unknown,
            id if id == Uuid::from_u128(2).to_string() => &known,
            _ => &ready,
        };
        async move {
            let fresh = port.state.lock().unwrap().intent.is_none();
            super::diagnostic_adapter::launch_admission(fresh, may_launch)?;
            advance(port, Uuid::from_u128(1)).await
        }
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("fresh launch deferred"));
    let unknown = unknown.state.lock().unwrap();
    assert_eq!(unknown.effects, 1, "ambiguous create is never repeated");
    assert_eq!(unknown.finishes, 0);
    assert!(unknown.launch.is_none() && unknown.receipt.is_none());
    let known = known.state.lock().unwrap();
    assert_eq!(known.effects, 1, "reconcile the acknowledged incarnation");
    assert_eq!(known.finishes, 1);
    assert_eq!(
        known.receipt.as_ref().unwrap().outcome,
        Outcome::RequiredChecksPassed
    );
    let ready = ready.state.lock().unwrap();
    assert_eq!(ready.effects, 0);
    assert!(ready.intent.is_none() && ready.receipt.is_none());
}

#[tokio::test]
async fn an_ordinary_queue_pass_admits_one_fresh_effect_and_preserves_the_next_request() {
    let first = Port::new();
    let next = Port::new();
    let requests = vec![
        json!({"id":Uuid::from_u128(1)}),
        json!({"id":Uuid::from_u128(2)}),
    ];
    let error = super::diagnostic_adapter::advance_requests(requests, |selected, may_launch| {
        let port = if selected["id"] == Uuid::from_u128(1).to_string() {
            &first
        } else {
            &next
        };
        async move {
            let fresh = port.state.lock().unwrap().intent.is_none();
            super::diagnostic_adapter::launch_admission(fresh, may_launch)?;
            advance(port, Uuid::from_u128(1)).await
        }
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("fresh launch deferred"));
    assert_eq!(first.state.lock().unwrap().effects, 1);
    assert_eq!(first.state.lock().unwrap().finishes, 1);
    assert_eq!(next.state.lock().unwrap().effects, 0);
    assert!(next.state.lock().unwrap().intent.is_none());
}

#[tokio::test]
async fn request_receipt_conserves_originals_and_replays_without_compute() {
    let port = Port::new();
    let first = advance(&port, Uuid::from_u128(1)).await.unwrap();
    let Advance::Complete(first) = first else {
        panic!("complete original evidence expected")
    };
    assert_eq!(first.observation, port.observation);
    assert_eq!(first.outcome, Outcome::RequiredChecksPassed);
    let Advance::Complete(replayed) = advance(&port, Uuid::from_u128(999)).await.unwrap() else {
        panic!("replay")
    };
    assert_eq!(first, replayed);
    assert_eq!(port.state.lock().unwrap().effects, 1);
    assert_eq!(
        first.intent.inputs.current_car,
        json!({"hold":"retain original hold"})
    );
}

#[tokio::test]
async fn unknown_create_is_not_retried_or_fabricated_as_a_receipt() {
    let port = Port::new();
    port.state.lock().unwrap().lost_ack = true;
    assert!(advance(&port, Uuid::from_u128(1)).await.is_err());
    let Advance::Unknown(reason) = advance(&port, Uuid::from_u128(2)).await.unwrap() else {
        panic!("unknown must remain visible")
    };
    assert!(reason.contains("never recreate compute"));
    let state = port.state.lock().unwrap();
    assert_eq!(state.effects, 1);
    assert_eq!(state.finishes, 0);
    assert!(state.receipt.is_none());
}

#[tokio::test]
async fn retention_outage_blocks_completion_then_resumes_same_known_incarnation() {
    let port = Port::new();
    port.state.lock().unwrap().fail_retention = true;
    assert!(advance(&port, Uuid::from_u128(1)).await.is_err());
    assert_eq!(port.state.lock().unwrap().finishes, 0);
    port.state.lock().unwrap().fail_retention = false;
    assert!(matches!(
        advance(&port, Uuid::from_u128(2)).await.unwrap(),
        Advance::Complete(_)
    ));
    assert_eq!(port.state.lock().unwrap().effects, 1);
}

#[tokio::test]
async fn completion_outage_replays_the_stored_original_receipt() {
    let port = Port::new();
    port.state.lock().unwrap().fail_finish = true;
    assert!(advance(&port, Uuid::from_u128(1)).await.is_err());
    let retained = port.state.lock().unwrap().receipt.clone().unwrap();
    port.state.lock().unwrap().fail_finish = false;
    let Advance::Complete(replayed) = advance(&port, Uuid::from_u128(2)).await.unwrap() else {
        panic!("replay")
    };
    assert_eq!(retained, *replayed);
    assert_eq!(port.state.lock().unwrap().effects, 1);
}

#[tokio::test]
async fn ordinary_car_progress_does_not_strand_a_known_receipt_or_erase_its_original_copy() {
    let mut port = Port::new();
    let Advance::Complete(original) = advance(&port, Uuid::from_u128(1)).await.unwrap() else {
        panic!("original absent")
    };
    port.inputs.current_car["ordinary_dock_progress"] = json!({"left_behind":3});
    let Advance::Complete(replayed) = advance(&port, Uuid::from_u128(2)).await.unwrap() else {
        panic!("replay absent")
    };
    assert_eq!(replayed, original);
    assert_eq!(port.state.lock().unwrap().effects, 1);
    assert!(
        replayed
            .intent
            .inputs
            .current_car
            .get("ordinary_dock_progress")
            .is_none()
    );
    port.inputs.car_binding["hold"] = json!("changed authority binding");
    assert!(advance(&port, Uuid::from_u128(3)).await.is_err());
    assert_eq!(port.state.lock().unwrap().effects, 1);
}

#[tokio::test]
async fn publishing_a_successor_does_not_repin_or_strand_the_original_protocol_receipt() {
    let mut port = Port::new();
    port.inputs.pinned_protocol["status"] = json!("active");
    let Advance::Complete(original) = advance(&port, Uuid::from_u128(1)).await.unwrap() else {
        panic!("original absent")
    };
    port.inputs.pinned_protocol["status"] = json!("retired");
    let Advance::Complete(replayed) = advance(&port, Uuid::from_u128(2)).await.unwrap() else {
        panic!("replay absent")
    };
    assert_eq!(replayed, original);
    assert_eq!(replayed.intent.inputs.pinned_protocol["status"], "active");
    port.inputs.pinned_protocol["version"] = json!(2);
    assert!(advance(&port, Uuid::from_u128(3)).await.is_err());
    assert_eq!(port.state.lock().unwrap().effects, 1);
}

#[tokio::test]
async fn namespace_identity_and_changed_inputs_refuse_before_new_effects() {
    let mut port = Port::new();
    port.inputs.observed_cluster_identity = "system:serviceaccount:boss-dev:dev-session".into();
    assert!(advance(&port, Uuid::from_u128(1)).await.is_err());
    assert_eq!(port.state.lock().unwrap().effects, 0);
    port.inputs.observed_cluster_identity = super::consist_job::CONTROLLER_IDENTITY.into();
    advance(&port, Uuid::from_u128(1)).await.unwrap();
    port.inputs.request.baseline = "e".repeat(40);
    assert!(advance(&port, Uuid::from_u128(2)).await.is_err());
    assert_eq!(port.state.lock().unwrap().effects, 1);
}

#[tokio::test]
async fn execution_failures_and_observer_unknown_are_not_candidate_failures() {
    let mut port = Port::new();
    port.observation.init_exit = Some(78);
    port.observation.worker_exit = None;
    port.observation.original_receipt.clear();
    let Advance::Complete(receipt) = advance(&port, Uuid::from_u128(1)).await.unwrap() else {
        panic!("retained failure")
    };
    assert_eq!(receipt.outcome, Outcome::ExecutionFailed);
    let mut port = Port::new();
    port.observation.observer_refusal = Some("original observer timeout".into());
    port.observation.original_receipt.clear();
    let Advance::Complete(receipt) = advance(&port, Uuid::from_u128(1)).await.unwrap() else {
        panic!("retained unknown")
    };
    assert_eq!(receipt.outcome, Outcome::ObserverUnknown);
}

#[tokio::test]
async fn known_init_failure_remains_execution_failure_when_worker_never_produced_logs() {
    let mut port = Port::new();
    port.observation.init_exit = Some(78);
    port.observation.worker_exit = None;
    port.observation.observer_refusal =
        Some("worker never started; full log request refusal retained".into());
    let Advance::Complete(receipt) = advance(&port, Uuid::from_u128(1)).await.unwrap() else {
        panic!("no retained receipt");
    };
    assert_eq!(receipt.outcome, Outcome::ExecutionFailed);
    assert!(receipt.observation.observer_refusal.is_some());
}

#[tokio::test]
async fn required_check_absence_and_uid_or_tree_mismatch_never_pass() {
    for mode in ["absent", "uid", "tree", "baseline", "missing-exit"] {
        let mut port = Port::new();
        let mut raw: serde_json::Value =
            serde_json::from_str(&port.observation.original_receipt).unwrap();
        match mode {
            "absent" => raw["runs"] = json!([]),
            "uid" => port.observation.job_uid = "other-job".into(),
            "tree" => raw["head"] = json!("e".repeat(40)),
            "baseline" => raw["baseline"] = json!("e".repeat(40)),
            "missing-exit" => raw["runs"][0]["exit"] = serde_json::Value::Null,
            _ => unreachable!(),
        }
        port.observation.original_receipt = raw.to_string();
        let result = advance(&port, Uuid::from_u128(1)).await;
        if mode == "absent" {
            let Advance::Complete(receipt) = result.unwrap() else {
                panic!("incomplete")
            };
            assert_eq!(receipt.outcome, Outcome::Incomplete);
        } else {
            assert!(result.is_err(), "{mode}");
        }
    }
}
#[test]
fn registered_diagnostic_protocol_uses_owning_loader_and_actor_projection() {
    let specs = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../../infra/platform/workflows/diagnostic-request.toml"),
        "platform",
        "diagnostic-request.toml",
    )
    .unwrap();
    assert_eq!(specs.len(), 1);
    let execute = specs[0]
        .steps
        .iter()
        .find(|step| step.title == "execute")
        .unwrap();
    assert_eq!(
        execute.selectors().assignee_id.as_deref(),
        Some(super::ACTOR)
    );
    assert!(execute.selectors().authority_role.is_none());
    assert_eq!(
        execute.metadata_defaults["diagnostic_operation"]["capacity"],
        1
    );
    assert_eq!(
        execute.metadata_defaults["diagnostic_operation"]["required_checks"],
        serde_json::json!(["alerts-are-packets"])
    );
    let request = json!({"car":Uuid::new_v4(),"candidate":"a".repeat(40),"baseline":"b".repeat(40),"receipt_sha256":"c".repeat(64)});
    let steps = boss_jobs::registry::materialize_steps(
        &specs[0],
        &boss_core::job::Subject::new("custom", "ordinary-fixture"),
        boss_core::job::JobId::new(),
        &json!({"request":request,"diagnostic_executor":super::ACTOR}),
        boss_core::job::StepId::new,
    );
    let execute = steps
        .iter()
        .find(|step| step.spec_slug.as_deref() == Some("execute"))
        .unwrap();
    assert_eq!(execute.metadata["request"], request);
    assert_eq!(execute.assignee_id.as_deref(), Some(super::ACTOR));
}
