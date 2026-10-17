//! Trusted lifecycle for a selected cluster backend (3869107c). Fixtures
//! exercise these ports; only the native transport can observe a real Job.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

// Private persistence/metadata IPC source adapters exist (6f3ef63c), but
// native scope/account verification and private mount installation remain owed.
// The selected native managed launch still refuses until those bindings exist.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "386 source checkpoint: future trusted broker adapter uses these transitions after cache/worker wiring; native managed launch refuses until then"
    )
)]
pub(crate) mod managed_cache;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub commit: String,
    pub main: String,
    pub tree: String,
    pub sha256: String,
    pub bytes: u64,
    pub members_sha256: String,
    pub members_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Prepared {
    pub run: String,
    pub observer: String,
    pub actor: String,
    pub image: String,
    pub login: String,
    pub bound: Duration,
    pub grace: Duration,
    pub snapshot: Snapshot,
    #[serde(skip)]
    pub bundle: Vec<u8>,
    #[serde(skip)]
    pub prompt: Vec<u8>,
    pub argv: Vec<String>,
    pub home_env: String,
    pub login_file: String,
    pub runtime_event: String,
    pub runtime_key: String,
    pub completed_event: String,
    pub transcript_pattern: String,
}

/// Native bytes and transport exit remain independent of parsed JSON.
#[derive(Debug, Clone)]
pub(crate) struct Native {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: Option<i32>,
}

#[derive(Debug, Clone)]
pub(crate) enum Effect {
    CreateWorkspace(Value),
    CreateJob(Value),
    ReadJob(String),
    ReadWorkspace(String),
    ListPods(String),
    Exec {
        pod: String,
        operation: String,
        input: Vec<u8>,
        fence: Option<Value>,
    },
    DeleteJob {
        name: String,
        options: Value,
    },
}

#[async_trait]
pub(crate) trait Transport: Send {
    /// Adapter retains partial native streams DURABLY even when this
    /// future is cancelled. Dropping an exec client does not stop a Pod.
    async fn perform(&mut self, effect: Effect, remaining: Duration) -> Result<Native, String>;
}

#[async_trait]
pub(crate) trait Journal: Send {
    async fn append(&mut self, kind: &str, bytes: &[u8]) -> Result<(), String>;
    async fn record(&mut self, key: &str, value: &Value) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Outcome {
    pub ending: String,
    pub job_uid: Option<String>,
    pub stop_observed: bool,
    pub evidence_complete: bool,
    pub workspace_retained: bool,
    pub observations: Vec<Value>,
}

pub(crate) async fn run<T: Transport, J: Journal>(
    prepared: &Prepared,
    transport: &mut T,
    journal: &mut J,
) -> Result<Outcome, String> {
    if prepared.home_env == "CODEX_HOME" && prepared.login_file == "auth.json" {
        return Err("managed login requires the approved private cache lease/worker wiring; no compute admitted".into());
    }
    use crate::launch_binding::JobBinding;
    use crate::launch_job;
    // Preparation, scheduling, recording and transfer all spend ONE bound.
    let deadline = tokio::time::Instant::now()
        .checked_add(prepared.bound)
        .ok_or("controller bound exceeds monotonic clock range")?;
    let spec = launch_job::Spec {
        run_id: &prepared.run,
        image: &prepared.image,
        login_secret: &prepared.login,
        bound: prepared.bound,
        grace: prepared.grace,
    };
    let intent = launch_job::manifest(&spec)?;
    if prepared.bundle.len() as u64 != prepared.snapshot.bytes
        || hash(&prepared.bundle) != prepared.snapshot.sha256
    {
        return Err("snapshot bytes do not match the prepared immutable input".into());
    }
    let mut outcome = Outcome {
        ending: "creation_unknown".into(),
        job_uid: None,
        stop_observed: false,
        evidence_complete: false,
        workspace_retained: true,
        observations: vec![],
    };
    journal.record("launch_intent", &json!({"schema":1,"run":prepared.run,"observer":prepared.observer,
        "actor":prepared.actor,"intent":intent,"snapshot":prepared.snapshot,"prompt_sha256":hash(&prepared.prompt)})).await?;
    let pvc = request(
        transport,
        journal,
        Effect::CreateWorkspace(launch_job::workspace_manifest(&spec)),
        deadline,
    )
    .await?;
    let pvc = object(&pvc)?;
    workspace_identity(&pvc, &launch_job::workspace_manifest(&spec))?;
    journal.record("workspace_binding", &pvc).await?;
    let created = request(
        transport,
        journal,
        Effect::CreateJob(intent.clone()),
        deadline,
    )
    .await;
    // An uncertain create is reconciled by the exact named intent; never a
    // second POST. A different attempt/name/UID is left for a loud finding.
    let created = match created {
        Ok(native) if native.exit == Some(0) => object(&native)?,
        other => {
            journal
                .record(
                    "creation_unknown",
                    &json!({"error":format!("{other:?}"),"intent":intent}),
                )
                .await?;
            job_observation(object(
                &request(
                    transport,
                    journal,
                    Effect::ReadJob(launch_job::name(&prepared.run)),
                    deadline,
                )
                .await?,
            )?)?
        }
    };
    let binding = JobBinding::from_created(&created, &spec)?;
    outcome.job_uid = Some(binding.uid().into());
    journal.record("job_binding", &created).await?;
    let execution:Result<(),String> = async {
        let running=loop {
            let (job,pods)=observe(transport,journal,&binding,deadline).await?;
            match binding.observe_running(&job,&pods) {
                Ok(running)=>break running,
                Err(why) if binding.scheduling(&job,&pods)? => {
                    journal.append("scheduling",why.as_bytes()).await?;
                    tokio::time::timeout_at(deadline,tokio::time::sleep(Duration::from_millis(100))).await.map_err(|_|"deadline while scheduling")?;
                }
                Err(why)=>return Err(why),
            }
        };
        journal.record("running_binding", &running.identity()).await?;
        let identity=bound_exec(transport,journal,&binding,&running,"identity",Vec::new(),None,deadline).await?;
        let fence=object(&identity)?;
        if fence["pod_uid"]!=running.identity()["pod_uid"] || fence["pid1_start_ticks"].as_u64().is_none_or(|n|n==0)
            || fence["pid_namespace_inode"].as_u64().is_none_or(|n|n==0) || fence["boot_id"].as_str().is_none_or(str::is_empty) {
            return Err("supervisor identity does not bind the actual observed Pod/incarnation".into());
        }
        journal.record("supervisor_fence",&fence).await?;
        for (operation, bytes, digest) in [
            ("bundle",prepared.bundle.as_slice(),prepared.snapshot.sha256.clone()),
            ("prompt",prepared.prompt.as_slice(),hash(&prepared.prompt)),
        ] {
            let result=bound_exec(transport,journal,&binding,&running,operation,bytes.to_vec(),Some(&fence),deadline).await?;
            let answer=object(&result)?;
            if answer["sha256"]!=digest || answer["bytes"].as_u64()!=Some(bytes.len() as u64) { return Err(format!("{operation} transfer readback differs")); }
        }
        let start=json!({"schema":1,"run":prepared.run,"snapshot":prepared.snapshot,
            "prompt_sha256":hash(&prepared.prompt),"argv":prepared.argv,"home_env":prepared.home_env,
            "runtime_event":prepared.runtime_event,"runtime_key":prepared.runtime_key,
            "completed_event":prepared.completed_event,"transcript_pattern":prepared.transcript_pattern});
        let mut start=start;start["login_file"]=json!(prepared.login_file);
        let native=bound_exec(transport,journal,&binding,&running,"start",serde_json::to_vec(&start).map_err(|e|e.to_string())?,Some(&fence),deadline).await?;
        let ack=object(&native)?;
        if ack["schema"]!=1 || ack["run"]!=prepared.run || ack["prompt_sha256"]!=hash(&prepared.prompt)
            || ack["snapshot_sha256"]!=prepared.snapshot.sha256 || ack["start_read"]!=true {
            return Err("no bound native START acknowledgment; a thread id is not START".into());
        }
        journal.record("observer_started",&json!({"schema":2,"run":prepared.run,"observer":prepared.observer,
            "actor":prepared.actor,"binding":running.identity(),"prompt_sha256":hash(&prepared.prompt),
            "snapshot_sha256":prepared.snapshot.sha256,"acknowledgment":ack})).await?;
        let collection=bound_exec(transport,journal,&binding,&running,"collect",Vec::new(),Some(&fence),deadline).await?;
        let archive=object(&collection)?;
        use base64::Engine;
        for key in ["stdout","stderr","last_message","transcript"] {
            let item=&archive["artifacts"][key];
            let text=item["data"].as_str().ok_or_else(||format!("artifact {key} missing, partial or unreadable"))?;
            let bytes=base64::engine::general_purpose::STANDARD.decode(text).map_err(|e|e.to_string())?;
            if item["bytes"].as_u64()!=Some(bytes.len() as u64)||item["sha256"]!=hash(&bytes){return Err(format!("artifact {key} digest/length differ"));}
            journal.append(key,&bytes).await?;
        }
        if archive["exit"].as_i64()!=Some(0) || archive["completed"]!=true || archive["ending"]!="native_exit" {
            return Err("native execution did not complete successfully".into());
        }
        let runtime=archive["runtime"].as_str().filter(|r|!r.is_empty()).ok_or("native runtime unavailable")?;
        let mut observed_runtime=None; let mut completed=false;
        let stdout=base64::engine::general_purpose::STANDARD.decode(archive["artifacts"]["stdout"]["data"].as_str().ok_or("no original native stdout")?).map_err(|e|e.to_string())?;
        for line in std::str::from_utf8(&stdout).map_err(|e|e.to_string())?.lines() {
            if let Ok(event)=serde_json::from_str::<Value>(line) {
                if event["type"]==prepared.runtime_event { observed_runtime=event[&prepared.runtime_key].as_str().map(str::to_string); }
                if event["type"]==prepared.completed_event { completed=true; }
            }
        }
        if observed_runtime.as_deref()!=Some(runtime) || !completed { return Err("archive lacks the exact native runtime and completed-turn evidence".into()); }
        journal.record("archive_acknowledgment",&json!({"run":prepared.run,"binding":running.identity(),"archive_sha256":hash(&collection.stdout),
            "runtime":runtime,"observed_usage":"unknown"})).await?;
        outcome.evidence_complete=true;
        Ok(())
    }.await;
    outcome.ending = match execution {
        Ok(()) => "native_exited".into(),
        Err(why) => why,
    };
    // Cleanup has a separate finite grace, never more execution time.
    // An accepted DELETE is recorded, then separately judged observations.
    let (stopped, observations) = retire(transport, journal, &binding, prepared.grace).await?;
    outcome.stop_observed = stopped;
    outcome.observations.extend(observations);
    outcome.observations.push(
        json!({"retention":"private workspace retained; no PVC garbage collection inferred"}),
    );
    journal
        .record(
            "launch_ending",
            &serde_json::to_value(&outcome).map_err(|e| e.to_string())?,
        )
        .await?;
    Ok(outcome)
}

async fn retire<T: Transport, J: Journal>(
    transport: &mut T,
    journal: &mut J,
    binding: &crate::launch_binding::JobBinding,
    grace: Duration,
) -> Result<(bool, Vec<Value>), String> {
    let stop_deadline = tokio::time::Instant::now()
        .checked_add(grace)
        .ok_or("cleanup grace exceeds monotonic clock range")?;
    let mut observations = Vec::new();
    let mut stopped = false;
    let deletion = request(
        transport,
        journal,
        Effect::DeleteJob {
            name: binding.name().into(),
            options: binding.delete_options(),
        },
        stop_deadline,
    )
    .await;
    observations.push(json!({"delete_transport":format!("{deletion:?}")}));
    loop {
        let observed = observe(transport, journal, binding, stop_deadline).await;
        match observed {
            Ok((job, pods))
                if job["kind"] == "Status" && job["reason"] == "NotFound" && job["code"] == 404 =>
            {
                if complete_empty_pods(&pods) {
                    stopped = true;
                    break;
                }
            }
            Ok((job, _)) if job["metadata"]["uid"] != binding.uid() => {
                observations.push(json!({"stop":"replacement UID; not deleted"}));
                break;
            }
            Err(why) => {
                observations.push(json!({"stop_unproven":why}));
                break;
            }
            _ => {}
        }
        if tokio::time::timeout_at(stop_deadline, tokio::time::sleep(Duration::from_millis(20)))
            .await
            .is_err()
        {
            break;
        }
    }
    Ok((stopped, observations))
}

fn workspace_identity(observed: &Value, intent: &Value) -> Result<(), String> {
    if observed["kind"] != "PersistentVolumeClaim"
        || observed["apiVersion"] != "v1"
        || observed["metadata"]["name"] != intent["metadata"]["name"]
        || observed["metadata"]["namespace"] != intent["metadata"]["namespace"]
        || observed["metadata"]["labels"] != intent["metadata"]["labels"]
        || observed["metadata"]["uid"]
            .as_str()
            .is_none_or(str::is_empty)
        || observed["metadata"]["resourceVersion"]
            .as_str()
            .is_none_or(str::is_empty)
        || !observed["metadata"]["ownerReferences"].is_null()
        || !observed["metadata"]["deletionTimestamp"].is_null()
    {
        return Err("workspace response does not bind this private retained attempt".into());
    }
    let mut spec = observed["spec"].clone();
    if let Some(volume) = spec.get("volumeName") {
        if volume.as_str().is_none_or(str::is_empty) {
            return Err("empty bound volume".into());
        }
        spec.as_object_mut()
            .ok_or("PVC spec absent")?
            .remove("volumeName");
    }
    if spec != intent["spec"] {
        return Err("workspace admission changed its judged storage intent".into());
    }
    Ok(())
}

async fn request<T: Transport, J: Journal>(
    t: &mut T,
    j: &mut J,
    e: Effect,
    deadline: tokio::time::Instant,
) -> Result<Native, String> {
    let remaining = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .ok_or("controller deadline")?;
    let n = tokio::time::timeout_at(deadline, t.perform(e, remaining))
        .await
        .map_err(|_| "controller transport deadline")??;
    j.append("native.stdout", &n.stdout).await?;
    j.append("native.stderr", &n.stderr).await?;
    j.append("native.exit", format!("{:?}", n.exit).as_bytes())
        .await?;
    Ok(n)
}
fn object(n: &Native) -> Result<Value, String> {
    if n.exit != Some(0) {
        return Err(format!(
            "native exit {:?}; original stderr retained",
            n.exit
        ));
    }
    serde_json::from_slice(&n.stdout).map_err(|e| format!("native response is not JSON: {e}"))
}
fn job_observation(raw: Value) -> Result<Value, String> {
    if raw["kind"] != "JobList" {
        return Ok(raw);
    }
    if raw["apiVersion"] != "batch/v1"
        || raw["metadata"]["resourceVersion"]
            .as_str()
            .is_none_or(str::is_empty)
        || raw["metadata"].get("continue").is_some_and(|v| v != "")
        || raw["metadata"]
            .get("remainingItemCount")
            .is_some_and(|v| v != 0)
    {
        return Err("native JobList is incomplete or untyped".into());
    }
    match raw["items"]
        .as_array()
        .ok_or("native JobList has no items")?
        .as_slice()
    {
        [job] => Ok(job.clone()),
        [] => {
            Ok(json!({"kind":"Status","reason":"NotFound","code":404,"native_complete_list":raw}))
        }
        _ => Err("native exact-name JobList is ambiguous".into()),
    }
}
async fn observe<T: Transport, J: Journal>(
    t: &mut T,
    j: &mut J,
    b: &crate::launch_binding::JobBinding,
    d: tokio::time::Instant,
) -> Result<(Value, Value), String> {
    let job = job_observation(object(
        &request(t, j, Effect::ReadJob(b.name().into()), d).await?,
    )?)?;
    let pods = object(&request(t, j, Effect::ListPods(b.uid().into()), d).await?)?;
    Ok((job, pods))
}
async fn bound_exec<T: Transport, J: Journal>(
    t: &mut T,
    j: &mut J,
    b: &crate::launch_binding::JobBinding,
    r: &crate::launch_binding::RunningBinding,
    operation: &str,
    input: Vec<u8>,
    fence: Option<&Value>,
    d: tokio::time::Instant,
) -> Result<Native, String> {
    let (job, pods) = observe(t, j, b, d).await?;
    r.recheck(&job, &pods)?;
    let answer = request(
        t,
        j,
        Effect::Exec {
            pod: r.pod_name().into(),
            operation: operation.into(),
            input,
            fence: fence.cloned(),
        },
        d,
    )
    .await?;
    let (job, pods) = observe(t, j, b, d).await?;
    r.recheck(&job, &pods)?;
    // Kubernetes exec has no UID precondition. These observations detect
    // replacement; they do NOT make name-based exec atomic with the UID.
    Ok(answer)
}
fn complete_empty_pods(p: &Value) -> bool {
    p["apiVersion"] == "v1"
        && p["kind"] == "PodList"
        && p["metadata"]["resourceVersion"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && p["metadata"].get("continue").is_none_or(|c| c == "")
        && p["metadata"]
            .get("remainingItemCount")
            .is_none_or(|c| c == 0)
        && p["items"].as_array().is_some_and(Vec::is_empty)
}

pub(crate) fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

pub(crate) async fn stop_known<T: Transport, J: Journal>(
    prepared: &Prepared,
    created: &Value,
    transport: &mut T,
    journal: &mut J,
) -> Result<Outcome, String> {
    let spec = crate::launch_job::Spec {
        run_id: &prepared.run,
        image: &prepared.image,
        login_secret: &prepared.login,
        bound: prepared.bound,
        grace: prepared.grace,
    };
    let binding = crate::launch_binding::JobBinding::from_created(created, &spec)?;
    let (stop_observed, observations) =
        retire(transport, journal, &binding, prepared.grace).await?;
    let outcome = Outcome {
        ending: "interrupted".into(),
        job_uid: Some(binding.uid().into()),
        stop_observed,
        evidence_complete: false,
        workspace_retained: true,
        observations,
    };
    journal
        .record(
            "interrupted_stop",
            &serde_json::to_value(&outcome).map_err(|e| e.to_string())?,
        )
        .await?;
    Ok(outcome)
}

/// A recovery only reads retained evidence after original compute is
/// independently observed gone. It cannot restart the harness or replace
/// an original terminal with success, and never deletes a PVC.
pub(crate) async fn recover<T: Transport, J: Journal>(
    prepared: &Prepared,
    created: &Value,
    workspace: &Value,
    transport: &mut T,
    journal: &mut J,
) -> Result<Value, String> {
    use crate::{launch_binding::JobBinding, launch_job};
    let stopped = stop_known(prepared, created, transport, journal).await?;
    if !stopped.stop_observed {
        return Err(
            "original compute stop remains unproven; recovery did not admit another Pod".into(),
        );
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or("recovery bound overflow")?;
    let spec = launch_job::Spec {
        run_id: &prepared.run,
        image: &prepared.image,
        login_secret: &prepared.login,
        bound: prepared.bound,
        grace: prepared.grace,
    };
    let observed = object(
        &request(
            transport,
            journal,
            Effect::ReadWorkspace(launch_job::workspace_name(&prepared.run)),
            deadline,
        )
        .await?,
    )?;
    workspace_identity(&observed, &launch_job::workspace_manifest(&spec))?;
    if observed["metadata"]["uid"] != workspace["metadata"]["uid"] {
        return Err("retained workspace UID was replaced".into());
    }
    let intent = launch_job::recovery_manifest(&spec)?;
    journal.record("recovery_intent", &intent).await?;
    let native = request(
        transport,
        journal,
        Effect::CreateJob(intent.clone()),
        deadline,
    )
    .await;
    let native = match native {
        Ok(native) if native.exit == Some(0) => object(&native)?,
        _ => job_observation(object(
            &request(
                transport,
                journal,
                Effect::ReadJob(launch_job::recovery_name(&prepared.run)),
                deadline,
            )
            .await?,
        )?)?,
    };
    let collector = JobBinding::from_recovery_created(&native, &spec)?;
    journal.record("recovery_binding", &native).await?;
    let collection:Result<Value,String>=async {
        let running=loop {
            let (job,pods)=observe(transport,journal,&collector,deadline).await?;
            match collector.observe_running(&job,&pods) {
                Ok(running)=>break running,
                Err(why) if collector.scheduling(&job,&pods)?=>{
                    journal.append("recovery_scheduling",why.as_bytes()).await?;
                    tokio::time::timeout_at(deadline,tokio::time::sleep(Duration::from_millis(100))).await.map_err(|_|"recovery scheduling deadline")?;
                },
                Err(why)=>return Err(why),
            }
        };
        let fence=object(&bound_exec(transport,journal,&collector,&running,"identity",Vec::new(),None,deadline).await?)?;
        if fence["pod_uid"]!=running.identity()["pod_uid"]{return Err("recovery supervisor has another PodUID".into());}
        let input=json!({"runtime_event":prepared.runtime_event,"runtime_key":prepared.runtime_key,"transcript_pattern":prepared.transcript_pattern});
        let result=object(&bound_exec(transport,journal,&collector,&running,"recover",serde_json::to_vec(&input).map_err(|e|e.to_string())?,Some(&fence),deadline).await?)?;
        let after=object(&request(transport,journal,Effect::ReadWorkspace(launch_job::workspace_name(&prepared.run)),deadline).await?)?;
        if after["metadata"]["uid"]!=workspace["metadata"]["uid"]{return Err("recovery observed a replacement workspace; provenance is unknown".into());}
        use base64::Engine;
        let items=result["artifacts"].as_object().ok_or("recovery has no artifact manifest")?;
        for (kind,item) in items {
            if !["stdout","stderr","last_message","transcript","native_metadata"].contains(&kind.as_str()){return Err("recovery contains an untyped artifact".into());}
            let bytes=base64::engine::general_purpose::STANDARD.decode(item["data"].as_str().ok_or("recovery artifact data missing")?).map_err(|e|e.to_string())?;
            if item["bytes"].as_u64()!=Some(bytes.len() as u64)||item["sha256"]!=hash(&bytes){return Err("recovery artifact digest/length mismatch".into());}
            journal.append(&format!("recovered_{kind}"),&bytes).await?;
        }
        journal.record("recovered_artifact_manifest",&result).await?;
        Ok(result)
    }.await;
    let (collector_stopped, stop) = retire(transport, journal, &collector, prepared.grace).await?;
    let receipt = json!({"original_job_uid":created["metadata"]["uid"],"workspace_uid":workspace["metadata"]["uid"],
        "collection":collection,"collector_stopped":collector_stopped,"stop_observations":stop,"workspace_retained":true,
        "provenance":"available retained workspace bytes; unwitnessed tail is not attested as native execution",
        "terminal":"interrupted; original outcome is never replaced by recovered files"});
    journal.record("recovery_terminal", &receipt).await?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_job;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct MemoryJournal {
        originals: Vec<(String, Vec<u8>)>,
        records: Vec<(String, Value)>,
    }
    #[async_trait]
    impl Journal for MemoryJournal {
        async fn append(&mut self, kind: &str, bytes: &[u8]) -> Result<(), String> {
            self.originals.push((kind.into(), bytes.into()));
            Ok(())
        }
        async fn record(&mut self, key: &str, value: &Value) -> Result<(), String> {
            self.records.push((key.into(), value.clone()));
            Ok(())
        }
    }
    struct MemoryTransport {
        answers: VecDeque<Native>,
        effects: Vec<Effect>,
    }
    #[async_trait]
    impl Transport for MemoryTransport {
        async fn perform(&mut self, effect: Effect, _: Duration) -> Result<Native, String> {
            self.effects.push(effect);
            self.answers.pop_front().ok_or("unexpected effect".into())
        }
    }
    fn native(value: Value) -> Native {
        Native {
            stdout: serde_json::to_vec(&value).unwrap(),
            stderr: Vec::new(),
            exit: Some(0),
        }
    }
    pub(super) fn prepared() -> Prepared {
        Prepared {
            run: "61be2c4a-68ee-4156-9709-7dc1ebe89b16".into(),
            observer: "controller".into(),
            actor: "agent-codex".into(),
            image: format!("registry.invalid/agent@sha256:{}", "a".repeat(64)),
            login: "fixture-login".into(),
            bound: Duration::from_secs(60),
            grace: Duration::from_millis(100),
            snapshot: Snapshot {
                commit: "b".repeat(40),
                main: "c".repeat(40),
                tree: "d".repeat(40),
                sha256: crate::launch_controller::hash(b"bundle"),
                bytes: 6,
                members_sha256: hash(b"members"),
                members_bytes: 7,
            },
            bundle: b"bundle".to_vec(),
            prompt: b"exact START\n".to_vec(),
            argv: vec!["/usr/local/bin/codex".into(), "-".into()],
            home_env: "CODEX_HOME".into(),
            login_file: "fixture-auth.json".into(),
            runtime_event: "thread.started".into(),
            runtime_key: "thread_id".into(),
            completed_event: "turn.completed".into(),
            transcript_pattern: "sessions/*/*/*/rollout-*-{runtime}.jsonl".into(),
        }
    }
    pub(super) fn objects(p: &Prepared) -> (Value, Value, Value) {
        let spec = launch_job::Spec {
            run_id: &p.run,
            image: &p.image,
            login_secret: &p.login,
            bound: p.bound,
            grace: p.grace,
        };
        let mut pvc = launch_job::workspace_manifest(&spec);
        pvc["metadata"]["uid"] = json!("pvc-uid");
        pvc["metadata"]["resourceVersion"] = json!("1");
        let mut job = launch_job::manifest(&spec).unwrap();
        job["metadata"]["uid"] = json!("job-uid");
        job["metadata"]["resourceVersion"] = json!("1");
        let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"pod","namespace":"boss-dev","uid":"pod-uid","resourceVersion":"1",
            "labels":job["spec"]["template"]["metadata"]["labels"],"ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","name":job["metadata"]["name"],"uid":"job-uid","controller":true}]},
            "spec":job["spec"]["template"]["spec"],"status":{"phase":"Running","containerStatuses":[{"name":"agent","containerID":"containerd://one","restartCount":0,"ready":true,"state":{"running":{}}}]}});
        let pods = json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1","continue":""},"items":[pod]});
        (pvc, job, pods)
    }
    fn successful_answers(p: &Prepared) -> Vec<Native> {
        let (pvc, job, pods) = objects(p);
        let acknowledgment = json!({"schema":1,"run":p.run,"prompt_sha256":hash(&p.prompt),"snapshot_sha256":p.snapshot.sha256,"start_read":true});
        let mut archive = json!({"schema":1,"runtime":"runtime-one","exit":0,"completed":true,"ending":"native_exit","artifacts":{}});
        use base64::Engine;
        for (key, bytes) in [
            (
                "stdout",
                "{\"type\":\"thread.started\",\"thread_id\":\"runtime-one\"}\n{\"type\":\"turn.completed\"}\n",
            ),
            ("stderr", ""),
            ("last_message", "done"),
            ("transcript", "original native transcript"),
        ] {
            archive["artifacts"][key] = json!({"data":base64::engine::general_purpose::STANDARD.encode(bytes),"bytes":bytes.len(),"sha256":hash(bytes.as_bytes())});
        }
        let mut answers = vec![
            native(pvc.clone()),
            native(job.clone()),
            native(job.clone()),
            native(pods.clone()),
        ];
        for result in [
            json!({"pod_uid":"pod-uid","pid1_start_ticks":1,"pid_namespace_inode":2,"boot_id":"fixture-boot"}),
            json!({"sha256":p.snapshot.sha256,"bytes":p.snapshot.bytes}),
            json!({"sha256":hash(&p.prompt),"bytes":p.prompt.len()}),
            acknowledgment,
            archive,
        ] {
            answers.extend([
                native(job.clone()),
                native(pods.clone()),
                native(result),
                native(job.clone()),
                native(pods.clone()),
            ]);
        }
        answers.extend([native(json!({"kind":"Status","status":"Success","code":200})),native(json!({"kind":"Status","reason":"NotFound","code":404})),native(json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"2","continue":""},"items":[]}))]);
        answers
    }
    #[tokio::test]
    async fn managed_login_refuses_before_compute_without_a_private_cache_lease() {
        let mut p = prepared();
        p.home_env = "CODEX_HOME".into();
        p.login_file = "auth.json".into();
        let mut transport = MemoryTransport {
            answers: successful_answers(&p).into(),
            effects: vec![],
        };
        let mut journal = MemoryJournal::default();
        assert!(run(&p, &mut transport, &mut journal).await.is_err());
        assert!(transport.effects.is_empty(), "no lease means no compute");
    }

    // Gate 8c300 failed this in-memory success path. Its 100ms cleanup
    // grace must measure virtual I/O, not the host's scheduling delays.
    #[tokio::test(start_paused = true)]
    async fn controller_creates_observes_transfers_and_stops_one_uid_without_worker_authority() {
        let p = prepared();
        let mut t = MemoryTransport {
            answers: successful_answers(&p).into(),
            effects: vec![],
        };
        let mut j = MemoryJournal::default();
        let outcome = run(&p, &mut t, &mut j).await.unwrap();
        assert!(outcome.stop_observed);
        assert!(outcome.evidence_complete);
        assert!(outcome.workspace_retained);
        assert!(j.records.iter().any(|(k, v)| k == "observer_started"
            && v["observer"] == "controller"
            && v["actor"] == "agent-codex"));
        assert!(t.effects.iter().any(|e|matches!(e,Effect::DeleteJob {options,..} if options["preconditions"]["uid"]=="job-uid")));
        assert!(!j.originals.is_empty());
        assert!(t.answers.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_cleanup_still_expires_at_the_unchanged_grace() {
        struct Stalled;
        #[async_trait]
        impl Transport for Stalled {
            async fn perform(&mut self, _: Effect, _: Duration) -> Result<Native, String> {
                std::future::pending().await
            }
        }
        let p = prepared();
        let (_, job, _) = objects(&p);
        let before = tokio::time::Instant::now();
        let result = stop_known(&p, &job, &mut Stalled, &mut MemoryJournal::default())
            .await
            .unwrap();
        assert!(!result.stop_observed);
        assert_eq!(before.elapsed(), p.grace);
    }

    #[tokio::test(start_paused = true)]
    async fn memory_success_does_not_spend_cleanup_grace_on_host_scheduling() {
        struct Delayed(MemoryTransport);
        #[async_trait]
        impl Transport for Delayed {
            async fn perform(&mut self, effect: Effect, bound: Duration) -> Result<Native, String> {
                if matches!(effect, Effect::DeleteJob { .. }) {
                    // Model a descheduled test thread, not native I/O latency.
                    std::thread::sleep(Duration::from_millis(150));
                }
                self.0.perform(effect, bound).await
            }
        }
        let p = prepared();
        let mut transport = Delayed(MemoryTransport {
            answers: successful_answers(&p).into(),
            effects: vec![],
        });
        let result = run(&p, &mut transport, &mut MemoryJournal::default())
            .await
            .unwrap();
        assert!(result.stop_observed);
        assert!(transport.0.answers.is_empty());
    }

    #[tokio::test]
    async fn every_failed_or_unknown_create_read_transfer_tail_is_not_success_and_keeps_original_error_bytes()
     {
        let p = prepared();
        for index in 0..successful_answers(&p).len() - 3 {
            for exit in [Some(9), None] {
                let mut answers = successful_answers(&p);
                let error = format!("original error at native observation {index}").into_bytes();
                answers[index].exit = exit;
                answers[index].stderr = error.clone();
                let mut t = MemoryTransport {
                    answers: answers.into(),
                    effects: vec![],
                };
                let mut j = MemoryJournal::default();
                let result = run(&p, &mut t, &mut j).await;
                assert!(
                    !result.as_ref().is_ok_and(|r| r.evidence_complete),
                    "observation{index} {exit:?} falsely completed"
                );
                assert!(
                    j.originals.iter().any(|(_, bytes)| bytes == &error),
                    "original diagnostic missing at{index}"
                );
            }
        }
    }

    #[tokio::test]
    async fn supervisor_replacement_and_incomplete_pod_pages_never_open_input() {
        for (index, patch) in [
            (
                6,
                json!({"pod_uid":"replacement","pid1_start_ticks":1,"pid_namespace_inode":2,"boot_id":"boot"}),
            ),
            (
                3,
                json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1","continue":"next"},"items":[]}),
            ),
        ] {
            let p = prepared();
            let mut answers = successful_answers(&p);
            answers[index] = native(patch);
            let mut t = MemoryTransport {
                answers: answers.into(),
                effects: vec![],
            };
            let mut j = MemoryJournal::default();
            let result = run(&p, &mut t, &mut j).await;
            assert!(!result.as_ref().is_ok_and(|r| r.evidence_complete));
            assert!(
                !t.effects
                    .iter()
                    .any(|e| matches!(e,Effect::Exec {operation,..} if operation!="identity"))
            );
            if index == 3 {
                assert!(
                    matches!(&t.effects[4], Effect::DeleteJob { .. }),
                    "an incomplete list was retried as scheduling"
                );
            }
        }
    }

    #[tokio::test]
    async fn interrupted_cleanup_reuses_only_the_original_native_uid_and_observes_stop() {
        let p = prepared();
        let (_, job, _) = objects(&p);
        let mut transport=MemoryTransport {answers:vec![
            native(json!({"kind":"Status","status":"Success","code":200})),
            native(json!({"apiVersion":"batch/v1","kind":"JobList","metadata":{"resourceVersion":"3","continue":""},"items":[]})),
            native(json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"3","continue":""},"items":[]})),
        ].into(),effects:vec![]};
        let mut journal = MemoryJournal::default();
        let result = stop_known(&p, &job, &mut transport, &mut journal)
            .await
            .unwrap();
        assert!(result.stop_observed);
        assert!(!result.evidence_complete);
        assert!(result.workspace_retained);
        assert!(
            matches!(&transport.effects[0],Effect::DeleteJob {options,..} if options["preconditions"]["uid"]=="job-uid")
        );
        assert_eq!(transport.effects.len(), 3);
    }

    #[tokio::test]
    async fn recovery_collects_available_bytes_without_login_or_rewriting_interrupted_outcome() {
        let p = prepared();
        let (pvc, original, _) = objects(&p);
        let spec = launch_job::Spec {
            run_id: &p.run,
            image: &p.image,
            login_secret: &p.login,
            bound: p.bound,
            grace: p.grace,
        };
        let mut collector = launch_job::recovery_manifest(&spec).unwrap();
        collector["metadata"]["uid"] = json!("collector-uid");
        collector["metadata"]["resourceVersion"] = json!("1");
        let mut pod = objects(&p).2["items"][0].clone();
        pod["metadata"]["ownerReferences"][0]["name"] = collector["metadata"]["name"].clone();
        pod["metadata"]["ownerReferences"][0]["uid"] = json!("collector-uid");
        pod["metadata"]["labels"] = collector["spec"]["template"]["metadata"]["labels"].clone();
        pod["spec"] = collector["spec"]["template"]["spec"].clone();
        let pods = json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1"},"items":[pod]});
        let empty_job = json!({"apiVersion":"batch/v1","kind":"JobList","metadata":{"resourceVersion":"2"},"items":[]});
        let empty_pods = json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"2"},"items":[]});
        let success = json!({"kind":"Status","status":"Success","code":200});
        let mut answers = vec![
            native(success.clone()),
            native(empty_job.clone()),
            native(empty_pods.clone()),
            native(pvc.clone()),
            native(collector.clone()),
            native(collector.clone()),
            native(pods.clone()),
        ];
        use base64::Engine;
        let bytes = b"original partial error\x00\xff";
        let recovered = json!({"schema":1,"completion":"interrupted","missing":{"transcript":"not retained"},"artifacts":{"stderr":{"data":base64::engine::general_purpose::STANDARD.encode(bytes),"bytes":bytes.len(),"sha256":hash(bytes)}}});
        for value in [
            json!({"pod_uid":"pod-uid","pid1_start_ticks":1,"pid_namespace_inode":2,"boot_id":"fixture"}),
            recovered,
        ] {
            answers.extend([
                native(collector.clone()),
                native(pods.clone()),
                native(value),
                native(collector.clone()),
                native(pods.clone()),
            ]);
        }
        answers.extend([
            native(pvc.clone()),
            native(success),
            native(empty_job),
            native(empty_pods),
        ]);
        let mut t = MemoryTransport {
            answers: answers.into(),
            effects: vec![],
        };
        let mut j = MemoryJournal::default();
        let receipt = recover(&p, &original, &pvc, &mut t, &mut j).await.unwrap();
        assert_eq!(receipt["collector_stopped"], true);
        assert_eq!(receipt["workspace_retained"], true);
        assert!(
            j.originals
                .iter()
                .any(|(kind, raw)| kind == "recovered_stderr" && raw == bytes)
        );
        assert!(t.answers.is_empty());
        assert!(
            !t.effects
                .iter()
                .any(|e| matches!(e,Effect::Exec {operation,..} if operation=="start"))
        );
        assert!(
            t.effects
                .iter()
                .filter_map(|e| if let Effect::CreateJob(job) = e {
                    Some(job)
                } else {
                    None
                })
                .all(|job| !job["spec"]["template"]["spec"]["volumes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.get("secret").is_some()))
        );
    }
}
