//! The existing conductor consumes its registered diagnostic station.
use super::diagnostics::*;
use super::{ACTOR, Conductor, Method, PAGE_LIMIT, Value, job_id, list_all_pages, log, step_done};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use uuid::Uuid;

#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod artifact_tests;
mod artifacts;
mod recovery;
#[cfg(test)]
mod recovery_refresh_tests;

fn find_step<'a>(job: &'a Value, slug: &str) -> Option<&'a Value> {
    super::find_step(job, slug, slug)
}

pub(super) async fn drain(conductor: &Conductor) -> Result<()> {
    drain_scope(conductor, DrainScope::Ordinary).await
}

/// This entrypoint cannot dispatch an ordinary diagnostic or admit compute.
pub(super) async fn drain_recovery_assessments(conductor: &Conductor) -> Result<()> {
    drain_scope(conductor, DrainScope::RecoveryAssessmentsOnly).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrainScope {
    Ordinary,
    RecoveryAssessmentsOnly,
}

enum Selection {
    Ordinary { may_launch: bool },
    RecoveryAssessment,
}

/// What the conductor's diagnostic queue can see, defined ONCE (backlog
/// 97170186). The drain's server filter, its client-side recheck and the
/// selected packet's recheck all read these markers, and the published
/// diagnostic-recovery-assessment contract declares the recovery set as
/// `metadata_schema.required_values`, which admission enforces — pinned
/// equal by admission_tests, because the queue's half is the conductor's
/// own identity in code and the contract's half is registry data (§9a).
/// Before, the contract only described them, so a packet admitted
/// without them (28e96fbc) sat invisible until a hand PATCH.
pub(super) fn queue_markers(recovery: bool) -> Value {
    if recovery {
        json!({"diagnostic_executor":ACTOR,"diagnostic_recovery_assessment":"v1"})
    } else {
        json!({"diagnostic_executor":ACTOR})
    }
}

pub(super) fn carries_markers(job: &Value, markers: &Value) -> bool {
    markers.as_object().is_some_and(|markers| {
        markers
            .iter()
            .all(|(key, value)| job["metadata"][key] == *value)
    })
}

/// The queue's read, exactly as the drain sends it.
pub(super) fn queue_path(markers: &Value, status: &str, offset: usize) -> String {
    let markers = markers.to_string();
    let filter = percent_encoding::utf8_percent_encode(&markers, crate::job::QUERY_VALUE);
    format!("/api/jobs?status={status}&metadata={filter}&limit={PAGE_LIMIT}&offset={offset}")
}

async fn drain_scope(conductor: &Conductor, scope: DrainScope) -> Result<()> {
    let markers = queue_markers(false);
    let requests = list_all_pages(|offset| {
        let path = queue_path(&markers, "open", offset);
        async move { conductor.api(Method::GET, &path, None).await }
    })
    .await?;
    let mut ours: Vec<Value> = requests
        .into_iter()
        .filter(|job| {
            carries_markers(job, &markers)
                && find_step(job, "execute").is_some_and(|step| !step_done(Some(step)))
        })
        .collect();
    ours.sort_by_key(|job| {
        (
            if find_step(job, "execute").is_some_and(|step| step["status"] == "active") {
                0
            } else {
                1
            },
            if job["priority"] == "urgent" { 0 } else { 1 },
            job["opened_at"].as_str().unwrap_or_default().to_owned(),
            job["id"].as_str().unwrap_or_default().to_owned(),
        )
    });
    if conductor.cfg.dry {
        if !ours.is_empty() {
            log("DRY: diagnostic request retained; no claim or native effect");
        }
        return Ok(());
    }
    if scope == DrainScope::RecoveryAssessmentsOnly {
        // Markers select candidates; the whole pinned protocol and its exact
        // decided authority are still validated by recovery::advance. Never
        // hydrate or claim an ordinary request on an occupied track.
        let recovery = queue_markers(true);
        let assessments = ours
            .into_iter()
            .filter(|job| carries_markers(job, &recovery))
            .collect();
        return advance_requests(assessments, |selected, _| {
            advance_selected(conductor, selected, Selection::RecoveryAssessment)
        })
        .await;
    }
    let recovery = queue_markers(true);
    let completed = if ours.iter().any(|job| {
        job["kind"] == "diagnostic-request"
            && job["workflow_version"] == 2
            && find_step(job, "execute").is_some_and(|step| step["status"] == "active")
    }) {
        list_all_pages(|offset| {
            let path = queue_path(&recovery, "closed", offset);
            async move { conductor.api(Method::GET, &path, None).await }
        })
        .await?
    } else {
        Vec::new()
    };
    let mut assessed = std::collections::BTreeSet::new();
    for candidate in completed
        .iter()
        .filter(|job| carries_markers(job, &recovery))
    {
        match recovery::covered_original(conductor, candidate).await {
            Ok(original) => {
                assessed.insert(original);
            }
            Err(error) => log(format!(
                "diagnostic recovery disposition unread: {error:#}; original queue position retained"
            )),
        }
    }
    let ours = recovery::pending_originals(ours, &assessed)?;
    advance_requests(ours, |selected, may_launch| {
        advance_selected(conductor, selected, Selection::Ordinary { may_launch })
    })
    .await
}

pub(super) async fn advance_requests<F, Fut>(requests: Vec<Value>, mut advance_one: F) -> Result<()>
where
    F: FnMut(Value, bool) -> Fut,
    Fut: std::future::Future<Output = Result<Advance>>,
{
    let mut refusal = None;
    for (position, selected) in requests.into_iter().enumerate() {
        let id = job_id(&selected)?.to_owned();
        // The oldest request keeps the one-effect-per-pass admission.
        // An UNKNOWN must not authorize another create; later packets
        // can reconcile only an intent already retained before this pass.
        match advance_one(selected, position == 0).await {
            Ok(Advance::Assessed) => log(format!(
                "diagnostic {id}: recovered refusal assessment retained; original operation and all subject holds unchanged"
            )),
            Ok(Advance::Pending) => log(format!(
                "diagnostic {id}: native worker pending; original attempt retained"
            )),
            Ok(Advance::Unknown(reason)) => log(format!(
                "diagnostic {id}: UNKNOWN — {reason}; source hold remains"
            )),
            Ok(Advance::Complete(receipt)) => log(format!(
                "diagnostic {id}: {:?}; no subject hold, review or departure changed",
                receipt.outcome
            )),
            Ok(Advance::Refused(refusal)) => log(format!(
                "diagnostic {id}: first fetch exited {}; pre-create refusal retained for invocation {}; no subject hold or departure changed",
                refusal.command["exit"], refusal.invocation
            )),
            Err(error) => {
                log(format!(
                    "diagnostic {id}: refused — {error:#}; source hold remains"
                ));
                if refusal.is_none() {
                    refusal = Some(error);
                }
            }
        }
    }
    refusal.map_or(Ok(()), Err)
}

async fn advance_selected(
    conductor: &Conductor,
    selected: Value,
    selection: Selection,
) -> Result<Advance> {
    let id = Uuid::parse_str(job_id(&selected)?)?;
    // Queue rows carry compact steps, never their admitted request. Read
    // only the selected packet whole, then recheck its binding before effects.
    let job = conductor
        .get_job(&id.to_string())
        .await
        .context("diagnostic selected detail read failed")?;
    if job["id"] != id.to_string()
        || job["kind"] != selected["kind"]
        || job["kind"]
            .as_str()
            .filter(|kind| !kind.is_empty())
            .is_none()
        || job["workflow_version"] != selected["workflow_version"]
        || job["workflow_version"]
            .as_i64()
            .filter(|version| *version > 0)
            .is_none()
        || !carries_markers(&job, &queue_markers(false))
        || job["status"] != "open"
    {
        bail!("diagnostic selected packet identity, protocol or executor changed");
    }
    let selected_step =
        find_step(&selected, "execute").context("diagnostic selected execute step absent")?;
    let step = find_step(&job, "execute")
        .context("diagnostic selected full execute step absent")?
        .clone();
    if selected_step["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .is_none()
        || step["id"] != selected_step["id"]
        || step["job_id"] != id.to_string()
        || step["assignee_id"] != ACTOR
        || (step["status"] != "ready" && step["status"] != "active")
    {
        bail!("diagnostic selected execute identity, actor or state changed");
    }
    let recovery =
        step["metadata"]["recovery_operation"]["handler"] == "retained-preparation-assessment";
    let may_launch = match selection {
        Selection::Ordinary { may_launch } => may_launch,
        Selection::RecoveryAssessment => {
            if !carries_markers(&job, &queue_markers(true)) || !recovery {
                bail!("occupied-track recovery selection has no registered assessment handler");
            }
            false
        }
    };
    let directory = PathBuf::from(&conductor.cfg.diagnostics_home).join(id.to_string());
    let content = crate::tenant_publish::service_on_door(&conductor.cfg.jobs, "content")?;
    let door = NativeDoor {
        conductor,
        job,
        step,
        directory,
        actor: crate::identity::conductor(),
        content,
        may_launch,
    };
    // One original request has one stable attempt. An explicit retry is
    // another admitted packet, never a fresh UUID on a polling pass.
    if recovery {
        recovery::advance(&door).await
    } else {
        advance(&door, id).await
    }
}

pub(super) fn launch_admission(fresh: bool, may_launch: bool) -> Result<()> {
    if fresh && !may_launch {
        bail!(
            "diagnostic fresh launch deferred: an earlier request owns this pass's effect admission; only retained attempts may reconcile"
        );
    }
    Ok(())
}

struct NativeDoor<'a> {
    conductor: &'a Conductor,
    job: Value,
    step: Value,
    directory: PathBuf,
    actor: String,
    content: String,
    may_launch: bool,
}

fn first_record_fits(
    step: &boss_core::job::Step,
    key: &str,
    value: &Value,
    actor: &boss_core::actor::ActorId,
) -> Result<bool> {
    // This is a size projection, never a fact published or retained.
    // Use the longest partition marker and leave room for the server's
    // timestamp spelling. The server still owns the staging refusal if
    // a concurrent write grows the step after this fresh read.
    let stamp = boss_core::publisher::EventStamp::new("jobs", actor.clone())
        .with_partition(boss_core::partition::Partition::Simulated);
    let mut next = step.clone();
    let shape = next.shape_hash();
    let mut metadata = next.metadata.as_object().cloned().unwrap_or_default();
    metadata.insert(key.into(), value.clone());
    next.metadata = Value::Object(metadata);
    boss_jobs::events::void_stamps_if_moved(&stamp, &shape, &mut next);
    let record = boss_jobs::first_record::FirstRecord::new(
        next.job_id,
        next.id,
        key,
        value,
        &stamp,
        Uuid::nil(),
    );
    let bytes = serde_json::to_vec(&record.event(&next, &stamp))?;
    Ok(bytes.len() <= boss_events::outbox::MAX_STAGED_EVENT_BYTES.saturating_sub(1024))
}

impl NativeDoor<'_> {
    fn id(&self) -> Result<&str> {
        job_id(&self.job)
    }
    fn step_id(&self) -> Result<&str> {
        self.step["id"]
            .as_str()
            .context("diagnostic step identity absent")
    }
    async fn journal(&self, observation: Value) -> Result<()> {
        let directory = self.directory.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let directory = super::diagnostic_journal::private_directory(&directory)?;
            super::diagnostic_journal::keep(&directory, &observation)
        })
        .await?
    }
    async fn response(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let http: &crate::machine_http::MachineClient = &self.conductor.http;
        let mut request = http
            .request(method.clone(), format!("{}{path}", self.conductor.cfg.jobs))
            .header("content-type", "application/json")
            .header("x-boss-user", crate::identity::header(&self.actor));
        if let Some(body) = &body {
            request = request.json(body);
        }
        self.journal(json!({"phase":"http-request","actor":self.actor,"http_method":method.as_str(),"path":path,"request":body})).await?;
        let mut response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                self.journal(json!({"phase":"http-transport-error","actor":self.actor,"http_method":method.as_str(),"path":path,"request":body,"error":error.to_string()})).await?;
                return Err(error);
            }
        };
        let status = response.status();
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(error) => {
                    self.journal(json!({"actor":self.actor,"http_method":method.as_str(),"path":path,"request":body,
                        "status":status.as_u16(),"complete":false,"error":error.to_string(),
                        "response_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)})).await?;
                    return Err(error.into());
                }
            }
        }
        self.journal(json!({"actor":self.actor,"http_method":method.as_str(),"path":path,"request":body,
            "status":status.as_u16(),"complete":true,
            "response_b64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)})).await?;
        Ok((status, bytes))
    }
    async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, bytes) = self.response(method.clone(), path, body).await?;
        if !status.is_success() {
            bail!("diagnostic {method} {path}: HTTP {status}; complete original response retained");
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
    async fn artifact(&self, value: &Value, record_key: Option<&str>) -> Result<Value> {
        let raw = serde_json::to_vec(value)?;
        let inline = if let Some(key) = record_key {
            let current = self
                .api(Method::GET, &format!("/api/jobs/{}", self.id()?), None)
                .await?;
            if current["id"] != self.id()? {
                bail!("diagnostic current record packet identity differs");
            }
            let step_id = self.step_id()?;
            let selected = current["steps"]
                .as_array()
                .context("diagnostic current record steps absent")?
                .iter()
                .find(|step| step["id"].as_str() == Some(step_id))
                .context("diagnostic current record step absent")?;
            let step: boss_core::job::Step = serde_json::from_value(selected.clone())?;
            if step.id.to_string() != self.step_id()? || step.job_id.to_string() != self.id()? {
                bail!("diagnostic current record step identity differs");
            }
            first_record_fits(&step, key, value, &self.actor.parse()?)?
        } else {
            raw.len() < boss_events::outbox::MAX_STAGED_EVENT_BYTES / 4
        };
        if inline {
            return Ok(value.clone());
        }
        if raw.len() as u64 > crate::attach::largest_file() {
            return Box::pin(artifacts::retain(self, &raw)).await;
        }
        self.attach_bytes(raw).await
    }
    async fn attach_bytes(&self, raw: Vec<u8>) -> Result<Value> {
        let file = tokio::task::spawn_blocking(move || -> Result<tempfile::NamedTempFile> {
            let mut file = tempfile::Builder::new()
                .prefix("boss-diagnostic-original-")
                .suffix(".json")
                .tempfile()?;
            file.write_all(&raw)?;
            file.flush()?;
            file.as_file().sync_all()?;
            Ok(file)
        })
        .await??;
        let attached = crate::attach::attach_at(
            &self.conductor.http,
            &self.content,
            &crate::attach::Target {
                kind: "job",
                id: self.id()?.into(),
                label: format!("diagnostic {}", self.id()?),
            },
            file.path(),
            crate::identity::Signature::As(self.actor.clone()),
        )
        .await?;
        Ok(
            json!({"kind":"attachment","file_id":attached.file_id,"sha256":attached.sha256,
            "size_bytes":attached.size_bytes,"encoding":"utf-8"}),
        )
    }
    async fn original_record(
        &self,
        key: &str,
    ) -> Result<Option<boss_jobs::first_record::FirstRecord>> {
        let path = format!(
            "/api/jobs/{}/steps/{}/records/{key}",
            self.id()?,
            self.step_id()?
        );
        // A missing record is one exact 404 at an existing, controlled door;
        // every other refusal/empty response remains an error.
        let (status, body) = self.response(Method::GET, &path, None).await?;
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("diagnostic immutable record read HTTP {status}; complete original retained");
        }
        let record: boss_jobs::first_record::FirstRecord = serde_json::from_slice(&body)?;
        if record.job_id.to_string() != self.id()?
            || record.step_id.to_string() != self.step_id()?
            || record.key != key
        {
            bail!("diagnostic immutable record identity differs");
        }
        let original: Value = serde_json::from_str(&record.value_json)?;
        if original != record.value
            || record.actor.to_string() != ACTOR
            || record.event_id.is_nil()
            || record.canonical != boss_core::job::canonical_json_bytes(&original)
            || record.digest != hex::encode(Sha256::digest(&record.canonical))
        {
            bail!("diagnostic immutable record original/actor/equality witness differs");
        }
        Ok(Some(record))
    }
    async fn original(&self, key: &str) -> Result<Option<Value>> {
        let Some(record) = self.original_record(key).await? else {
            return Ok(None);
        };
        self.record_value(&record).await.map(Some)
    }
    async fn record_value(&self, record: &boss_jobs::first_record::FirstRecord) -> Result<Value> {
        self.read_artifact(&record.value).await
    }
    async fn read_artifact(&self, value: &Value) -> Result<Value> {
        if value.get("kind").and_then(Value::as_str) == Some("chunked-json") {
            artifacts::read(self, value).await
        } else if value.get("kind").and_then(Value::as_str) == Some("attachment") {
            let text = crate::dispatch_brief::read_at(
                &self.conductor.http,
                &self.content,
                &crate::identity::header(&self.actor),
                value,
            )
            .await?;
            Ok(serde_json::from_str(&text)?)
        } else {
            Ok(value.clone())
        }
    }
    async fn keep(&self, key: &str, value: &Value) -> Result<Value> {
        Ok(self.keep_record(key, value).await?.1)
    }
    async fn keep_record(
        &self,
        key: &str,
        value: &Value,
    ) -> Result<(boss_jobs::first_record::FirstRecordResult, Value)> {
        let reference = self.artifact(value, Some(key)).await?;
        let (status, raw) = self
            .response(
                Method::POST,
                &format!(
                    "/api/jobs/{}/steps/{}/metadata/records",
                    self.id()?,
                    self.step_id()?
                ),
                Some(json!({"key":key,"value":reference,"expected_absence":true})),
            )
            .await?;
        if !status.is_success() {
            bail!(
                "diagnostic first record write HTTP {status}; complete original response retained"
            );
        }
        let result: boss_jobs::first_record::FirstRecordResult = serde_json::from_slice(&raw)?;
        let accepted = match &result {
            boss_jobs::first_record::FirstRecordResult::Recorded(record)
                if status == reqwest::StatusCode::CREATED =>
            {
                record
            }
            boss_jobs::first_record::FirstRecordResult::Replayed(record)
                if status == reqwest::StatusCode::OK =>
            {
                record
            }
            boss_jobs::first_record::FirstRecordResult::Recorded(_)
            | boss_jobs::first_record::FirstRecordResult::Replayed(_) => {
                bail!("diagnostic first record disposition and HTTP status disagree; no new effect")
            }
            _ => bail!("diagnostic original record was not accepted; no completion"),
        };
        let original = self
            .original_record(key)
            .await?
            .context("diagnostic original record read-back absent")?;
        if accepted != &original {
            bail!(
                "diagnostic first record response differs from its authoritative original; no new effect"
            );
        }
        let value = self.record_value(&original).await?;
        Ok((result, value))
    }
}

#[async_trait]
impl Door for NativeDoor<'_> {
    async fn inputs(&self) -> Result<Inputs> {
        if self.actor != ACTOR || crate::identity::conductor() != self.actor {
            bail!("diagnostic writes require the running registered conductor actor");
        }
        super::consist_job::validate_clone_url(&self.conductor.cfg.fork_url)?;
        // Ordinary capacity waits must not persist a launch intent. A
        // known attempt resumes even while its own worker occupies capacity.
        let fresh = self.original("diagnostic.intent").await?.is_none();
        launch_admission(fresh, self.may_launch)?;
        let path = self.directory.clone();
        let observed = tokio::task::spawn_blocking(move || {
            super::consist_job::diagnostic_identity(&path, fresh)
        })
        .await??;
        let request: Request = serde_json::from_value(self.step["metadata"]["request"].clone())?;
        let kind = self.job["kind"]
            .as_str()
            .context("diagnostic request protocol absent")?;
        let version = self.job["workflow_version"]
            .as_i64()
            .filter(|v| *v > 0)
            .context("diagnostic pinned protocol version absent")?;
        let protocol = self
            .api(
                Method::GET,
                &format!("/api/workflows/{kind}/versions/{version}"),
                None,
            )
            .await?;
        let published: boss_jobs::registry::WorkflowSpec =
            serde_json::from_value(protocol.clone())?;
        if published.kind != kind
            || i64::from(published.version) != version
            // A rejected row was never published either (50ded63d).
            || !published.status.was_published()
        {
            bail!("diagnostic pinned published protocol identity differs");
        }
        let history = self
            .api(
                Method::GET,
                &format!("/api/jobs/{}/events?limit=1000", self.id()?),
                None,
            )
            .await?;
        let creations: Vec<&Value> = history["data"]
            .as_array()
            .context("diagnostic admission history absent")?
            .iter()
            .filter(|event| event["kind"] == boss_jobs::events::JOB_CREATED)
            .collect();
        if creations.len() != 1 {
            bail!("diagnostic admission must have exactly one original creation fact");
        }
        let admitted = creations[0];
        if admitted["actor"]
            .as_str()
            .filter(|actor| !actor.is_empty())
            .is_none()
            || admitted["payload"]["id"] != self.id()?
            || admitted["payload"]["kind"] != kind
            || admitted["payload"]["workflow_version"] != version
            || admitted["payload"]["metadata"]["request"] != self.step["metadata"]["request"]
        {
            bail!(
                "diagnostic current request differs from its original admitted actor-bound packet"
            );
        }
        let spec = published
            .steps
            .iter()
            .find(|step| step.title == "execute")
            .context("pinned diagnostic operation absent")?;
        if spec.selectors().assignee_id.as_deref() != Some(ACTOR) {
            bail!("diagnostic pinned actor binding differs");
        }
        let operation = &spec.metadata_defaults["diagnostic_operation"];
        if operation["handler"] != "isolated-consist"
            || !matches!(operation["version"].as_u64(), Some(1 | 2))
            || operation["capacity"] != 1
        {
            bail!("diagnostic operation has no registered fixed controller adapter");
        }
        let required_checks = serde_json::from_value(operation["required_checks"].clone())?;
        let car = self
            .api(Method::GET, &format!("/api/jobs/{}", request.car), None)
            .await?;
        if car["id"] != request.car.to_string()
            || car["kind"] != "ship-a-change"
            || car["status"] != "open"
        {
            bail!("diagnostic subject is not an open source car");
        }
        let review = find_step(&car, "review").context("diagnostic car review absent")?;
        if review["metadata"]["hold"]
            .as_str()
            .filter(|hold| !hold.is_empty())
            .is_none()
        {
            bail!("diagnostic car is not held");
        }
        let receipt = car["metadata"]
            .get("regate_receipt")
            .or_else(|| find_step(&car, "gate")?.get("metadata")?.get("receipt"))
            .context("diagnostic car complete receipt absent")?;
        let bytes = if let Some(text) = receipt.as_str() {
            text.as_bytes().to_vec()
        } else {
            serde_json::to_vec(receipt)?
        };
        let receipt_value: Value = serde_json::from_slice(&bytes)?;
        if hex::encode(Sha256::digest(&bytes)) != request.receipt_sha256
            || receipt_value["head"] != request.candidate
            || receipt_value["verdict"] != "green"
            || review["metadata"]["hold_sha"] != request.candidate
        {
            bail!("diagnostic source head/hold/full receipt binding differs");
        }
        let policy = self
            .api(
                Method::GET,
                &format!(
                    "/api/delivery/policy/{}",
                    crate::delivery_policy::POLICY_NAME
                ),
                None,
            )
            .await?;
        let parsed = crate::delivery_policy::parse(serde_json::from_value(policy.clone())?)?;
        if parsed != self.conductor.policy || !parsed.is_from_registry() {
            bail!("diagnostic policy is not the current authoritative registry row");
        }
        let edit_level = self
            .api(Method::GET, crate::dispatch::EDIT_LEVEL_PATH, None)
            .await?;
        if crate::gate::handable_edit_level(Some(&edit_level)).is_none() {
            bail!("diagnostic edit-level read is unavailable or malformed");
        }
        let car_binding = json!({"id":car["id"],"kind":car["kind"],"branch":car["metadata"]["branch"],
            "hold":review["metadata"]["hold"],"hold_sha":review["metadata"]["hold_sha"],
            "receipt_sha256":request.receipt_sha256});
        Ok(Inputs {
            request,
            admitted_original: admitted.clone(),
            pinned_protocol: protocol,
            car_binding,
            current_car: car,
            policy,
            edit_level,
            required_checks,
            expected_executor_identity: super::consist_job::CONTROLLER_IDENTITY.into(),
            observed_cluster_identity: observed,
        })
    }
    async fn claim(&self) -> Result<()> {
        if self.step["status"] == "active" {
            if self.step["assignee_id"] != ACTOR {
                bail!("diagnostic active claim belongs to another actor");
            }
            return Ok(());
        }
        if self.step["status"] != "ready" || self.step["assignee_id"] != ACTOR {
            bail!("diagnostic step is not ready for its registered actor");
        }
        self.api(
            Method::POST,
            &format!("/api/jobs/{}/steps/{}/claim", self.id()?, self.step_id()?),
            Some(json!({})),
        )
        .await?;
        Ok(())
    }
    async fn first_intent(&self, proposed: &Intent) -> Result<FirstIntent> {
        if let Some(old) = self.original("diagnostic.intent").await? {
            return Ok(FirstIntent::Replayed(serde_json::from_value(old)?));
        }
        let (disposition, accepted) = self
            .keep_record("diagnostic.intent", &serde_json::to_value(proposed)?)
            .await?;
        let intent: Intent = serde_json::from_value(accepted)?;
        // Equal intent data says nothing about which concurrent caller
        // recorded it. Only the owning door's disposition admits an effect.
        Ok(match disposition {
            boss_jobs::first_record::FirstRecordResult::Recorded(_) => {
                FirstIntent::Recorded(intent)
            }
            boss_jobs::first_record::FirstRecordResult::Replayed(_) => {
                FirstIntent::Replayed(intent)
            }
            _ => bail!("diagnostic first intent has no accepted record disposition"),
        })
    }
    async fn launch_record(&self) -> Result<Option<Launch>> {
        if let Some(value) = self.original("diagnostic.launch").await? {
            return Ok(Some(serde_json::from_value(value)?));
        }
        let Some(value) = self.original("diagnostic.intent").await? else {
            return Ok(None);
        };
        let intent: Intent = serde_json::from_value(value)?;
        let directory = self.directory.clone();
        let recovered = tokio::task::spawn_blocking(move || {
            super::diagnostic_journal::recovered_launch(
                &directory,
                &intent.job_name(),
                &intent.inputs.request.baseline,
            )
        })
        .await??;
        if let Some(launch) = &recovered {
            self.keep_launch(launch).await?;
        }
        Ok(recovered)
    }
    async fn receipt_record(&self) -> Result<Option<Receipt>> {
        self.original("diagnostic.receipt")
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    async fn launch(&self, intent: &Intent) -> Result<Launch> {
        super::consist_job::validate_clone_url(&self.conductor.cfg.fork_url)?;
        let invocation = if allows_pre_create_refusal(intent) {
            let invocation = Uuid::new_v4();
            let proposed =
                json!({"phase":"preparing","invocation":invocation,"attempt":intent.attempt});
            let (disposition, accepted) =
                self.keep_record("diagnostic.invocation", &proposed).await?;
            if !matches!(
                disposition,
                boss_jobs::first_record::FirstRecordResult::Recorded(_)
            ) || accepted != proposed
            {
                bail!("preparation invocation is already owned or differs; no new effect");
            }
            Some(invocation)
        } else {
            None
        };
        let clone = self.conductor.cfg.clone.clone();
        let url = self.conductor.cfg.fork_url.clone();
        let directory = self.directory.clone();
        let intent = intent.clone();
        let budget = self.conductor.policy.consist_budget.as_secs();
        tokio::task::spawn_blocking(move || -> Result<Launch> {
            let name = intent.job_name();
            let tree = directory.with_extension("assembly");
            if let Some(invocation) = invocation {
                super::diagnostic_journal::keep(&directory,
                    &json!({"phase":"preparing","invocation":invocation,"attempt":intent.attempt}))?;
            }
            // Separate normal assembly; no car branch, hold or conductor HEAD moves.
            let git = |args: &[&str]| -> Result<std::process::Output> {
                let output = crate::git_auth::command()
                    .args(["-c", "core.hooksPath=/dev/null", "-C", &clone])
                    .args(args)
                    .output()?;
                super::diagnostic_journal::command(&directory, args, None, &output)?;
                if !output.status.success() {
                    if args == ["fetch", "origin"] && let Some(invocation) = invocation {
                        let refusal = PreCreateRefusal { schema:1, intent:intent.clone(), invocation,
                            native_launch:NativeLaunchAbsent::Absent,
                            command:super::diagnostic_journal::command_value(args,None,&output),
                            originals:super::diagnostic_journal::originals(&directory)? };
                        validate_pre_create_refusal(&refusal)?;
                        return Err(refusal.into());
                    }
                    bail!("diagnostic fixed git operation failed; complete originals retained");
                }
                Ok(output)
            };
            git(&["fetch", "origin"])?;
            let baseline = String::from_utf8(git(&["rev-parse", "origin/main"])?.stdout)?;
            if baseline.trim() != intent.inputs.request.baseline {
                bail!("diagnostic baseline moved before assembly");
            }
            let branch = intent.inputs.current_car["metadata"]["branch"]
                .as_str()
                .context("diagnostic source branch absent")?;
            let reference = format!("refs/heads/{branch}");
            git(&["check-ref-format", &reference])?;
            let remote = String::from_utf8(git(&["ls-remote", "fork", &reference])?.stdout)?;
            let expected = format!("{}\t{reference}", intent.inputs.request.candidate);
            if remote.trim() != expected {
                bail!("diagnostic forge source head differs from the complete source receipt");
            }
            git(&["fetch", "fork", &intent.inputs.request.candidate])?;
            git(&[
                "worktree",
                "add",
                "--lock",
                "--detach",
                tree.to_str()
                    .context("diagnostic assembly path is not UTF-8")?,
                &intent.inputs.request.baseline,
            ])?;
            let merge = std::process::Command::new("git")
                .arg("-C")
                .arg(&tree)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "merge",
                    "--no-edit",
                    "--no-ff",
                    &intent.inputs.request.candidate,
                ])
                .output()?;
            super::diagnostic_journal::command(
                &directory,
                &["normal-merge", &intent.inputs.request.candidate],
                None,
                &merge,
            )?;
            if !merge.status.success() {
                bail!("diagnostic normal assembly conflicted; no automatic source resolution");
            }
            let head = std::process::Command::new("git")
                .arg("-C")
                .arg(&tree)
                .args(["rev-parse", "HEAD"])
                .output()?;
            super::diagnostic_journal::command(&directory, &["assembled-head"], None, &head)?;
            if !head.status.success() {
                bail!("diagnostic assembled head unavailable");
            }
            let assembled = String::from_utf8(head.stdout)?.trim().to_string();
            git(&["push", "fork", &format!("{assembled}:refs/heads/{name}")])?;
            let manifest = super::consist_job::diagnostic_manifest(
                &name,
                &url,
                &assembled,
                &intent.inputs.request.baseline,
                budget,
                &intent.inputs.edit_level,
            )?;
            if let Some(invocation) = invocation {
                super::diagnostic_journal::keep(&directory,
                    &json!({"phase":"entering-create","invocation":invocation,"attempt":intent.attempt}))?;
            }
            super::consist_job::diagnostic_create(&directory, &manifest, &assembled)
        })
        .await?
    }
    async fn keep_launch(&self, launch: &Launch) -> Result<()> {
        let accepted = self
            .keep("diagnostic.launch", &serde_json::to_value(launch)?)
            .await?;
        if accepted != serde_json::to_value(launch)? {
            bail!("diagnostic launch original differs");
        }
        Ok(())
    }
    async fn observe(&self, intent: &Intent, launch: &Launch) -> Result<Option<Observation>> {
        let directory = self.directory.clone();
        let name = intent.job_name();
        let launch = launch.clone();
        let baseline = intent.inputs.request.baseline.clone();
        tokio::task::spawn_blocking(move || {
            super::consist_job::diagnostic_observe(&directory, &name, &baseline, &launch)
        })
        .await?
    }
    async fn keep_receipt(&self, receipt: &Receipt) -> Result<()> {
        let accepted = self
            .keep("diagnostic.receipt", &serde_json::to_value(receipt)?)
            .await?;
        if accepted != serde_json::to_value(receipt)? {
            bail!("diagnostic full receipt differs from retained original");
        }
        Ok(())
    }
    async fn finish(&self, receipt: &Receipt) -> Result<()> {
        let value = serde_json::to_value(&receipt.outcome)?;
        let outcome = value.as_str().context("diagnostic outcome is not text")?;
        self.complete_result(outcome, &serde_json::to_value(receipt)?)
            .await
    }
    async fn pre_create_refusal_record(&self) -> Result<Option<PreCreateRefusal>> {
        self.original("diagnostic.pre-create-refusal")
            .await?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    async fn keep_pre_create_refusal(&self, refusal: &PreCreateRefusal) -> Result<()> {
        validate_pre_create_refusal(refusal)?;
        let value = serde_json::to_value(refusal)?;
        if self.keep("diagnostic.pre-create-refusal", &value).await? != value {
            bail!("pre-create refusal original differs; no completion");
        }
        Ok(())
    }
    async fn finish_pre_create_refusal(&self, refusal: &PreCreateRefusal) -> Result<()> {
        validate_pre_create_refusal(refusal)?;
        self.complete_result("refused", &serde_json::to_value(refusal)?)
            .await
    }
}

impl NativeDoor<'_> {
    async fn complete_result(&self, outcome: &str, result: &Value) -> Result<()> {
        let evidence = self.artifact(result, None).await?;
        self.api(
            Method::PATCH,
            &format!(
                "/api/jobs/{}/steps/{}/metadata",
                self.id()?,
                self.step_id()?
            ),
            Some(json!({"outcome":outcome,"evidence":evidence})),
        )
        .await?;
        self.api(
            Method::PUT,
            &format!("/api/jobs/{}/steps/{}", self.id()?, self.step_id()?),
            Some(json!({"status":"completed"})),
        )
        .await?;
        let current = self
            .api(Method::GET, &format!("/api/jobs/{}", self.id()?), None)
            .await?;
        let step =
            find_step(&current, "execute").context("completed diagnostic step read-back absent")?;
        if step["id"] != self.step_id()?
            || step["status"] != "completed"
            || step["metadata"]["outcome"] != outcome
            || step["metadata"]["evidence"] != evidence
        {
            bail!("diagnostic completion read-back differs; no completion claim");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::JobsRepository;
    use std::sync::Arc;

    /// How long a fixture here waits on something it does not judge — two
    /// owning calls settling, a re-executed test binary reaching the forge
    /// stub — before it names a hang. A backstop, never the subject: train
    /// and car gates on 2026-10-22 went red at the old fifteen and thirty
    /// seconds on cars that never touched this file (gates 9b3ced66 and
    /// bae2e406), because work done while three gates share the test phase
    /// is slow, not wrong (f10419f9). What each test judges is in its
    /// assertions.
    const FIXTURE_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(300);

    // The list and detail are served by the owning jobs router. Faults
    // affect only the selected detail response, before any controller effect.
    async fn selected_request_fixture(
        fault: Option<&str>,
    ) -> (
        Conductor,
        Arc<std::sync::Mutex<Vec<String>>>,
        Arc<boss_jobs::InMemoryJobs>,
        Value,
        tempfile::TempDir,
    ) {
        use boss_core::{
            job::{Job, JobStatus, Priority, Step, StepStatus, Subject},
            port::EventBus,
            publisher::DomainPublisher,
        };
        use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let mut job = Job::new(
            "diagnostic-request",
            Subject::new("custom", "ordinary-fixture"),
            "Ordinary selected diagnostic",
            ACTOR,
            Priority::Urgent,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
        );
        let request = serde_json::to_value(proposed().inputs.request).unwrap();
        job.status = JobStatus::Open;
        if fault == Some("active-v2") {
            job.workflow_version = 2;
        }
        job.metadata = json!({"diagnostic_executor":ACTOR,"request":request});
        jobs.create_job(&job).await.unwrap();
        let mut step = Step::new(job.id, "task", "Execute", 0);
        step.spec_slug = Some("execute".into());
        step.status = if matches!(fault, Some("active" | "active-v2")) {
            StepStatus::Active
        } else {
            StepStatus::Ready
        };
        step.assignee_id = Some(ACTOR.into());
        step.metadata = json!({"request":request});
        jobs.add_step(&step).await.unwrap();
        let bus = boss_testing::RecordingEventBus::new();
        let publisher_bus: Arc<dyn EventBus> = bus.clone();
        let policy = FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build();
        let state = boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(publisher_bus, "jobs"),
            Arc::new(policy),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let selected_path = format!("/api/jobs/{}", job.id);
        let fault = fault
            .filter(|fault| !matches!(*fault, "active" | "active-v2"))
            .map(str::to_owned);
        let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let recorded = recorded.clone();
                let selected_path = selected_path.clone();
                let fault = fault.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    recorded
                        .lock()
                        .unwrap()
                        .push(format!("{} {path}", request.method()));
                    let response = next.run(request).await;
                    if path != selected_path || fault.is_none() {
                        return response;
                    }
                    use axum::response::IntoResponse;
                    match fault.as_deref().unwrap() {
                        "refused" => {
                            return (
                                axum::http::StatusCode::FORBIDDEN,
                                "ordinary selected read refusal",
                            )
                                .into_response();
                        }
                        "empty" => return axum::http::StatusCode::OK.into_response(),
                        "null" => return axum::Json(Value::Null).into_response(),
                        _ => {}
                    }
                    assert!(response.status().is_success());
                    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                    match fault.as_deref().unwrap() {
                        "job" => value["id"] = json!(Uuid::new_v4()),
                        "kind" => value["kind"] = json!("another-protocol"),
                        "version" => value["workflow_version"] = json!(2),
                        "executor" => {
                            value["metadata"]["diagnostic_executor"] = json!("another-actor")
                        }
                        "closed" => value["status"] = json!("cancelled"),
                        "step" => value["steps"][0]["id"] = json!(Uuid::new_v4()),
                        "step-job" => value["steps"][0]["job_id"] = json!(Uuid::new_v4()),
                        "assignee" => value["steps"][0]["assignee_id"] = json!("another-actor"),
                        "pending" => value["steps"][0]["status"] = json!("pending"),
                        "missing-step" => value["steps"] = json!([]),
                        other => panic!("unknown fixture fault: {other}"),
                    }
                    axum::Json(value).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        conductor.cfg.jobs = format!("http://{address}");
        conductor.cfg.diagnostics_home = directory
            .path()
            .join("absent-journal")
            .display()
            .to_string();
        // A fixed invalid clone URL stops beyond hydration without native
        // compute, immutable records, claims or journal writes.
        conductor.cfg.fork_url = "file:///ordinary-invalid-clone".into();
        (
            conductor,
            calls,
            jobs,
            serde_json::to_value(job).unwrap(),
            directory,
        )
    }

    #[tokio::test]
    async fn a_refused_active_request_does_not_starve_the_next_admitted_request() {
        use boss_core::job::{Job, Priority, Step, StepStatus, Subject};
        let (conductor, calls, jobs, first, _directory) =
            selected_request_fixture(Some("active")).await;
        let mut next = Job::new(
            "diagnostic-request",
            Subject::new("custom", "next-fixture"),
            "Next ordinary diagnostic",
            ACTOR,
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap(),
        );
        next.status = boss_core::job::JobStatus::Open;
        next.metadata = first["metadata"].clone();
        jobs.create_job(&next).await.unwrap();
        let mut step = Step::new(next.id, "task", "Execute", 0);
        step.spec_slug = Some("execute".into());
        step.assignee_id = Some(ACTOR.into());
        step.status = StepStatus::Ready;
        step.metadata = json!({"request":next.metadata["request"]});
        jobs.add_step(&step).await.unwrap();
        let before = jobs.recorded_events().len();
        let error = drain(&conductor).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("credential-free HTTP repository URL")
                || error
                    .to_string()
                    .contains("running registered conductor actor")
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "GET /api/jobs".to_owned(),
                format!("GET /api/jobs/{}", first["id"].as_str().unwrap()),
                format!("GET /api/jobs/{}", next.id),
            ],
            "a refused active packet must not monopolize every polling pass"
        );
        assert_eq!(jobs.recorded_events().len(), before);
        assert!(!std::path::Path::new(&conductor.cfg.diagnostics_home).exists());
    }

    #[tokio::test]
    async fn drain_hydrates_the_selected_compact_row_through_the_owning_detail_door() {
        let (conductor, calls, jobs, job, _directory) = selected_request_fixture(None).await;
        let id = job["id"].as_str().unwrap();
        let list = conductor
            .api(Method::GET, "/api/jobs?status=open", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(list["total"], 1);
        assert_eq!(list["full"], false);
        let compact_metadata = &list["data"][0]["steps"][0]["metadata"];
        assert!(compact_metadata.is_null() || compact_metadata == &json!({}));
        assert!(
            serde_json::from_value::<Request>(
                list["data"][0]["steps"][0]["metadata"]["request"].clone()
            )
            .is_err()
        );
        let full = conductor.get_job(id).await.unwrap();
        assert_eq!(
            full["steps"][0]["metadata"]["request"],
            job["metadata"]["request"]
        );
        calls.lock().unwrap().clear();
        let before = jobs.recorded_events().len();
        let error = drain(&conductor).await.unwrap_err();
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["GET /api/jobs".to_owned(), format!("GET /api/jobs/{id}")],
            "the compact row must be hydrated before any controller operation"
        );
        let message = error.to_string();
        assert!(
            message.contains("credential-free HTTP repository URL")
                || message.contains("running registered conductor actor"),
            "{error:#}"
        );
        assert_eq!(jobs.recorded_events().len(), before);
        assert!(!std::path::Path::new(&conductor.cfg.diagnostics_home).exists());
    }

    #[tokio::test]
    async fn selected_detail_read_and_identity_refusals_precede_every_diagnostic_effect() {
        for fault in [
            "refused",
            "empty",
            "null",
            "job",
            "kind",
            "version",
            "executor",
            "closed",
            "step",
            "step-job",
            "assignee",
            "pending",
            "missing-step",
        ] {
            let (conductor, calls, jobs, job, _directory) =
                selected_request_fixture(Some(fault)).await;
            let before = jobs.recorded_events().len();
            let error = drain(&conductor).await.unwrap_err();
            assert!(
                error.to_string().contains("diagnostic selected"),
                "{fault}: {error:#}"
            );
            assert_eq!(
                *calls.lock().unwrap(),
                vec![
                    "GET /api/jobs".to_owned(),
                    format!("GET /api/jobs/{}", job["id"].as_str().unwrap())
                ],
                "{fault}: only reads allowed"
            );
            assert_eq!(
                jobs.recorded_events().len(),
                before,
                "{fault}: no immutable record or claim"
            );
            assert!(
                !std::path::Path::new(&conductor.cfg.diagnostics_home).exists(),
                "{fault}: no journal mutation"
            );
        }
    }

    #[tokio::test]
    async fn active_v2_reads_closed_assessments_without_bypassing_ordinary_binding() {
        let (conductor, calls, jobs, _, directory) =
            selected_request_fixture(Some("active-v2")).await;
        let before = jobs.recorded_events().len();
        let error = drain(&conductor).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("credential-free HTTP repository URL")
                || error
                    .to_string()
                    .contains("running registered conductor actor"),
            "{error:#}"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.as_str() == "GET /api/jobs")
                .count(),
            2,
            "owning API must accept the closed-assessment string filter"
        );
        assert!(calls.iter().all(|call| call.starts_with("GET ")));
        assert_eq!(jobs.recorded_events().len(), before);
        assert!(!directory.path().join("absent-journal").exists());
    }

    pub(super) async fn owning_records(
        allow_update: bool,
    ) -> (String, Arc<boss_jobs::InMemoryJobs>, Value, Value) {
        owning_records_with_fault(allow_update, None).await
    }

    async fn owning_records_with_fault(
        allow_update: bool,
        fault: Option<&'static str>,
    ) -> (String, Arc<boss_jobs::InMemoryJobs>, Value, Value) {
        use boss_core::{
            job::{Job, Priority, Step, StepStatus, Subject},
            port::EventBus,
            publisher::DomainPublisher,
        };
        use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
        let jobs = Arc::new(boss_jobs::InMemoryJobs::new());
        let job = Job::new(
            "diagnostic-request",
            Subject::new("custom", "ordinary-fixture"),
            "Ordinary diagnostic",
            ACTOR,
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(),
        );
        jobs.create_job(&job).await.unwrap();
        let mut step = Step::new(job.id, "task", "execute", 0);
        step.status = StepStatus::Active;
        jobs.add_step(&step).await.unwrap();
        let mut policy = FakePolicyClient::builder().allow(
            "platform-admin",
            Action::Read,
            Resource::job(),
            Scope::All,
        );
        if allow_update {
            policy = policy.allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            );
        }
        let bus = boss_testing::RecordingEventBus::new();
        let publisher_bus: Arc<dyn EventBus> = bus.clone();
        let state = boss_jobs::http::JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(publisher_bus, "jobs"),
            Arc::new(policy.build()),
            Arc::new(boss_clock_client::WallClockClient),
        );
        let absent = Arc::new(tokio::sync::Barrier::new(2));
        let writes = Arc::new(tokio::sync::Mutex::new(()));
        let app = boss_jobs::http::router(state).layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let absent = absent.clone();
                let writes = writes.clone();
                async move {
                    let read = request.method() == axum::http::Method::GET
                        && request.uri().path().ends_with("/records/diagnostic.intent");
                    let write = request.method() == axum::http::Method::POST
                        && request.uri().path().ends_with("/metadata/records");
                    // Both callers actually observe absence. Serializing the
                    // real POST handlers then exposes a valid server replay,
                    // rather than an unrelated version-CAS refusal.
                    let _write = if write && fault == Some("concurrent") {
                        Some(writes.lock().await)
                    } else {
                        None
                    };
                    let response = next.run(request).await;
                    if read
                        && fault == Some("concurrent")
                        && response.status() == axum::http::StatusCode::NOT_FOUND
                    {
                        absent.wait().await;
                    }
                    if !write
                        || fault.is_none()
                        || fault == Some("concurrent")
                        || !response.status().is_success()
                    {
                        return response;
                    }
                    use axum::response::IntoResponse;
                    if fault == Some("transport-ambiguous") {
                        return (
                            axum::http::StatusCode::BAD_GATEWAY,
                            "original accepted by the owning router; acknowledgement unavailable",
                        )
                            .into_response();
                    }
                    let mut status = response.status();
                    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                        .await
                        .unwrap();
                    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                    match fault.unwrap() {
                        "missing-disposition" => {
                            value.as_object_mut().unwrap().remove("result");
                        }
                        "disposition-status" => status = axum::http::StatusCode::OK,
                        "record-actor" => value["record"]["actor"] = json!("another-actor"),
                        "record-job" => value["record"]["job_id"] = json!(Uuid::new_v4()),
                        "record-step" => value["record"]["step_id"] = json!(Uuid::new_v4()),
                        "record-key" => value["record"]["key"] = json!("another-record"),
                        "record-event" => value["record"]["event_id"] = json!(Uuid::nil()),
                        "record-digest" => value["record"]["digest"] = json!("0".repeat(64)),
                        "record-canonical" => value["record"]["canonical"] = json!([0]),
                        "record-value" => value["record"]["value"] = json!({"another":"value"}),
                        other => panic!("unknown record fixture fault: {other}"),
                    }
                    (status, axum::Json(value)).into_response()
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            format!("http://{address}"),
            jobs,
            serde_json::to_value(job).unwrap(),
            serde_json::to_value(step).unwrap(),
        )
    }

    fn proposed() -> Intent {
        Intent {
            attempt: Uuid::new_v4(),
            inputs: Inputs {
                request: Request {
                    car: Uuid::new_v4(),
                    candidate: "a".repeat(40),
                    baseline: "b".repeat(40),
                    receipt_sha256: "c".repeat(64),
                },
                admitted_original: json!({"original": "complete ordinary receipt\n".repeat(1000)}),
                pinned_protocol: json!({"version":1}),
                current_car: json!({"hold":"unchanged"}),
                car_binding: json!({"hold":"unchanged"}),
                policy: json!({"version":1}),
                edit_level: json!({"edit_level":"data"}),
                required_checks: vec!["ordinary-check".into()],
                expected_executor_identity: super::super::consist_job::CONTROLLER_IDENTITY.into(),
                observed_cluster_identity: super::super::consist_job::CONTROLLER_IDENTITY.into(),
            },
        }
    }

    #[tokio::test]
    async fn actual_fetch128_retains_versioned_refusal_through_the_owning_record_door() {
        if std::env::var_os("BOSS_PRECREATE_REFUSAL_FIXTURE").is_none() {
            // The native adapter reads its own credential configuration.
            // Give this fixture a private child environment, never the pod's.
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","train::diagnostic_adapter::tests::actual_fetch128_retains_versioned_refusal_through_the_owning_record_door","--nocapture"])
                .env_clear().env("PATH",std::env::var_os("PATH").unwrap())
                .env("HOME",home.path()).env("GIT_CONFIG_NOSYSTEM","1")
                .env("GIT_TERMINAL_PROMPT","0")
                .env("BOSS_PRECREATE_REFUSAL_FIXTURE","child")
                .env("BOSS_TRAIN_FORGE_TOKEN_FILE",home.path().join("absent-fixture-token"))
                .env("BOSS_TRAIN_FORGE_URL","http://fixture.invalid")
                .env("BOSS_MACHINE_TOKEN_DIR",home.path().join("absent-machine-token"))
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (base, jobs, job, step) = owning_records(true).await;
        let root = tempfile::tempdir().unwrap();
        let clone = root.path().join("clone");
        assert!(
            std::process::Command::new("git")
                .args(["init", clone.to_str().unwrap()])
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("HOME", root.path())
                .output()
                .unwrap()
                .status
                .success()
        );
        let mut conductor = Conductor::diagnostic_fixture(&clone);
        conductor.cfg.jobs = base;
        conductor.cfg.fork_url = "http://fixture.invalid/source.git".into();
        let door = NativeDoor {
            conductor: &conductor,
            job: job.clone(),
            step: step.clone(),
            directory: super::super::diagnostic_journal::private_directory(
                &root.path().join("journal"),
            )
            .unwrap(),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let mut intent = proposed();
        intent.inputs.pinned_protocol = json!({"steps":[{"title":"execute","metadata_defaults":{"diagnostic_operation":{"version":2}}}]});
        let error = door.launch(&intent).await.unwrap_err();
        let refusal = error.downcast_ref::<PreCreateRefusal>().unwrap();
        validate_pre_create_refusal(refusal).unwrap();
        assert_eq!(refusal.command["exit"], 128);
        assert!(door.launch_record().await.unwrap().is_none());
        door.keep_pre_create_refusal(refusal).await.unwrap();
        assert_eq!(
            door.pre_create_refusal_record().await.unwrap().as_ref(),
            Some(refusal)
        );
        door.finish_pre_create_refusal(refusal).await.unwrap();
        let after = jobs
            .get_job(&serde_json::from_value(job["id"].clone()).unwrap())
            .await
            .unwrap()
            .unwrap();
        let steps = jobs.list_steps(&after.id).await.unwrap();
        assert_eq!(steps[0].metadata["outcome"], "refused");
        assert!(door.original("diagnostic.launch").await.unwrap().is_none());
        // Even a direct repeat cannot reacquire preparation ownership.
        assert!(door.launch(&intent).await.is_err());
        let originals = super::super::diagnostic_journal::originals(&door.directory).unwrap();
        assert_eq!(
            originals
                .iter()
                .filter(|r| r["observation"]["argv"] == json!(["fetch", "origin"]))
                .count(),
            1
        );
        assert!(!door.directory.with_extension("assembly").exists());
    }

    struct IntentPort<'a> {
        native: NativeDoor<'a>,
        original: Intent,
        dispositions: &'a std::sync::Mutex<Vec<&'static str>>,
        effects: &'a std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Door for IntentPort<'_> {
        async fn inputs(&self) -> Result<Inputs> {
            Ok(self.original.inputs.clone())
        }
        async fn claim(&self) -> Result<()> {
            Ok(())
        }
        async fn first_intent(&self, proposed: &Intent) -> Result<FirstIntent> {
            let accepted = self.native.first_intent(proposed).await?;
            self.dispositions.lock().unwrap().push(match &accepted {
                FirstIntent::Recorded(_) => "recorded",
                FirstIntent::Replayed(_) => "replayed",
            });
            Ok(accepted)
        }
        async fn launch_record(&self) -> Result<Option<Launch>> {
            Ok(None)
        }
        async fn receipt_record(&self) -> Result<Option<Receipt>> {
            Ok(None)
        }
        async fn launch(&self, _intent: &Intent) -> Result<Launch> {
            self.effects
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Launch {
                job_uid: "ordinary-port-incarnation".into(),
                assembled: "d".repeat(40),
                manifest: json!({"fixed":"ordinary port manifest"}),
                original_output: json!({"complete":"ordinary port acknowledgement"}),
            })
        }
        async fn keep_launch(&self, _launch: &Launch) -> Result<()> {
            Ok(())
        }
        async fn observe(&self, _intent: &Intent, _launch: &Launch) -> Result<Option<Observation>> {
            Ok(None)
        }
        async fn keep_receipt(&self, _receipt: &Receipt) -> Result<()> {
            panic!("pending port has no receipt")
        }
        async fn finish(&self, _receipt: &Receipt) -> Result<()> {
            panic!("pending port cannot finish")
        }
    }

    #[tokio::test]
    async fn concurrent_identical_intents_keep_the_owning_record_disposition_and_one_effect() {
        let (base, jobs, job, step) = owning_records_with_fault(true, Some("concurrent")).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let original = proposed();
        let dispositions = std::sync::Mutex::new(Vec::new());
        let effects = std::sync::atomic::AtomicUsize::new(0);
        let port = IntentPort {
            native: NativeDoor {
                conductor: &conductor,
                job,
                step,
                directory: directory.path().join("journal"),
                actor: ACTOR.into(),
                content: crate::attach::tests::store().await,
                may_launch: true,
            },
            original: original.clone(),
            dispositions: &dispositions,
            effects: &effects,
        };
        let (first, second) = tokio::time::timeout(FIXTURE_BACKSTOP, async {
            tokio::join!(
                advance(&port, original.attempt),
                advance(&port, original.attempt)
            )
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "both owning first-record calls must settle; after {FIXTURE_BACKSTOP:?} the dispositions were {:?} and the effects {}",
                dispositions.lock().unwrap(),
                effects.load(std::sync::atomic::Ordering::SeqCst)
            )
        });
        assert!(
            first.is_ok() && second.is_ok(),
            "{:?} {:?}",
            first.as_ref().err().map(ToString::to_string),
            second.as_ref().err().map(ToString::to_string)
        );
        let mut observed = dispositions.lock().unwrap().clone();
        observed.sort();
        assert_eq!(
            effects.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "original dispositions: {observed:?}"
        );
        assert_eq!(observed, ["recorded", "replayed"]);
        assert_eq!(
            jobs.recorded_events()
                .iter()
                .filter(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
                .count(),
            1
        );
        let kept = port
            .native
            .original("diagnostic.intent")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept, serde_json::to_value(original).unwrap());
    }

    #[tokio::test]
    async fn record_disposition_and_provenance_refusals_never_launch_or_erase_the_original() {
        for fault in [
            "missing-disposition",
            "disposition-status",
            "record-actor",
            "record-job",
            "record-step",
            "record-key",
            "record-event",
            "record-digest",
            "record-canonical",
            "record-value",
        ] {
            let (base, jobs, job, step) = owning_records_with_fault(true, Some(fault)).await;
            let directory = tempfile::tempdir().unwrap();
            let mut conductor = Conductor::diagnostic_fixture(directory.path());
            conductor.cfg.jobs = base;
            let original = proposed();
            let dispositions = std::sync::Mutex::new(Vec::new());
            let effects = std::sync::atomic::AtomicUsize::new(0);
            let port = IntentPort {
                native: NativeDoor {
                    conductor: &conductor,
                    job,
                    step,
                    directory: directory.path().join("journal"),
                    actor: ACTOR.into(),
                    content: crate::attach::tests::store().await,
                    may_launch: true,
                },
                original: original.clone(),
                dispositions: &dispositions,
                effects: &effects,
            };
            assert!(
                advance(&port, original.attempt).await.is_err(),
                "{fault}: ambiguous response is not launch authority"
            );
            assert_eq!(
                effects.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{fault}"
            );
            assert_eq!(
                port.native
                    .original("diagnostic.intent")
                    .await
                    .unwrap()
                    .unwrap(),
                serde_json::to_value(original.clone()).unwrap()
            );
            let events = jobs.recorded_events().len();
            assert!(
                matches!(
                    advance(&port, original.attempt).await.unwrap(),
                    Advance::Unknown(_)
                ),
                "{fault}: replay keeps the original UNKNOWN"
            );
            assert_eq!(
                effects.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{fault}: no retry"
            );
            assert_eq!(
                jobs.recorded_events().len(),
                events,
                "{fault}: immutable replay emits no new fact"
            );
            assert!(
                super::super::diagnostic_journal::originals(&port.native.directory)
                    .unwrap()
                    .iter()
                    .any(|row| row["observation"]["http_method"] == "POST"
                        && row["observation"]["complete"] == true),
                "{fault}: complete original response retained"
            );
        }
    }

    #[tokio::test]
    async fn accepted_intent_with_lost_response_and_changed_binding_never_becomes_fresh() {
        let (base, jobs, job, step) =
            owning_records_with_fault(true, Some("transport-ambiguous")).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let original = proposed();
        let dispositions = std::sync::Mutex::new(Vec::new());
        let effects = std::sync::atomic::AtomicUsize::new(0);
        let mut port = IntentPort {
            native: NativeDoor {
                conductor: &conductor,
                job,
                step,
                directory: directory.path().join("journal"),
                actor: ACTOR.into(),
                content: crate::attach::tests::store().await,
                may_launch: true,
            },
            original: original.clone(),
            dispositions: &dispositions,
            effects: &effects,
        };
        assert!(advance(&port, original.attempt).await.is_err());
        let events = jobs.recorded_events().len();
        assert!(matches!(
            advance(&port, original.attempt).await.unwrap(),
            Advance::Unknown(_)
        ));
        port.original.inputs.request.candidate = "e".repeat(40);
        let error = advance(&port, original.attempt).await.err().unwrap();
        assert!(error.to_string().contains("current authoritative inputs"));
        assert_eq!(effects.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(jobs.recorded_events().len(), events);
        assert_eq!(
            port.native
                .original("diagnostic.intent")
                .await
                .unwrap()
                .unwrap(),
            serde_json::to_value(original).unwrap()
        );
    }

    #[tokio::test]
    async fn diagnostic_fetch_uses_published_auth_and_keeps_refusal_originals() {
        use std::io::{Read, Write};
        if let Ok(root) = std::env::var("BOSS_DIAGNOSTIC_AUTH_FIXTURE") {
            boss_testing::reexec::ready();
            let root = std::path::Path::new(&root);
            let mut conductor = Conductor::diagnostic_fixture(&root.join("clone"));
            conductor.cfg.fork_url = std::env::var("BOSS_TRAIN_FORGE_URL").unwrap();
            let door = NativeDoor {
                conductor: &conductor,
                job: json!({}),
                step: json!({}),
                directory: super::super::diagnostic_journal::private_directory(
                    &root.join("journal"),
                )
                .unwrap(),
                actor: ACTOR.into(),
                content: "http://127.0.0.1:9".into(),
                may_launch: true,
            };
            assert!(door.launch(&proposed()).await.is_err());
            assert!(!door.directory.with_extension("assembly").exists());
            assert!(
                super::super::diagnostic_journal::recovered_launch(
                    &door.directory,
                    "unused",
                    "unused"
                )
                .unwrap()
                .is_none()
            );
            return;
        }
        for authenticated in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let git = |args: &[&str]| {
                let output = std::process::Command::new("git")
                    .args(args)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("HOME", root.path())
                    .output()
                    .unwrap();
                assert!(output.status.success());
            };
            let repo = root.path().join("source.git");
            let clone = root.path().join("clone");
            git(&["init", "--bare", repo.to_str().unwrap()]);
            git(&["init", clone.to_str().unwrap()]);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/source.git", listener.local_addr().unwrap());
            git(&[
                "-C",
                clone.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                &url,
            ]);
            let token = root.path().join("fixture.token");
            if authenticated {
                std::fs::write(&token, "ordinary-fixed-fixture-token").unwrap();
            }
            let backend_root = root.path().to_path_buf();
            // The fixture waits for the child's fetch for as long as the
            // child lives, not for a clock: its 30 s used to start before
            // the spawn, so the exec of this whole debug binary on a loaded gate
            // pod was charged to the fetch and redded gate 5fc86d87 on
            // correct code (backlog 7e30ddb0). The child's own work is
            // bounded below, from its `ready()`.
            let child_gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let gone = child_gone.clone();
            let server = std::thread::spawn(move || {
                loop {
                    // Read BEFORE the accept: a child that ended had
                    // already connected, so its request is in the backlog
                    // and this accept still takes it.
                    let ended = gone.load(std::sync::atomic::Ordering::SeqCst);
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket.set_read_timeout(Some(FIXTURE_BACKSTOP)).unwrap();
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                assert_eq!(socket.read(&mut byte).unwrap(), 1);
                                request.extend(byte);
                            }
                            let authorized =
                                String::from_utf8(request).unwrap().lines().any(|line| {
                                    line.eq_ignore_ascii_case(
                                        "Authorization: token ordinary-fixed-fixture-token",
                                    )
                                });
                            if !authorized {
                                socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                                return false;
                            }
                            let output = std::process::Command::new("git")
                                .arg("http-backend")
                                .env("GIT_PROJECT_ROOT", backend_root)
                                .env("GIT_HTTP_EXPORT_ALL", "1")
                                .env("PATH_INFO", "/source.git/info/refs")
                                .env("QUERY_STRING", "service=git-upload-pack")
                                .env("REQUEST_METHOD", "GET")
                                .output()
                                .unwrap();
                            assert!(output.status.success());
                            socket
                                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n")
                                .unwrap();
                            socket.write_all(&output.stdout).unwrap();
                            return true;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(!ended, "fixture request absent: the child ended without it");
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
            });
            // A child process owns the complete fixture environment; parallel tests
            // cannot race credential variables or accidentally read mounted tokens.
            let mut child = boss_testing::reexec::ReExec::new(
                "train::diagnostic_adapter::tests::diagnostic_fetch_uses_published_auth_and_keeps_refusal_originals",
            );
            child
                .command()
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("HOME", root.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("BOSS_DIAGNOSTIC_AUTH_FIXTURE", root.path())
                .env("BOSS_TRAIN_FORGE_TOKEN_FILE", token)
                .env("BOSS_TRAIN_FORGE_URL", &url);
            let output = child.spawn().finish(std::time::Duration::from_secs(30));
            child_gone.store(true, std::sync::atomic::Ordering::SeqCst);
            // Joined before the child's verdict is read, so a server that
            // saw no request does not hide what the child said.
            let served = server.join();
            assert!(
                output.succeeded(),
                "child fixture failed (start {:?}, work {:?}): {}\n{}",
                output.start,
                output.work,
                output.stdout,
                output.stderr
            );
            let observed_auth = served.unwrap();
            let originals =
                super::super::diagnostic_journal::originals(&root.path().join("journal")).unwrap();
            assert_eq!(
                originals[0]["observation"]["argv"],
                json!(["fetch", "origin"])
            );
            assert!(
                !originals[0]["observation"]["stderr_b64"]
                    .as_str()
                    .unwrap()
                    .is_empty()
                    || authenticated
            );
            for row in &originals {
                use base64::Engine;
                for field in ["stdout_b64", "stderr_b64"] {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(row["observation"][field].as_str().unwrap())
                        .unwrap();
                    assert!(
                        !String::from_utf8_lossy(&bytes).contains("ordinary-fixed-fixture-token")
                    );
                }
                assert!(
                    !row["observation"]["argv"]
                        .to_string()
                        .contains("ordinary-fixed-fixture-token")
                );
            }
            assert_eq!(
                observed_auth, authenticated,
                "declared forge transport must actually authenticate"
            );
            assert_eq!(
                originals[0]["observation"]["exit"],
                if authenticated { 0 } else { 128 }
            );
            assert_eq!(originals.len(), if authenticated { 2 } else { 1 });
            if authenticated {
                assert_eq!(
                    originals[1]["observation"]["argv"],
                    json!(["rev-parse", "origin/main"])
                );
                assert_ne!(originals[1]["observation"]["exit"], 0);
            }
            assert!(originals.iter().all(
                |row| row["observation"]["argv"] != json!(["create", "-f", "-", "-o", "json"])
            ));
        }
    }

    #[tokio::test]
    async fn diagnostic_url_refusal_precedes_assembly_and_journal_effects() {
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(&directory.path().join("absent-clone"));
        let mut dummy_userinfo =
            reqwest::Url::parse("https://forge.invalid/example/repository.git").unwrap();
        dummy_userinfo.set_username("review-user").unwrap();
        dummy_userinfo.set_password(Some("not-a-secret")).unwrap();
        conductor.cfg.fork_url = dummy_userinfo.to_string();
        let path =
            super::super::diagnostic_journal::private_directory(&directory.path().join("journal"))
                .unwrap();
        let door = NativeDoor {
            conductor: &conductor,
            job: json!({}),
            step: json!({}),
            directory: path,
            actor: ACTOR.into(),
            content: "http://127.0.0.1:9".into(),
            may_launch: true,
        };
        let failure = door.launch(&proposed()).await.unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("credential-free HTTP repository URL")
        );
        assert!(
            super::super::diagnostic_journal::originals(&door.directory)
                .unwrap()
                .is_empty()
        );
        assert!(!door.directory.with_extension("assembly").exists());
    }

    #[tokio::test]
    async fn real_record_door_keeps_full_original_and_replay_has_no_new_fact() {
        let (base, jobs, job, step) = owning_records(true).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let door = NativeDoor {
            conductor: &conductor,
            job,
            step,
            directory: directory.path().join("journal"),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let original = proposed();
        let before = jobs.recorded_events().len();
        let FirstIntent::Recorded(saved) = door.first_intent(&original).await.unwrap() else {
            panic!("first record absent")
        };
        assert_eq!(saved, original);
        let facts = jobs.recorded_events().len();
        assert!(
            facts > before,
            "owning door emits its first record and DAG reevaluation facts"
        );
        assert_eq!(
            jobs.recorded_events()
                .iter()
                .filter(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
                .count(),
            1
        );
        let mut next = original.clone();
        next.attempt = Uuid::new_v4();
        let FirstIntent::Replayed(replayed) = door.first_intent(&next).await.unwrap() else {
            panic!("replay absent")
        };
        assert_eq!(replayed, original);
        assert_eq!(jobs.recorded_events().len(), facts);
        assert_eq!(
            door.original("diagnostic.intent").await.unwrap().unwrap(),
            serde_json::to_value(original).unwrap()
        );
    }

    #[tokio::test]
    async fn real_record_refusal_retains_full_http_body_and_creates_no_fact() {
        let (base, jobs, job, step) = owning_records(false).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let door = NativeDoor {
            conductor: &conductor,
            job,
            step,
            directory: directory.path().join("journal"),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let before = jobs.recorded_events().len();
        assert!(door.first_intent(&proposed()).await.is_err());
        assert_eq!(jobs.recorded_events().len(), before);
        let originals = super::super::diagnostic_journal::originals(&door.directory).unwrap();
        let refusals: Vec<_> = originals
            .iter()
            .filter(|row| row["observation"]["status"] == 403)
            .collect();
        assert!(!refusals.is_empty());
        for row in refusals {
            let encoded = row["observation"]["response_b64"].as_str().unwrap();
            assert!(
                !base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn an_inline_sized_original_is_attached_when_its_first_record_event_is_too_large() {
        let (base, jobs, job, step) = owning_records(true).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let door = NativeDoor {
            conductor: &conductor,
            job,
            step,
            directory: directory.path().join("journal"),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let mut original = proposed();
        original.inputs.admitted_original =
            json!({"full_original":"diagnostic output".repeat(12_000)});
        let raw = serde_json::to_vec(&original).unwrap();
        assert!(raw.len() < boss_events::outbox::MAX_STAGED_EVENT_BYTES / 4);
        let FirstIntent::Recorded(saved) = door.first_intent(&original).await.unwrap() else {
            panic!("original not recorded")
        };
        assert_eq!(saved, original);
        let recorded = jobs.recorded_events();
        let event = recorded
            .iter()
            .find(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
            .unwrap();
        let encoded = serde_json::to_vec(event).unwrap();
        assert!(
            encoded.len() <= boss_events::outbox::MAX_STAGED_EVENT_BYTES,
            "{} byte immutable event exceeds {} despite an inline-sized original",
            encoded.len(),
            boss_events::outbox::MAX_STAGED_EVENT_BYTES
        );
        assert_eq!(event.payload["record"]["value"]["kind"], "attachment");
        let facts = recorded.len();
        let mut retry = original.clone();
        retry.attempt = Uuid::new_v4();
        let FirstIntent::Replayed(saved) = door.first_intent(&retry).await.unwrap() else {
            panic!("original not replayed")
        };
        assert_eq!(saved, original);
        assert_eq!(jobs.recorded_events().len(), facts);
    }

    #[tokio::test]
    async fn first_record_sizing_reads_current_step_metadata_before_choosing_inline() {
        let (base, jobs, job, step) = owning_records(true).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let old: boss_core::job::Step = serde_json::from_value(step.clone()).unwrap();
        let mut current = old.clone();
        current.metadata = json!({"ordinary_kept":"kept".repeat(100_000)});
        let (_, read) = jobs.get_step_versioned(&old.id).await.unwrap().unwrap();
        jobs.update_step_if_unchanged_at(&current, read, chrono::Utc::now(), &[])
            .await
            .unwrap();
        let door = NativeDoor {
            conductor: &conductor,
            job,
            step,
            directory: directory.path().join("journal"),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let mut original = proposed();
        original.inputs.admitted_original = json!({"full_original":"diagnostic".repeat(12_000)});
        let value = serde_json::to_value(&original).unwrap();
        let actor = ACTOR.parse().unwrap();
        assert!(first_record_fits(&old, "diagnostic.intent", &value, &actor).unwrap());
        assert!(!first_record_fits(&current, "diagnostic.intent", &value, &actor).unwrap());
        let FirstIntent::Recorded(saved) = door.first_intent(&original).await.unwrap() else {
            panic!("original not recorded")
        };
        assert_eq!(saved, original);
        let recorded = jobs.recorded_events();
        let event = recorded
            .iter()
            .find(|event| event.kind == boss_jobs::events::STEP_FIRST_RECORDED)
            .unwrap();
        assert!(
            serde_json::to_vec(event).unwrap().len() <= boss_events::outbox::MAX_STAGED_EVENT_BYTES
        );
        assert_eq!(event.payload["record"]["value"]["kind"], "attachment");
        assert_eq!(
            event.payload["step"]["metadata"]["ordinary_kept"],
            current.metadata["ordinary_kept"]
        );
    }

    #[tokio::test]
    async fn full_output_above_event_bound_round_trips_through_real_content_and_record_ports() {
        let (base, jobs, job, step) = owning_records(true).await;
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        conductor.cfg.jobs = base;
        let door = NativeDoor {
            conductor: &conductor,
            job,
            step,
            directory: directory.path().join("journal"),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: true,
        };
        let mut original = proposed();
        let full = "ordinary original é \\ literal\r\n".repeat(40_000);
        original.inputs.admitted_original = json!({"full_original_output":full});
        assert!(
            serde_json::to_vec(&original).unwrap().len()
                > boss_events::outbox::MAX_STAGED_EVENT_BYTES
        );
        let FirstIntent::Recorded(saved) = door.first_intent(&original).await.unwrap() else {
            panic!("original not stored")
        };
        assert_eq!(saved, original);
        let facts = jobs.recorded_events().len();
        let mut retry = original.clone();
        retry.attempt = Uuid::new_v4();
        let FirstIntent::Replayed(saved) = door.first_intent(&retry).await.unwrap() else {
            panic!("replay not stored")
        };
        assert_eq!(saved, original);
        assert_eq!(jobs.recorded_events().len(), facts);
        assert_eq!(
            saved.inputs.admitted_original["full_original_output"]
                .as_str()
                .unwrap()
                .as_bytes(),
            full.as_bytes()
        );
    }

    #[tokio::test]
    async fn truncated_http_response_keeps_received_bytes_before_reporting_unknown() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let partial = b"ordinary partial diagnostic response\r\n";
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 8192];
            let received = stream.read(&mut request).await.unwrap();
            assert!(received > 0, "the client sent no request bytes");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            stream.write_all(partial).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            stream.shutdown().await.unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(directory.path());
        // This ordinary transport fixture reads no credential source.
        conductor.http =
            crate::machine_http::MachineClient::unstamped(reqwest::Client::builder()).unwrap();
        conductor.cfg.jobs = format!("http://{address}");
        let path =
            super::super::diagnostic_journal::private_directory(&directory.path().join("journal"))
                .unwrap();
        let door = NativeDoor {
            conductor: &conductor,
            job: json!({}),
            step: json!({}),
            directory: path,
            actor: ACTOR.into(),
            content: "http://127.0.0.1:9".into(),
            may_launch: true,
        };
        assert!(door.api(Method::GET, "/ordinary", None).await.is_err());
        let observations = super::super::diagnostic_journal::originals(&door.directory).unwrap();
        assert!(
            observations.iter().any(
                |row| row["observation"]["response_b64"]
                    .as_str()
                    .is_some_and(|encoded| {
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
                            .unwrap()
                            == partial
                    })
            ),
            "received original bytes disappeared on a transport refusal"
        );
    }
}
