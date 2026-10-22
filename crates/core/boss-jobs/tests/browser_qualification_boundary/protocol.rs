use super::*;
use boss_jobs::{
    browser_qualification as judgment,
    browser_qualification_adapter::{BrowserQualification, HttpObservationFiles},
    first_record::FirstRecord,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) async fn packet(jobs: &InMemoryJobs, id: boss_core::job::JobId) -> Value {
    let mut value = serde_json::to_value(jobs.get_job(&id).await.unwrap().unwrap()).unwrap();
    value["steps"] = serde_json::to_value(jobs.list_steps(&id).await.unwrap()).unwrap();
    value
}

pub(super) struct Fixture {
    pub(super) app: Router,
    pub(super) jobs: Arc<InMemoryJobs>,
    pub(super) registry: Arc<InMemoryWorkflows>,
    pub(super) car: boss_core::job::JobId,
    pub(super) observation: boss_core::job::JobId,
    observe: StepId,
    pub(super) qualify: StepId,
    accept: StepId,
    value: Value,
    content: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.content.abort();
    }
}
impl Fixture {
    pub(super) async fn new() -> Self {
        Self::with_content_fault("").await
    }
    pub(super) async fn with_content_fault(fault: &str) -> Self {
        let binding = judgment::contract().unwrap();
        let registry = Arc::new(InMemoryWorkflows::for_bootstrap());
        let car_spec = boss_jobs::seed_loader::parse_workflows(
            include_str!("../../../../../infra/platform/workflows/ship-a-change.toml"),
            "platform",
            "ship-a-change.toml",
        )
        .unwrap()
        .remove(0);
        let observation_spec = boss_jobs::seed_loader::parse_workflows(
            include_str!(
                "../../../../../infra/platform/workflows/browser-renderer-observation.toml"
            ),
            "platform",
            "browser-renderer-observation.toml",
        )
        .unwrap()
        .remove(0);
        registry.seed(car_spec.clone()).unwrap();
        registry.seed(observation_spec.clone()).unwrap();
        let jobs = Arc::new(InMemoryJobs::new());
        let mut car = Job::new(
            "ship-a-change",
            Subject::new("custom", "renderer"),
            "Landed renderer",
            "emp-observer",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap(),
        );
        car.id = serde_json::from_value(binding["car"].clone()).unwrap();
        car.workflow_version = car_spec.version;
        car.status = boss_core::job::JobStatus::Open;
        car.metadata = json!({"merged":"true"});
        jobs.create_job(&car).await.unwrap();
        let mut accept = StepId::new();
        for mut step in
            materialize_steps(&car_spec, &car.subject, car.id, &car.metadata, StepId::new)
        {
            match step.spec_slug.as_deref().unwrap() {
                "opened" | "scope" | "build" | "gate" | "review" => {
                    step.status = StepStatus::Completed
                }
                "proven" => step.status = StepStatus::Ready,
                "attended-qualified" => {
                    accept = step.id;
                    step.status = StepStatus::Pending;
                }
                _ => {}
            }
            if step.spec_slug.as_deref() == Some("gate") {
                step.metadata["receipt"] = json!({"head":binding["source_head"],"verdict":"green"});
            }
            if step.spec_slug.as_deref() == Some("review") {
                step.metadata["release"] = json!({"review":"source-only control"});
            }
            jobs.add_step(&step).await.unwrap();
        }
        let mut observation = Job::new(
            "browser-renderer-observation",
            Subject::new("custom", "renderer"),
            "Attended observation",
            "emp-observer",
            Priority::Standard,
            car.opened_on,
        );
        observation.workflow_version = observation_spec.version;
        observation.metadata = json!({"car":binding["car"]});
        observation.status = boss_core::job::JobStatus::Open;
        jobs.create_job(&observation).await.unwrap();
        let (mut observe, mut qualify) = (StepId::new(), StepId::new());
        for mut step in materialize_steps(
            &observation_spec,
            &observation.subject,
            observation.id,
            &observation.metadata,
            StepId::new,
        ) {
            match step.spec_slug.as_deref().unwrap() {
                "filed" => step.status = StepStatus::Completed,
                "observe" => {
                    step.status = StepStatus::Ready;
                    observe = step.id;
                }
                "qualify" => qualify = step.id,
                _ => {}
            }
            jobs.add_step(&step).await.unwrap();
        }
        let output = json!({"schema":1,"cases":binding["cases"].as_array().unwrap().iter().map(|id|json!({"id":id,"assertions":binding["assertions"][id.as_str().unwrap()].as_array().unwrap().iter().map(|name|json!({"name":name,"passed":true,"actual":"attended test original"})).collect::<Vec<_>>(),"requests":if id=="question" {vec![json!({"url":"/api/jobs/synthetic-question/steps/synthetic-step/metadata","method":"PATCH","body":json!({"verdict":"answered","answer":"Tuesday evening"}).to_string(),"allowed":true}),json!({"url":"/api/jobs/synthetic-question/steps/synthetic-step","method":"PUT","body":json!({"status":"completed"}).to_string(),"allowed":true})]}else{vec![]},"error":null})).collect::<Vec<_>>()});
        let mut data = BTreeMap::new();
        let mut listing = Vec::new();
        let mut attachment = |bytes: Vec<u8>| {
            let id = uuid::Uuid::new_v4().to_string();
            let hash = hex::encode(Sha256::digest(&bytes));
            let size = bytes.len();
            listing.push(json!({"id":id,"target":{"kind":"job","id":observation.id},"sha256":hash,"size_bytes":size,"deleted_at":null}));
            data.insert(id.clone(), bytes);
            json!({"id":id,"sha256":hash,"bytes":size})
        };
        // The contract's pinned renderer, not the served tree file: a drift
        // fails the one named check in browser_qualification.rs, not this
        // fixture (review 6ac43de2 F1, 2026-10-20).
        let asset =
            attachment(include_bytes!("../fixtures/browser-renderer/answer-question.js").to_vec());
        let suite = attachment(
            include_bytes!("../../../../../infra/step-plugins/browser-renderer-cases.js").to_vec(),
        );
        let report = attachment(serde_json::to_vec(&output).unwrap());
        let value = json!({"schema":1,"assurance":"attended-browser-observation","binding":binding,
            "asset":{"url":"https://boss.algedonic.dev/plugins/answer-question.js","status":200,"headers":{},"expected_sha256":binding["asset"]["sha256"],"observed_sha256":binding["asset"]["sha256"],"file":asset},
            "suite":{"url":"https://boss.algedonic.dev/plugins/browser-renderer-cases.js","status":200,"headers":{},"expected_sha256":binding["suite"]["sha256"],"observed_sha256":binding["suite"]["sha256"],"file":suite},"report":report,"output":output});
        match fault {
            "missing" => {
                listing.pop();
            }
            "target" => listing[0]["target"]["id"] = json!(uuid::Uuid::new_v4()),
            "digest" => listing[0]["sha256"] = json!("b".repeat(64)),
            "bytes" => {
                data.get_mut(asset["id"].as_str().unwrap()).unwrap()[0] ^= 1;
            }
            "partial" => {
                data.get_mut(suite["id"].as_str().unwrap()).unwrap().pop();
            }
            "duplicate" => listing.push(listing[0].clone()),
            _ => {}
        }
        let content_app = Router::new()
            .route(
                "/api/files",
                axum::routing::get(move || {
                    let listing = listing.clone();
                    async move { axum::Json(listing) }
                }),
            )
            .route(
                "/api/files/{id}",
                axum::routing::get(
                    move |axum::extract::Path(id): axum::extract::Path<String>| {
                        let data = data.clone();
                        async move { data.get(&id).cloned().unwrap() }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let content = tokio::spawn(async move {
            axum::serve(listener, content_app).await.unwrap();
        });
        let bus = RecordingEventBus::new();
        let bus_dyn: Arc<dyn EventBus> = bus.clone();
        let policy = Arc::new(FakePolicyClient::builder().with_default_rules().build());
        let mut state = JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        );
        state.kind_registry = Some(registry.clone());
        state.presence_key = Some(Arc::new(boss_jobs::http::PresenceKey::fixed(
            SESSION_KEY.to_vec(),
        )));
        let http = boss_core::machine_token::Client::unstamped(
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15)),
        )
        .unwrap();
        let files = Arc::new(HttpObservationFiles::with_client(
            format!("http://{address}"),
            http,
        ));
        let agents = Arc::new(boss_jobs::agents::InMemoryAgents::new());
        for (name, accepting) in [
            ("attended-browser-qualify-v1", false),
            ("attended-browser-accept-v1", true),
        ] {
            state.completion_contracts.insert(
                name.into(),
                Arc::new(BrowserQualification {
                    jobs: jobs.clone(),
                    workflows: registry.clone(),
                    agents: agents.clone(),
                    files: files.clone(),
                    accepting,
                }),
            );
        }
        Self {
            app: router(state),
            jobs,
            registry,
            car: car.id,
            observation: observation.id,
            observe,
            qualify,
            accept,
            value,
            content,
        }
    }
    pub(super) fn path(&self, s: StepId) -> String {
        format!("/api/jobs/{}/steps/{s}", self.observation)
    }
    /// The observer's own signed-in browser, as browser-observation.js
    /// sends it: the record through the signer door, the session's own
    /// sign-off stamp on the step that now holds it, then status alone.
    pub(super) async fn observe(&self) -> FirstRecord {
        let result = call_signed(
            &self.app,
            "emp-observer",
            "POST",
            format!("{}/metadata/records", self.path(self.observe)),
            json!({"key":"observation","value":self.value,"expected_absence":true}),
        )
        .await;
        assert_eq!(result.0, 201);
        assert_eq!(
            call_signed(
                &self.app,
                "emp-observer",
                "POST",
                format!("{}/sign-offs", self.path(self.observe)),
                json!({"role":"platform-admin"})
            )
            .await
            .0,
            200
        );
        assert_eq!(
            call_signed(
                &self.app,
                "emp-observer",
                "PUT",
                self.path(self.observe),
                json!({"status":"completed"})
            )
            .await
            .0,
            204
        );
        self.jobs
            .first_step_record(&self.observe, "observation")
            .await
            .unwrap()
            .unwrap()
    }
    pub(super) async fn qualification(&self, original: &FirstRecord) -> FirstRecord {
        let value = judgment::judge(
            original,
            &packet(&self.jobs, self.observation).await,
            &packet(&self.jobs, self.car).await,
            "emp-reviewer",
            "I independently reviewed the full originals.",
        )
        .unwrap();
        assert_eq!(
            call_as(
                &self.app,
                "emp-reviewer",
                "POST",
                format!("{}/metadata/records", self.path(self.qualify)),
                json!({"key":"qualification","value":value,"expected_absence":true})
            )
            .await
            .0,
            201
        );
        self.jobs
            .first_step_record(&self.qualify, "qualification")
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn complete_independent_originals_and_all_attachment_bytes_qualify_via_public_doors() {
    let f = Fixture::new().await;
    let original = f.observe().await;
    let qualification = f.qualification(&original).await;
    assert_eq!(
        call_as(
            &f.app,
            "emp-reviewer",
            "PUT",
            f.path(f.qualify),
            json!({"status":"completed"})
        )
        .await
        .0,
        204
    );
    let car = packet(&f.jobs, f.car).await;
    let published = boss_jobs::seed_loader::parse_workflows(
        include_str!("../../../../../infra/platform/workflows/ship-a-change.toml"),
        "platform",
        "ship-a-change.toml",
    )
    .unwrap()
    .remove(0);
    let accepted = judgment::acceptance(
        &car,
        &serde_json::to_value(published).unwrap(),
        &packet(&f.jobs, f.observation).await,
        &original,
        &qualification,
    )
    .unwrap();
    // Both records name both heads: the gated source AND the renderer the
    // browser ran, which differ since train #1279 (review 5e15da5b N1).
    let binding = judgment::contract().unwrap();
    for record in [&qualification.value, &accepted] {
        assert_eq!(record["source_head"], binding["source_head"], "{record}");
        assert_eq!(record["asset_head"], binding["asset_head"], "{record}");
        assert_eq!(record["asset"], binding["asset"], "{record}");
    }
    let path = format!("/api/jobs/{}/steps/{}", f.car, f.accept);
    assert_eq!(
        call(
            &f.app,
            "POST",
            format!("{path}/metadata/records"),
            json!({"key":"attended_qualification","value":accepted,"expected_absence":true})
        )
        .await
        .0,
        201
    );
    assert_eq!(
        call(
            &f.app,
            "PATCH",
            format!("/api/jobs/{}/metadata", f.car),
            json!({"attended_qualified":"true"})
        )
        .await
        .0,
        204
    );
    let before = packet(&f.jobs, f.car).await;
    assert_eq!(
        call(&f.app, "PUT", path, json!({"status":"completed"}))
            .await
            .0,
        204
    );
    let after = packet(&f.jobs, f.car).await;
    assert_eq!(after["status"], "closed");
    assert_eq!(after["metadata"]["outcome"], "attended-qualified");
    assert_eq!(
        call(
            &f.app,
            "PUT",
            format!("/api/jobs/{}/steps/{}", f.car, f.accept),
            json!({"status":"completed"})
        )
        .await
        .0,
        204
    );
    let proof = judgment::step(&after, "proven").unwrap();
    assert_ne!(proof["status"], "completed");
    assert_eq!(
        proof["metadata"],
        judgment::step(&before, "proven").unwrap()["metadata"]
    );
}

#[tokio::test]
async fn wrong_completion_actor_and_missing_content_refuse_the_real_adapter() {
    let f = Fixture::new().await;
    let original = f.observe().await;
    let _qualification = f.qualification(&original).await;
    assert_eq!(
        call(
            &f.app,
            "PUT",
            f.path(f.qualify),
            json!({"status":"completed"})
        )
        .await
        .0,
        409
    );
    f.content.abort();
    assert_eq!(
        call_as(
            &f.app,
            "emp-reviewer",
            "PUT",
            f.path(f.qualify),
            json!({"status":"completed"})
        )
        .await
        .0,
        409
    );
    assert_ne!(
        f.jobs.get_step(&f.qualify).await.unwrap().unwrap().status,
        StepStatus::Completed
    );
}

/// THE FORGERY review 8c30cbc7 measured on this car (rework_owed_20261019
/// on item c1d16aa6): the machine door takes `x-boss-user` as sent, so an
/// agent naming a person — or the observer's own id typed with no session
/// behind it — wrote the observation and the car closed attended-qualified
/// with nobody present. The observation is the signer's key now: only a
/// verified gateway session of a person holding the step's sign-off role
/// records it, and every other caller is refused before any original
/// exists to qualify.
#[tokio::test]
async fn a_machine_door_caller_sending_a_human_id_cannot_record_an_observation() {
    let f = Fixture::new().await;
    let records = format!("{}/metadata/records", f.path(f.observe));
    let body = json!({"key":"observation","value":f.value,"expected_absence":true});
    for actor in ["emp-david", "emp-observer", "agent-claude"] {
        let (code, refusal) = call_as(&f.app, actor, "POST", records.clone(), body.clone()).await;
        assert_eq!(code, 409, "{actor} on the machine door: {refusal}");
    }
    // An agent's own verified session is a session, not a person.
    let (code, _) = call_signed(
        &f.app,
        "agent-claude",
        "POST",
        records.clone(),
        body.clone(),
    )
    .await;
    assert_eq!(code, 409, "an agent session is not a person signer");
    // The merge door is the same key's other way in.
    let (code, _) = call_as(
        &f.app,
        "emp-david",
        "PATCH",
        format!("{}/metadata", f.path(f.observe)),
        json!({"observation": f.value}),
    )
    .await;
    assert_eq!(code, 409, "the merge door must refuse the signer's key too");
    assert!(
        f.jobs
            .first_step_record(&f.observe, "observation")
            .await
            .unwrap()
            .is_none(),
        "no observation original exists after every refused write"
    );
    // Positive control: the person's own signed-in browser records it.
    let original = f.observe().await;
    assert_eq!(original.actor.to_string(), "emp-observer");
}

/// A packet pinned to a row whose observation field names no signer — every
/// packet filed before this rework, or one converted onto a row that drops
/// the writer — carries an observation any Update holder could have
/// written. The judgment refuses it on the PINNED field, so the shape test
/// on the record's actor is never the only thing standing.
#[tokio::test]
async fn judgment_refuses_an_observation_whose_pinned_field_is_not_signer_written() {
    let f = Fixture::new().await;
    let original = f.observe().await;
    let car = packet(&f.jobs, f.car).await;
    let mut observation = packet(&f.jobs, f.observation).await;
    judgment::judge(&original, &observation, &car, "emp-reviewer", "reviewed")
        .expect("control: the signer-pinned packet qualifies");
    let steps = observation["steps"].as_array_mut().unwrap();
    let observe = steps
        .iter_mut()
        .find(|step| step["spec_slug"] == "observe")
        .unwrap();
    for field in observe["fields"].as_array_mut().unwrap() {
        if field["name"] == "observation" {
            field["writer"] = Value::Null;
        }
    }
    let refused = judgment::judge(&original, &observation, &car, "emp-reviewer", "reviewed")
        .expect_err("an observation field with no signer writer must not qualify");
    assert!(
        refused.to_string().contains("signer"),
        "the refusal names the missing signer: {refused}"
    );
}

/// An unreadable pinned protocol is an infrastructure fault, not a judgment
/// of the evidence (review 5e15da5b F1, review 8c30cbc7 N2). The contract
/// reads the protocol BEFORE the write, so both completion doors — the step
/// PUT and the whole-job close — answer it 500 naming the packet and the
/// protocol it could not read, write nothing, and settle on the re-send.
/// They used to answer "409 completion contract: storage error", which named
/// no packet and read as a refusal of the evidence; 409 stays for that.
#[tokio::test]
async fn an_unreadable_pinned_protocol_is_a_500_naming_the_packet_not_an_evidence_refusal() {
    let f = Fixture::new().await;
    let original = f.observe().await;
    let _qualification = f.qualification(&original).await;
    let observation = f.jobs.get_job(&f.observation).await.unwrap().unwrap();
    let pinned = format!("{} v{}", observation.kind, observation.workflow_version);
    f.registry.fail_version_read_after(&observation.kind, 0);
    let (code, body) = call_text(
        &f.app,
        "emp-reviewer",
        None,
        "PUT",
        f.path(f.qualify),
        json!({"status":"completed"}),
    )
    .await;
    f.registry.restore_version_read(&observation.kind);
    assert_eq!(code, 500, "the step door: {body}");
    for name in [observation.id.to_string(), pinned] {
        assert!(
            body.contains(&name),
            "the step door's 500 names {name}: {body}"
        );
    }
    assert_ne!(
        f.jobs.get_step(&f.qualify).await.unwrap().unwrap().status,
        StepStatus::Completed,
        "nothing was written over the unread protocol"
    );
    assert_eq!(
        call_as(
            &f.app,
            "emp-reviewer",
            "PUT",
            f.path(f.qualify),
            json!({"status":"completed"})
        )
        .await
        .0,
        204,
        "the re-send the 500 asks for settles the completion"
    );

    // The whole-job close of a protected outcome reads the same way. Its
    // door reads the protocol for other judgments first, so the contract's
    // read is the LAST one a clean close makes — counted on the control,
    // which is the evidence refusal it always was.
    let car = f.jobs.get_job(&f.car).await.unwrap().unwrap();
    let mut whole = serde_json::to_value(&car).unwrap();
    whole["status"] = json!("closed");
    whole["metadata"]["outcome"] = json!("attended-qualified");
    let before = f.registry.version_reads(&car.kind);
    let (code, body) = call_text(
        &f.app,
        "emp-observer",
        None,
        "PUT",
        format!("/api/jobs/{}", f.car),
        whole.clone(),
    )
    .await;
    let reads = f.registry.version_reads(&car.kind) - before;
    assert_eq!(
        code, 409,
        "control: a read contract still refuses as evidence: {body}"
    );
    assert!(reads > 0, "control: the close reads the protocol");
    f.registry.fail_version_read_after(&car.kind, reads - 1);
    let (code, body) = call_text(
        &f.app,
        "emp-observer",
        None,
        "PUT",
        format!("/api/jobs/{}", f.car),
        whole,
    )
    .await;
    f.registry.restore_version_read(&car.kind);
    assert_eq!(code, 500, "the job door: {body}");
    for name in [
        car.id.to_string(),
        format!("{} v{}", car.kind, car.workflow_version),
    ] {
        assert!(
            body.contains(&name),
            "the job door's 500 names {name}: {body}"
        );
    }
    assert_ne!(
        f.jobs.get_job(&f.car).await.unwrap().unwrap().status,
        boss_core::job::JobStatus::Closed,
        "nothing was written over the unread protocol"
    );
}
