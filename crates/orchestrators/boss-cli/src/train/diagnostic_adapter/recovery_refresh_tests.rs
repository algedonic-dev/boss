//! Owning refresh/HTTP/immutable-record fixture, without a cluster credential.
use super::*;
use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
use boss_jobs::JobsRepository;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn active_reconcile_tick_services_retained_recovery_without_ordinary_admission() {
    if in_private_child(
        "active_reconcile_tick_services_retained_recovery_without_ordinary_admission",
    ) {
        return;
    }
    let fixture = Fixture::new(None).await;
    let before = fixture.original_bytes().await;
    let trains = fixture
        .conductor
        .api(Method::GET, "/api/jobs?kind=pr-train&status=open", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trains["data"][0]["id"], "occupied-fixture-train");
    assert_eq!(trains["data"][0]["steps"][0]["status"], "ready");
    fixture.calls.lock().unwrap().clear();
    let work = super::super::entry::loop_work(&super::super::entry::Phase::Reconcile);
    // Walk the actual dispatch table, not Refresh: its cadence is retired.
    // Other phases need forge/native fixtures; this fixture executes the
    // exact evidence-only work selected by the standing reconcile tick.
    for item in work {
        if *item == super::super::entry::Work::DrainRecoveryAssessments {
            super::super::entry::execute_work(&fixture.conductor, *item, false, chrono::Utc::now())
                .await
                .unwrap();
        }
    }
    assert_eq!(
        fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()[0]
            .status,
        StepStatus::Completed,
        "active reconcile dispatch starves recovery"
    );
    assert_eq!(fixture.original_bytes().await, before);
    for id in &fixture.ordinary {
        assert_eq!(
            fixture.jobs.list_steps(id).await.unwrap()[0].status,
            StepStatus::Ready
        );
        assert!(
            !fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.contains(&format!("/api/jobs/{id}")))
        );
    }
    assert_eq!(
        fixture
            .jobs
            .get_job(&fixture.car.id)
            .await
            .unwrap()
            .unwrap()
            .metadata,
        fixture.car.metadata
    );
    assert!(!PathBuf::from(&fixture.conductor.cfg.clone).exists());
}

#[tokio::test]
async fn active_reconcile_recovery_refusal_and_dark_read_preserve_unknown_and_other_work() {
    if in_private_child(
        "active_reconcile_recovery_refusal_and_dark_read_preserve_unknown_and_other_work",
    ) {
        return;
    }
    for fault in ["journal", "dark-list"] {
        let fixture = Fixture::new(Some(fault)).await;
        let before = fixture.original_bytes().await;
        let work = super::super::entry::loop_work(&super::super::entry::Phase::Reconcile);
        let mut reached_recovery = false;
        let mut reached_gate_queue = false;
        let mut reconciled = false;
        for item in work {
            match item {
                super::super::entry::Work::Reconcile => reconciled = true,
                super::super::entry::Work::DrainRecoveryAssessments => {
                    assert!(reconciled, "recovery must not delay train reconciliation");
                    super::super::entry::execute_work(
                        &fixture.conductor,
                        *item,
                        false,
                        chrono::Utc::now(),
                    )
                    .await
                    .unwrap();
                    reached_recovery = true;
                }
                super::super::entry::Work::GateQueue => reached_gate_queue = true,
                _ => {}
            }
        }
        assert!(
            reached_recovery && reached_gate_queue,
            "{fault}: actual tick omitted work"
        );
        assert_ne!(
            fixture
                .jobs
                .list_steps(&fixture.assessment.id)
                .await
                .unwrap()[0]
                .status,
            StepStatus::Completed
        );
        assert_eq!(fixture.original_bytes().await, before);
        assert_eq!(
            fixture
                .jobs
                .get_job(&fixture.car.id)
                .await
                .unwrap()
                .unwrap()
                .metadata,
            fixture.car.metadata
        );
        assert!(!PathBuf::from(&fixture.conductor.cfg.clone).exists());
    }
}

#[tokio::test]
async fn dry_active_reconcile_recovery_neither_claims_nor_reads_private_originals() {
    if in_private_child("dry_active_reconcile_recovery_neither_claims_nor_reads_private_originals")
    {
        return;
    }
    let mut fixture = Fixture::new(None).await;
    fixture.conductor.cfg.dry = true;
    let before = fixture.original_bytes().await;
    let events = fixture.jobs.recorded_events().len();
    fixture.calls.lock().unwrap().clear();
    for item in super::super::entry::loop_work(&super::super::entry::Phase::Reconcile) {
        if *item == super::super::entry::Work::DrainRecoveryAssessments {
            super::super::entry::execute_work(&fixture.conductor, *item, true, chrono::Utc::now())
                .await
                .unwrap();
        }
    }
    assert_eq!(
        fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()[0]
            .status,
        StepStatus::Ready
    );
    assert_eq!(fixture.original_bytes().await, before);
    assert_eq!(fixture.jobs.recorded_events().len(), events);
    let calls = fixture.calls.lock().unwrap();
    assert!(!calls.is_empty(), "actual scheduled drain was not reached");
    assert!(calls.iter().all(|s| s.starts_with("GET ")
        && !s.contains("/records/")
        && !s.contains("/api/workflows/")));
}

fn in_private_child(name: &str) -> bool {
    if std::env::var_os("BOSS_RECOVERY_REFRESH_FIXTURE").is_some() {
        return false;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("train::diagnostic_adapter::recovery_refresh_tests::{name}"),
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home.path())
        .env("BOSS_ACTOR", ACTOR)
        .env("BOSS_RECOVERY_REFRESH_FIXTURE", "child")
        .env("BOSS_MACHINE_TOKEN_DIR", home.path().join("absent-token"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

#[tokio::test]
async fn occupied_track_completes_only_the_authorized_retained_assessment() {
    if std::env::var_os("BOSS_RECOVERY_REFRESH_FIXTURE").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "train::diagnostic_adapter::recovery_refresh_tests::occupied_track_completes_only_the_authorized_retained_assessment", "--nocapture"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", home.path())
            .env("BOSS_ACTOR", ACTOR)
            .env("BOSS_RECOVERY_REFRESH_FIXTURE", "child")
            .env("BOSS_MACHINE_TOKEN_DIR", home.path().join("absent-token"))
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = Fixture::new(None).await;
    let before = fixture.original_bytes().await;
    let events = fixture.jobs.recorded_events().len();
    let list = fixture
        .conductor
        .api(Method::GET, "/api/jobs?status=open", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        list["total"], 5,
        "fixture queue must contain actual open original, two ordinary requests, assessment and held car"
    );
    fixture.calls.lock().unwrap().clear();
    fixture.conductor.refresh().await.unwrap();
    let step = fixture
        .jobs
        .list_steps(&fixture.assessment.id)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        step.status,
        StepStatus::Completed,
        "occupied track starved a separately authorized recovery assessment; calls={:?}",
        fixture.calls.lock().unwrap()
    );
    assert_eq!(step.metadata["outcome"], "recovered-refusal");
    assert_eq!(
        fixture.original_bytes().await,
        before,
        "original diagnostic or its journal changed"
    );
    assert!(fixture.jobs.recorded_events().len() > events);
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("/api/workflows/diagnostic-recovery-assessment/versions/1")),
        "did not validate the actual pinned published authority"
    );
    for id in &fixture.ordinary {
        assert!(
            !fixture
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|s| s.contains(&format!("/api/jobs/{id}"))),
            "busy recovery hydrated an unrelated ordinary diagnostic"
        );
        let steps = fixture.jobs.list_steps(id).await.unwrap();
        assert_eq!(steps[0].status, StepStatus::Ready);
        assert!(steps[0].metadata.get("records").is_none());
    }
    assert!(
        !PathBuf::from(&fixture.conductor.cfg.clone).exists(),
        "busy recovery assembled or fetched a clone"
    );
    fixture.assert_car_hold_preserved().await;
    let full = fixture
        .conductor
        .get_job(&fixture.assessment.id.to_string())
        .await
        .unwrap();
    let completed = find_step(&full, "execute").unwrap().clone();
    let door = NativeDoor {
        conductor: &fixture.conductor,
        job: full,
        step: completed,
        directory: PathBuf::from(&fixture.conductor.cfg.diagnostics_home)
            .join(fixture.assessment.id.to_string()),
        actor: ACTOR.into(),
        content: crate::tenant_publish::service_on_door(&fixture.conductor.cfg.jobs, "content")
            .unwrap(),
        may_launch: false,
    };
    let before_replay = fixture.jobs.recorded_events().len();
    assert!(matches!(
        recovery::advance(&door).await.unwrap(),
        Advance::Assessed
    ));
    assert_eq!(
        fixture.jobs.recorded_events().len(),
        before_replay,
        "completed replay appended another disposition"
    );
    assert_eq!(fixture.original_bytes().await, before);
}

#[tokio::test]
async fn busy_recovery_refuses_conflicting_originals_without_changing_the_subject() {
    if in_private_child("busy_recovery_refuses_conflicting_originals_without_changing_the_subject")
    {
        return;
    }
    for fault in [
        "protocol",
        "authority",
        "admission",
        "request",
        "journal",
        "identity",
        "native-job",
        "native-pod",
        "native-tail",
        "native-command",
        "outcome",
        "authority-decision",
        "authority-actor",
        "authority-digest",
    ] {
        let fixture = Fixture::new(Some(fault)).await;
        let before = fixture.original_bytes().await;
        fixture.calls.lock().unwrap().clear();
        fixture.conductor.refresh().await.unwrap();
        let step = fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()
            .remove(0);
        assert_ne!(
            step.status,
            StepStatus::Completed,
            "{fault}: false recovered refusal"
        );
        assert_eq!(
            fixture.original_bytes().await,
            before,
            "{fault}: original changed"
        );
        {
            let calls = fixture.calls.lock().unwrap();
            assert!(
                calls
                    .iter()
                    .any(|s| s.contains("/api/workflows/diagnostic-recovery-assessment/versions/1")),
                "{fault}: recovery was not actually reached"
            );
            assert!(
                calls.iter().any(|s| s.contains("kind=ship-a-change")),
                "{fault}: recovery refusal skipped dock claim withdrawal"
            );
        }
        fixture.assert_car_hold_preserved().await;
    }
}

#[tokio::test]
async fn dry_occupied_refresh_keeps_assessment_ready_without_claim_or_native_read() {
    if in_private_child("dry_occupied_refresh_keeps_assessment_ready_without_claim_or_native_read")
    {
        return;
    }
    let mut fixture = Fixture::new(None).await;
    fixture.conductor.cfg.dry = true;
    let before = fixture.original_bytes().await;
    let events = fixture.jobs.recorded_events().len();
    fixture.calls.lock().unwrap().clear();
    drain_recovery_assessments(&fixture.conductor)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()[0]
            .status,
        StepStatus::Ready
    );
    assert_eq!(fixture.original_bytes().await, before);
    assert_eq!(fixture.jobs.recorded_events().len(), events);
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.starts_with("GET "))
    );
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("/records/") || s.contains("/api/workflows/"))
    );
}

#[tokio::test]
async fn a_spoofed_busy_handler_cannot_fall_through_to_ordinary_execution() {
    if in_private_child("a_spoofed_busy_handler_cannot_fall_through_to_ordinary_execution") {
        return;
    }
    let fixture = Fixture::new(Some("handler")).await;
    let before = fixture.original_bytes().await;
    fixture.calls.lock().unwrap().clear();
    fixture.conductor.refresh().await.unwrap();
    assert_eq!(
        fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()[0]
            .status,
        StepStatus::Ready
    );
    assert_eq!(fixture.original_bytes().await, before);
    {
        let calls = fixture.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|s| s.contains(&fixture.assessment.id.to_string()))
        );
        assert!(
            !calls
                .iter()
                .any(|s| s.contains("/records/") || s.contains("/api/workflows/")),
            "spoofed handler reached private recovery or ordinary intent reads"
        );
    }
    fixture.assert_car_hold_preserved().await;
}

struct Fixture {
    conductor: Conductor,
    jobs: Arc<boss_jobs::InMemoryJobs>,
    assessment: Job,
    original: Job,
    original_directory: PathBuf,
    calls: Arc<Mutex<Vec<String>>>,
    ordinary: Vec<boss_core::job::JobId>,
    car: Job,
    _root: tempfile::TempDir,
}

#[tokio::test]
async fn oversized_optional_recovery_keeps_ordinary_gate_recording_alive() {
    use super::super::carried_verdict::{Finished, GateCluster, GateJob, GatePod};
    use std::os::unix::fs::PermissionsExt;
    if in_private_child("oversized_optional_recovery_keeps_ordinary_gate_recording_alive") {
        return;
    }
    let mut fixture = Fixture::new(None).await;
    let original = fixture.original_bytes().await;
    let directory = PathBuf::from(&fixture.conductor.cfg.diagnostics_home)
        .join(fixture.assessment.id.to_string());
    let large = directory.join("retained-large-original");
    let bytes = vec![0xff; 16 * 1024 * 1024 + 1];
    std::fs::write(&large, &bytes).unwrap();
    // mode-bits-ok: private retained journal fixture, never executable.
    std::fs::set_permissions(&large, std::fs::Permissions::from_mode(0o600)).unwrap();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    let mut gate = Job::new(
        "gate-run",
        Subject::new("custom", "fixture"),
        "Unrelated gate",
        ACTOR,
        Priority::Standard,
        date,
    );
    gate.status = JobStatus::Open;
    let head = "a".repeat(40);
    gate.metadata = json!({"sha":head,"branch":"fix/ordinary"});
    fixture.jobs.create_job(&gate).await.unwrap();
    let mut step = Step::new(gate.id, "task", "Record the gate verdict", 0);
    step.spec_slug = Some("record-verdict".into());
    step.status = StepStatus::Ready;
    fixture.jobs.add_step(&step).await.unwrap();
    struct Cluster {
        job: GateJob,
        log: String,
    }
    #[async_trait]
    impl GateCluster for Cluster {
        async fn gate_jobs(&self, _: &str) -> Result<Vec<GateJob>> {
            Ok(vec![self.job.clone()])
        }
        async fn gate_pod(&self, _: &str, _: &str) -> Result<GatePod> {
            Ok(super::super::carried_verdict::fixtures::ended(&self.log))
        }
        async fn gate_log(&self, _: &str, _: &str) -> Result<String> {
            Ok(self.log.clone())
        }
    }
    let receipt = json!({"head":head,"verdict":"green","mode":"full"}).to_string();
    let log = format!(
        "gate-runner: receipt {receipt}\ngate-runner: receipt-end v1 job=gate-ordinary verdict=green bytes={} sha256={}\n",
        receipt.len(),
        hex::encode(Sha256::digest(receipt.as_bytes()))
    );
    fixture.conductor.diagnostic_cluster(Box::new(Cluster {
        job: GateJob {
            name: "gate-ordinary".into(),
            uid: "original-uid".into(),
            packet: gate.id.to_string(),
            created: "2026-10-16T00:00:00Z".into(),
            carrier: true,
            finished: Some(Finished {
                failed: false,
                reason: "Complete".into(),
                at: "2026-10-16T00:01:00Z".into(),
            }),
        },
        log,
    }));
    fixture.conductor.refresh().await.unwrap();
    assert_ne!(
        fixture
            .jobs
            .list_steps(&fixture.assessment.id)
            .await
            .unwrap()[0]
            .status,
        StepStatus::Completed
    );
    fixture
        .conductor
        .settle_gate_runs(boss_clock_client::wall_now())
        .await;
    let recorded = fixture.jobs.list_steps(&gate.id).await.unwrap().remove(0);
    assert_eq!(
        recorded.status,
        StepStatus::Completed,
        "optional recovery suppressed the ordinary recorder"
    );
    let actual: Value =
        serde_json::from_str(recorded.metadata["receipt"].as_str().unwrap()).unwrap();
    assert_eq!(actual["verdict"], "green");
    assert_eq!(actual["head"], head);
    assert_eq!(fixture.original_bytes().await, original);
    assert_eq!(std::fs::read(large).unwrap(), bytes);
}

impl Fixture {
    async fn assert_car_hold_preserved(&self) {
        let car = self.jobs.get_job(&self.car.id).await.unwrap().unwrap();
        let mut expected = self.car.metadata.clone();
        expected
            .as_object_mut()
            .unwrap()
            .remove(crate::gate::DOCK_WAITING);
        assert_eq!(
            car.metadata, expected,
            "held refresh changed a hold/strike or failed claim withdrawal"
        );
    }
    async fn original_bytes(&self) -> (Vec<u8>, Vec<u8>, Vec<(PathBuf, Vec<u8>)>) {
        fn files(
            root: &std::path::Path,
            path: &std::path::Path,
            out: &mut Vec<(PathBuf, Vec<u8>)>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                assert!(!metadata.is_symlink());
                if metadata.is_dir() {
                    files(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(path).unwrap(),
                    ));
                }
            }
        }
        let mut journal = Vec::new();
        files(
            &self.original_directory,
            &self.original_directory,
            &mut journal,
        );
        journal.sort_by(|a, b| a.0.cmp(&b.0));
        (
            serde_json::to_vec(&self.jobs.get_job(&self.original.id).await.unwrap()).unwrap(),
            serde_json::to_vec(&self.jobs.list_steps(&self.original.id).await.unwrap()).unwrap(),
            journal,
        )
    }

    async fn new(fault: Option<&str>) -> Self {
        use axum::response::IntoResponse;
        use boss_core::{port::EventBus, publisher::DomainPublisher};
        use boss_policy_client::{FakePolicyClient, Scope, controls};
        let root = tempfile::tempdir().unwrap();
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let mut protocol = boss_jobs::seed_loader::parse_workflows(
            include_str!(
                "../../../../../../infra/platform/workflows/diagnostic-recovery-assessment.toml"
            ),
            "platform",
            "diagnostic-recovery-assessment.toml",
        )
        .unwrap()
        .remove(0);
        protocol.version = 1;
        protocol.status = boss_jobs::registry::WorkflowStatus::Active;
        let protocol = serde_json::to_value(protocol).unwrap();
        let operation = &protocol["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["title"] == "execute")
            .unwrap()["metadata_defaults"]["recovery_operation"];
        let mut original = Job::new(
            "diagnostic-request",
            Subject::new("custom", "fixture"),
            "Original UNKNOWN",
            ACTOR,
            Priority::Urgent,
            date,
        );
        original.status = JobStatus::Open;
        original.id = serde_json::from_value(operation["original_request"].clone()).unwrap();
        original.workflow_version = 2;
        let request = json!({"car":Uuid::new_v4(),"candidate":"a".repeat(40),"baseline":"b".repeat(40),"receipt_sha256":"c".repeat(64)});
        original.metadata =
            json!({"diagnostic_executor":ACTOR,"request":request,"hold":"unchanged","strikes":2});
        jobs.create_job(&original).await.unwrap();
        let mut original_step = Step::new(original.id, "task", "execute", 0);
        original_step.spec_slug = Some("execute".into());
        original_step.status = StepStatus::Active;
        original_step.assignee_id = Some(ACTOR.into());
        original_step.metadata = json!({"request":request});
        jobs.add_step(&original_step).await.unwrap();
        let mut ordinary = Vec::new();
        for priority in [Priority::Urgent, Priority::Standard] {
            let mut job = Job::new(
                "diagnostic-request",
                Subject::new("custom", "fixture"),
                "Unrelated fresh diagnostic",
                ACTOR,
                priority,
                date,
            );
            job.status = JobStatus::Open;
            job.metadata = json!({"diagnostic_executor":ACTOR});
            jobs.create_job(&job).await.unwrap();
            let mut step = Step::new(job.id, "task", "execute", 0);
            step.spec_slug = Some("execute".into());
            step.assignee_id = Some(ACTOR.into());
            step.status = StepStatus::Ready;
            jobs.add_step(&step).await.unwrap();
            ordinary.push(job.id);
        }
        let mut car = Job::new(
            "ship-a-change",
            Subject::new("custom", "fixture"),
            "Held source car",
            ACTOR,
            Priority::Urgent,
            date,
        );
        car.status = JobStatus::Open;
        car.metadata = json!({"hold":"required exact-head review","strikes":2,crate::gate::DOCK_WAITING:{"held":"old claim"}});
        jobs.create_job(&car).await.unwrap();
        let mut assessment = Job::new(
            "diagnostic-recovery-assessment",
            Subject::new("custom", "fixture"),
            "Recover original",
            ACTOR,
            Priority::Urgent,
            date,
        );
        assessment.status = JobStatus::Open;
        assessment.workflow_version = 1;
        let assessment_request = json!({"original_request":original.id});
        assessment.metadata = json!({"diagnostic_executor":ACTOR,"diagnostic_recovery_assessment":"v1","request":assessment_request});
        jobs.create_job(&assessment).await.unwrap();
        let mut step = Step::new(assessment.id, "task", "execute", 0);
        step.spec_slug = Some("execute".into());
        step.status = StepStatus::Ready;
        step.assignee_id = Some(ACTOR.into());
        step.metadata = json!({"request":assessment_request,"recovery_operation":operation});
        if fault == Some("handler") {
            step.metadata["recovery_operation"]["handler"] = json!("unknown-handler");
        }
        jobs.add_step(&step).await.unwrap();
        let admission = json!({"kind":boss_jobs::events::JOB_CREATED,"actor":"fixture-filer","payload":serde_json::to_value(&assessment).unwrap()});
        let resolution = json!({"anchor":"R1","decision":"Authorize a separate versioned read-only recovery assessment that conserves the original v2 intent and full native journal, validates exact invocation and fixed pre-create baseline rejection plus native absence, and appends an explicitly recovered refusal without compute, hold or intent effects. Alternatively retain UNKNOWN and authorize explicit retirement before a fresh current-baseline request. Future v3 baseline refusal behavior is distinct from this existing-attempt authority."});
        // This digest is copied from the published authority row, whose
        // input is canonical JSON, not the step-shape delimiter encoding.
        assert_eq!(
            hex::encode(Sha256::digest(serde_json::to_vec(&resolution).unwrap())),
            operation["resolution_sha256"]
        );
        let authority = json!({"id":operation["authority_design"],"kind":"design-doc","steps":[{"title":"review","spec_slug":"review","status":"completed","completed_by":"emp-david","metadata":{"resolutions":[resolution]}}]});
        let policy = FakePolicyClient::builder()
            .allow(
                "platform-admin",
                controls::READ_JOB.action(),
                controls::READ_JOB.resource(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                controls::UPDATE_STEP.action(),
                controls::UPDATE_STEP.resource(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                controls::UPDATE_JOB.action(),
                controls::UPDATE_JOB.resource(),
                Scope::All,
            )
            .build();
        let bus = boss_testing::RecordingEventBus::new();
        let publisher_bus: Arc<dyn EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(publisher_bus, "jobs"),
            Arc::new(policy),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let expected_admission = admission.clone();
        let mut expected_authority = authority.clone();
        let mut expected_protocol = protocol.clone();
        match fault {
            Some("authority-decision") => {
                expected_authority["steps"][0]["metadata"]["resolutions"][0]["decision"] =
                    json!("changed decision")
            }
            Some("authority-actor") => expected_authority["steps"][0]["completed_by"] = Value::Null,
            Some("authority-digest") => {
                expected_protocol["steps"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|s| s["title"] == "execute")
                    .unwrap()["metadata_defaults"]["recovery_operation"]["resolution_sha256"] =
                    json!("0".repeat(64))
            }
            _ => {}
        }
        let assessment_id = assessment.id.to_string();
        let dark_list = fault == Some("dark-list");
        let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn(move |request: axum::extract::Request,next:axum::middleware::Next| {
            let calls=observed.clone(); let admission=expected_admission.clone();let authority=expected_authority.clone();let protocol=expected_protocol.clone();let assessment_id=assessment_id.clone();
            async move {
                let path=request.uri().path().to_owned();
                calls.lock().unwrap().push(format!("{} {}",request.method(),request.uri()));
                if request.method()==Method::GET {
                    if dark_list && path == "/api/jobs" && request.uri().query().unwrap_or_default().contains("metadata=") {return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();}
                    if path==format!("/api/jobs/{assessment_id}/events") {return axum::Json(json!({"data":[admission],"total":1})).into_response();}
                    if path==format!("/api/jobs/{}",authority["id"].as_str().unwrap()) {return axum::Json(authority).into_response();}
                    if path=="/api/workflows/diagnostic-recovery-assessment/versions/1" {return axum::Json(protocol).into_response();}
                    if path=="/api/jobs" && request.uri().query().unwrap_or_default().contains("kind=pr-train") {return axum::Json(json!({"data":[{"id":"occupied-fixture-train","steps":[{"title":"merged","status":"ready"}]}],"total":1})).into_response();}
                }
                next.run(request).await
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut conductor = Conductor::diagnostic_fixture(&root.path().join("absent-clone"));
        conductor.cfg.jobs = format!("http://{address}");
        conductor.cfg.diagnostics_home = root.path().join("journals").to_str().unwrap().into();
        conductor.http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        let original_directory = super::super::diagnostic_journal::private_directory(
            &root.path().join("original-journal"),
        )
        .unwrap();
        let content = crate::attach::tests::store().await;
        let source = NativeDoor {
            conductor: &conductor,
            job: serde_json::to_value(&original).unwrap(),
            step: serde_json::to_value(&original_step).unwrap(),
            directory: original_directory.clone(),
            actor: ACTOR.into(),
            content: content.clone(),
            may_launch: false,
        };
        let intent = Intent {
            attempt: serde_json::from_value(operation["original_request"].clone()).unwrap(),
            inputs: Inputs {
                request: serde_json::from_value(request.clone()).unwrap(),
                admitted_original: json!({"payload":original}),
                pinned_protocol: json!({"kind":"diagnostic-request","version":2,"steps":[{"title":"execute","metadata_defaults":{"diagnostic_operation":{"version":2}}}]}),
                current_car: json!({}),
                car_binding: json!({}),
                policy: json!({}),
                edit_level: json!({}),
                required_checks: vec!["fixture-check".into()],
                expected_executor_identity: super::super::consist_job::CONTROLLER_IDENTITY.into(),
                observed_cluster_identity: super::super::consist_job::CONTROLLER_IDENTITY.into(),
            },
        };
        let invocation =
            json!({"phase":"preparing","invocation":Uuid::new_v4(),"attempt":intent.attempt});
        source
            .keep("diagnostic.intent", &serde_json::to_value(&intent).unwrap())
            .await
            .unwrap();
        source
            .keep("diagnostic.invocation", &invocation)
            .await
            .unwrap();
        let intent_record = source
            .original_record("diagnostic.intent")
            .await
            .unwrap()
            .unwrap();
        let invocation_record = source
            .original_record("diagnostic.invocation")
            .await
            .unwrap()
            .unwrap();
        let command = |args: Value, stdout: Vec<u8>| {
            super::super::diagnostic_journal::command_value(
                &args
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap())
                    .collect::<Vec<_>>(),
                None,
                &std::process::Output {
                    status: std::os::unix::process::ExitStatusExt::from_raw(0),
                    stdout,
                    stderr: b"complete fixture stderr".to_vec(),
                },
            )
        };
        let journal_rows = [
            invocation.clone(),
            command(json!(["fetch", "origin"]), vec![]),
            command(
                json!(["rev-parse", "origin/main"]),
                format!("{}\n", "d".repeat(40)).into_bytes(),
            ),
        ];
        let journal = json!({"files":journal_rows.iter().enumerate().map(|(i,o)| {let raw=serde_json::to_vec(&json!({"sequence":i,"observation":o})).unwrap(); json!({"path":format!("{i:020}-{}.json",hex::encode(Sha256::digest(&raw))),"bytes_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,raw)})}).collect::<Vec<_>>()});
        let empty = json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":"fixture-original","continue":""},"items":[]});
        let mut originals = Vec::new();
        for (args, body) in [
            (
                json!(["auth", "whoami", "-o", "json"]),
                json!({"status":{"userInfo":{"username":super::super::consist_job::CONTROLLER_IDENTITY}}}),
            ),
            (
                json!([
                    "get",
                    "jobs",
                    "--field-selector",
                    format!("metadata.name={}", intent.job_name()),
                    "-o",
                    "json"
                ]),
                empty.clone(),
            ),
            (json!(["get", "pods", "-o", "json"]), empty.clone()),
        ] {
            originals.push(json!({"sequence":originals.len(),"observation":{"phase":"native-invocation","namespace":"boss-dev","program":"/usr/local/bin/kubectl","args":args}}));
            originals.push(json!({"sequence":originals.len(),"observation":command(args,serde_json::to_vec(&body).unwrap())}));
        }
        let native = json!({"namespace":"boss-dev","identity":super::super::consist_job::CONTROLLER_IDENTITY,"jobs":empty,"pods":empty,"command_originals":originals});
        let mut assessment_value = json!({"schema":1,"outcome":"recovered-refusal","assessment_request":assessment.id,"subject":{"original_request":original.id,"car":request["car"]},"protocol":protocol,"authority":authority,"admission":admission,"intent_record":intent_record,"invocation_record":invocation_record,"intent":intent,"journal":journal,"native":native,"observed_baseline":"d".repeat(40)});
        match fault {
            Some("protocol") => assessment_value["protocol"]["version"] = json!(2),
            Some("authority") => {
                assessment_value["authority"]["steps"][0]["metadata"]["resolutions"][0]["decision"] =
                    json!("spoofed")
            }
            Some("admission") => assessment_value["admission"]["actor"] = json!("spoofed"),
            Some("request") => {
                assessment_value["subject"]["original_request"] = json!(Uuid::new_v4())
            }
            Some("journal") => {
                assessment_value["journal"]["files"][0]["bytes_b64"] = json!("broken")
            }
            Some("identity") => {
                assessment_value["native"]["identity"] = json!("another-native-actor")
            }
            Some("native-job") => {
                assessment_value["native"]["jobs"]["items"] =
                    json!([{"metadata":{"name":intent.job_name()}}])
            }
            Some("native-pod") => {
                assessment_value["native"]["pods"]["items"] = json!([{"metadata":{"name":intent.job_name(),"namespace":"boss-dev","uid":"existing"}}])
            }
            Some("native-tail") => {
                assessment_value["native"]["jobs"]["metadata"]["continue"] = json!("unread")
            }
            Some("native-command") => {
                assessment_value["native"]["command_originals"][1]["observation"]["exit"] = json!(1)
            }
            Some("outcome") => assessment_value["outcome"] = json!("passed"),
            None
            | Some(
                "authority-decision" | "authority-actor" | "authority-digest" | "handler"
                | "dark-list",
            ) => {}
            Some(other) => panic!("unknown fixture fault {other}"),
        }
        let door = NativeDoor {
            conductor: &conductor,
            job: serde_json::to_value(&assessment).unwrap(),
            step: serde_json::to_value(step).unwrap(),
            directory: super::super::diagnostic_journal::private_directory(
                &PathBuf::from(&conductor.cfg.diagnostics_home).join(assessment.id.to_string()),
            )
            .unwrap(),
            actor: ACTOR.into(),
            content,
            may_launch: false,
        };
        door.keep("diagnostic.recovery-assessment", &assessment_value)
            .await
            .unwrap();
        Self {
            conductor,
            jobs,
            assessment,
            original,
            original_directory,
            calls,
            ordinary,
            car,
            _root: root,
        }
    }
}
