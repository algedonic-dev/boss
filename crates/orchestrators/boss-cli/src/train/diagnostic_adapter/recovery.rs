//! A separate assessment records evidence; it never resumes native execution.
use super::*;
use base64::Engine;

async fn retain_assessment(door: &NativeDoor<'_>, value: &Value) -> Result<()> {
    if let Some(old) = door.original("diagnostic.recovery-assessment").await? {
        if old != *value {
            bail!("recovery assessment original differs");
        }
    } else if door.keep("diagnostic.recovery-assessment", value).await? != *value {
        bail!("recovery assessment readback differs");
    }
    let current = door
        .api(Method::GET, &format!("/api/jobs/{}", door.id()?), None)
        .await?;
    let step = find_step(&current, "execute").context("recovery completion step absent")?;
    if current["id"] != door.id()? || step["id"] != door.step_id()? {
        bail!("recovery completion identity differs");
    }
    if step["status"] == "completed" {
        let evidence = &step["metadata"]["evidence"];
        let evidence = door.read_artifact(evidence).await?;
        if step["metadata"]["outcome"] != "recovered-refusal" || evidence != *value {
            bail!("recovery completed evidence differs");
        }
        return Ok(());
    }
    door.complete_result("recovered-refusal", value).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AssessmentRequest {
    original_request: Uuid,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    handler: String,
    version: u32,
    original_request: Uuid,
    authority_design: Uuid,
    authority_anchor: String,
    resolution_sha256: String,
}

/// Only the conductor resolves native evidence. The caller supplies an identity,
/// never a journal path, a namespace, a command, or a claimed native result.
pub(super) async fn advance(door: &NativeDoor<'_>) -> Result<Advance> {
    if door.actor != ACTOR || crate::identity::conductor() != ACTOR {
        bail!("recovery requires the registered conductor actor");
    }
    let kind = door.job["kind"]
        .as_str()
        .context("recovery protocol kind absent")?;
    let version = door.job["workflow_version"]
        .as_u64()
        .filter(|v| *v > 0)
        .context("recovery pinned version absent")?;
    let protocol = door
        .api(
            Method::GET,
            &format!("/api/workflows/{kind}/versions/{version}"),
            None,
        )
        .await?;
    let spec: boss_jobs::registry::WorkflowSpec = serde_json::from_value(protocol.clone())?;
    if spec.kind != kind
        || u64::try_from(spec.version) != Ok(version)
        || spec.status == boss_jobs::registry::WorkflowStatus::Draft
    {
        bail!("recovery published protocol differs");
    }
    let execute = spec
        .steps
        .iter()
        .find(|s| s.title == "execute")
        .context("recovery operation absent")?;
    let operation: Operation =
        serde_json::from_value(execute.metadata_defaults["recovery_operation"].clone())?;
    if execute.selectors().assignee_id.as_deref() != Some(ACTOR)
        || operation.handler != "retained-preparation-assessment"
        || operation.version != 1
    {
        bail!("recovery has no registered versioned reader");
    }
    let request: AssessmentRequest =
        serde_json::from_value(door.step["metadata"]["request"].clone())?;
    if request.original_request.is_nil()
        || request.original_request != operation.original_request
        || request.original_request.to_string() == door.id()?
    {
        bail!("recovery request is outside its decided original-attempt scope");
    }
    let history = door
        .api(
            Method::GET,
            &format!("/api/jobs/{}/events?limit=1000", door.id()?),
            None,
        )
        .await?;
    let creations: Vec<_> = history["data"]
        .as_array()
        .context("recovery admission absent")?
        .iter()
        .filter(|e| e["kind"] == boss_jobs::events::JOB_CREATED)
        .collect();
    if creations.len() != 1
        || creations[0]["payload"]["id"] != door.id()?
        || creations[0]["payload"]["kind"] != kind
        || creations[0]["payload"]["workflow_version"] != version
        || creations[0]["payload"]["metadata"]["request"] != door.step["metadata"]["request"]
        || creations[0]["actor"].as_str().is_none_or(str::is_empty)
    {
        bail!("recovery original admission binding differs");
    }
    let authority = door
        .api(
            Method::GET,
            &format!("/api/jobs/{}", operation.authority_design),
            None,
        )
        .await?;
    let review = find_step(&authority, "review").context("recovery authority review absent")?;
    let resolutions = review["metadata"]["resolutions"]
        .as_array()
        .context("recovery authority decisions absent")?;
    let matches: Vec<_> = resolutions
        .iter()
        .filter(|r| r["anchor"] == operation.authority_anchor)
        .collect();
    if authority["id"] != operation.authority_design.to_string()
        || authority["kind"] != "design-doc"
        || review["status"] != "completed"
        || review["completed_by"].as_str().is_none_or(str::is_empty)
        || matches.len() != 1
        // The published producer hashes compact, sorted JSON. Step-shape
        // canonical_json_bytes uses a different historical delimiter encoding.
        || hex::encode(Sha256::digest(serde_json::to_vec(matches[0])?))
            != operation.resolution_sha256
    {
        bail!("recovery authority differs from the published decided contract");
    }
    let already_completed = door.step["status"] == "completed";
    if !already_completed {
        door.claim().await?;
    }
    let original = door
        .api(
            Method::GET,
            &format!("/api/jobs/{}", request.original_request),
            None,
        )
        .await?;
    let original_step = find_step(&original, "execute")
        .context("original diagnostic execute absent")?
        .clone();
    if original["id"] != request.original_request.to_string()
        || original["kind"] != "diagnostic-request"
        || original["workflow_version"] != 2
        || original["status"] != "open"
        || original_step["status"] != "active"
        || original_step["assignee_id"] != ACTOR
    {
        bail!("recovery subject is not the decided active original v2 diagnostic");
    }
    // Reads of the original API records are journalled on THIS assessment,
    // keeping the original preparation journal byte-for-byte untouched.
    let source = NativeDoor {
        conductor: door.conductor,
        job: original.clone(),
        step: original_step,
        directory: door.directory.clone(),
        actor: door.actor.clone(),
        content: door.content.clone(),
        may_launch: false,
    };
    let intent_record = source
        .original_record("diagnostic.intent")
        .await?
        .context("original immutable intent absent")?;
    let intent: Intent = serde_json::from_value(source.record_value(&intent_record).await?)?;
    let invocation_record = source
        .original_record("diagnostic.invocation")
        .await?
        .context("original winning invocation absent")?;
    let invocation = source.record_value(&invocation_record).await?;
    if intent.attempt != request.original_request
        || intent.inputs.admitted_original["payload"]["id"] != request.original_request.to_string()
        || intent.inputs.admitted_original["payload"]["kind"] != "diagnostic-request"
        || intent.inputs.admitted_original["payload"]["workflow_version"] != 2
        || intent.inputs.admitted_original["payload"]["metadata"]["request"]
            != source.step["metadata"]["request"]
        || intent.inputs.pinned_protocol["kind"] != "diagnostic-request"
        || intent.inputs.pinned_protocol["version"] != 2
        || intent.inputs.expected_executor_identity
            != super::super::consist_job::CONTROLLER_IDENTITY
        || intent.inputs.observed_cluster_identity != intent.inputs.expected_executor_identity
        || serde_json::to_value(&intent.inputs.request)? != source.step["metadata"]["request"]
        || !allows_pre_create_refusal(&intent)
    {
        bail!("original intent/request/version binding differs");
    }
    for key in [
        "diagnostic.launch",
        "diagnostic.receipt",
        "diagnostic.pre-create-refusal",
    ] {
        if source.original_record(key).await?.is_some() {
            bail!("recovery conflicts with an original launch or disposition");
        }
    }
    if let Some(value) = door.original("diagnostic.recovery-assessment").await? {
        if value["schema"] != 1
            || value["assessment_request"] != door.id()?
            || value["subject"]["original_request"] != request.original_request.to_string()
            || value["intent_record"] != serde_json::to_value(&intent_record)?
            || value["invocation_record"] != serde_json::to_value(&invocation_record)?
            || value["authority"]["id"] != authority["id"]
            || !same_protocol(&value["protocol"], &protocol)
            || value["intent"] != serde_json::to_value(&intent)?
            || value["admission"] != *creations[0]
            || value["subject"]["car"] != intent.inputs.request.car.to_string()
            || value["outcome"] != "recovered-refusal"
            || value["native"]["namespace"] != "boss-dev"
            || value["native"]["identity"] != super::super::consist_job::CONTROLLER_IDENTITY
        {
            bail!("recovery replay binding differs");
        }
        let originals = parse_snapshot(&value["journal"])?;
        let kept_review =
            find_step(&value["authority"], "review").context("retained authority review absent")?;
        let kept_resolutions = kept_review["metadata"]["resolutions"]
            .as_array()
            .context("retained authority decisions absent")?;
        if kept_review["status"] != "completed"
            || kept_resolutions
                .iter()
                .filter(|r| r["anchor"] == operation.authority_anchor)
                .collect::<Vec<_>>()
                != matches
        {
            bail!("retained recovery authority differs");
        }
        let observed = assess(&intent, &invocation, &originals, &value["native"]["jobs"])?;
        validate_native(&value["native"], &intent.job_name())?;
        if value["observed_baseline"] != observed {
            bail!("recovery replay judgment differs");
        }
        retain_assessment(door, &value).await?;
        return Ok(Advance::Assessed);
    }
    if already_completed {
        bail!("completed assessment has no immutable disposition");
    }
    let retained = door.original("recovery.journal").await?;
    let fresh = retained.is_none();
    let path = PathBuf::from(&door.conductor.cfg.diagnostics_home)
        .join(request.original_request.to_string());
    let reader = door.directory.clone();
    // A named actor is not a native identity. Authenticate before the first
    // private read, and again before a retained snapshot resumes assessment.
    let journal = tokio::task::spawn_blocking(move || {
        authenticated_snapshot(
            || super::super::consist_job::diagnostic_identity(&reader, false),
            || match retained {
                Some(value) => Ok(value),
                None => snapshot(&path),
            },
        )
    })
    .await??;
    if fresh && door.keep("recovery.journal", &journal).await? != journal {
        bail!("recovery whole journal readback differs");
    }
    let originals = parse_snapshot(&journal)?;
    let native = if let Some(value) = door.original("recovery.native").await? {
        value
    } else {
        let path = door.directory.clone();
        let name = intent.job_name();
        let raw=tokio::task::spawn_blocking(move||->Result<Value>{
            let control=super::super::consist_job::ConsistControl::recorded_session(&path)?;
            let jobs=control.json(&["get","jobs","--field-selector",&format!("metadata.name={name}"),"-o","json"])?;
            let pods=control.json(&["get","pods","-o","json"])?;
            let originals=super::super::diagnostic_journal::originals(&path)?;
            Ok(json!({"namespace":"boss-dev","identity":super::super::consist_job::CONTROLLER_IDENTITY,"jobs":jobs,"pods":pods,"command_originals":originals}))
        }).await??;
        if door.keep("recovery.native", &raw).await? != raw {
            bail!("recovery native originals readback differs");
        }
        raw
    };
    if native["namespace"] != "boss-dev"
        || native["identity"] != super::super::consist_job::CONTROLLER_IDENTITY
    {
        bail!("recovery native scope differs");
    }
    let observed = assess(&intent, &invocation, &originals, &native["jobs"])?;
    validate_native(&native, &intent.job_name())?;
    let value = json!({"schema":1,"outcome":"recovered-refusal","assessment_request":door.id()?,
        "subject":{"original_request":request.original_request,"car":intent.inputs.request.car},
        "protocol":protocol,"authority":authority,"admission":creations[0],"original_packet":original,
        "intent_record":intent_record,"invocation_record":invocation_record,"intent":intent,
        "journal":journal,"native":native,"observed_baseline":observed});
    retain_assessment(door, &value).await?;
    Ok(Advance::Assessed)
}

fn snapshot(path: &std::path::Path) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_dir() {
        bail!("original journal unavailable; never create a replacement");
    }
    super::super::diagnostic_journal::private_directory(path)?;
    let lock_path = path.join("journal.lock");
    let lock_meta = std::fs::symlink_metadata(&lock_path)?;
    if !lock_meta.is_file()
        || lock_meta.uid() != rustix::process::geteuid().as_raw()
        || lock_meta.mode() & 0o077 != 0
    {
        bail!("original journal lock unsafe");
    }
    let lock = std::fs::File::open(lock_path)?;
    lock.lock_shared()?;
    let mut files = Vec::new();
    for item in std::fs::read_dir(path)? {
        let item = item?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF8 original name"))?;
        if name == "journal.lock" {
            continue;
        }
        let candidates = if name == "pending" {
            super::super::diagnostic_journal::private_directory(&item.path())?;
            std::fs::read_dir(item.path())?
                .map(|r| {
                    r.map(|r| {
                        (
                            format!("pending/{}", r.file_name().to_string_lossy()),
                            r.path(),
                        )
                    })
                })
                .collect::<std::io::Result<Vec<_>>>()?
        } else {
            vec![(name, item.path())]
        };
        for (name, file) in candidates {
            let m = std::fs::symlink_metadata(&file)?;
            if !m.is_file()
                || m.uid() != rustix::process::geteuid().as_raw()
                || m.mode() & 0o077 != 0
            {
                bail!("unsafe original journal file");
            }
            let raw = std::fs::read(file)?;
            files.push(json!({"path":name,"bytes_b64":base64::engine::general_purpose::STANDARD.encode(&raw)}));
        }
    }
    files.sort_by_key(|f| f["path"].as_str().unwrap_or_default().to_owned());
    Ok(json!({"files":files}))
}

fn authenticated_snapshot<A, R>(authenticate: A, read: R) -> Result<Value>
where
    A: FnOnce() -> Result<String>,
    R: FnOnce() -> Result<Value>,
{
    if authenticate()? != super::super::consist_job::CONTROLLER_IDENTITY {
        bail!("current native identity is not the registered conductor; no private read");
    }
    read()
}

fn parse_snapshot(value: &Value) -> Result<Vec<Value>> {
    let files = value["files"]
        .as_array()
        .context("whole original files absent")?;
    let mut originals = Vec::new();
    let mut accepted = std::collections::BTreeSet::new();
    let mut pending = Vec::new();
    let mut paths = std::collections::BTreeSet::new();
    for file in files {
        let name = file["path"].as_str().context("original filename absent")?;
        if !paths.insert(name) {
            bail!("duplicated original filename");
        }
        let raw = stream(file, "bytes_b64")?;
        if name.starts_with("pending/") {
            pending.push(raw);
            continue;
        }
        let row: Value = serde_json::from_slice(&raw)?;
        let sequence = row["sequence"]
            .as_u64()
            .context("original sequence absent")?;
        if name != format!("{sequence:020}-{}.json", hex::encode(Sha256::digest(&raw))) {
            bail!("original filename/hash differs");
        }
        accepted.insert(raw);
        originals.push(row);
    }
    originals.sort_by_key(|r| r["sequence"].as_u64());
    if originals.is_empty() || pending.iter().any(|raw| !accepted.contains(raw)) {
        bail!("original journal has interrupted unpublished bytes");
    }
    for (sequence, row) in originals.iter().enumerate() {
        if row["sequence"].as_u64() != Some(sequence as u64) {
            bail!("original journal sequence differs");
        }
    }
    Ok(originals)
}

pub(super) fn pending_originals(
    requests: Vec<Value>,
    assessed: &std::collections::BTreeSet<Uuid>,
) -> Result<Vec<Value>> {
    requests
        .into_iter()
        .filter_map(
            |job| match job_id(&job).and_then(|id| Ok(Uuid::parse_str(id)?)) {
                Ok(id) if assessed.contains(&id) => None,
                Ok(_) => Some(Ok(job)),
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
}

/// The normal queue reader consumes completed disposition evidence. It does
/// not write the original or let an assessment admit another diagnostic.
pub(super) async fn covered_original(conductor: &Conductor, selected: &Value) -> Result<Uuid> {
    let id = Uuid::parse_str(job_id(selected)?)?;
    let job = conductor.get_job(&id.to_string()).await?;
    let step = find_step(&job, "execute")
        .context("assessment disposition step absent")?
        .clone();
    if job["id"] != id.to_string()
        || job["kind"] != selected["kind"]
        || job["workflow_version"] != selected["workflow_version"]
        || job["status"] != "closed"
        || job["metadata"]["diagnostic_executor"] != ACTOR
        || step["status"] != "completed"
        || step["assignee_id"] != ACTOR
        || step["job_id"] != id.to_string()
        || step["metadata"]["outcome"] != "recovered-refusal"
    {
        bail!("assessment is not an independently completed disposition");
    }
    let request: AssessmentRequest = serde_json::from_value(step["metadata"]["request"].clone())?;
    let directory = PathBuf::from(&conductor.cfg.diagnostics_home).join(id.to_string());
    let content = crate::tenant_publish::service_on_door(&conductor.cfg.jobs, "content")?;
    let door = NativeDoor {
        conductor,
        job,
        step,
        directory,
        actor: crate::identity::conductor(),
        content,
        may_launch: false,
    };
    if !matches!(advance(&door).await?, Advance::Assessed) {
        bail!("assessment disposition unread");
    }
    Ok(request.original_request)
}

fn assess(
    intent: &Intent,
    invocation: &Value,
    originals: &[Value],
    native: &Value,
) -> Result<String> {
    if !allows_pre_create_refusal(intent)
        || intent.attempt.is_nil()
        || invocation["phase"] != "preparing"
        || invocation["attempt"] != intent.attempt.to_string()
        || invocation["invocation"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .is_none_or(|id| id.is_nil())
    {
        bail!("recovery has no exact original v2 winning invocation");
    }
    let preparations: Vec<_> = originals
        .iter()
        .enumerate()
        .filter(|(_, row)| row["observation"]["phase"] == "preparing")
        .collect();
    if preparations.len() != 1 || preparations[0].1["observation"] != *invocation {
        bail!("recovery preparation is absent, duplicated or conflicting");
    }
    let start = preparations[0].0;
    let fetch = &originals
        .get(start + 1)
        .context("successful fetch original absent")?["observation"];
    let baseline = &originals
        .get(start + 2)
        .context("baseline read original absent")?["observation"];
    if fetch["argv"] != json!(["fetch", "origin"])
        || fetch["exit"] != 0
        || baseline["argv"] != json!(["rev-parse", "origin/main"])
        || baseline["exit"] != 0
    {
        bail!("recovery requires the adjacent successful fetch and baseline read");
    }
    for (sequence, row) in originals.iter().enumerate() {
        if row["sequence"].as_u64() != Some(sequence as u64) {
            bail!("recovery journal is not contiguous");
        }
        let o = &row["observation"];
        if o == invocation {
            continue;
        }
        if o["phase"] == "native-invocation" {
            if o["namespace"] != "boss-dev"
                || o["program"] != "/usr/local/bin/kubectl"
                || !readonly_native(&o["args"])
            {
                bail!("recovery contains possible native effect or conflicting namespace");
            }
            if originals
                .get(sequence + 1)
                .is_none_or(|row| row["observation"]["argv"] != o["args"])
            {
                bail!("native invocation has no matching completed command");
            }
        } else if o["argv"].is_array() {
            stream(o, "stdout_b64")?;
            stream(o, "stderr_b64")?;
            if o["version"] != 1
                || !o["input_b64"].is_null()
                || o["exit"].as_i64().is_none()
                || !((sequence == start + 1 && o == fetch)
                    || (sequence == start + 2 && o == baseline)
                    || readonly_native(&o["argv"]))
            {
                bail!("recovery contains incomplete command or preparation beyond rejection");
            }
            if readonly_native(&o["argv"])
                && (o["exit"] != 0
                    || sequence == 0
                    || originals[sequence - 1]["observation"]["phase"] != "native-invocation")
            {
                bail!("native preparation command is unread or unmatched");
            }
            if o["argv"] == json!(["auth", "whoami", "-o", "json"]) {
                let identity: Value = serde_json::from_slice(&stream(o, "stdout_b64")?)?;
                if identity["status"]["userInfo"]["username"]
                    != super::super::consist_job::CONTROLLER_IDENTITY
                {
                    bail!("original native identity conflicts with conductor");
                }
            }
        } else if o["phase"] == "http-request" {
            let response = &originals
                .get(sequence + 1)
                .context("interrupted original HTTP request")?["observation"];
            if response["http_method"] != o["http_method"]
                || response["path"] != o["path"]
                || response["request"] != o["request"]
                || response["complete"] != true
                || !matches!(response["status"].as_u64(), Some(200 | 201 | 204 | 404))
                || (response["status"] == 404 && o["http_method"] != "GET")
            {
                bail!("recovery contains interrupted original HTTP exchange");
            }
            stream(response, "response_b64")?;
            if sequence > start && o["http_method"] != "GET" {
                bail!("recovery contains a later write");
            }
        } else if o["complete"] == true && o["http_method"].is_string() {
            stream(o, "response_b64")?;
            if sequence == 0 || originals[sequence - 1]["observation"]["phase"] != "http-request" {
                bail!("recovery HTTP response has no original request");
            }
        } else {
            bail!("recovery contains unknown or interrupted preparation evidence");
        }
    }
    let observed = String::from_utf8(stream(baseline, "stdout_b64")?)?;
    let observed = observed.strip_suffix('\n').unwrap_or(&observed);
    let valid = |s: &str| {
        s.len() == 40
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !valid(observed)
        || !valid(&intent.inputs.request.baseline)
        || observed == intent.inputs.request.baseline
    {
        bail!("recovery has no fixed valid baseline mismatch");
    }
    validate_absence(native)?;
    Ok(observed.into())
}

fn stream(value: &Value, field: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(
        value[field]
            .as_str()
            .with_context(|| format!("original {field} absent"))?,
    )?)
}
fn same_protocol(a: &Value, b: &Value) -> bool {
    let body = |value: &Value| {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("status");
        }
        value
    };
    body(a) == body(b)
}
fn readonly_native(args: &Value) -> bool {
    args == &json!(["auth", "whoami", "-o", "json"])
        || args == &json!(["auth", "can-i", "create", "jobs"])
        || args
            == &json!([
                "get",
                "jobs",
                "-l",
                "boss.pipeline/diagnostic=conductor",
                "-o",
                "json"
            ])
}
fn validate_absence(native: &Value) -> Result<()> {
    if native["apiVersion"] != "v1"
        || native["kind"] != "List"
        || !native["metadata"]["resourceVersion"].is_string()
        || (!native["metadata"]["continue"].is_null() && native["metadata"]["continue"] != "")
        || native["metadata"]
            .get("remainingItemCount")
            .is_some_and(|count| !count.is_null() && count.as_u64() != Some(0))
        || native["items"]
            .as_array()
            .is_none_or(|items| !items.is_empty())
    {
        bail!("fixed native absence is unavailable, incomplete or conflicting");
    }
    Ok(())
}

fn validate_pod_absence(native: &Value, name: &str) -> Result<()> {
    let items = native["items"]
        .as_array()
        .context("native Pod census absent")?;
    let mut header = native.clone();
    header["items"] = json!([]);
    validate_absence(&header)?;
    let prefix = format!("{name}-");
    for item in items {
        let m = &item["metadata"];
        let pod = m["name"].as_str().context("native Pod identity absent")?;
        if m["namespace"] != "boss-dev" || m["uid"].as_str().is_none_or(str::is_empty) {
            bail!("native Pod scope/UID unread");
        }
        if pod == name
            || pod.starts_with(&prefix)
            || m["generateName"] == prefix
            || m["labels"]["job-name"] == name
            || m["labels"]["batch.kubernetes.io/job-name"] == name
            || m["ownerReferences"].as_array().is_some_and(|owners| {
                owners
                    .iter()
                    .any(|owner| owner["kind"] == "Job" && owner["name"] == name)
            })
        {
            bail!("native Pod incarnation conflicts with absent-launch assessment");
        }
    }
    Ok(())
}

fn validate_native(native: &Value, name: &str) -> Result<()> {
    if native["namespace"] != "boss-dev"
        || native["identity"] != super::super::consist_job::CONTROLLER_IDENTITY
    {
        bail!("native assessment scope differs");
    }
    validate_absence(&native["jobs"])?;
    validate_pod_absence(&native["pods"], name)?;
    let originals = native["command_originals"]
        .as_array()
        .context("full native command originals absent")?;
    let identity_args = json!(["auth", "whoami", "-o", "json"]);
    let identities: Vec<_> = originals
        .iter()
        .enumerate()
        .filter(|(_, row)| row["observation"]["argv"] == identity_args)
        .collect();
    if identities.is_empty() {
        bail!("native identity original absent or conflicting");
    }
    for (position, row) in identities {
        let command = &row["observation"];
        if position == 0
            || command["version"] != 1
            || command["exit"] != 0
            || !command["input_b64"].is_null()
            || originals[position - 1]["observation"]["phase"] != "native-invocation"
            || originals[position - 1]["observation"]["namespace"] != "boss-dev"
            || originals[position - 1]["observation"]["program"] != "/usr/local/bin/kubectl"
            || originals[position - 1]["observation"]["args"] != identity_args
        {
            bail!("native identity invocation/exit differs");
        }
        stream(command, "stderr_b64")?;
        let identity: Value = serde_json::from_slice(&stream(command, "stdout_b64")?)?;
        if identity["status"]["userInfo"]["username"]
            != super::super::consist_job::CONTROLLER_IDENTITY
        {
            bail!("native identity original differs from assessment actor");
        }
    }
    for (sequence, row) in originals.iter().enumerate() {
        if row["sequence"].as_u64() != Some(sequence as u64) {
            bail!("native command journal incomplete");
        }
        if row["observation"]["phase"] == "entering-create"
            || row["observation"]["args"]
                .as_array()
                .is_some_and(|a| a.first() == Some(&json!("create")))
        {
            bail!("native assessment journal contains a possible effect");
        }
    }
    for (args, expected) in [
        (
            json!([
                "get",
                "jobs",
                "--field-selector",
                format!("metadata.name={name}"),
                "-o",
                "json"
            ]),
            &native["jobs"],
        ),
        (json!(["get", "pods", "-o", "json"]), &native["pods"]),
    ] {
        let matches: Vec<_> = originals
            .iter()
            .enumerate()
            .filter(|(_, row)| row["observation"]["argv"] == args)
            .collect();
        if matches.len() != 1 {
            bail!("native census command absent or duplicated");
        }
        let (position, row) = matches[0];
        let command = &row["observation"];
        if command["version"] != 1
            || command["exit"] != 0
            || !command["input_b64"].is_null()
            || position == 0
            || originals[position - 1]["observation"]["phase"] != "native-invocation"
            || originals[position - 1]["observation"]["namespace"] != "boss-dev"
            || originals[position - 1]["observation"]["program"] != "/usr/local/bin/kubectl"
            || originals[position - 1]["observation"]["args"] != args
        {
            bail!("native census invocation/exit differs");
        }
        stream(command, "stderr_b64")?;
        let actual: Value = serde_json::from_slice(&stream(command, "stdout_b64")?)?;
        if &actual != expected {
            bail!("native census differs from full original command stream");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn command(argv: &[&str], stdout: &[u8], exit: i32) -> Value {
        super::super::super::diagnostic_journal::command_value(
            argv,
            None,
            &std::process::Output {
                status: std::process::ExitStatus::from_raw(exit << 8),
                stdout: stdout.into(),
                stderr: b"whole stderr\xff".into(),
            },
        )
    }
    fn fixture() -> (Intent, Value, Vec<Value>, Value) {
        let intent = Intent {
            attempt: Uuid::new_v4(),
            inputs: Inputs {
                request: Request {
                    car: Uuid::new_v4(),
                    candidate: "a".repeat(40),
                    baseline: "b".repeat(40),
                    receipt_sha256: "c".repeat(64),
                },
                admitted_original: json!({}),
                pinned_protocol: json!({"steps":[{"title":"execute","metadata_defaults":{"diagnostic_operation":{"version":2}}}]}),
                current_car: json!({}),
                car_binding: json!({}),
                policy: json!({}),
                edit_level: json!({}),
                required_checks: vec!["alerts-are-packets".into()],
                expected_executor_identity: super::super::super::consist_job::CONTROLLER_IDENTITY
                    .into(),
                observed_cluster_identity: super::super::super::consist_job::CONTROLLER_IDENTITY
                    .into(),
            },
        };
        let invocation =
            json!({"phase":"preparing","invocation":Uuid::new_v4(),"attempt":intent.attempt});
        let observations = vec![
            invocation.clone(),
            command(&["fetch", "origin"], b"", 0),
            command(
                &["rev-parse", "origin/main"],
                format!("{}\n", "d".repeat(40)).as_bytes(),
                0,
            ),
        ];
        let originals = observations
            .into_iter()
            .enumerate()
            .map(|(sequence, observation)| json!({"sequence":sequence,"observation":observation}))
            .collect();
        let native = json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":"opaque","continue":""},"items":[]});
        (intent, invocation, originals, native)
    }
    #[test]
    fn witnessed_successful_fetch_mismatch_has_a_separate_recovered_refusal() {
        let (intent, invocation, originals, native) = fixture();
        assert_eq!(
            assess(&intent, &invocation, &originals, &native).unwrap(),
            "d".repeat(40)
        );
        assert!(
            validate_pre_create_refusal(&PreCreateRefusal {
                schema: 1,
                intent,
                invocation: serde_json::from_value(invocation["invocation"].clone()).unwrap(),
                native_launch: NativeLaunchAbsent::Absent,
                command: originals[2]["observation"].clone(),
                originals
            })
            .is_err(),
            "v2 first-fetch meaning changed"
        );
    }
    #[test]
    fn execution_fetch_failure_interruption_conflict_and_native_identity_stay_unknown() {
        for case in 0..12 {
            let (mut intent, invocation, mut originals, mut native) = fixture();
            match case {
                0=>originals.push(json!({"sequence":3,"observation":{"phase":"entering-create"}})),
                1=>originals.push(json!({"sequence":3,"observation":{"phase":"native-invocation","args":["create","-f","-"]}})),
                2=>originals[1]["observation"]["exit"]=json!(128),
                3=>originals[2]["observation"]["exit"]=Value::Null,
                4=>originals[1]["observation"]["stderr_b64"]=Value::Null,
                5=>originals.push(json!({"sequence":3,"observation":invocation})),
                6=>originals[2]["sequence"]=json!(9),
                7=>native["items"]=json!([{"metadata":{"name":intent.job_name(),"uid":"another"}}]),
                8=>native["metadata"]["continue"]=json!("unread-tail"),
                9=>intent.inputs.request.baseline="d".repeat(40),
                10=>originals[2]["observation"]["stdout_b64"]=json!(base64::engine::general_purpose::STANDARD.encode(b"not-a-commit\n")),
                _=>originals.push(json!({"sequence":3,"observation":command(&["fetch","fork"],b"",0)})),
            }
            assert!(
                assess(&intent, &invocation, &originals, &native).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn mismatch_requires_both_full_command_streams_and_the_winning_invocation() {
        for field in ["stdout_b64", "stderr_b64"] {
            let (intent, invocation, mut originals, native) = fixture();
            originals[1]["observation"][field] = json!("invalid base64!");
            assert!(assess(&intent, &invocation, &originals, &native).is_err());
        }
        let (intent, mut invocation, originals, native) = fixture();
        invocation["invocation"] = json!(Uuid::new_v4());
        assert!(assess(&intent, &invocation, &originals, &native).is_err());
    }
    #[tokio::test]
    async fn assessment_original_is_read_back_and_replays_without_changing_subject() {
        use boss_jobs::JobsRepository;
        let (base, jobs, job, step) = super::super::tests::owning_records(true).await;
        let root = tempfile::tempdir().unwrap();
        let mut conductor = Conductor::diagnostic_fixture(root.path());
        conductor.cfg.jobs = base;
        let door = NativeDoor {
            conductor: &conductor,
            job: job.clone(),
            step,
            directory: super::super::super::diagnostic_journal::private_directory(
                &root.path().join("journal"),
            )
            .unwrap(),
            actor: ACTOR.into(),
            content: crate::attach::tests::store().await,
            may_launch: false,
        };
        let mut subject: boss_core::job::Job = serde_json::from_value(job.clone()).unwrap();
        subject.id = boss_core::job::JobId::new();
        subject.workflow_version = 2;
        jobs.create_job(&subject).await.unwrap();
        let mut original_step = boss_core::job::Step::new(subject.id, "task", "execute", 0);
        original_step.status = boss_core::job::StepStatus::Active;
        original_step.spec_slug = Some("execute".into());
        original_step.assignee_id = Some(ACTOR.into());
        jobs.add_step(&original_step).await.unwrap();
        let source = NativeDoor {
            conductor: &conductor,
            job: serde_json::to_value(&subject).unwrap(),
            step: serde_json::to_value(&original_step).unwrap(),
            directory: super::super::super::diagnostic_journal::private_directory(
                &root.path().join("original"),
            )
            .unwrap(),
            actor: ACTOR.into(),
            content: door.content.clone(),
            may_launch: false,
        };
        let original_intent =
            json!({"version":2,"intent":"whole immutable old intent","invocation":Uuid::new_v4()});
        source
            .keep("diagnostic.intent", &original_intent)
            .await
            .unwrap();
        let before_subject =
            serde_json::to_vec(&jobs.get_job(&subject.id).await.unwrap().unwrap()).unwrap();
        let before_steps =
            serde_json::to_vec(&jobs.list_steps(&subject.id).await.unwrap()).unwrap();
        let before_journal =
            super::super::super::diagnostic_journal::originals(&source.directory).unwrap();
        // Exercise the final assessment and completed-step replay through
        // the same multi-file transport required by the retained native journal.
        let value = json!({"schema":1,"outcome":"recovered-refusal","subject":{"original_request":subject.id},"originals":"x".repeat(14_053_784)});
        retain_assessment(&door, &value).await.unwrap();
        assert_eq!(
            door.original("diagnostic.recovery-assessment")
                .await
                .unwrap(),
            Some(value.clone())
        );
        let before = jobs.recorded_events().len();
        retain_assessment(&door, &value).await.unwrap();
        assert_eq!(
            jobs.recorded_events().len(),
            before,
            "replay performed another state transition"
        );
        assert!(door.original("diagnostic.intent").await.unwrap().is_none());
        assert!(door.original("diagnostic.launch").await.unwrap().is_none());
        assert_eq!(
            serde_json::to_vec(&jobs.get_job(&subject.id).await.unwrap().unwrap()).unwrap(),
            before_subject
        );
        assert_eq!(
            serde_json::to_vec(&jobs.list_steps(&subject.id).await.unwrap()).unwrap(),
            before_steps
        );
        assert_eq!(
            super::super::super::diagnostic_journal::originals(&source.directory).unwrap(),
            before_journal,
            "assessment changed original private journal"
        );
    }
    #[test]
    fn registry_binds_decided_authority_and_rejects_caller_native_evidence() {
        let specs = boss_jobs::seed_loader::parse_workflows(
            include_str!(
                "../../../../../../infra/platform/workflows/diagnostic-recovery-assessment.toml"
            ),
            "platform",
            "diagnostic-recovery-assessment.toml",
        )
        .unwrap();
        assert_eq!(specs.len(), 1);
        let execute = specs[0]
            .steps
            .iter()
            .find(|s| s.title == "execute")
            .unwrap();
        assert_eq!(execute.selectors().assignee_id.as_deref(), Some(ACTOR));
        let operation: Operation =
            serde_json::from_value(execute.metadata_defaults["recovery_operation"].clone())
                .unwrap();
        assert_eq!(operation.version, 1);
        assert_eq!(
            operation.original_request.to_string(),
            "688b4f42-9e65-4f58-aaf7-53c841113cd0"
        );
        assert_eq!(
            operation.resolution_sha256,
            "6ed8a6cdec0b2170bbbce9606f4e46ee321c7afd2a1acbcf6b0264431c3c9089"
        );
        assert!(
            serde_json::from_value::<AssessmentRequest>(
                json!({"original_request":operation.original_request})
            )
            .is_ok()
        );
        for key in ["namespace", "journal", "native", "command", "intent"] {
            let mut value = json!({"original_request":operation.original_request});
            value[key] = json!("caller supplied");
            assert!(
                serde_json::from_value::<AssessmentRequest>(value).is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn whole_journal_snapshot_conserves_bytes_and_refuses_interrupted_staging() {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = super::super::super::diagnostic_journal::private_directory(
            &root.path().join("original"),
        )
        .unwrap();
        super::super::super::diagnostic_journal::keep(&path, &json!({"whole":"original\n\u{ff}"}))
            .unwrap();
        let raw = snapshot(&path).unwrap();
        let parsed = parse_snapshot(&raw).unwrap();
        assert_eq!(
            parsed,
            super::super::super::diagnostic_journal::originals(&path).unwrap()
        );
        let mut corrupted = raw.clone();
        corrupted["files"][0]["bytes_b64"] =
            json!(base64::engine::general_purpose::STANDARD.encode(b"changed"));
        assert!(parse_snapshot(&corrupted).is_err());
        let partial = path.join("pending").join("interrupted");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(partial)
            .unwrap();
        file.write_all(b"partial original bytes").unwrap();
        drop(file);
        let retained = snapshot(&path).unwrap();
        assert!(parse_snapshot(&retained).is_err());
        assert_eq!(
            super::super::super::diagnostic_journal::originals(&path).unwrap(),
            parsed
        );
        assert!(snapshot(&root.path().join("absent")).is_err());
        assert!(!root.path().join("absent").exists());
    }
    #[tokio::test]
    async fn separately_assessed_original_releases_only_its_queue_position() {
        let original = Uuid::new_v4();
        let next = Uuid::new_v4();
        let packets = vec![json!({"id":original}), json!({"id":next})];
        let assessed = std::collections::BTreeSet::from([original]);
        let pending = pending_originals(packets.clone(), &assessed).unwrap();
        assert_eq!(pending, vec![packets[1].clone()]);
        super::super::advance_requests(pending, |job, may_launch| async move {
            assert_eq!(job["id"], next.to_string());
            assert!(may_launch);
            Ok(Advance::Pending)
        })
        .await
        .unwrap();
        assert_eq!(
            pending_originals(packets.clone(), &std::collections::BTreeSet::new()).unwrap(),
            packets
        );
    }
    #[test]
    fn fixed_namespace_pod_census_accepts_unrelated_work_but_refuses_an_original_incarnation() {
        let name = format!("consist-{}", Uuid::new_v4().simple());
        let other = json!({"metadata":{"name":"unrelated-pod","namespace":"boss-dev","uid":"native-positive"}});
        let list = json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":"opaque"},"items":[other]});
        validate_pod_absence(&list, &name).unwrap();
        for field in 0..4 {
            let mut raw = list.clone();
            let pod = &mut raw["items"][0]["metadata"];
            match field {
                0 => pod["name"] = json!(format!("{name}-native")),
                1 => pod["generateName"] = json!(format!("{name}-")),
                2 => pod["labels"]["job-name"] = json!(name),
                _ => pod["ownerReferences"] = json!([{"kind":"Job","name":name,"uid":"original"}]),
            }
            assert!(
                validate_pod_absence(&raw, &name).is_err(),
                "incarnation field {field}"
            );
        }
    }
    #[test]
    fn kubectl_list_wrapper_empty_resource_version_is_a_real_positive_shape() {
        let (intent, invocation, originals, mut native) = fixture();
        // The retained native-jobs-143 original has exactly this kubectl
        // wrapper. Its empty RV is not a failed API read or an unread tail.
        native["metadata"]["resourceVersion"] = json!("");
        assess(&intent, &invocation, &originals, &native).unwrap();
    }
    #[test]
    fn an_unread_remaining_count_cannot_prove_native_absence() {
        let (_, _, _, native) = fixture();
        for count in [
            json!(-1),
            json!("unread"),
            json!(false),
            json!([]),
            json!({}),
            json!(0.5),
        ] {
            let mut malformed = native.clone();
            malformed["metadata"]["remainingItemCount"] = count.clone();
            assert!(
                validate_absence(&malformed).is_err(),
                "an unread remaining count became complete absence: {count}"
            );
        }
        for count in [Value::Null, json!(0)] {
            let mut complete = native.clone();
            complete["metadata"]["remainingItemCount"] = count;
            validate_absence(&complete).unwrap();
        }
        validate_absence(&native).unwrap();
    }
    #[test]
    fn conflicting_native_identity_in_original_journal_stays_unknown() {
        for username in [
            super::super::super::consist_job::CONTROLLER_IDENTITY,
            "another-native-actor",
        ] {
            let (intent, invocation, mut originals, native) = fixture();
            let args = json!(["auth", "whoami", "-o", "json"]);
            let stdout =
                serde_json::to_vec(&json!({"status":{"userInfo":{"username":username}}})).unwrap();
            originals.splice(0..0,[json!({"observation":{"phase":"native-invocation","program":"/usr/local/bin/kubectl","namespace":"boss-dev","args":args}}),json!({"observation":command(&["auth","whoami","-o","json"],&stdout,0)})]);
            for (sequence, row) in originals.iter_mut().enumerate() {
                row["sequence"] = json!(sequence);
            }
            assert_eq!(
                assess(&intent, &invocation, &originals, &native).is_ok(),
                username == super::super::super::consist_job::CONTROLLER_IDENTITY
            );
        }
    }
    #[test]
    fn native_absence_is_bound_to_whole_census_streams_and_observed_identity() {
        let name = format!("consist-{}", Uuid::new_v4().simple());
        let empty =
            json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":""},"items":[]});
        let mut observations = Vec::new();
        for (args, stdout) in [
            (
                vec![
                    "auth".to_owned(),
                    "whoami".into(),
                    "-o".into(),
                    "json".into(),
                ],
                json!({"status":{"userInfo":{"username":super::super::super::consist_job::CONTROLLER_IDENTITY}}}),
            ),
            (
                vec![
                    "get".into(),
                    "jobs".into(),
                    "--field-selector".into(),
                    format!("metadata.name={name}"),
                    "-o".into(),
                    "json".into(),
                ],
                empty.clone(),
            ),
            (
                vec!["get".into(), "pods".into(), "-o".into(), "json".into()],
                empty.clone(),
            ),
        ] {
            observations.push(json!({"phase":"native-invocation","program":"/usr/local/bin/kubectl","namespace":"boss-dev","args":args}));
            let argv: Vec<_> = args.iter().map(String::as_str).collect();
            observations.push(command(&argv, &serde_json::to_vec(&stdout).unwrap(), 0));
        }
        let originals: Vec<_> = observations
            .into_iter()
            .enumerate()
            .map(|(sequence, observation)| json!({"sequence":sequence,"observation":observation}))
            .collect();
        let native = json!({"namespace":"boss-dev","identity":super::super::super::consist_job::CONTROLLER_IDENTITY,"jobs":empty,"pods":empty,"command_originals":originals});
        validate_native(&native, &name).unwrap();
        for fault in 0..6 {
            let mut bad = native.clone();
            match fault {
                0 => {
                    bad["command_originals"][1]["observation"]["stdout_b64"] = json!(
                        base64::engine::general_purpose::STANDARD.encode(
                            serde_json::to_vec(
                                &json!({"status":{"userInfo":{"username":"another-native-actor"}}})
                            )
                            .unwrap()
                        )
                    )
                }
                1 => {
                    bad["command_originals"][3]["observation"]["stdout_b64"] = json!(
                        base64::engine::general_purpose::STANDARD.encode(b"different original")
                    )
                }
                2 => bad["command_originals"][5]["observation"]["stderr_b64"] = Value::Null,
                3 => bad["namespace"] = json!("another-namespace"),
                4 => {
                    bad["command_originals"][2]["observation"]["args"] =
                        json!(["get", "jobs", "-o", "json"])
                }
                _ => bad["command_originals"][5]["observation"]["exit"] = json!(1),
            }
            assert!(
                validate_native(&bad, &name).is_err(),
                "native proof fault {fault}"
            );
        }
        // A current reader guard and the census session each read identity.
        // Repeated reads are legitimate; conflicting identities are not.
        let mut repeated = native.clone();
        let rows = repeated["command_originals"].as_array_mut().unwrap();
        let prior = rows[0..2].to_vec();
        rows.extend(prior);
        for (sequence, row) in rows.iter_mut().enumerate() {
            row["sequence"] = json!(sequence);
        }
        validate_native(&repeated, &name).unwrap();
        let last = repeated["command_originals"].as_array().unwrap().len() - 1;
        repeated["command_originals"][last]["observation"]["stdout_b64"] = json!(
            base64::engine::general_purpose::STANDARD.encode(
                serde_json::to_vec(
                    &json!({"status":{"userInfo":{"username":"conflicting-reader"}}})
                )
                .unwrap()
            )
        );
        assert!(validate_native(&repeated, &name).is_err());
    }
    #[test]
    fn current_native_identity_precedes_every_private_reader_including_resume() {
        for resume in [false, true] {
            for fault in ["native-error", "wrong-native-actor", "registered-conductor"] {
                let calls = std::cell::RefCell::new(Vec::new());
                let original = json!({"whole":"retained private original"});
                let result = authenticated_snapshot(
                    || {
                        calls.borrow_mut().push("native-identity");
                        if fault == "native-error" {
                            bail!("actual native identity unavailable");
                        }
                        Ok(if fault == "wrong-native-actor" {
                            "another-actor"
                        } else {
                            super::super::super::consist_job::CONTROLLER_IDENTITY
                        }
                        .into())
                    },
                    || {
                        calls.borrow_mut().push(if resume {
                            "retained-snapshot"
                        } else {
                            "private-journal"
                        });
                        Ok(original.clone())
                    },
                );
                if fault == "registered-conductor" {
                    assert_eq!(result.unwrap(), original);
                    assert_eq!(
                        *calls.borrow(),
                        vec![
                            "native-identity",
                            if resume {
                                "retained-snapshot"
                            } else {
                                "private-journal"
                            }
                        ]
                    );
                } else {
                    assert!(
                        result.is_err(),
                        "{fault}: private reader ran under named actor without native identity"
                    );
                    assert_eq!(
                        *calls.borrow(),
                        vec!["native-identity"],
                        "{fault}: private evidence read before authenticating native actor"
                    );
                }
            }
        }
    }
}
