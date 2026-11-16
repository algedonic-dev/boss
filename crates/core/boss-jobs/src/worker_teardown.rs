//! Fixed physical-return adapter. Request metadata and attachments never
//! supply an obligation or select a verifier; the server loads both.
use std::collections::BTreeMap;

use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use boss_core::job::{Job, JobId, Step, StepStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    JobsRepository,
    pools::{PoolKind, PoolSpec},
    port::JobsError,
    registry::{WorkflowRegistry, WorkflowSpec},
    resource_leases::{ResourceReturnVerifier, StockObligation, VerifiedStockReturn},
};

const VERB: &str = "worker-teardown-witness";
const SPEC: &[u8] = include_bytes!("../../../../infra/ops/verbs/worker-teardown-witness.json");
fn refused(reason: &str) -> JobsError {
    crate::resource_leases::error(&format!("worker-teardown-unqualified: {reason}"))
}
fn hash(raw: &[u8]) -> String {
    hex::encode(Sha256::digest(raw))
}

/// One frozen tuple, shared with the exact argv qualification below.
pub fn target(o: &StockObligation) -> Result<Value, JobsError> {
    let i = o
        .identity
        .as_ref()
        .ok_or_else(|| refused("resource identity unbound"))?;
    Ok(
        json!({"namespace":i.namespace,"job_uid":i.job_uid,"pvc_uid":i.pvc_uid,"run_id":i.run_id,
        "acquisition":o.claim.id,"generation":o.claim.generation,"pool":o.allocation.pool,
        "pool_version":o.allocation.version,"amount":o.allocation.amount,"reclaimer":o.reclaimer}),
    )
}
pub fn argv(o: &StockObligation) -> Result<Vec<String>, JobsError> {
    let value = target(o)?;
    let mut args = vec![
        "bash".into(),
        "infra/forge/worker-teardown-witness.sh".into(),
    ];
    for key in [
        "namespace",
        "job_uid",
        "pvc_uid",
        "run_id",
        "acquisition",
        "generation",
        "pool",
        "pool_version",
        "amount",
        "reclaimer",
    ] {
        let value = &value[key];
        args.push(match value.as_str() {
            Some(s) => s.to_owned(),
            None => value.to_string(),
        });
    }
    Ok(args)
}
/// Compile-time source identity, not a caller's claim about installed code.
pub fn source_hashes() -> BTreeMap<&'static str, String> {
    [
        (
            "worker-teardown-witness.py",
            include_bytes!("../../../../infra/forge/worker-teardown-witness.py").as_slice(),
        ),
        (
            "worker-teardown-witness.sh",
            include_bytes!("../../../../infra/forge/worker-teardown-witness.sh").as_slice(),
        ),
        (
            "gate-storage.py",
            include_bytes!("../../../../infra/forge/gate-storage.py").as_slice(),
        ),
        (
            "volume-snapshots.py",
            include_bytes!("../../../../infra/forge/volume-snapshots.py").as_slice(),
        ),
    ]
    .into_iter()
    .map(|(name, raw)| (name, hash(raw)))
    .collect()
}
fn channel(value: &Value) -> Result<Vec<u8>, JobsError> {
    if value["complete"] != true {
        return Err(refused("incomplete native stream"));
    }
    let raw = STANDARD
        .decode(
            value["base64"]
                .as_str()
                .ok_or_else(|| refused("stream bytes absent"))?,
        )
        .map_err(|_| refused("invalid stream encoding"))?;
    if raw.len() > 102400
        || value["bytes"].as_u64() != Some(raw.len() as u64)
        || value["retained_bytes"].as_u64() != Some(raw.len() as u64)
        || value["sha256"].as_str() != Some(hash(&raw).as_str())
    {
        return Err(refused("native stream byte/hash conservation failed"));
    }
    Ok(raw)
}

pub fn qualify_operation(
    o: &StockObligation,
    pool: &PoolSpec,
    job: &Job,
    execute: &Step,
    protocol: &WorkflowSpec,
) -> Result<VerifiedStockReturn, JobsError> {
    if pool.name != o.allocation.pool
        || pool.version != o.allocation.version
        || !pool.status.was_published()
        || pool.kind != PoolKind::Stock
        || pool.reclaimer.as_deref() != Some(o.reclaimer.as_str())
        || pool.return_verifier.as_deref() != Some("worker-teardown")
        || pool.program.is_none()
        || pool.identity_pin_unenforced.is_some()
    {
        return Err(refused(
            "pinned pool return contract or executor identity unavailable",
        ));
    }
    if job.kind != "ops-request"
        || job.subject.id != "forge"
        || protocol.kind != job.kind
        || protocol.version != job.workflow_version
        || !protocol.status.was_published()
        || execute.job_id != job.id
        || execute.spec_slug.as_deref() != Some("execute")
        || execute.status != StepStatus::Completed
        || execute
            .completed_by
            .as_ref()
            .map(ToString::to_string)
            .as_deref()
            != Some(crate::runner_credential::ACTOR)
    {
        return Err(refused(
            "not a completed credentialed operation on its published pinned protocol",
        ));
    }
    let specs: Vec<_> = protocol
        .steps
        .iter()
        .filter(|s| s.title == "execute")
        .collect();
    if specs.len() != 1 || specs[0].executor.as_deref() != Some("runner:ops") {
        return Err(refused("pinned execute credential declaration absent"));
    }
    for key in ["disposition", "output", "exit_code", "executed"] {
        let fields: Vec<_> = specs[0].fields.iter().filter(|f| f.name == key).collect();
        if fields.len() != 1 || fields[0].writer.as_deref() != Some("runner:ops") {
            return Err(refused("pinned execute evidence is not runner-owned"));
        }
    }
    let md = &execute.metadata;
    let expected =
        json!({"verb":VERB,"host":"forge","verb_file_sha256":hash(SPEC),"argv":argv(o)?});
    if md["executed"] != expected
        || md["disposition"] != "answered"
        || md["exit_code"] != "0"
        || md["runner_host"] != "forge"
        || !md["streams_unread"].is_null()
    {
        return Err(refused("actual executed envelope, exit or capture differs"));
    }
    let out = channel(&md["streams"]["stdout"])?;
    let err = channel(&md["streams"]["stderr"])?;
    let mut combined = out.clone();
    combined.extend(err);
    if md["output"].as_str().map(str::as_bytes) != Some(combined.as_slice()) {
        return Err(refused("protected output differs from native channels"));
    }
    let record: Value =
        serde_json::from_slice(&out).map_err(|_| refused("native witness JSON unread"))?;
    if record["schema"] != 1
        || record["claim"] != "worker-teardown-only"
        || record["state"] != "absent"
        || record["target"] != target(o)?
        || record["request"] != job.id.to_string()
        || record["errors"] != json!([])
        || record["source"] != json!(source_hashes())
        || !record["manifest_attachment_receipt"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty() && s.len() <= 8192)
        || record["observed_at"]
            .as_str()
            .is_none_or(|s| chrono::DateTime::parse_from_rfc3339(s).is_err())
    {
        return Err(refused(
            "witness target, source, durability or outcome differs",
        ));
    }
    for key in ["namespace", "service_account"] {
        if record["controls"][key]
            .as_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .is_none_or(|id| id.is_nil())
        {
            return Err(refused("known-positive connection identity unavailable"));
        }
    }
    for key in ["jobs", "pvcs"] {
        let inventory = &record["inventories"][key];
        if inventory["complete"] != true
            || !inventory["pages"]
                .as_u64()
                .is_some_and(|n| (1..=32).contains(&n))
            || inventory["count"].as_u64().is_none_or(|n| n > 512)
            || !inventory["resource_version"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && s.len() <= 128)
        {
            return Err(refused("complete bounded UID inventory unavailable"));
        }
    }
    Ok(VerifiedStockReturn::from_qualified_operation(
        o.clone(),
        *job.id.inner().as_uuid(),
    ))
}

/// A routing hint only. The selected pool/obligation must still pass the
/// complete protected-operation qualification before any ledger write.
pub(crate) fn operation_pool(execute: &Step) -> Result<String, JobsError> {
    let raw = channel(&execute.metadata["streams"]["stdout"])?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| refused("native witness JSON unread"))?;
    value["target"]["pool"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
        .ok_or_else(|| refused("witness pool absent"))
}

/// Only the server installs this adapter; callers supply an operation id.
pub struct WorkerTeardownVerifier<'a, R: JobsRepository> {
    pub jobs: &'a R,
    pub workflows: &'a dyn WorkflowRegistry,
    pub pool: &'a PoolSpec,
}
#[async_trait]
impl<R: JobsRepository> ResourceReturnVerifier for WorkerTeardownVerifier<'_, R> {
    async fn verify(
        &self,
        obligation: &StockObligation,
        evidence: uuid::Uuid,
    ) -> Result<VerifiedStockReturn, JobsError> {
        let id = JobId::from_uuid(evidence);
        let job = self
            .jobs
            .get_job(&id)
            .await?
            .ok_or_else(|| refused("operation absent"))?;
        let steps = self.jobs.list_steps(&id).await?;
        let executes: Vec<_> = steps
            .iter()
            .filter(|s| s.spec_slug.as_deref() == Some("execute"))
            .collect();
        if executes.len() != 1 {
            return Err(refused("operation execute identity ambiguous"));
        }
        let spec = self
            .workflows
            .get_version(&job.kind, job.workflow_version)
            .await
            .map_err(|e| refused(&e.to_string()))?;
        qualify_operation(obligation, self.pool, &job, executes[0], &spec)
    }
}
