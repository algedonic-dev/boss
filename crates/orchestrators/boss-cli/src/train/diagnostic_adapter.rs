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

fn find_step<'a>(job: &'a Value, slug: &str) -> Option<&'a Value> {
    super::find_step(job, slug, slug)
}

pub(super) async fn drain(conductor: &Conductor) -> Result<()> {
    let filter = percent_encoding::utf8_percent_encode(
        &json!({"diagnostic_executor":ACTOR}).to_string(),
        crate::job::QUERY_VALUE,
    )
    .to_string();
    let requests = list_all_pages(|offset| {
        let filter = &filter;
        async move {
            conductor
                .api(
                    Method::GET,
                    &format!(
                        "/api/jobs?status=open&metadata={filter}&limit={PAGE_LIMIT}&offset={offset}"
                    ),
                    None,
                )
                .await
        }
    })
    .await?;
    let mut ours: Vec<Value> = requests
        .into_iter()
        .filter(|job| {
            job["metadata"]["diagnostic_executor"] == ACTOR
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
    advance_requests(ours, |selected, may_launch| {
        advance_selected(conductor, selected, may_launch)
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
    may_launch: bool,
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
        || job["metadata"]["diagnostic_executor"] != ACTOR
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
    advance(&door, id).await
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
        let http: &boss_core::machine_token::Client = &self.conductor.http;
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
                return Err(error.into());
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
    async fn original(&self, key: &str) -> Result<Option<Value>> {
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
        if record.value.get("kind").and_then(Value::as_str) == Some("attachment") {
            let text = crate::dispatch_brief::read_at(
                &self.conductor.http,
                &self.content,
                &crate::identity::header(&self.actor),
                &record.value,
            )
            .await?;
            Ok(Some(serde_json::from_str(&text)?))
        } else {
            Ok(Some(record.value))
        }
    }
    async fn keep(&self, key: &str, value: &Value) -> Result<Value> {
        let reference = self.artifact(value, Some(key)).await?;
        let raw = self
            .api(
                Method::POST,
                &format!(
                    "/api/jobs/{}/steps/{}/metadata/records",
                    self.id()?,
                    self.step_id()?
                ),
                Some(json!({"key":key,"value":reference,"expected_absence":true})),
            )
            .await?;
        let result: boss_jobs::first_record::FirstRecordResult = serde_json::from_value(raw)?;
        match result {
            boss_jobs::first_record::FirstRecordResult::Recorded(_)
            | boss_jobs::first_record::FirstRecordResult::Replayed(_) => {}
            _ => bail!("diagnostic original record was not accepted; no completion"),
        }
        self.original(key)
            .await?
            .context("diagnostic original record read-back absent")
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
            || published.status == boss_jobs::registry::WorkflowStatus::Draft
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
            || operation["version"] != 1
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
        let accepted = self
            .keep("diagnostic.intent", &serde_json::to_value(proposed)?)
            .await?;
        let intent: Intent = serde_json::from_value(accepted)?;
        Ok(if intent == *proposed {
            FirstIntent::Recorded(intent)
        } else {
            FirstIntent::Replayed(intent)
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
        let clone = self.conductor.cfg.clone.clone();
        let url = self.conductor.cfg.fork_url.clone();
        let directory = self.directory.clone();
        let intent = intent.clone();
        let budget = self.conductor.policy.consist_budget.as_secs();
        tokio::task::spawn_blocking(move || -> Result<Launch> {
            let name = intent.job_name();
            let tree = directory.with_extension("assembly");
            // Separate normal assembly; no car branch, hold or conductor HEAD moves.
            let git = |args: &[&str]| -> Result<std::process::Output> {
                let output = crate::git_auth::command()
                    .args(["-c", "core.hooksPath=/dev/null", "-C", &clone])
                    .args(args)
                    .output()?;
                super::diagnostic_journal::command(&directory, args, None, &output)?;
                if !output.status.success() {
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
        let evidence = self.artifact(&serde_json::to_value(receipt)?, None).await?;
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
        job.metadata = json!({"diagnostic_executor":ACTOR,"request":request});
        jobs.create_job(&job).await.unwrap();
        let mut step = Step::new(job.id, "task", "Execute", 0);
        step.spec_slug = Some("execute".into());
        step.status = if fault == Some("active") {
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
        let fault = fault.filter(|fault| *fault != "active").map(str::to_owned);
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
            boss_core::machine_token::Client::unstamped(reqwest::Client::builder()).unwrap();
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

    async fn owning_records(
        allow_update: bool,
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
        let app = boss_jobs::http::router(state);
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
    async fn diagnostic_fetch_uses_published_auth_and_keeps_refusal_originals() {
        use std::io::{Read, Write};
        if let Ok(root) = std::env::var("BOSS_DIAGNOSTIC_AUTH_FIXTURE") {
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
            let server = std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            socket
                                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                                .unwrap();
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
                            assert!(
                                std::time::Instant::now() < deadline,
                                "fixture request absent"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(error) => panic!("{error}"),
                    }
                }
            });
            // A child process owns the complete fixture environment; parallel tests
            // cannot race credential variables or accidentally read mounted tokens.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "train::diagnostic_adapter::tests::diagnostic_fetch_uses_published_auth_and_keeps_refusal_originals", "--nocapture"])
                .env_clear().env("PATH", std::env::var_os("PATH").unwrap())
                .env("HOME", root.path()).env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("BOSS_DIAGNOSTIC_AUTH_FIXTURE", root.path())
                .env("BOSS_TRAIN_FORGE_TOKEN_FILE", token)
                .env("BOSS_TRAIN_FORGE_URL", &url)
                .output().unwrap();
            let observed_auth = server.join().unwrap();
            assert!(output.status.success(), "child fixture failed");
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
            boss_core::machine_token::Client::unstamped(reqwest::Client::builder()).unwrap();
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
