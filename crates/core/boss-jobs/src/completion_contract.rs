//! Registry-declared evidence contracts run on every public completion door.
//! The declaration is read from the packet's pinned protocol, never its metadata.
use crate::{port::JobsRepository, registry::WorkflowRegistry};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use boss_core::job::{Job, Step, StepStatus};
use std::{collections::BTreeMap, sync::Arc};

#[async_trait]
pub trait CompletionContract: Send + Sync {
    async fn validate(
        &self,
        job: &Job,
        step: &Step,
        actor: &boss_policy_client::User,
    ) -> Result<()>;
}

pub type Contracts = BTreeMap<String, Arc<dyn CompletionContract>>;

/// A read the contract could not make — not a judgment of the evidence.
/// The contract reads the packet's pinned protocol (and, at a close, its
/// steps) BEFORE anything is written, so an unreadable one decides nothing:
/// the doors answer it 500 naming the packet and the read, and keep 409 for
/// a contract that was read and refused (review 5e15da5b F1, closing review
/// 8c30cbc7 N2 — a storage error used to read "409 completion contract",
/// naming no packet, as if the evidence had failed).
#[derive(Debug, thiserror::Error)]
pub enum Unread {
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    Steps(String),
}

/// The read that failed, when `why` is one rather than a refusal.
pub fn unread(why: &anyhow::Error) -> Option<&Unread> {
    why.downcast_ref()
}

pub async fn declaration(
    registry: Option<&Arc<dyn WorkflowRegistry>>,
    job: &Job,
    step: &Step,
) -> Result<Option<String>> {
    let Some(registry) = registry else {
        ensure!(
            step.metadata.get("completion_contract").is_none(),
            "pinned completion contract unavailable"
        );
        return Ok(None);
    };
    let spec = match registry.get_version(&job.kind, job.workflow_version).await {
        Ok(spec) => spec,
        Err(crate::registry::WorkflowError::NotFound(_))
            if step.metadata.get("completion_contract").is_none() =>
        {
            return Ok(None);
        }
        Err(why) => return Err(Unread::Protocol(why.to_string()).into()),
    };
    let Some(definition) = spec
        .steps
        .iter()
        .find(|s| Some(s.title.as_str()) == step.spec_slug.as_deref())
    else {
        // An ad-hoc step cannot acquire a protocol terminal by choosing its title.
        ensure!(
            step.metadata.get("completion_contract").is_none(),
            "step has no pinned completion contract"
        );
        return Ok(None);
    };
    match definition.metadata_defaults.get("completion_contract") {
        None => Ok(None),
        Some(value) => Ok(Some(
            value
                .as_str()
                .filter(|s| !s.is_empty())
                .context("invalid pinned completion contract")?
                .to_owned(),
        )),
    }
}

pub async fn validate(
    registry: Option<&Arc<dyn WorkflowRegistry>>,
    contracts: &Contracts,
    job: &Job,
    step: &Step,
    actor: &boss_policy_client::User,
) -> Result<()> {
    if !matches!(step.status, StepStatus::Completed | StepStatus::Skipped) {
        return Ok(());
    }
    ensure!(
        registry.is_some() || contracts.is_empty(),
        "pinned protocol reader unavailable"
    );
    let Some(name) = declaration(registry, job, step).await? else {
        return Ok(());
    };
    ensure!(
        step.status == StepStatus::Completed,
        "a contracted completion cannot be skipped"
    );
    contracts
        .get(&name)
        .context("completion contract adapter unavailable")?
        .validate(job, step, actor)
        .await
}

/// A whole-job PUT must not synthesize the outcome of a protected terminal.
pub async fn validate_close<R: JobsRepository>(
    registry: Option<&Arc<dyn WorkflowRegistry>>,
    contracts: &Contracts,
    jobs: &R,
    job: &Job,
    actor: &boss_policy_client::User,
) -> Result<()> {
    let Some(outcome) = job
        .metadata
        .get("outcome")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    let Some(registry) = registry else {
        ensure!(contracts.is_empty(), "pinned protocol reader unavailable");
        return Ok(());
    };
    let spec = match registry.get_version(&job.kind, job.workflow_version).await {
        Ok(spec) => spec,
        Err(crate::registry::WorkflowError::NotFound(_)) => return Ok(()),
        Err(why) => return Err(Unread::Protocol(why.to_string()).into()),
    };
    for definition in &spec.steps {
        if definition
            .terminal
            .as_ref()
            .is_some_and(|t| t.outcome == outcome)
            && definition
                .metadata_defaults
                .get("completion_contract")
                .is_some()
        {
            let steps = jobs
                .list_steps(&job.id)
                .await
                .map_err(|why| Unread::Steps(why.to_string()))?;
            let step = steps
                .iter()
                .find(|s| s.spec_slug.as_deref() == Some(definition.title.as_str()))
                .context("protected terminal missing")?;
            if step.status != StepStatus::Completed {
                bail!("protected outcome requires its completed typed terminal");
            }
            validate(Some(registry), contracts, job, step, actor).await?;
        }
    }
    Ok(())
}
