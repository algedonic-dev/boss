//! The ops runner credential's rotation, driven against the REAL door.
//!
//! The Secret store here writes every key it holds into a directory as a
//! file, the way kubelet projects a Secret volume, and the jobs-API stub
//! is mounted behind `boss_jobs::runner_credential::mount` over that same
//! directory — so every "verified by effect" below is the production door
//! resolving a value the handler staged, and every "not yet" is the door
//! not seeing a write the mount has not propagated. `lag` holds writes
//! back from the directory until `propagate`, which is kubelet's 60-90 s.

use super::*;
use std::path::PathBuf;
use std::sync::Mutex;

const NS: &str = "boss";
const SECRET: &str = "ops-runner-credential";
const CREDENTIAL: &str = "ops-runner-credential-forge";
const HOST: &str = "forge";
const JOB: &str = "1e50e66b-b501-4d45-8da7-c162a0f41d54";
const OTHER_JOB: &str = "6c9183de-0000-4000-8000-000000000001";
const THIRD_JOB: &str = "6c9183de-0000-4000-8000-000000000002";

// Fake values, shaped like the broker's (43 characters, base64url).
const OLD: &str = "oldOLDoldOLDoldOLDoldOLDoldOLDoldOLDold0001";
const LEFT: &str = "leftLEFTleftLEFTleftLEFTleftLEFTleftLEFT003";
const GCP: &str = "gcpGCPgcpGCPgcpGCPgcpGCPgcpGCPgcpGCPgcp0002";

// ----- the Secret, projected into a directory as kubelet does -----

struct MountedSecret {
    map: Mutex<HashMap<String, String>>,
    dir: PathBuf,
    lag: Mutex<bool>,
    writes: Mutex<usize>,
    revision: Mutex<u64>,
    lost_write_ack: Mutex<bool>,
}

impl MountedSecret {
    fn new(name: &str) -> Arc<Self> {
        let dir = boss_testing::scratch_dir(&format!("ops-runner-credential-{name}"));
        Arc::new(Self {
            map: Mutex::default(),
            dir,
            lag: Mutex::new(false),
            writes: Mutex::new(0),
            revision: Mutex::new(1),
            lost_write_ack: Mutex::new(false),
        })
    }
    fn seed(&self, key: &str, value: &str) {
        *self.revision.lock().unwrap() += 1;
        self.map
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        self.project();
    }
    fn get(&self, key: &str) -> Option<String> {
        self.map
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .filter(|v| !v.is_empty())
    }
    fn writes(&self) -> usize {
        *self.writes.lock().unwrap()
    }
    fn lag(&self, on: bool) {
        *self.lag.lock().unwrap() = on;
    }
    /// Kubelet's refresh: the directory becomes the Secret as it stands,
    /// whole — files for every key, and none for a key it no longer has.
    fn project(&self) {
        for e in std::fs::read_dir(&self.dir).unwrap() {
            std::fs::remove_file(e.unwrap().path()).unwrap();
        }
        for (k, v) in self.map.lock().unwrap().iter() {
            boss_testing::write_file(&self.dir.join(k), v);
        }
    }
    fn propagate(&self) {
        self.lag(false);
        self.project();
    }
}

#[async_trait]
impl SecretStore for MountedSecret {
    async fn read_secret(&self, ns: &str, name: &str) -> Result<Option<SecretData>, String> {
        assert_eq!((ns, name), (NS, SECRET));
        let revision = self.revision.lock().unwrap();
        Ok(Some(SecretData {
            uid: "runner-fixture-uid".into(),
            version: revision.to_string(),
            data: self
                .map
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }))
    }
    async fn write_keys_if(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
        observed: &SecretData,
    ) -> Result<WriteAt, String> {
        assert_eq!((ns, name), (NS, SECRET));
        {
            let mut revision = self.revision.lock().unwrap();
            if observed.uid != "runner-fixture-uid" || observed.version != revision.to_string() {
                return Ok(WriteAt::Moved);
            }
            let mut map = self.map.lock().unwrap();
            for (key, value) in entries {
                assert!(key.starts_with("forge."));
                map.insert((*key).into(), (*value).into());
            }
            *revision += 1;
            *self.writes.lock().unwrap() += 1;
        }
        if !*self.lag.lock().unwrap() {
            self.project();
        }
        if std::mem::take(&mut *self.lost_write_ack.lock().unwrap()) {
            return Err("fixture lost Secret acknowledgment after commit".into());
        }
        Ok(WriteAt::Written)
    }
    async fn read_key(&self, ns: &str, name: &str, key: &str) -> Result<Option<String>, String> {
        assert_eq!((ns, name), (NS, SECRET), "the declared Secret, and only it");
        Ok(self.map.lock().unwrap().get(key).cloned())
    }
    async fn write_key(&self, ns: &str, name: &str, key: &str, value: &str) -> Result<(), String> {
        self.write_keys(ns, name, &[(key, value)]).await
    }
    async fn write_keys(
        &self,
        ns: &str,
        name: &str,
        entries: &[(&str, &str)],
    ) -> Result<(), String> {
        assert_eq!((ns, name), (NS, SECRET), "the declared Secret, and only it");
        *self.writes.lock().unwrap() += 1;
        *self.revision.lock().unwrap() += 1;
        {
            let mut m = self.map.lock().unwrap();
            for (k, v) in entries {
                assert!(
                    k.starts_with(&format!("{HOST}.")),
                    "a declaration for {HOST} writes {HOST}'s keys only: {k}"
                );
                m.insert((*k).to_string(), (*v).to_string());
            }
        }
        if !*self.lag.lock().unwrap() {
            self.project();
        }
        Ok(())
    }
}

// ----- the jobs API: a rotation packet, stateful, behind the real door -----

type Captured = Arc<Mutex<Vec<(String, JsonValue)>>>;

struct Jobs {
    url: String,
    steps: Arc<Mutex<Vec<(String, String)>>>,
    writes: Captured,
    rotations: Captured,
    /// Who the packet's `delivered` step names as its completer — the
    /// deposit's actor unless a case says otherwise (review 3930a3eb, N2).
    delivered_by: Arc<Mutex<Option<String>>>,
    phase_fault: Arc<Mutex<Option<String>>>,
    attempts: Captured,
    missing_receipt: Arc<Mutex<bool>>,
    registry_response: Arc<Mutex<Option<JsonValue>>>,
    canonical_registry_row: JsonValue,
    /// What each phase post presented in the broker-stage header, in
    /// order (design 6e28ed42) — `None` when the header was absent.
    stage_tokens: Arc<Mutex<Vec<Option<String>>>>,
}

impl Jobs {
    fn status(&self, slug: &str) -> String {
        self.steps
            .lock()
            .unwrap()
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, st)| st.clone())
            .unwrap_or_default()
    }
    fn phases(&self) -> Vec<String> {
        self.rotations
            .lock()
            .unwrap()
            .iter()
            .map(|(p, _)| p.clone())
            .collect()
    }
    fn step_fields(&self, slug: &str) -> JsonValue {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == format!("step-{slug}/metadata"))
            .map(|(_, v)| v.clone())
            .next_back()
            .unwrap_or(JsonValue::Null)
    }
    fn everything_written(&self) -> String {
        let w = self.writes.lock().unwrap();
        let r = self.rotations.lock().unwrap();
        w.iter()
            .chain(r.iter())
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

async fn jobs(dir: PathBuf, step_statuses: &[(&str, &str)]) -> Jobs {
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::get, routing::post, routing::put};

    let steps = Arc::new(Mutex::new(
        step_statuses
            .iter()
            .map(|(s, st)| (s.to_string(), st.to_string()))
            .collect::<Vec<_>>(),
    ));
    let writes: Captured = Default::default();
    let rotations: Captured = Default::default();
    let slug_of = |sid: &str| sid.trim_start_matches("step-").to_string();
    let (st_get, st_put) = (steps.clone(), steps.clone());
    let (w_put, w_merge) = (writes.clone(), writes.clone());
    let rot = rotations.clone();
    let phase_fault = Arc::new(Mutex::new(None::<String>));
    let fault = phase_fault.clone();
    let attempts: Captured = Default::default();
    let phase_attempts = attempts.clone();
    let missing_receipt = Arc::new(Mutex::new(false));
    let omit = missing_receipt.clone();
    let stage_tokens = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let presented_tokens = stage_tokens.clone();
    let registry = Arc::new(boss_jobs::credentials::InMemoryCredentials::new(vec![
        boss_jobs::credentials::CredentialRow {
            id: CREDENTIAL.into(),
            kind: "ops-runner-credential".into(),
            issuer: "credential-broker".into(),
            principal: "runner:ops".into(),
            scopes: json!([]),
            storage_location: "test Secret".into(),
            consumers: json!([]),
            rotation_policy: "on-demand".into(),
            rotated_at: None,
            notes: String::new(),
        },
    ]));
    let delivery_registry = registry.clone();
    let get_registry = registry.clone();
    let canonical_registry_row = serde_json::to_value(
        boss_jobs::credentials::CredentialsRegistry::get(registry.as_ref(), CREDENTIAL)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let registry_response = Arc::new(Mutex::new(None::<JsonValue>));
    let response_override = registry_response.clone();
    let delivered_by = Arc::new(Mutex::new(Some(DEPOSIT_ACTOR.to_string())));
    let by_get = delivered_by.clone();
    let app = Router::new()
        .route(
            "/api/jobs/{id}",
            get(move |Path(id): Path<String>| {
                let st = st_get.clone();
                let by = by_get.lock().unwrap().clone();
                async move {
                    let steps: Vec<JsonValue> = st
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(slug, status)| {
                            let completed_by = (slug == "delivered").then(|| by.clone()).flatten();
                            json!({"id": format!("step-{slug}"), "spec_slug": slug, "status": status, "completed_by": completed_by})
                        })
                        .collect();
                    Json(json!({"id": id, "metadata": {}, "steps": steps}))
                }
            }),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}",
            put(
                move |Path((id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                    let (st, w) = (st_put.clone(), w_put.clone());
                    async move {
                        if let Some(refused) =
                            crate::handlers::listing_stub::end_state_step_put(&id, &sid, &body)
                        {
                            return refused;
                        }
                        if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
                            let slug = slug_of(&sid);
                            if let Some(row) = st.lock().unwrap().iter_mut().find(|(s, _)| *s == slug)
                            {
                                row.1 = status.to_string();
                            }
                        }
                        w.lock().unwrap().push((sid, body));
                        Json(json!({"ok": true})).into_response()
                    }
                },
            ),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}/metadata",
            axum::routing::patch(
                move |Path((_id, sid)): Path<(String, String)>, Json(body): Json<JsonValue>| {
                    let w = w_merge.clone();
                    async move {
                        w.lock().unwrap().push((format!("{sid}/metadata"), body));
                        Json(json!({"ok": true}))
                    }
                },
            ),
        )
        .route(
            "/api/credentials/{id}/rotation/{phase}",
            post(
                move |Path((id, phase)): Path<(String, String)>, headers:axum::http::HeaderMap, Json(body): Json<JsonValue>| {
                    let rot = rot.clone();
                    let fault = fault.clone();
                    let attempts = phase_attempts.clone();
                    let registry=registry.clone();
                    let omit=omit.clone();
                    let presented_tokens = presented_tokens.clone();
                    async move {
                        attempts.lock().unwrap().push((format!("{id}/{phase}"), body.clone()));
                        let stage_token = headers
                            .get(boss_jobs::credentials::broker_stage::HEADER)
                            .map(|v| v.to_str().unwrap().to_owned());
                        presented_tokens.lock().unwrap().push(stage_token.clone());
                        let before = fault.lock().unwrap().as_deref() == Some(&format!("before:{phase}"));
                        if before {
                            *fault.lock().unwrap() = None;
                            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        use boss_jobs::credentials::CredentialsRegistry;
                        let user: boss_policy_client::User = serde_json::from_str(headers.get("x-boss-user").unwrap().to_str().unwrap()).unwrap();
                        // As the real door: a presented (there, verified) stage
                        // token is recorded as the workload, not the typed label.
                        let actor = match stage_token {
                            Some(_) => boss_core::actor::ActorId::Automation(
                                boss_jobs::credentials::broker_stage::ACTOR
                                    .trim_start_matches("automation:")
                                    .into(),
                            ),
                            None => user.ambient_actor().unwrap(),
                        };
                        let stamp=boss_core::publisher::EventStamp::new("jobs",actor);
                        let mut evidence=body.clone();
                        evidence["credential_id"]=json!(id);
                        let result=registry.record_rotation(&id,RotationPhase::parse(&phase).unwrap(),evidence,&stamp).await;
                        let outcome=match result {
                            Ok(outcome)=>outcome,
                            Err(error)=>return (axum::http::StatusCode::CONFLICT,error.to_string()).into_response(),
                        };
                        if !matches!(outcome,boss_jobs::credentials::receipt::RotationOutcome::Replayed { .. }) {
                            rot.lock().unwrap().push((format!("{id}/{phase}"),body));
                        }
                        if *omit.lock().unwrap() { return Json(json!({"recorded":true})).into_response(); }
                        if fault.lock().unwrap().as_deref()==Some(&format!("after:{phase}")) {
                            *fault.lock().unwrap()=None;
                            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        Json(json!({"recorded":true,"observation":outcome})).into_response()
                    }
                },
            ),
        )
        .route(
            "/api/credentials/{id}",
            get(move |Path(id): Path<String>| {
                let registry = get_registry.clone();
                let response_override = response_override.clone();
                async move {
                    if let Some(body) = response_override.lock().unwrap().clone() {
                        return Json(body).into_response();
                    }
                    use boss_jobs::credentials::CredentialsRegistry;
                    match registry.get(&id).await.unwrap() {
                        Some(row) => Json(row).into_response(),
                        None => axum::http::StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        );
    // THE REAL DOOR, over the directory the Secret projects into.
    let app = app.merge(boss_jobs::credentials::http::delivery_router(
        boss_jobs::credentials::http::CredentialsApiState::new(delivery_registry),
    ));
    let app = boss_jobs::runner_credential::mount(app, dir);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Jobs {
        url: format!("http://{addr}"),
        steps,
        writes,
        rotations,
        delivered_by,
        phase_fault,
        attempts,
        missing_receipt,
        registry_response,
        canonical_registry_row,
        stage_tokens,
    }
}

#[tokio::test]
async fn a_foreign_or_incomplete_registry_row_cannot_mint_a_runner_credential() {
    let mut observations = Vec::new();
    for variant in [
        "kind",
        "principal",
        "id",
        "missing-id",
        "malformed",
        "canonical",
    ] {
        let secrets = MountedSecret::new(&format!("registry-{variant}"));
        secrets.seed("forge.current", OLD);
        let jobs = jobs(secrets.dir.clone(), FRESH).await;
        let mut row = jobs.canonical_registry_row.clone();
        match variant {
            "kind" => row["kind"] = json!("forgejo-access-token"),
            "principal" => row["principal"] = json!("runner:other"),
            "id" => row["id"] = json!("another-credential"),
            "missing-id" => {
                row.as_object_mut().unwrap().remove("id");
            }
            "malformed" => row = json!(["not a registry row"]),
            "canonical" => {}
            _ => unreachable!(),
        }
        *jobs.registry_response.lock().unwrap() = Some(row);
        let original = secrets.map.lock().unwrap().clone();
        let result = handler(&jobs, &secrets).invoke(&args(), &scope()).await;
        observations.push((
            variant,
            result.is_err(),
            secrets.writes(),
            *secrets.map.lock().unwrap() == original,
            jobs.phases().is_empty(),
        ));
    }
    assert!(
        observations
            .iter()
            .all(|(variant, refused, writes, conserved, no_phase)| {
                if *variant == "canonical" {
                    !*refused && *writes == 1 && !*conserved && !*no_phase
                } else {
                    *refused && *writes == 0 && *conserved && *no_phase
                }
            }),
        "registry identity must refuse before mint or Secret effects: {observations:?}"
    );
}

const FRESH: &[(&str, &str)] = &[
    ("scope", "completed"),
    ("issue", "ready"),
    ("install", "pending"),
    ("verify", "pending"),
    ("delivered", "pending"),
    ("revoke", "pending"),
];

// ----- wiring -----

const NO_WAIT: ResolvePoll = ResolvePoll {
    attempts: 1,
    interval: Duration::ZERO,
};

fn handler(jobs: &Jobs, secrets: &Arc<MountedSecret>) -> Arc<CredentialRotateOpsRunner> {
    CredentialRotateOpsRunner::with_poll(jobs.url.clone(), secrets.clone(), NO_WAIT)
}

fn args_for(credential: &str, host: &str) -> Vec<(String, Value)> {
    [
        ("secret_namespace", NS),
        ("secret_name", SECRET),
        ("credential_id", credential),
        ("host", host),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), Value::String((*v).into())))
    .collect()
}

fn args() -> Vec<(String, Value)> {
    args_for(CREDENTIAL, HOST)
}

fn ctx(kind: &str, metadata: JsonValue) -> InvocationContext {
    ctx_for(JOB, kind, metadata)
}

fn ctx_for(job: &str, kind: &str, metadata: JsonValue) -> InvocationContext {
    InvocationContext {
        event_timestamp: None,
        rule_name: "broker-rotates-the-ops-runner-credential-on-the-forge".into(),
        triggering_event_id: format!("evt-{kind}"),
        triggering_topic: format!("step.done.{kind}"),
        event_payload: json!({
            "job_id": job,
            "step_id": format!("step-{kind}"),
            "kind": kind,
            "subject_kind": "custom",
            "subject_id": CREDENTIAL,
            "metadata": metadata,
        }),
    }
}

fn scope() -> InvocationContext {
    ctx("credential-rotation", json!({}))
}

fn delivered(l8: &str) -> InvocationContext {
    ctx(
        "credential-delivery",
        json!({"delivered_last_eight": l8, "delivered_to": "forge:/etc/boss/ops-runner.credential"}),
    )
}

/// What the real door answers for `value`, read straight off the
/// directory the jobs API mounts.
fn door(secrets: &MountedSecret, value: &str) -> boss_jobs::runner_credential::Resolution {
    boss_jobs::runner_credential::resolve(&secrets.dir, value)
}

fn resolved(host: &str, slot: &'static str) -> boss_jobs::runner_credential::Resolution {
    boss_jobs::runner_credential::Resolution::Resolved {
        host: host.into(),
        slot,
    }
}

/// A rotation of the forge's credential, staged: the scope firing ran and
/// the door sees the staged value. `boss-gcp` holds a value of its own.
async fn staged_unacknowledged(name: &str) -> (Jobs, Arc<MountedSecret>, String) {
    let secrets = MountedSecret::new(name);
    secrets.seed("forge.current", OLD);
    secrets.seed("forge.previous", LEFT);
    secrets.seed("boss-gcp.current", GCP);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    handler(&jobs, &secrets)
        .invoke(&args(), &scope())
        .await
        .expect("the scope firing stages and verifies");
    let value = secrets.get("forge.next").expect("a value staged");
    (jobs, secrets, value)
}

/// This fixture calls the actual mounted-generation resolver and credential
/// owner command. Its fake Jobs row is not proof of the production Jobs CAS.
async fn acknowledge(jobs: &Jobs, secrets: &MountedSecret, job: &str, value: &str) {
    let local = secrets.dir.join("fixture-local-installed");
    boss_testing::write_file(&local, value);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&local).unwrap(), value);
    let client = boss_core::machine_token::Client::unstamped(reqwest::Client::builder())
        .expect("fake loopback fixture uses no estate credential");
    let response = client
        .get(format!("{}{WHOAMI_PATH}", jobs.url))
        .header("x-boss-user", dispatcher_reader_header())
        .header(HEADER, value)
        .send()
        .await
        .unwrap();
    let identity: JsonValue = response.json().await.unwrap();
    let context = &identity["delivery"];
    assert_eq!(context["job_id"], job);
    let response = client
        .post(format!(
            "{}/api/credentials/{CREDENTIAL}/delivery",
            jobs.url
        ))
        .header("x-boss-user", dispatcher_reader_header())
        .header(HEADER, value)
        .json(&json!({"job_id":job,"attempt":context["attempt"],
            "secret_uid":context["secret_uid"]}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: JsonValue = response.json().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "{body}");
    *jobs.delivered_by.lock().unwrap() = Some(boss_jobs::runner_credential::ACTOR.into());
    for (slug, status) in jobs.steps.lock().unwrap().iter_mut() {
        if slug == "delivered" {
            *status = "completed".into();
        }
    }
}

async fn staged(name: &str) -> (Jobs, Arc<MountedSecret>, String) {
    let (jobs, secrets, value) = staged_unacknowledged(name).await;
    acknowledge(&jobs, &secrets, JOB, &value).await;
    (jobs, secrets, value)
}

// ----- pure -----

#[test]
fn a_fresh_value_is_the_machine_tokens_shape_and_never_repeats() {
    let (a, b) = (fresh_value(), fresh_value());
    assert_eq!(a.len(), 43, "32 bytes, base64url, unpadded");
    assert!(
        a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "a header-safe alphabet, no whitespace: {a}"
    );
    assert_ne!(a, b);
}

#[test]
fn the_provenance_beside_a_slot_is_no_slot_the_door_reads() {
    assert_eq!(slot_of(&slot_key("forge", "next")), Some(("forge", "next")));
    assert_eq!(slot_of(&minted_for_key("forge", "next")), None);
    assert_eq!(slot_of(&minted_for_key("forge", "current")), None);
}

#[test]
fn a_promotion_takes_only_this_packets_staged_value_and_only_the_one_delivered() {
    let held = Held {
        current: Some(OLD.into()),
        next: Some("stagedSTAGEDstagedSTAGEDstagedSTAGEDab12cd34".into()),
        next_for: Some(JOB.into()),
        ..Held::default()
    };
    assert_eq!(
        plan_promotion(&held, JOB, "ab12cd34", HOST),
        Ok(Promotion::Promote {
            value: "stagedSTAGEDstagedSTAGEDstagedSTAGEDab12cd34".into()
        })
    );
    let err = plan_promotion(&held, JOB, "00000000", HOST).unwrap_err();
    assert!(
        err.contains("00000000") && err.contains("ab12cd34"),
        "{err}"
    );
    let err = plan_promotion(&held, OTHER_JOB, "ab12cd34", HOST).unwrap_err();
    assert!(
        err.contains(JOB),
        "names the packet the value is staged for: {err}"
    );

    let done = Held {
        current: Some("V".into()),
        current_for: Some(JOB.into()),
        ..Held::default()
    };
    assert_eq!(
        plan_promotion(&done, JOB, "anything", HOST),
        Ok(Promotion::Done { value: "V".into() })
    );
}

#[test]
fn the_scope_may_name_the_old_value_only_by_currents_last_eight() {
    assert!(judge_scope_naming(None, None, Some(OLD), HOST).is_ok());
    assert!(judge_scope_naming(None, Some(last_eight(OLD)), Some(OLD), HOST).is_ok());
    assert!(judge_scope_naming(None, Some("zzzzzzzz"), Some(OLD), HOST).is_err());
    assert!(judge_scope_naming(None, Some("zzzzzzzz"), None, HOST).is_err());
    assert!(judge_scope_naming(Some("a-name"), None, Some(OLD), HOST).is_err());
}

// ----- the scope firing -----

#[tokio::test]
async fn the_scope_firing_stages_a_value_the_door_resolves_and_leaves_current_valid() {
    let (jobs, secrets, value) = staged_unacknowledged("stage").await;

    assert_eq!(value.len(), 43);
    assert_eq!(secrets.get("forge.next.minted-for").as_deref(), Some(JOB));
    assert_eq!(secrets.get("forge.current").as_deref(), Some(OLD));
    assert_eq!(door(&secrets, &value), resolved("forge", "next"));
    assert_eq!(
        door(&secrets, OLD),
        resolved("forge", "current"),
        "the runner's held value keeps resolving until the host has the new one"
    );
    assert_eq!(door(&secrets, GCP), resolved("boss-gcp", "current"));

    for slug in ["issue", "install", "verify"] {
        assert_eq!(jobs.status(slug), "completed", "{slug}");
    }
    assert_eq!(jobs.status("delivered"), "pending", "the host's to record");
    assert_eq!(
        jobs.status("revoke"),
        "pending",
        "nothing revoked before delivery"
    );
    assert_eq!(
        jobs.step_fields("install")["delivery"],
        DELIVERY_OFF_HOST,
        "what readies the packet's `delivered` step"
    );
    assert!(
        jobs.step_fields("verify")["verified"]
            .as_str()
            .unwrap()
            .contains("resolved to host forge, slot next"),
        "{}",
        jobs.step_fields("verify")
    );
    assert_eq!(
        jobs.phases(),
        vec![
            format!("{CREDENTIAL}/minted"),
            format!("{CREDENTIAL}/installed"),
            format!("{CREDENTIAL}/verified")
        ]
    );
    let written = jobs.everything_written();
    assert!(
        !written.contains(&value),
        "the value reached the record:\n{written}"
    );
    assert!(!written.contains(OLD), "the old value reached the record");
}

#[tokio::test]
async fn a_value_the_door_cannot_see_yet_is_not_yet_and_its_redelivery_mints_nothing() {
    let secrets = MountedSecret::new("lag");
    secrets.seed("forge.current", OLD);
    secrets.lag(true);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let h = handler(&jobs, &secrets);

    let err = h
        .invoke(&args(), &scope())
        .await
        .expect_err("not propagated");
    assert!(
        !err.is_permanent() && err.to_string().contains("not yet"),
        "a transient the redelivery schedule carries: {err}"
    );
    let value = secrets.get("forge.next").expect("staged");
    assert_eq!(jobs.status("install"), "completed", "install is on record");
    assert_ne!(jobs.status("verify"), "completed");

    secrets.propagate();
    h.invoke(&args(), &scope())
        .await
        .expect("the redelivery verifies");
    assert_eq!(
        secrets.get("forge.next").as_deref(),
        Some(value.as_str()),
        "the redelivery found its own value staged and minted nothing"
    );
    assert_eq!(secrets.writes(), 1, "one write across both firings");
    assert_eq!(
        jobs.phases()
            .iter()
            .filter(|p| p.ends_with("/minted"))
            .count(),
        1
    );
    assert_eq!(jobs.status("verify"), "completed");
}

/// The door resolving the staged value to ANOTHER host means the mounted
/// slots disagree with the declaration — a fault no redelivery repairs, so
/// it is refused by name, and nothing is promoted.
#[tokio::test]
async fn a_staged_value_the_door_names_another_host_by_is_refused_permanently() {
    let secrets = MountedSecret::new("wrong-host");
    secrets.lag(true);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let h = handler(&jobs, &secrets);
    let _ = h.invoke(&args(), &scope()).await.expect_err("not yet");
    let value = secrets.get("forge.next").unwrap();
    // The mount the jobs API reads holds this value under boss-gcp's slot.
    boss_testing::write_file(&secrets.dir.join("boss-gcp.next"), &value);
    let err = h
        .invoke(&args(), &scope())
        .await
        .expect_err("another host's");
    assert!(
        err.is_permanent() && err.to_string().contains("boss-gcp"),
        "{err}"
    );
    assert_ne!(jobs.status("verify"), "completed");
}

#[tokio::test]
async fn the_scope_naming_a_value_the_host_does_not_hold_mints_nothing() {
    let secrets = MountedSecret::new("naming");
    secrets.seed("forge.current", OLD);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let err = handler(&jobs, &secrets)
        .invoke(
            &args(),
            &ctx(
                "credential-rotation",
                json!({"old_token_last_eight": "zzzzzzzz"}),
            ),
        )
        .await
        .expect_err("refused");
    assert!(err.is_permanent(), "{err}");
    assert_eq!(secrets.writes(), 0);
    assert!(jobs.phases().is_empty());
}

#[tokio::test]
async fn an_undeclared_registry_row_mints_nothing() {
    let secrets = MountedSecret::new("undeclared");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let mut c = scope();
    c.event_payload["subject_id"] = json!("ops-runner-credential-w-1");
    let err = handler(&jobs, &secrets)
        .invoke(&args_for("ops-runner-credential-w-1", "w-1"), &c)
        .await
        .expect_err("no row");
    assert!(
        err.is_permanent() && err.to_string().contains("registry row"),
        "{err}"
    );
    assert_eq!(secrets.writes(), 0);
}

#[tokio::test]
async fn a_declaration_the_door_could_not_read_or_a_subject_it_does_not_name_is_refused() {
    let secrets = MountedSecret::new("decl");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    for host in ["Forge", "forge.next", "-forge", ""] {
        let err = handler(&jobs, &secrets)
            .invoke(&args_for(CREDENTIAL, host), &scope())
            .await
            .expect_err(host);
        assert!(err.is_permanent(), "{host}: {err}");
    }
    let mut c = scope();
    c.event_payload["subject_id"] = json!("ops-runner-credential-boss-gcp");
    let err = handler(&jobs, &secrets)
        .invoke(&args(), &c)
        .await
        .expect_err("another credential's packet");
    assert!(err.is_permanent(), "{err}");
    assert_eq!(secrets.writes(), 0);
}

// ----- the delivery firing -----

#[tokio::test]
async fn a_forged_deposit_label_without_an_authenticated_owner_receipt_promotes_nothing() {
    let mut accepted = Vec::new();
    for actor in [DEPOSIT_ACTOR, boss_jobs::runner_credential::ACTOR] {
        let (jobs, secrets, value) = staged_unacknowledged("forged-label-no-owner").await;
        *jobs.delivered_by.lock().unwrap() = Some(actor.into());
        let before = secrets.writes();
        let result = handler(&jobs, &secrets)
            .invoke(&args(), &delivered(last_eight(&value)))
            .await;
        if result.is_ok() {
            accepted.push(actor);
        } else {
            assert_eq!(secrets.writes(), before);
            assert_eq!(secrets.get("forge.current").as_deref(), Some(OLD));
            assert_ne!(jobs.status("revoke"), "completed");
        }
    }
    assert!(
        accepted.is_empty(),
        "typed completer labels are not host installation: {accepted:?}"
    );
}

#[tokio::test]
async fn a_recorded_delivery_promotes_the_value_and_the_old_one_resolves_nothing() {
    let (jobs, secrets, value) = staged("promote").await;
    handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("promoted");

    assert_eq!(
        secrets.get("forge.current").as_deref(),
        Some(value.as_str())
    );
    assert_eq!(
        secrets.get("forge.current.minted-for").as_deref(),
        Some(JOB)
    );
    assert_eq!(secrets.get("forge.next"), None, "next is blank");
    assert_eq!(secrets.get("forge.previous"), None, "previous is cleared");
    assert_eq!(door(&secrets, &value), resolved("forge", "current"));
    assert_eq!(
        door(&secrets, OLD),
        boss_jobs::runner_credential::Resolution::Unmatched,
        "the old value is dead"
    );
    assert_eq!(
        door(&secrets, GCP),
        resolved("boss-gcp", "current"),
        "another host's slots are not this rotation's"
    );
    assert_eq!(jobs.status("revoke"), "completed");
    let revoke = jobs.step_fields("revoke");
    assert!(
        revoke["confirmed_dead"]
            .as_str()
            .unwrap()
            .contains("resolved to host forge, slot current")
            && revoke["confirmed_dead"]
                .as_str()
                .unwrap()
                .contains(&format!("(…{}) answered resolved: false", last_eight(OLD))),
        "{revoke}"
    );
    assert_eq!(
        jobs.phases().last().map(String::as_str),
        Some(format!("{CREDENTIAL}/revoked").as_str())
    );
    let written = jobs.everything_written();
    assert!(
        !written.contains(&value) && !written.contains(OLD),
        "{written}"
    );
}

#[tokio::test]
async fn a_delivery_of_another_value_promotes_nothing() {
    let (jobs, secrets, value) = staged("wrong-delivery").await;
    let before = secrets.writes();
    let err = handler(&jobs, &secrets)
        .invoke(&args(), &delivered("00000000"))
        .await
        .expect_err("the host delivered something the Secret does not stage");
    assert!(err.is_permanent(), "{err}");
    assert_eq!(secrets.writes(), before);
    assert_eq!(secrets.get("forge.current").as_deref(), Some(OLD));
    assert_eq!(secrets.get("forge.next").as_deref(), Some(value.as_str()));
    assert_ne!(jobs.status("revoke"), "completed");
}

#[tokio::test]
async fn a_promotion_the_mount_has_not_refreshed_is_not_yet_and_its_redelivery_finishes() {
    let (jobs, secrets, value) = staged("promote-lag").await;
    secrets.lag(true);
    let h = handler(&jobs, &secrets);
    let err = h
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect_err("the door still reads the staged slots");
    assert!(
        !err.is_permanent() && err.to_string().contains("not yet"),
        "{err}"
    );
    assert_eq!(
        secrets.get("forge.current").as_deref(),
        Some(value.as_str())
    );
    assert_ne!(
        jobs.status("revoke"),
        "completed",
        "no revoke without the effect"
    );
    let writes = secrets.writes();

    secrets.propagate();
    acknowledge(&jobs, &secrets, JOB, &value).await;
    h.invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("the redelivery confirms");
    assert_eq!(secrets.writes(), writes, "promoted once");
    assert_eq!(jobs.status("revoke"), "completed");
    assert_eq!(
        door(&secrets, OLD),
        boss_jobs::runner_credential::Resolution::Unmatched
    );

    // A finished rotation, redelivered: nothing written, nothing recorded.
    let phases = jobs.phases().len();
    h.invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("a no-op");
    assert_eq!(secrets.writes(), writes);
    assert_eq!(jobs.phases().len(), phases);
}

#[tokio::test]
async fn the_first_rotation_of_a_host_promotes_over_nothing() {
    let secrets = MountedSecret::new("first");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let h = handler(&jobs, &secrets);
    h.invoke(&args(), &scope()).await.expect("staged");
    let value = secrets.get("forge.next").unwrap();
    acknowledge(&jobs, &secrets, JOB, &value).await;
    h.invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("promoted");
    assert_eq!(door(&secrets, &value), resolved("forge", "current"));
    assert!(
        jobs.step_fields("revoke")["revoked"]
            .as_str()
            .unwrap()
            .contains("held no current value"),
        "{}",
        jobs.step_fields("revoke")
    );
}

#[tokio::test]
async fn a_delivery_whose_verify_never_landed_verifies_before_it_promotes() {
    let secrets = MountedSecret::new("late-verify");
    secrets.seed("forge.current", OLD);
    secrets.lag(true);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let h = handler(&jobs, &secrets);
    let _ = h.invoke(&args(), &scope()).await.expect_err("not yet");
    let value = secrets.get("forge.next").unwrap();
    // The scope firing's redeliveries ran out; the host's deposit proved
    // the value through the door later and recorded delivery.
    secrets.propagate();
    acknowledge(&jobs, &secrets, JOB, &value).await;
    h.invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("verified, then promoted");
    assert_eq!(jobs.status("verify"), "completed");
    assert_eq!(jobs.status("revoke"), "completed");
}

// ----- review 3930a3eb -----

/// F1 (blocking). The reviewer's scenario: P1 stages v1, the forge's
/// deposit proves and INSTALLS v1 (it installs `next` as soon as the door
/// resolves it), and before P1 is promoted P2 is scoped. P2's stage used
/// to write `next` = v2 over v1 and keep it nowhere, so the file the
/// forge presents resolved to no slot until the next converge installed
/// v2. The stage now carries the displaced value into `previous` in the
/// same merge-patch — the door resolves `previous`, and the promotion
/// blanks it, so it still dies with the next completed rotation.
#[tokio::test]
async fn review_a_second_stage_keeps_the_value_the_host_already_installed() {
    let (_p1, secrets, v1) = staged("second-stage").await;
    // The forge's deposit installed v1: it is what the runner presents.
    let installed = v1.clone();

    let p2 = jobs(secrets.dir.clone(), FRESH).await;
    handler(&p2, &secrets)
        .invoke(
            &args(),
            &ctx_for(OTHER_JOB, "credential-rotation", json!({})),
        )
        .await
        .expect("the second packet stages");
    let v2 = secrets.get("forge.next").expect("v2 staged");
    assert_ne!(v2, v1);
    assert_eq!(
        secrets.get("forge.next.minted-for").as_deref(),
        Some(OTHER_JOB)
    );
    assert_eq!(
        door(&secrets, &installed),
        resolved("forge", "previous"),
        "the value the forge already installed keeps resolving after a second stage"
    );
    assert_eq!(door(&secrets, &v2), resolved("forge", "next"));
    assert_eq!(door(&secrets, OLD), resolved("forge", "current"));
    assert_eq!(secrets.writes(), 2, "one merge-patch per stage");

    acknowledge(&p2, &secrets, OTHER_JOB, &v2).await;
    // P2 completes: every value but v2 dies, the carried one included.
    handler(&p2, &secrets)
        .invoke(
            &args(),
            &ctx_for(
                OTHER_JOB,
                "credential-delivery",
                json!({"delivered_last_eight": last_eight(&v2)}),
            ),
        )
        .await
        .expect("promoted");
    assert_eq!(door(&secrets, &v2), resolved("forge", "current"));
    for dead in [installed.as_str(), OLD] {
        assert_eq!(
            door(&secrets, dead),
            boss_jobs::runner_credential::Resolution::Unmatched
        );
    }
    assert!(
        p2.step_fields("revoke")["confirmed_dead"]
            .as_str()
            .unwrap()
            .contains(&format!(
                "(…{}) answered resolved: false",
                last_eight(&installed)
            )),
        "the carried value is confirmed dead too: {}",
        p2.step_fields("revoke")
    );
}

/// R1: a third stage can retire the value still installed on a host.
/// The installed event must name that retirement without storing a secret.
#[tokio::test]
async fn a_third_stage_records_the_previous_value_it_drops() {
    let (_p1, secrets, v1) = staged("third-stage-drop").await;
    let p2 = jobs(secrets.dir.clone(), FRESH).await;
    handler(&p2, &secrets)
        .invoke(
            &args(),
            &ctx_for(OTHER_JOB, "credential-rotation", json!({})),
        )
        .await
        .expect("second stage");
    let v2 = secrets.get("forge.next").expect("second staged value");
    let p3 = jobs(secrets.dir.clone(), FRESH).await;
    handler(&p3, &secrets)
        .invoke(
            &args(),
            &ctx_for(THIRD_JOB, "credential-rotation", json!({})),
        )
        .await
        .expect("third stage");
    assert_eq!(secrets.get("forge.previous"), Some(v2.clone()));
    assert_eq!(
        door(&secrets, &v1),
        boss_jobs::runner_credential::Resolution::Unmatched
    );
    let rotations = p3.rotations.lock().unwrap();
    let installed = rotations
        .iter()
        .find(|(phase, _)| phase == &format!("{CREDENTIAL}/installed"))
        .expect("installed event");
    assert_eq!(installed.1["dropped_previous"], last_eight(&v1));
    assert_eq!(installed.1["carried_to_previous"], last_eight(&v2));
    assert!(!serde_json::to_string(&installed.1).unwrap().contains(&v1));
    assert!(!serde_json::to_string(&installed.1).unwrap().contains(&v2));
}

#[tokio::test]
async fn a_lost_installed_post_replays_the_original_dropped_previous_observation() {
    let (_p1, secrets, v1) = staged("lost-installed-original").await;
    let p2 = jobs(secrets.dir.clone(), FRESH).await;
    handler(&p2, &secrets)
        .invoke(
            &args(),
            &ctx_for(OTHER_JOB, "credential-rotation", json!({})),
        )
        .await
        .unwrap();
    let v2 = secrets.get("forge.next").unwrap();
    let p3 = jobs(secrets.dir.clone(), FRESH).await;
    *p3.phase_fault.lock().unwrap() = Some("before:installed".into());
    assert!(
        handler(&p3, &secrets)
            .invoke(
                &args(),
                &ctx_for(THIRD_JOB, "credential-rotation", json!({}))
            )
            .await
            .is_err()
    );
    let v3 = secrets.get("forge.next").unwrap();
    let writes = secrets.writes();
    handler(&p3, &secrets)
        .invoke(
            &args(),
            &ctx_for(THIRD_JOB, "credential-rotation", json!({})),
        )
        .await
        .unwrap();
    let attempts = p3.attempts.lock().unwrap();
    let installed: Vec<_> = attempts
        .iter()
        .filter(|(phase, _)| phase.ends_with("/installed"))
        .collect();
    assert_eq!(
        installed.len(),
        2,
        "Secret commit does not prove the refused phase POST committed"
    );
    assert_eq!(
        installed[0].1, installed[1].1,
        "replay the complete original command"
    );
    assert_eq!(installed[1].1["dropped_previous"], last_eight(&v1));
    assert_eq!(installed[1].1["carried_to_previous"], last_eight(&v2));
    assert_eq!(secrets.get("forge.next"), Some(v3));
    assert_eq!(
        secrets.writes(),
        writes,
        "redelivery never remints or rewrites slots"
    );
    assert_eq!(p3.status("install"), "completed");
}

#[tokio::test]
async fn promotion_done_replays_original_old_and_carried_history_after_restart() {
    let (jobs, secrets, value) = staged("promotion-original-history").await;
    *jobs.phase_fault.lock().unwrap() = Some("before:revoked".into());
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &delivered(last_eight(&value)))
            .await
            .is_err()
    );
    let writes = secrets.writes();
    handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .unwrap();
    let attempts = jobs.attempts.lock().unwrap();
    let revoked: Vec<_> = attempts
        .iter()
        .filter(|(phase, _)| phase.ends_with("/revoked"))
        .collect();
    assert_eq!(revoked.len(), 2);
    assert_eq!(
        revoked[0].1, revoked[1].1,
        "Promotion::Done must preserve the original full command"
    );
    assert_eq!(revoked[1].1["old_last_eight"], last_eight(OLD));
    assert_eq!(secrets.writes(), writes, "restart does not promote twice");
    assert_eq!(jobs.status("revoke"), "completed");
}

#[tokio::test]
async fn a_success_status_without_the_original_owner_receipt_never_completes_issue() {
    let secrets = MountedSecret::new("missing-owner-receipt");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    *jobs.missing_receipt.lock().unwrap() = true;
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err(),
        "silence is not an owner receipt"
    );
    assert_ne!(jobs.status("issue"), "completed");
}

#[tokio::test]
async fn a_superseded_unacknowledged_candidate_is_refused_instead_of_reminted() {
    let secrets = MountedSecret::new("superseded-unacknowledged");
    let p1 = jobs(secrets.dir.clone(), FRESH).await;
    *p1.phase_fault.lock().unwrap() = Some("before:installed".into());
    assert!(
        handler(&p1, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err()
    );
    let p2 = jobs(secrets.dir.clone(), FRESH).await;
    handler(&p2, &secrets)
        .invoke(
            &args(),
            &ctx_for(OTHER_JOB, "credential-rotation", json!({})),
        )
        .await
        .unwrap();
    let next = secrets.get("forge.next");
    let writes = secrets.writes();
    assert!(
        handler(&p1, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err(),
        "unknown original effect must remain pending"
    );
    assert_eq!(
        secrets.writes(),
        writes,
        "redelivery cannot silently start a new attempt"
    );
    assert_eq!(secrets.get("forge.next"), next);
    assert_ne!(p1.status("install"), "completed");
}

#[tokio::test]
async fn lost_secret_ack_and_committed_phase_ack_are_read_back_without_duplicate_effects() {
    let secrets = MountedSecret::new("lost-secret-and-phase-acks");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    *secrets.lost_write_ack.lock().unwrap() = true;
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err()
    );
    let candidate = secrets.get("forge.next").unwrap();
    assert!(
        jobs.phases().is_empty(),
        "a lost Secret acknowledgment is not a claimed completed mint"
    );
    *jobs.phase_fault.lock().unwrap() = Some("after:installed".into());
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err()
    );
    assert_ne!(jobs.status("install"), "completed");
    handler(&jobs, &secrets)
        .invoke(&args(), &scope())
        .await
        .unwrap();
    assert_eq!(secrets.get("forge.next"), Some(candidate.clone()));
    assert_eq!(
        secrets.writes(),
        1,
        "readback and receipt replay never mint or install twice"
    );
    assert_eq!(
        jobs.phases(),
        [
            format!("{CREDENTIAL}/minted"),
            format!("{CREDENTIAL}/installed"),
            format!("{CREDENTIAL}/verified")
        ]
    );
    acknowledge(&jobs, &secrets, JOB, &candidate).await;
    *secrets.lost_write_ack.lock().unwrap() = true;
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &delivered(last_eight(&candidate)))
            .await
            .is_err()
    );
    *jobs.phase_fault.lock().unwrap() = Some("after:revoked".into());
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &delivered(last_eight(&candidate)))
            .await
            .is_err()
    );
    handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&candidate)))
        .await
        .unwrap();
    assert_eq!(
        secrets.writes(),
        2,
        "promotion with lost acknowledgment is not repeated"
    );
    assert_eq!(
        jobs.phases()
            .iter()
            .filter(|phase| phase.ends_with("/revoked"))
            .count(),
        1
    );
    assert_eq!(jobs.status("revoke"), "completed");
    for value in [OLD, candidate.as_str()] {
        assert!(!jobs.everything_written().contains(value));
    }
}

#[tokio::test]
async fn malformed_foreign_missing_and_changed_uid_witnesses_never_promote() {
    for (field, value) in [
        ("job_id", json!(OTHER_JOB)),
        ("uid", json!("replacement-uid")),
        ("credential_id", json!("foreign")),
        ("commands", json!({})),
        ("version", json!(2)),
    ] {
        let (jobs, secrets, candidate) = staged(&format!("refused-witness-{field}")).await;
        let key = witness_key(HOST, JOB);
        let mut witness: JsonValue = serde_json::from_str(&secrets.get(&key).unwrap()).unwrap();
        witness[field] = value;
        secrets.seed(&key, &serde_json::to_string(&witness).unwrap());
        let before = secrets.writes();
        assert!(
            handler(&jobs, &secrets)
                .invoke(&args(), &delivered(last_eight(&candidate)))
                .await
                .is_err()
        );
        assert_eq!(secrets.writes(), before);
        assert_eq!(door(&secrets, OLD), resolved(HOST, "current"));
        assert_ne!(jobs.status("revoke"), "completed");
    }
    for damaged in ["", "not-json"] {
        let (jobs, secrets, candidate) = staged("missing-malformed-witness").await;
        secrets.seed(&witness_key(HOST, JOB), damaged);
        let before = secrets.writes();
        assert!(
            handler(&jobs, &secrets)
                .invoke(&args(), &delivered(last_eight(&candidate)))
                .await
                .is_err()
        );
        assert_eq!(secrets.writes(), before);
    }
}

#[tokio::test]
async fn malformed_original_phase_commands_refuse_before_the_first_owner_post() {
    let secrets = MountedSecret::new("malformed-original-command");
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    *secrets.lost_write_ack.lock().unwrap() = true;
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err()
    );
    let key = witness_key(HOST, JOB);
    let mut witness: JsonValue = serde_json::from_str(&secrets.get(&key).unwrap()).unwrap();
    witness["commands"]["installed"]["secret_uid"] = JsonValue::Null;
    secrets.seed(&key, &serde_json::to_string(&witness).unwrap());
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &scope())
            .await
            .is_err(),
        "malformed command cannot become an original owner fact"
    );
    assert!(jobs.phases().is_empty());
    assert_ne!(jobs.status("install"), "completed");
}

#[tokio::test]
async fn distinct_registered_stage_and_delivery_actors_converge_without_impersonating_each_other() {
    let rule_name = |path: &str| {
        let raw = std::fs::read_to_string(boss_testing::repo_root().join(path)).unwrap();
        let rule: toml::Value = toml::from_str(&raw).unwrap();
        rule["rule"][0]["name"].as_str().unwrap().to_string()
    };
    let stage_rule =
        rule_name("infra/dispatcher/rules/broker-rotates-the-forge-ops-runner-credential.toml");
    let delivery_rule = rule_name(
        "infra/dispatcher/rules/broker-promotes-the-forge-ops-runner-credential-on-delivery.toml",
    );
    assert_ne!(
        stage_rule, delivery_rule,
        "actual registry actors are distinct"
    );
    let secrets = MountedSecret::new("actual-distinct-registry-actors");
    secrets.seed("forge.current", OLD);
    secrets.lag(true);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    let mut stage = scope();
    stage.rule_name = stage_rule;
    assert!(
        handler(&jobs, &secrets)
            .invoke(&args(), &stage)
            .await
            .is_err()
    );
    assert_eq!(jobs.status("install"), "completed");
    assert_ne!(jobs.status("verify"), "completed");
    let candidate = secrets.get("forge.next").unwrap();
    secrets.propagate();
    acknowledge(&jobs, &secrets, JOB, &candidate).await;
    let mut delivery = delivered(last_eight(&candidate));
    delivery.rule_name = delivery_rule;
    handler(&jobs, &secrets)
        .invoke(&args(), &delivery)
        .await
        .expect("delivery's real actor converges its own observations");
    assert_eq!(jobs.status("revoke"), "completed");
}

#[tokio::test]
async fn promotion_records_an_existing_previous_value_even_when_stage_had_no_next() {
    let secrets = MountedSecret::new("previous-without-next");
    secrets.seed("forge.current", OLD);
    secrets.seed("forge.previous", LEFT);
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    handler(&jobs, &secrets)
        .invoke(&args(), &scope())
        .await
        .unwrap();
    let candidate = secrets.get("forge.next").unwrap();
    acknowledge(&jobs, &secrets, JOB, &candidate).await;
    handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&candidate)))
        .await
        .unwrap();
    let phases = jobs.rotations.lock().unwrap();
    let revoked = &phases
        .iter()
        .find(|(phase, _)| phase.ends_with("/revoked"))
        .unwrap()
        .1;
    assert_eq!(
        revoked["carried_last_eight"],
        last_eight(LEFT),
        "all retired original candidates remain in the record"
    );
    assert_eq!(
        door(&secrets, LEFT),
        boss_jobs::runner_credential::Resolution::Unmatched
    );
}

/// N2. The promotion kills the old value, so it believes a delivery only
/// from the deposit that proved the new one on the host: a `delivered`
/// step completed by hand — its last eight is printed on the install
/// step for anyone to copy — promotes nothing.
#[tokio::test]
async fn a_delivery_completed_by_anyone_but_the_deposit_promotes_nothing() {
    let (jobs, secrets, value) = staged("hand-delivery").await;
    *jobs.delivered_by.lock().unwrap() = Some("emp-david".into());
    let before = secrets.writes();
    let err = handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect_err("a hand completion");
    assert!(
        err.is_permanent() && err.to_string().contains("emp-david"),
        "{err}"
    );
    assert_eq!(secrets.writes(), before, "nothing promoted");
    assert_eq!(door(&secrets, OLD), resolved("forge", "current"));
    assert_ne!(jobs.status("revoke"), "completed");
}

/// R4: the checkout-token deposit is a different executor. Sharing its
/// actor lets its completion pass the runner-delivery attribution check.
#[tokio::test]
async fn the_checkout_token_deposit_cannot_attest_runner_delivery() {
    let (jobs, secrets, value) = staged("checkout-actor").await;
    *jobs.delivered_by.lock().unwrap() = Some("automation:credential-deposit".into());
    let before = secrets.writes();
    let err = handler(&jobs, &secrets)
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect_err("checkout-token deposit is not the runner deposit");
    assert!(err.is_permanent(), "{err}");
    assert_eq!(secrets.writes(), before);
    assert_eq!(door(&secrets, OLD), resolved("forge", "current"));
    assert_ne!(jobs.status("revoke"), "completed");
}

/// The registered client attribution remains conserved. The resolver and
/// original owner receipt independently establish promotion authority.
#[test]
fn the_deposit_preserves_its_registered_client_attribution() {
    let script = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/forge/runner-credential-deposit.sh"),
    )
    .expect("the deposit script");
    assert!(
        script.contains(&format!("\\\"id\\\":\\\"{DEPOSIT_ACTOR}\\\"")),
        "runner-credential-deposit.sh must sign its x-boss-user as {DEPOSIT_ACTOR}"
    );
}

#[test]
fn the_registered_runner_deposit_matches_its_client_attribution() {
    let text = std::fs::read_to_string(
        boss_testing::repo_root().join("infra/platform/automations/runner-credential-deposit.toml"),
    )
    .expect("the runner deposit registration");
    let document: toml::Value = toml::from_str(&text).expect("valid automation TOML");
    let rows = document["automation"].as_array().expect("automation rows");
    assert_eq!(rows.len(), 1, "exactly one runner deposit registration");
    assert_eq!(
        rows[0]["id"].as_str(),
        Some(DEPOSIT_ACTOR),
        "registered runner deposit must match client attribution"
    );
}

// ----- the broker-stage token (design 6e28ed42) -----

const STAGE_TOKEN: &str = "fake.projected-stage-token.not-a-real-signature";

fn presenting(
    jobs: &Jobs,
    secrets: &Arc<MountedSecret>,
    token_file: &std::path::Path,
) -> Arc<CredentialRotateOpsRunner> {
    CredentialRotateOpsRunner::with_stage_token(
        jobs.url.clone(),
        secrets.clone(),
        NO_WAIT,
        Some(token_file.to_path_buf()),
    )
}

/// The inactive behaviour this car must not change: with no token file
/// named — every deployment today — no phase post carries the header and
/// the phases are recorded under the rule's label, as before.
#[tokio::test]
async fn without_a_token_file_no_phase_presents_a_stage_token() {
    let (jobs, _secrets, _value) = staged_unacknowledged("stage-token-off").await;
    let presented = jobs.stage_tokens.lock().unwrap().clone();
    assert_eq!(presented, vec![None, None, None]);
}

#[tokio::test]
async fn a_named_token_file_is_presented_on_every_phase_and_read_again_each_time() {
    let secrets = MountedSecret::new("stage-token-on");
    secrets.seed("forge.current", OLD);
    let token_file = boss_testing::scratch_dir("broker-stage-token-file").join("token");
    boss_testing::write_file(&token_file, &format!("{STAGE_TOKEN}\n"));
    let jobs = jobs(secrets.dir.clone(), FRESH).await;
    presenting(&jobs, &secrets, &token_file)
        .invoke(&args(), &scope())
        .await
        .expect("staged: the owner's receipts name the workload actor");
    for slug in ["issue", "install", "verify"] {
        assert_eq!(jobs.status(slug), "completed", "{slug}");
    }
    assert_eq!(
        *jobs.stage_tokens.lock().unwrap(),
        vec![Some(STAGE_TOKEN.to_owned()); 3],
        "minted, installed and verified each present the projected token"
    );

    // Kubelet rotates the projected file; the promotion reads it afresh.
    let value = secrets.get("forge.next").expect("a value staged");
    acknowledge(&jobs, &secrets, JOB, &value).await;
    let rotated = format!("{STAGE_TOKEN}.rotated");
    boss_testing::write_file(&token_file, &rotated);
    presenting(&jobs, &secrets, &token_file)
        .invoke(&args(), &delivered(last_eight(&value)))
        .await
        .expect("promoted");
    assert_eq!(jobs.status("revoke"), "completed");
    assert_eq!(
        jobs.stage_tokens.lock().unwrap().last().cloned().flatten(),
        Some(rotated)
    );
    let written = jobs.everything_written();
    assert!(
        !written.contains(STAGE_TOKEN),
        "the token reached the record"
    );
}

/// Fail closed BEFORE anything is minted: a stage that was told to
/// present a token and cannot read one never falls back to the label.
#[tokio::test]
async fn a_named_token_file_that_cannot_be_presented_mints_nothing() {
    for (name, content) in [
        ("absent", None),
        ("empty", Some(String::new())),
        ("blank", Some("  \n".to_owned())),
        ("oversized", Some("a".repeat(9000))),
        ("two-lines", Some(format!("{STAGE_TOKEN}\nsecond-line"))),
    ] {
        let secrets = MountedSecret::new(&format!("stage-token-{name}"));
        secrets.seed("forge.current", OLD);
        let token_file = boss_testing::scratch_dir("broker-stage-token-file").join("token");
        if let Some(content) = &content {
            boss_testing::write_file(&token_file, content);
        }
        let jobs = jobs(secrets.dir.clone(), FRESH).await;
        let error = presenting(&jobs, &secrets, &token_file)
            .invoke(&args(), &scope())
            .await
            .expect_err(name);
        let said = format!("{error:?}");
        assert!(said.contains("broker-stage token"), "{name}: {said}");
        assert!(
            !said.contains(STAGE_TOKEN),
            "{name}: the error carries the token"
        );
        assert_eq!(
            secrets.get("forge.next"),
            None,
            "{name}: a value was minted"
        );
        assert!(jobs.phases().is_empty(), "{name}");
        assert!(jobs.stage_tokens.lock().unwrap().is_empty(), "{name}");
    }
}

/// The workload token travels no further than the estate token does: a
/// jobs URL that is not one of the estate's own hosts gets neither.
#[test]
fn the_stage_token_is_withheld_from_a_host_that_is_not_the_estates() {
    let token_file = boss_testing::scratch_dir("broker-stage-token-host").join("token");
    boss_testing::write_file(&token_file, STAGE_TOKEN);
    let secrets = MountedSecret::new("stage-token-host");
    let to = |url: &str| {
        CredentialRotateOpsRunner::with_stage_token(
            url.to_owned(),
            secrets.clone(),
            NO_WAIT,
            Some(token_file.clone()),
        )
        .stage_token()
    };
    assert!(matches!(to("http://127.0.0.1:7900"), Ok(Some(header)) if header.is_sensitive()));
    for url in ["http://jobs.not-the-estate.example:7900", "not a url"] {
        let said = format!("{:?}", to(url).expect_err(url));
        assert!(said.contains("not one of the estate's own hosts"), "{said}");
        assert!(!said.contains(STAGE_TOKEN), "{said}");
    }
}
